//! Arena death with promotion: the reset-time consumer of the escapee
//! list and its hold-counts (`rfc/model/memory/arena-reset.md`).
//!
//! Phase 1 implements **retention only** — the safe default and,
//! per the RFC, the whole of the first implementation: no copying, no
//! identity machinery, no reference fixup. Sparse-block evacuation is
//! purely additive and lands later.
//!
//! The algorithm:
//!
//! 1. **Fixpoint** over the destructor log and the escapee list: from the
//!    escapees whose hold-count is still non-zero, mark the surviving
//!    subgraph, then run pre-destructors of dying, unescaped objects.
//!    Destructors run PHP code: they may create new escapes (bumping
//!    counts, appending to the list) and track new destructors — hence
//!    the loop. No holder slot is ever dereferenced, so a holder that died
//!    before now cannot dangle the reset (the remembered-set bug this
//!    replaces).
//! 2. **Count**: external references are already each root's `refcount`
//!    (its `IS_ESCAPEE` hold-count, kept live by the barrier and holder
//!    teardown); this pass only adds internal edges between survivors and
//!    one compensating retain per heap entity a survivor holds (its
//!    release-at-reset record assumed the holder would die; the survivor
//!    now owes its own release at its real death).
//! 3. **Retain blocks** carrying survivors: rewrite each survivor's
//!    category to GcHeap in place (the pointer-tag alternative was
//!    rejected exactly because this rewrite must be possible), stamp
//!    the blocks `BLOCK_KIND_RETAINED`, keep them out of the pool.
//! 4. Release-at-reset log: one release per record, with real teardown
//!    dispatch for entities that die of it.

use std::collections::{HashMap, HashSet};

use crate::memory::arena::Arena;
use crate::memory::block_pool::{BLOCK_KIND_RETAINED, BlockHeader};
use crate::object::Object;
use crate::refcount::{
    ARENA_RESET_MARK, IS_ESCAPEE, MEMORY_CATEGORY_MASK, MemoryCategory, RcHeader, is_object,
    ll_release, ll_retain,
};

/// Recursion guard for the reset fixpoint. Pure and non-recreating
/// destructors converge in rounds bounded by the object count; this caps
/// the pathological case (a destructor that endlessly creates new
/// destructor-bearing objects). Hitting it is an error, not a silent drop
/// of the un-settled tail — dropping it would dangle
/// (`rfc/model/memory/arena-reset.md`, "Recursion bound").
const ARENA_RESET_MAX_ROUNDS: usize = 10_000;

/// Full arena death: fixpoint, promotion by retention, deferred
/// releases, blocks home. Replaces bare `Arena::reset` wherever the
/// object model is in play.
///
/// `arena` is a raw pointer for the reason this whole function exists:
/// it runs `__destruct` bodies, and those reenter the runtime and
/// resolve this same arena to allocate, log escapes, or track more
/// destructors. An exclusive borrow held across the settling loop would
/// alias every one of those reentrant uses (audit H5). So each arena
/// operation below takes its own short-lived borrow, and **no borrow is
/// ever live across a call that can run user code** — which is also why
/// the drains collect first and act afterwards, rather than doing the
/// work inside the drain closure.
///
/// # Safety
/// The arena must not be reachable by running PHP code anymore (no
/// live stack); destructors invoked here may still allocate into it.
pub unsafe fn arena_reset_full(arena: *mut Arena) {
    let mut survivors: Vec<*mut RcHeader> = Vec::new();
    let mut retained: HashSet<usize> = HashSet::new();
    // `survivors[..counted]` have already been counted and retained. New
    // survivors past it are the current round's delta.
    let mut counted = 0usize;
    let mut rounds = 0usize;

    // The whole reset is one settling loop — no separate "release tail".
    // Each pass settles the arena side (surviving escapees, the destructors
    // of the dying, and survivor re-traces), counts and retains what it
    // found, then runs the deferred releases. A release runs teardown that
    // can create more work (a released entity's `__destruct`, run while it
    // is still alive, may escape or allocate), so we loop until a pass
    // releases nothing. The recursion cap is the only backstop.
    loop {
        // --- Settle: escapees, dying destructors, survivor re-trace (H2).
        // Every `__destruct` runs here, with its object still fully alive;
        // nothing is freed until the survivor set below is final.
        loop {
            let mut progress = false;

            let mut round = Vec::new();
            unsafe { (*arena).drain_escapees(|e| round.push(e)) };
            for a in round {
                progress = true;
                // Count back to zero (every holder let go): survives only if
                // an internal edge reaches it — the subgraph trace covers it.
                if unsafe { (*a).flags } & IS_ESCAPEE == 0 {
                    continue;
                }
                unsafe { mark_subgraph(a, &mut survivors) };
            }

            // Bump cursor moved ⇒ a destructor allocated ("dirty"): it may
            // have stored a fresh arena object into an already-traced
            // survivor (arena→arena, not an escape), so re-read survivors'
            // children (audit H2). A "pure" destructor needs no re-trace —
            // the runtime stand-in for the compile-time purity class.
            let before = unsafe { (*arena).bump_cursor() };
            let mut round_dtors = Vec::new();
            unsafe { (*arena).drain_destructors(|o| round_dtors.push(o)) };
            for obj in round_dtors {
                progress = true;
                if unsafe { (*obj).flags } & ARENA_RESET_MARK != 0 {
                    continue; // escaped objects survive; they do not destruct
                }
                unsafe { crate::object::run_pre_destructor(obj as *mut Object) };
            }
            if unsafe { (*arena).bump_cursor() } != before {
                unsafe { retrace_survivors(&mut survivors) };
            }

            if !progress {
                break;
            }
            rounds += 1;
            assert!(rounds <= ARENA_RESET_MAX_ROUNDS, "arena reset did not converge");
        }

        // --- Count + retain the new survivors, BEFORE any release. Their
        // compensating retains for held heap entities must land before the
        // matching release-log releases, or a heap child could hit zero and
        // free early. External refs are already the IS_ESCAPEE hold-count;
        // this adds internal arena→arena edges and those compensations.
        for &surv in &survivors[counted..] {
            unsafe { count_children(surv) };
        }
        for &surv in &survivors[counted..] {
            unsafe {
                // 00 = GcHeap; drop the transient arena-reset mark and
                // IS_ESCAPEE. The mark lives in the GC-state field, so
                // clearing it also leaves the promoted object's GC state at
                // 00 (LIVE), the correct fresh heap state.
                (*surv).flags &= !(MEMORY_CATEGORY_MASK | ARENA_RESET_MARK | IS_ESCAPEE);
            }
            let block = BlockHeader::of_ptr(surv as *const u8) as usize;
            if retained.insert(block) {
                unsafe { (*(block as *mut BlockHeader)).kind = BLOCK_KIND_RETAINED };
            }
        }
        counted = survivors.len();

        // --- Deferred releases. Teardown here (destructor first, then free)
        // may create new work; a new escape settles as an ordinary escape
        // next pass (the survivor it stored into is GcHeap by now). Loop
        // while the log yields anything.
        // Collect the round, then release it: `die` runs `__destruct`,
        // which reenters and resolves this same arena, so the drain's
        // borrow must already be gone by then (audit H5). Entries those
        // destructors append stay in the log and the next pass takes
        // them — the same settling the loop already relies on (H7).
        let mut round_releases = Vec::new();
        unsafe { (*arena).drain_release_log(|entity| round_releases.push(entity)) };
        if round_releases.is_empty() {
            break;
        }
        for entity in round_releases {
            unsafe {
                if ll_release(entity) {
                    die(entity);
                }
            }
        }
        rounds += 1;
        assert!(rounds <= ARENA_RESET_MAX_ROUNDS, "arena reset did not converge");
    }

    // The weak walk — after every destructor has settled and the
    // survivors' categories are rewritten, before the pages go back:
    // dying entries get their cells nulled, promoted survivors are
    // recognized by their new category and keep resolving
    // (`rfc/model/weak-references.md`, "Death notification"). Runs no
    // user code, so it cannot grow the logs behind the settled fixpoint.
    unsafe { crate::weak::drain_arena_weak_log(arena) };

    // The survivor list becomes the retained blocks' object index. A
    // bump-filled block has no stride to divide by, so this inventory
    // is the only way the walk can enumerate its occupants — without it
    // they are root sources and a ring among them never dies
    // (`rfc/model/gc/retained-block-walk.md`). Registered after the
    // fixpoint has settled and before the blocks are disposed of, so
    // every entry describes a survivor that is staying.
    index_retained_blocks(&survivors);

    unsafe { (*arena).finish_reset(|block| retained.contains(&(block as usize))) };
}

/// Group the settled survivors by the block holding them and hand each
/// group to the retained-index registry.
///
/// One index per block rather than one per reset: both enumerators
/// reach a block first — the census by the 64 KiB alignment mask, the
/// synchronous walk by scanning the region registry — so an index found
/// from a block address costs no second mapping (`dev/DECISIONS.md`,
/// 2026-08-03). A survivor whose block was *not* retained cannot occur
/// here: retention is decided from this same list.
fn index_retained_blocks(survivors: &[*mut RcHeader]) {
    let mut by_block: HashMap<usize, Vec<usize>> = HashMap::new();
    for &surv in survivors {
        let block = BlockHeader::of_ptr(surv as *const u8) as usize;
        by_block.entry(block).or_default().push(surv as usize);
    }
    for (block, occupants) in by_block {
        crate::memory::retained::register(block, occupants);
    }
}

/// Entity teardown dispatch from a bare header — the uniform kind
/// switch. Went through `ll_object_die` directly until weak cells and
/// reference boxes could land in the release log; the kind switch frees
/// them correctly (a bare `ll_object_die` on a kind-3/5 entity would
/// read a class pointer that is not there).
unsafe fn die(entity: *mut RcHeader) {
    unsafe { crate::object::ll_entity_die(entity) };
}

#[inline]
unsafe fn is_arena_entity(p: *mut RcHeader) -> bool {
    !p.is_null() && unsafe { (*p).memory_category() } == MemoryCategory::RequestArena
}

/// Mark the surviving subgraph from one escapee root: the root and
/// everything it references transitively inside the arena. A non-root
/// survivor (reached only by an internal edge) has its count zeroed at
/// first mark so the counting pass can rebuild it from edges; a root keeps
/// its `refcount` — that is already its external hold-count.
unsafe fn mark_subgraph(root: *mut RcHeader, survivors: &mut Vec<*mut RcHeader>) {
    if !unsafe { is_arena_entity(root) } {
        return; // stale entry: overwritten or never an arena value
    }
    let mut stack = Vec::new();
    unsafe { mark_one(root, survivors, &mut stack) };

    while let Some(obj) = stack.pop() {
        unsafe {
            crate::object::for_each_counted_child(obj, |child| {
                if is_arena_entity(child) {
                    mark_one(child, survivors, &mut stack);
                }
            });
        }
    }
}

/// Re-read every survivor's current children and mark any newly-appeared
/// arena child. A destructor may have stored a fresh arena object into an
/// already-traced survivor — an arena→arena store the barrier does not
/// escape — so that child would otherwise be missed and dangle once the
/// survivor is promoted (audit H2). Cheap when nothing changed: an
/// already-marked child is skipped by the arena-reset-mark test. The index
/// walk (not an iterator) re-scans survivors appended by `mark_subgraph`
/// mid-loop.
unsafe fn retrace_survivors(survivors: &mut Vec<*mut RcHeader>) {
    let mut i = 0;
    while i < survivors.len() {
        let s = survivors[i];
        i += 1;
        if !is_object(unsafe { (*s).flags }) {
            continue; // leaf entity: no reference slots
        }
        unsafe {
            crate::object::for_each_counted_child(s as *mut Object, |child| {
                if is_arena_entity(child) && (*child).flags & ARENA_RESET_MARK == 0 {
                    mark_subgraph(child, survivors);
                }
            });
        }
    }
}

unsafe fn mark_one(
    e: *mut RcHeader,
    survivors: &mut Vec<*mut RcHeader>,
    stack: &mut Vec<*mut Object>,
) {
    if unsafe { (*e).flags } & ARENA_RESET_MARK != 0 {
        return;
    }
    unsafe {
        (*e).flags |= ARENA_RESET_MARK;
        // Roots (still IS_ESCAPEE) keep their external hold-count; a
        // survivor reached only internally has none, so start it at zero
        // and let the counting pass rebuild it from internal edges.
        if (*e).flags & IS_ESCAPEE == 0 {
            (*e).refcount = 0;
        }
    }
    survivors.push(e);
    if is_object(unsafe { (*e).flags }) {
        stack.push(e as *mut Object);
    }
}

/// One counting pass over a survivor's reference slots: +1 to arena
/// children (internal edges), a compensating retain to heap entities
/// (their release-at-reset record no longer matches a dying holder).
unsafe fn count_children(surv: *mut RcHeader) {
    if !is_object(unsafe { (*surv).flags }) {
        return; // leaf entity: no reference slots
    }
    unsafe {
        crate::object::for_each_counted_child(surv as *mut Object, |child| {
            match (*child).memory_category() {
                MemoryCategory::RequestArena => (*child).refcount += 1,
                MemoryCategory::GcHeap => ll_retain(child),
                _ => {}
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::ClassBuilder;
    use crate::memory::barrier::ref_store;
    use crate::memory::block_pool::BLOCK_KIND_ARENA;
    use crate::memory::context::{LLContext, set_current_context};
    use crate::object::{ll_object_die, new_constructed};
    use crate::refcount::{DESTRUCTOR_PENDING, DESTRUCTOR_RAN};
    use crate::value::{Tag, Value};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Entity pointer behind a Box slot, or null for scalar/null Boxes.
    fn entity_checked(v: &Value) -> *mut RcHeader {
        if v.is_refcounted() {
            v.entity_ptr()
        } else {
            std::ptr::null_mut()
        }
    }

    /// Store `value` into `holder`'s slot at `offset` through the real
    /// barrier, as generated code would.
    unsafe fn store_prop(arena: *mut Arena, holder: *mut Object, offset: u32, value: *mut Object) {
        unsafe {
            let slot = Object::prop_at(holder, offset);
            let old = entity_checked(&*slot);
            let new = if value.is_null() {
                Value::null()
            } else {
                Value::entity(Tag::Object, value as *mut RcHeader)
            };
            ref_store(arena, holder as *mut RcHeader, slot, old, new);
        }
    }

    #[test]
    fn no_escapes_returns_every_block() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Temp").prop("x", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };
        let block = BlockHeader::of_ptr(obj as *const u8);

        unsafe { arena_reset_full(&mut arena) };

        // The block went home: a fresh arena must get it back.
        let mut second = Arena::new();
        let p = second.alloc(8);
        assert_eq!(BlockHeader::of_ptr(p), block);
        second.reset(|_| {});
    }

    #[test]
    fn escaped_object_survives_with_exact_count_and_retained_block() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Session").prop("x", true).build();
        let holder_cls = ClassBuilder::new("Cache").prop("last", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };

        unsafe { store_prop(&mut arena, holder, 16, obj) };
        let block = BlockHeader::of_ptr(obj as *const u8);
        assert_eq!(unsafe { (*block).kind }, BLOCK_KIND_ARENA);

        unsafe { arena_reset_full(&mut arena) };

        let o = unsafe { &*obj };
        assert_eq!(
            o.rc.memory_category(),
            MemoryCategory::GcHeap,
            "recategorized in place"
        );
        assert_eq!(o.rc.refcount, 1, "exactly the one external reference");
        assert_eq!(o.rc.flags & ARENA_RESET_MARK, 0, "transient mark cleared");
        assert_eq!(unsafe { (*block).kind }, BLOCK_KIND_RETAINED);

        // The survivor is an ordinary counted object now: its one
        // reference is the holder's slot, so the holder's death
        // releases it and cascades into the survivor's own teardown.
        unsafe {
            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }
    }

    #[test]
    fn internal_edges_survive_and_are_counted() {
        let _g = crate::memory::block_pool::test_guard();
        let node = ClassBuilder::new("Node").prop("next", true).build();
        let holder_cls = ClassBuilder::new("Root").prop("head", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let a = unsafe { new_constructed(&mut ctx, node, MemoryCategory::RequestArena) };
        let b = unsafe { new_constructed(&mut ctx, node, MemoryCategory::RequestArena) };

        unsafe {
            store_prop(&mut arena, a, 16, b); // arena→arena: no logs
            store_prop(&mut arena, holder, 16, a); // escape of a (and b transitively)
            arena_reset_full(&mut arena);
        }

        unsafe {
            assert_eq!((*a).rc.memory_category(), MemoryCategory::GcHeap);
            assert_eq!((*b).rc.memory_category(), MemoryCategory::GcHeap);
            assert_eq!((*a).rc.refcount, 1, "one external reference");
            assert_eq!((*b).rc.refcount, 1, "one internal edge from a");
        }
    }

    #[test]
    fn survivor_holding_heap_entity_compensates_the_release_log() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Keeper").prop("cfg", true).build();
        let holder_cls = ClassBuilder::new("Slot").prop("v", true).build();
        let cfg_cls = ClassBuilder::new("Config").build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let cfg = unsafe { new_constructed(&mut ctx, cfg_cls, MemoryCategory::GcHeap) };
        let keeper = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };

        unsafe {
            // Heap entity into an arena container: retain + release log.
            store_prop(&mut arena, keeper, 16, cfg);
            assert_eq!((*cfg).rc.refcount, 2);
            // The keeper escapes.
            store_prop(&mut arena, holder, 16, keeper);
            arena_reset_full(&mut arena);
        }

        // Log's -1 and the survivor compensation +1 cancel out: the
        // keeper legitimately holds cfg.
        assert_eq!(unsafe { (*cfg).rc.refcount }, 2);

        // Keeper dies for real: phase 2 releases cfg.
        unsafe {
            assert!(crate::refcount::ll_release(keeper as *mut RcHeader));
            ll_object_die(keeper);
        }
        assert_eq!(
            unsafe { (*cfg).rc.refcount },
            1,
            "exactly one release at real death"
        );
    }

    #[test]
    fn heap_entity_of_a_dying_holder_dies_with_teardown() {
        let _g = crate::memory::block_pool::test_guard();
        static CFG_DTORS: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn cfg_dtor(_o: *mut Object) {
            CFG_DTORS.fetch_add(1, Ordering::Relaxed);
        }

        let cfg_cls = ClassBuilder::new("DoomedCfg")
            .destructor(cfg_dtor as *const ())
            .build();
        let tmp_cls = ClassBuilder::new("Tmp").prop("cfg", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let cfg = unsafe { new_constructed(&mut ctx, cfg_cls, MemoryCategory::GcHeap) };
        let tmp = unsafe { new_constructed(&mut ctx, tmp_cls, MemoryCategory::RequestArena) };

        unsafe {
            store_prop(&mut arena, tmp, 16, cfg);
            // The test's own reference goes away: the arena holds the last one.
            assert!(!crate::refcount::ll_release(cfg as *mut RcHeader));
            arena_reset_full(&mut arena);
        }

        assert_eq!(
            CFG_DTORS.load(Ordering::Relaxed),
            1,
            "release log's last release must run real teardown"
        );
    }

    #[test]
    fn destructor_created_escape_survives_already_destructed() {
        let _g = crate::memory::block_pool::test_guard();
        static HOLDER_SLOT: AtomicUsize = AtomicUsize::new(0);
        static DTORS: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn escaping_dtor(obj: *mut Object) {
            DTORS.fetch_add(1, Ordering::Relaxed);
            // `$GLOBALS['x'] = $this;` — through the real barrier, with
            // the TLS context (as generated destructor code would).
            let holder = HOLDER_SLOT.load(Ordering::Relaxed) as *mut Object;
            unsafe {
                let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
                let slot = Object::prop_at(holder, 16);
                ref_store(
                    arena,
                    holder as *mut RcHeader,
                    slot,
                    std::ptr::null_mut(),
                    Value::entity(Tag::Object, obj as *mut RcHeader),
                );
            }
        }

        let holder_cls = ClassBuilder::new("Globals").prop("x", true).build();
        let cls = ClassBuilder::new("LastWill")
            .destructor(escaping_dtor as *const ())
            .build();

        // One raw pointer per entity, reused — the shape generated code
        // actually has (an `LLContext*` in a register). Taking a fresh
        // `&mut arena`/`&mut ctx` per call would retag, invalidating the
        // pointer `set_current_context` parked in TLS.
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let ctx_ptr: *mut LLContext = &mut ctx;
        set_current_context(ctx_ptr);

        let holder = unsafe { new_constructed(ctx_ptr, holder_cls, MemoryCategory::GcHeap) };
        HOLDER_SLOT.store(holder as usize, Ordering::Relaxed);
        let obj = unsafe { new_constructed(ctx_ptr, cls, MemoryCategory::RequestArena) };

        unsafe { arena_reset_full(arena_ptr) };
        set_current_context(std::ptr::null_mut());

        assert_eq!(DTORS.load(Ordering::Relaxed), 1);
        unsafe {
            assert_eq!(
                (*obj).rc.memory_category(),
                MemoryCategory::GcHeap,
                "the destructor-created escape was caught by the fixpoint"
            );
            assert_eq!((*obj).rc.refcount, 1);
            assert_ne!(
                (*obj).rc.flags & DESTRUCTOR_RAN,
                0,
                "survives already-destructed"
            );
            assert_ne!((*obj).rc.flags & DESTRUCTOR_PENDING, 0);
        }
    }

    #[test]
    fn overwritten_slot_is_stale_and_only_the_final_target_survives() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Val").build();
        let holder_cls = ClassBuilder::new("One").prop("v", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };

        unsafe {
            store_prop(&mut arena, holder, 16, a); // logged
            store_prop(&mut arena, holder, 16, b); // same slot, logged again
            arena_reset_full(&mut arena);
        }

        unsafe {
            assert_eq!((*b).rc.memory_category(), MemoryCategory::GcHeap);
            assert_eq!((*b).rc.refcount, 1, "deduplicated: one slot, one count");
            // `a` was conservatively marked but is unreferenced: floating
            // garbage of this reset, never a dangling pointer.
        }
    }

    /// Regression for the remembered-set dangle (C2): a heap holder can die
    /// before the arena resets. The old design logged holder *slots* and
    /// read them back at reset, so a freed holder's slot was dereferenced
    /// (and its stale contents re-counted). The escape counter never reads
    /// a slot: the holder's teardown already dropped the count (`lose`), so
    /// reset sees the true, live external count.
    #[test]
    fn holder_death_before_reset_neither_dangles_nor_miscounts() {
        let _g = crate::memory::block_pool::test_guard();
        let holder_cls = ClassBuilder::new("Box").prop("v", true).build();
        let val_cls = ClassBuilder::new("Val").build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let h1 = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let h2 = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let a = unsafe { new_constructed(&mut ctx, val_cls, MemoryCategory::RequestArena) };

        unsafe {
            // A escapes into two heap holders: hold-count 2.
            store_prop(&mut arena, h1, 16, a);
            store_prop(&mut arena, h2, 16, a);
            assert_eq!((*a).rc.refcount, 2, "two heap holders");

            // H1 dies before reset. Its teardown drops the count (lose) and
            // frees its memory — including the slot that held A. The old
            // slot-based reset would read that freed slot and re-count A to
            // 2; the counter leaves the count at exactly 1.
            assert!(crate::refcount::ll_release(h1 as *mut RcHeader));
            ll_object_die(h1);
            assert_eq!((*a).rc.refcount, 1, "H1's death dropped the count");

            arena_reset_full(&mut arena);

            // A survived (H2 holds it), promoted with exactly one
            // reference, and no freed slot was ever dereferenced.
            assert_eq!((*a).rc.memory_category(), MemoryCategory::GcHeap, "promoted");
            assert_eq!((*a).rc.refcount, 1, "exactly H2's reference, not two");

            // H2 dies for real: A cascades to teardown.
            assert!(crate::refcount::ll_release(h2 as *mut RcHeader));
            ll_object_die(h2);
        }
    }

    /// Overwriting a slot that held the last reference to a heap object
    /// tears that object down (destructor + children + free), rather than
    /// leaking it — the store barrier's displaced-value path (audit
    /// barrier.rs:76, previously an empty TODO).
    #[test]
    fn overwriting_the_last_reference_tears_down_the_displaced_object() {
        let _g = crate::memory::block_pool::test_guard();
        static DTORS: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn dtor(_o: *mut Object) {
            DTORS.fetch_add(1, Ordering::Relaxed);
        }

        let val_cls = ClassBuilder::new("Val").destructor(dtor as *const ()).build();
        let holder_cls = ClassBuilder::new("Holder").prop("x", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let owner = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let a = unsafe { new_constructed(&mut ctx, val_cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, val_cls, MemoryCategory::GcHeap) };

        unsafe {
            store_prop(&mut arena, owner, 16, a); // owner->x = a (a.rc 2)
            assert!(!crate::refcount::ll_release(a as *mut RcHeader)); // a.rc 1 (the slot)

            // Overwrite: A's last reference (the slot) goes away → A dies and
            // its destructor runs. The old code released A but never tore it
            // down.
            store_prop(&mut arena, owner, 16, b);
            assert_eq!(DTORS.load(Ordering::Relaxed), 1, "displaced A was torn down");

            // cleanup: owner death releases b's slot reference (b.rc 2 → 1),
            // then drop b's creator reference.
            assert!(crate::refcount::ll_release(owner as *mut RcHeader));
            ll_object_die(owner);
            assert!(crate::refcount::ll_release(b as *mut RcHeader));
            ll_object_die(b);
        }
    }

    /// Regression for H2: a "dirty" destructor stores a *fresh* arena object
    /// into an already-traced survivor. That store is arena→arena, so the
    /// barrier does not escape it; without re-tracing the survivor after a
    /// dirty destructor, the new child is never marked and dangles once the
    /// survivor is promoted. The reset watches the arena bump cursor to know
    /// a destructor allocated, then re-reads the survivors' children.
    #[test]
    fn dirty_destructor_storing_into_a_survivor_traces_the_new_child() {
        let _g = crate::memory::block_pool::test_guard();

        static SURVIVOR: AtomicUsize = AtomicUsize::new(0);
        static NODE_CLS: AtomicUsize = AtomicUsize::new(0);
        static NEW_CHILD: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn mutate_survivor_dtor(_o: *mut Object) {
            let node_cls = NODE_CLS.load(Ordering::Relaxed) as *const crate::class::Class;
            let s = SURVIVOR.load(Ordering::Relaxed) as *mut Object;
            // `$s->next = new Node();` — a fresh arena object stored into an
            // already-traced survivor (arena→arena: not an escape).
            let node =
                unsafe { new_constructed(std::ptr::null_mut(), node_cls, MemoryCategory::RequestArena) };
            NEW_CHILD.store(node as usize, Ordering::Relaxed);
            unsafe {
                let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
                store_prop(arena, s, 16, node);
            }
        }

        let node_cls = ClassBuilder::new("Node").prop("next", true).build();
        let holder_cls = ClassBuilder::new("Cache").prop("keep", true).build();
        let trigger_cls = ClassBuilder::new("Trigger")
            .destructor(mutate_survivor_dtor as *const ())
            .build();

        // One raw pointer each, reused (see the note in
        // `destructor_created_escape_survives_already_destructed`): the
        // destructor reenters and resolves this same arena, so the reset
        // must be handed the very pointer the context holds.
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let ctx_ptr: *mut LLContext = &mut ctx;
        set_current_context(ctx_ptr);

        let holder = unsafe { new_constructed(ctx_ptr, holder_cls, MemoryCategory::GcHeap) };
        let s = unsafe { new_constructed(ctx_ptr, node_cls, MemoryCategory::RequestArena) };
        let _trigger = unsafe { new_constructed(ctx_ptr, trigger_cls, MemoryCategory::RequestArena) };

        NODE_CLS.store(node_cls as usize, Ordering::Relaxed);
        SURVIVOR.store(s as usize, Ordering::Relaxed);
        NEW_CHILD.store(0, Ordering::Relaxed);

        unsafe {
            // S escapes into the heap holder → it is a survivor.
            store_prop(arena_ptr, holder, 16, s);
            // Trigger is unheld with a destructor (tracked); at reset its
            // destructor stores a fresh Node into survivor S.
            arena_reset_full(arena_ptr);
        }
        set_current_context(std::ptr::null_mut());

        let node = NEW_CHILD.load(Ordering::Relaxed) as *mut Object;
        assert!(!node.is_null(), "the destructor created the child");
        unsafe {
            assert_eq!((*s).rc.memory_category(), MemoryCategory::GcHeap, "survivor promoted");
            assert_eq!(
                (*node).rc.memory_category(),
                MemoryCategory::GcHeap,
                "the destructor-added child was traced and promoted, not left to die with the arena"
            );
            assert_eq!((*node).rc.refcount, 1, "held once, by the survivor's slot");

            // Teardown cascades holder → s → node with no dangling.
            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }
    }

    /// Regression for H7: a release-log entity's `__destruct` runs during
    /// the release drain and appends a *new* release-log entry (it stores a
    /// heap reference into a still-alive arena container). The single-pass
    /// reset drained the log once and dropped that late entry, tripping
    /// finish_reset's "logs drained" assert; the settling loop re-drains it.
    #[test]
    fn release_log_grown_during_the_drain_is_still_drained() {
        let _g = crate::memory::block_pool::test_guard();
        static C2_PTR: AtomicUsize = AtomicUsize::new(0);
        static B_PTR: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn a_dtor(_o: *mut Object) {
            // A, dying, stores heap B into the arena container C2 → appends
            // a release-log entry *while the log is being drained*.
            let c2 = C2_PTR.load(Ordering::Relaxed) as *mut Object;
            let b = B_PTR.load(Ordering::Relaxed) as *mut Object;
            unsafe {
                let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
                store_prop(arena, c2, 16, b);
            }
        }

        let cont_cls = ClassBuilder::new("Container").prop("x", true).build();
        let a_cls = ClassBuilder::new("A").destructor(a_dtor as *const ()).build();
        let b_cls = ClassBuilder::new("B").build();

        // One raw pointer each, reused: `a_dtor` reenters and resolves
        // this same arena during the release drain.
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let ctx_ptr: *mut LLContext = &mut ctx;
        set_current_context(ctx_ptr);

        let c1 = unsafe { new_constructed(ctx_ptr, cont_cls, MemoryCategory::RequestArena) };
        let c2 = unsafe { new_constructed(ctx_ptr, cont_cls, MemoryCategory::RequestArena) };
        let a = unsafe { new_constructed(ctx_ptr, a_cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(ctx_ptr, b_cls, MemoryCategory::GcHeap) };

        C2_PTR.store(c2 as usize, Ordering::Relaxed);
        B_PTR.store(b as usize, Ordering::Relaxed);

        unsafe {
            // Heap A into arena container C1 → release-log entry, A retained.
            store_prop(arena_ptr, c1, 16, a);
            // A's only remaining reference is the log's (creator ref dropped).
            assert!(!crate::refcount::ll_release(a as *mut RcHeader));

            // Reset: releasing A runs a_dtor, which appends B's release-log
            // entry mid-drain; the loop must still drain it.
            arena_reset_full(arena_ptr);

            // B was retained by the store and released once by the re-drained
            // log: back to the creator's single reference (not leaked at 2).
            assert_eq!((*b).rc.refcount, 1, "B's late release-log entry was drained");

            assert!(ll_release(b as *mut RcHeader));
            ll_object_die(b);
        }
        set_current_context(std::ptr::null_mut());
    }
}
