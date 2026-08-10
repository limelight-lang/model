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
//!    now owes its own release at its real death). A **COW** survivor is
//!    counted apart, in [`reconcile_cow_counts`], after the fixpoint: its
//!    count is a value the mutator reads and destructors are mutator code,
//!    so it cannot be zeroed while they still run.
//!
//! Children come from `walk::trace_entity`, the crate's one
//! kind-dispatched tracer, and not from a private "object or leaf" test.
//! A reference box has one counted child, and promotion that treats it as
//! a leaf leaves the referent in the dying arena
//! (`dev/DECISIONS.md`, 2026-08-04).
//! 3. **Retain blocks** carrying survivors: rewrite each survivor's
//!    category to GcHeap in place (the pointer-tag alternative was
//!    rejected exactly because this rewrite must be possible), stamp
//!    the blocks `BLOCK_KIND_RETAINED`, keep them out of the pool.
//!    A survivor that had a **block to itself** is the exception, and
//!    the stamp is what it is exempt from: its block is a large entity's
//!    own allocation, which the arena took through `Arena::alloc_entity`
//!    and hands over here instead of retaining — out of the arena's
//!    large-run log, into nothing else, since the run registry has held
//!    it since it was allocated (`rfc/model/memory/large-entities.md`).
//! 4. Release-at-reset log: one release per record, with real teardown
//!    dispatch for entities that die of it.

use std::collections::{HashMap, HashSet};

use crate::journal::kinds::journal_event;
use crate::memory::arena::Arena;
use crate::memory::block_pool::{BLOCK_KIND_RETAINED, BlockHeader};
use crate::object::Object;
use crate::refcount::{
    ARENA_RESET_MARK, COW, IS_ESCAPEE, MEMORY_CATEGORY_MASK, MemoryCategory, RcHeader, ll_release,
    ll_retain,
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
    journal_event!(
        crate::journal::kinds::KIND_ARENA_RESET_BEGIN,
        arena as u64,
        0,
        0
    );
    let mut survivors: Vec<*mut RcHeader> = Vec::new();
    // Each COW survivor's count at the instant it was promoted, which is
    // the last instant the reset can attribute it to arena holders. What
    // happens to the count after that belongs to whoever changed it, and
    // the reconciliation keeps it as a delta.
    let mut cow_at_promotion: Vec<(*mut RcHeader, u32)> = Vec::new();
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
            assert!(
                rounds <= ARENA_RESET_MAX_ROUNDS,
                "arena reset did not converge"
            );
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
            // Out-of-line memory comes with the survivor, before the
            // category stops describing where it lives. Asked through one
            // kind-dispatched call so promotion keeps knowing nothing
            // about any layout (`rfc/model/strings.md`).
            if !unsafe { carry_external_memory(arena, surv) } {
                // Refused. The bytes stay where they are and the block
                // holding them stays out of circulation with the
                // survivors' blocks — the same mechanism, for the same
                // reason. Reset has no caller left to report to, which is
                // why there is a fallback here at all.
                let payload_block = unsafe { external_memory_block(surv) };
                if payload_block != 0 {
                    let header = payload_block as *mut BlockHeader;
                    if retained.insert(payload_block) {
                        unsafe {
                            crate::memory::block_pool::store_block_kind(
                                &raw mut (*header).kind,
                                BLOCK_KIND_RETAINED,
                            )
                        };
                    }
                    // Pinned, and not merely retained: this block is held
                    // for bytes rather than for occupants, and an
                    // occupant's death says nothing about them. Without
                    // the pin, a survivor of the same block dying would
                    // hand the payload back to the pool. The bytes have a
                    // death event of their own — the owning entity's free
                    // — and it spends this pin (`retained.rs`, blocks
                    // retained for bytes; `dev/DECISIONS.md`,
                    // 2026-08-08).
                    crate::memory::retained::pin(payload_block);
                }
            }
            unsafe {
                if (*surv).flags & COW != 0 {
                    cow_at_promotion.push((surv, (*surv).refcount));
                }
                // 00 = GcHeap; drop the transient arena-reset mark and
                // IS_ESCAPEE. The mark lives in the GC-state field, so
                // clearing it also leaves the promoted object's GC state at
                // 00 (LIVE), the correct fresh heap state.
                (*surv).flags &= !(MEMORY_CATEGORY_MASK | ARENA_RESET_MARK | IS_ESCAPEE);
            }
            // A survivor that had a block to itself keeps it, and none of
            // the retention machinery applies to it: the block is not
            // shared, so it has nothing to index, and its kind is what
            // routes the free — restamped `BLOCK_KIND_RETAINED` it would
            // send a multi-megabyte run to the 64 KiB block pool at the
            // entity's eventual death. What the reset does instead is
            // hand ownership over: the run leaves the arena's log, so the
            // reset stops freeing it, and the registry entry it was given
            // at allocation is what the walk finds it by from now on
            // (`rfc/model/memory/large-entities.md`).
            //
            // This arm displaces a stamp that ran unconditionally, and an
            // omitted test would be silent, which is why it is the one of
            // the four rules that owes a test.
            let block = BlockHeader::of_ptr(surv as *const u8) as usize;
            if unsafe { is_in_a_block_of_its_own(surv) } {
                let forgotten = unsafe { (*arena).forget_large(surv as *mut u8) };
                debug_assert!(
                    forgotten,
                    "a promoted large entity was not one of this arena's runs"
                );
            } else if retained.insert(block) {
                unsafe {
                    crate::memory::block_pool::store_block_kind(
                        &raw mut (*(block as *mut BlockHeader)).kind,
                        BLOCK_KIND_RETAINED,
                    )
                };
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
        assert!(
            rounds <= ARENA_RESET_MAX_ROUNDS,
            "arena reset did not converge"
        );
    }

    // The weak walk — after every destructor has settled and the
    // survivors' categories are rewritten, before the pages go back:
    // dying entries get their cells nulled, promoted survivors are
    // recognized by their new category and keep resolving
    // (`rfc/model/weak-references.md`, "Death notification"). Runs no
    // user code, so it cannot grow the logs behind the settled fixpoint.
    // COW counts last, for the reason they were left alone until now: the
    // fixpoint is where mutator code runs, and on a COW entity the count
    // is what that code reads to decide whether a write may go in place.
    unsafe { reconcile_cow_counts(&survivors, &cow_at_promotion) };

    unsafe { crate::weak::drain_arena_weak_log(arena) };

    // The survivor list becomes the retained blocks' object index. A
    // bump-filled block has no stride to divide by, so this inventory
    // is the only way the walk can enumerate its occupants — without it
    // they are root sources and a ring among them never dies
    // (`rfc/model/gc/retained-block-walk.md`). Registered after the
    // fixpoint has settled and before the blocks are disposed of, so
    // every entry describes a survivor that is staying.
    let emptied = index_retained_blocks(&survivors);

    unsafe { (*arena).finish_reset(|block| retained.contains(&(block as usize))) };

    // Blocks whose every survivor died inside this reset — the shape a
    // heap reference box produces, where the element it made an escapee
    // is promoted and then torn down by the box's own logged release. No
    // later death will report such a block empty, so the reset hands it
    // over itself, and only **after** `finish_reset`: the arena's block
    // chain is threaded through the very headers the pool overwrites, so
    // a block returned before that walk cuts the chain under it. The
    // route is `ll_free` rather than the pool directly, because the block
    // still reads `BLOCK_KIND_RETAINED` and that is the one path which
    // both parks it under a live epoch and drops the index first.
    for block in emptied {
        unsafe { crate::memory::stdapi::ll_free(block as *mut u8) };
    }

    // After the frees, so that every death this reset caused falls
    // between the pair (`journal::kinds::KIND_ARENA_RESET_BEGIN`).
    journal_event!(
        crate::journal::kinds::KIND_ARENA_RESET_END,
        arena as u64,
        survivors.len() as u64,
        retained.len() as u64
    );
}

/// Bring a survivor's out-of-line memory with it, if its kind has any.
/// One call, kind-dispatched, so nothing about any layout leaks into the
/// reset: promotion holds a block by the address of a header and does
/// not otherwise look inside an entity.
///
/// False when the carry was refused; the caller keeps the memory alive
/// instead. Kinds with nothing out of line answer true.
///
/// # Safety
/// `surv` is a live survivor of `arena`, mid-reset.
unsafe fn carry_external_memory(arena: *mut Arena, surv: *mut RcHeader) -> bool {
    match unsafe { external_memory(surv) } {
        External::StringPayload(s) => unsafe { crate::string::carry_payload_out_of(arena, s) },
        External::ArrayStorage(a) => unsafe {
            crate::array::entity::carry_storage_out_of(arena, a)
        },
        External::None => true,
    }
}

/// What a survivor owns outside its own entity. Two kinds do today; the
/// rest answer [`External::None`] and cost one flags read.
enum External {
    None,
    StringPayload(*mut crate::string::LLStringDynamic),
    ArrayStorage(*mut crate::array::entity::LLArray),
}

/// Classify a survivor once, so the carry and the block it falls back on
/// cannot disagree about what the entity is.
///
/// # Safety
/// `surv` must be a live entity.
unsafe fn external_memory(surv: *mut RcHeader) -> External {
    use crate::refcount::{ENTITY_KIND_MASK, EntityKind, STRING_OUT_OF_LINE};
    let flags = unsafe { (*surv).flags };
    match flags & ENTITY_KIND_MASK {
        // Only the dynamic layout holds bytes out of line; an inline
        // string carries them behind its own header and moves with it.
        k if k == EntityKind::String.to_flags() && flags & STRING_OUT_OF_LINE != 0 => {
            External::StringPayload(surv as *mut crate::string::LLStringDynamic)
        }
        k if k == EntityKind::Array.to_flags() => {
            External::ArrayStorage(surv as *mut crate::array::entity::LLArray)
        }
        _ => External::None,
    }
}

/// The block holding a survivor's out-of-line memory, or 0 — the address
/// the caller retains when [`carry_external_memory`] refused. An
/// OS-direct payload has no block of the arena's and never refuses.
///
/// # Safety
/// As [`carry_external_memory`].
unsafe fn external_memory_block(surv: *mut RcHeader) -> usize {
    let memory = match unsafe { external_memory(surv) } {
        External::StringPayload(s) => unsafe { (*s).data },
        External::ArrayStorage(a) => unsafe { crate::array::entity::storage_address(a) },
        External::None => return 0,
    };
    if memory.is_null() {
        return 0;
    }
    BlockHeader::of_ptr(memory as *const u8) as usize
}

/// Group the settled survivors by the block holding them and hand each
/// group to the retained-index registry. The blocks that came back empty
/// — every occupant already dead when the index was built — are returned,
/// and their disposal is the caller's.
///
/// One index per block rather than one per reset: both enumerators
/// reach a block first — the census by the 64 KiB alignment mask, the
/// synchronous walk by scanning the region registry — so an index found
/// from a block address costs no second mapping (`dev/DECISIONS.md`,
/// 2026-08-03). A survivor whose block was *not* retained cannot occur
/// here: retention is decided from this same list.
fn index_retained_blocks(survivors: &[*mut RcHeader]) -> Vec<usize> {
    let mut emptied = Vec::new();
    let mut by_block: HashMap<usize, Vec<usize>> = HashMap::new();
    for &surv in survivors {
        // A block with one occupant, whose address is computed from the
        // block's own, needs no inventory to be walkable — and an entry
        // here would put it on the path that ends at the block pool.
        if unsafe { is_in_a_block_of_its_own(surv) } {
            continue;
        }
        let block = BlockHeader::of_ptr(surv as *const u8) as usize;
        by_block.entry(block).or_default().push(surv as usize);
    }
    for (block, occupants) in by_block {
        // The addresses are this reset's own survivors, so they are
        // readable, which is what `register` asks of its caller.
        if unsafe { crate::memory::retained::register(block, occupants) } {
            emptied.push(block);
        }
    }
    emptied
}

/// True for a survivor that occupies a block-aligned allocation alone
/// (`memory::large_entity`), which the arena's entity door gives an
/// entity past one block payload. Such a block is not shared with
/// anything, so the reset neither retains nor indexes it; the block kind
/// is the whole of the test, because a large-entity kind is only ever
/// stamped on a block that holds exactly one entity.
///
/// # Safety
/// `surv` is a live entity address.
#[inline]
unsafe fn is_in_a_block_of_its_own(surv: *mut RcHeader) -> bool {
    let block = BlockHeader::of_ptr(surv as *const u8);
    crate::memory::large_entity::is_large_entity(unsafe { *(block as *const u32) })
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

    while let Some(e) = stack.pop() {
        unsafe {
            crate::walk::trace_entity(e, |child| {
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
        unsafe {
            crate::walk::trace_entity(s, |child| {
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
    stack: &mut Vec<*mut RcHeader>,
) {
    if unsafe { (*e).flags } & ARENA_RESET_MARK != 0 {
        return;
    }
    unsafe {
        (*e).flags |= ARENA_RESET_MARK;
        // Roots (still IS_ESCAPEE) keep their external hold-count; a
        // survivor reached only internally has none, so start it at zero
        // and let the counting pass rebuild it from internal edges.
        //
        // A COW entity is the exception, because its count is live all
        // through the fixpoint: `values.md` maintains it in every memory
        // category, so a destructor's `unset` reaches `ll_release` and
        // decrements it. Zeroing here would make that decrement underflow
        // inside the reset. Its count is settled once instead, by
        // [`reconcile_cow_counts`], after the last destructor has run.
        if (*e).flags & (IS_ESCAPEE | COW) == 0 {
            (*e).refcount = 0;
        }
    }
    survivors.push(e);
    stack.push(e);
}

/// Settle every COW survivor's count now that the fixpoint is over and
/// no user code can run again.
///
/// Two contributions, and the split is the whole design. **Edges** — the
/// references surviving entities hold — replace what the count said at
/// promotion time, because the holders that died with the arena never
/// released and there is no list of them to subtract. **The delta** —
/// whatever changed the count after promotion — is carried across
/// untouched, because promotion happens inside the settling loop and the
/// release-log drain runs `__destruct` bodies after it: a destructor may
/// hand an already-promoted string to a heap object that outlives the
/// request, and that reference belongs to nobody the edge walk can see.
///
/// The earlier version assigned the edge count outright, which erased
/// exactly those holders and left the string with one count and two
/// holders (`dev/DECISIONS.md`, 2026-08-04).
///
/// `at_promotion` is each COW survivor's count at the instant its
/// category was rewritten — the last instant the reset can attribute it
/// to arena holders.
///
/// # Safety
/// Every survivor is live, the fixpoint has settled, and no user code can
/// run again before the blocks are disposed of.
unsafe fn reconcile_cow_counts(survivors: &[*mut RcHeader], at_promotion: &[(*mut RcHeader, u32)]) {
    if at_promotion.is_empty() {
        return;
    }
    // Address → (edges seen so far, delta since promotion).
    let mut settled: HashMap<usize, (u32, i64)> = HashMap::with_capacity(at_promotion.len());
    for &(s, at) in at_promotion {
        let now = unsafe { (*s).refcount } as i64;
        settled.insert(s as usize, (0, now - at as i64));
    }

    for &s in survivors {
        debug_assert!(
            traceable_in_full(unsafe { (*s).flags }),
            "a survivor of a kind `trace_entity` skips would have its              references erased here, not conservatively ignored"
        );
        unsafe {
            crate::walk::trace_entity(s, |child| {
                if let Some(entry) = settled.get_mut(&(child as usize)) {
                    entry.0 += 1;
                }
            });
        }
    }

    for &(s, _) in at_promotion {
        let (edges, delta) = settled[&(s as usize)];
        let settled_count = edges as i64 + delta;
        debug_assert!(
            settled_count >= 0,
            "a COW survivor lost more references than it had"
        );
        unsafe { (*s).refcount = settled_count.max(0) as u32 };
    }
}

/// True when `walk::trace_entity` enumerates **all** of this entity's
/// counted children rather than skipping it.
///
/// The tracer's own skips are conservative for the collector — an omitted
/// source only removes in-edges, so its targets stay pinned — and they
/// are the opposite of conservative for a pass that decides a count from
/// the edges it finds. Box is the kind left out: its payload is C memory
/// nothing here can read.
fn traceable_in_full(flags: u32) -> bool {
    use crate::refcount::{ENTITY_KIND_MASK, ENTITY_KIND_SHIFT, EntityKind};
    let kind = (flags & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT;
    const OBJECT: u32 = EntityKind::Object as u32;
    const LAZY: u32 = EntityKind::Lazy as u32;
    const REFERENCE: u32 = EntityKind::Reference as u32;
    const STRING: u32 = EntityKind::String as u32;
    const WEAKREF: u32 = EntityKind::WeakRef as u32;
    const ARRAY: u32 = EntityKind::Array as u32;
    // String and WeakRef are leaves, so "skipped" and "enumerated in
    // full" are the same answer for them.
    matches!(kind, OBJECT | LAZY | REFERENCE | STRING | WEAKREF | ARRAY)
}

/// One counting pass over a survivor's reference slots: +1 to arena
/// children (internal edges), a compensating retain to heap entities
/// (their release-at-reset record no longer matches a dying holder).
unsafe fn count_children(surv: *mut RcHeader) {
    unsafe {
        crate::walk::trace_entity(surv, |child| match (*child).memory_category() {
            MemoryCategory::RequestArena => (*child).refcount += 1,
            MemoryCategory::GcHeap => ll_retain(child),
            _ => {}
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
            assert!(ref_store(arena, holder as *mut RcHeader, slot, old, new));
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

    /// An arena referent behind a surviving reference box outlives the
    /// reset, and comes out of it with exactly one holder.
    ///
    /// **What carries it is the escape count, since the box moved to the
    /// heap** (S3.1): storing an arena object into a heap box is a
    /// crossing, so the object is an escapee in its own right and the
    /// reset promotes it from the escapee log. The test was written for a
    /// different mechanism — promotion gated recursion on `is_object`, so
    /// every other kind was a leaf and the arena object behind an *arena*
    /// `&` was never marked, dying with the reset while a promoted box
    /// still pointed at it. That configuration cannot be built any more,
    /// because no box is an arena entity; the assertions below are worth
    /// keeping for the survival and the count, not as a guard on the
    /// recursion.
    #[test]
    fn a_surviving_reference_box_carries_its_referent() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Node").prop("x", true).build();
        let holder_cls = ClassBuilder::new("Cache").prop("last", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let target = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };
        let r = crate::reference::ll_reference_new();

        unsafe {
            assert!(ref_store(
                &mut arena,
                r as *mut RcHeader,
                &raw mut (*r).value,
                std::ptr::null_mut(),
                Value::entity(Tag::Object, target as *mut RcHeader),
            ));
            // The heap holder takes the box, which is what keeps the box
            // — and through it the referent — reachable past the reset.
            let slot = Object::prop_at(holder, 16);
            assert!(ref_store(
                &mut arena,
                holder as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Reference, r as *mut RcHeader),
            ));
        }

        unsafe { arena_reset_full(&mut arena) };

        assert_eq!(
            unsafe { (*(target as *mut RcHeader)).memory_category() },
            MemoryCategory::GcHeap,
            "the referent stayed behind in the dying arena"
        );
        assert_eq!(
            unsafe { (*(target as *mut RcHeader)).refcount },
            1,
            "the box's slot is its one holder"
        );
    }

    /// An arena array reached from an escaping object takes its storage
    /// with it. The route matters: an array is a COW entity, so it never
    /// escapes on its own — the barrier copies a COW value out of the
    /// arena instead — and it becomes a survivor only as a **child** of
    /// something that did escape. That child edge is what the array's
    /// tracing arm added, and this is the first thing to walk it.
    ///
    /// Without the carry the storage goes back to the block pool at the
    /// reset while the promoted array still points at it, so the array
    /// reads whatever the next owner of those bytes writes.
    #[test]
    fn an_array_reached_from_an_escapee_carries_its_storage_out() {
        use crate::array::entity::ll_array_new;
        use crate::array::table::Key;
        use crate::memory::block_pool::{BLOCK_KIND_BUFFER, BLOCK_MASK};
        let _g = crate::memory::block_pool::test_guard();
        let holder_cls = ClassBuilder::new("Cache").prop("last", true).build();
        let owner_cls = ClassBuilder::new("Owner").prop("items", true).build();

        // One raw pointer per arena and per context, reused: `ll_array_new`
        // resolves the arena from the mounted context rather than taking
        // one, and a fresh `&mut` per call would retag the pointer parked
        // in TLS (`dev/WORKFLOW.md`, Miri).
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut context = LLContext { arena: arena_ptr };
        let context_ptr: *mut LLContext = &mut context;
        crate::memory::context::set_current_context(context_ptr);

        let holder =
            unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
        let owner =
            unsafe { new_constructed(&mut *context_ptr, owner_cls, MemoryCategory::RequestArena) };
        let array = unsafe { ll_array_new(MemoryCategory::RequestArena) };

        let storage_before = unsafe {
            (*array).table.insert(
                crate::array::entity::category_of(array),
                Key::Int(1),
                Value::int(11),
            );
            (*array).table.insert(
                crate::array::entity::category_of(array),
                Key::Int(2),
                Value::int(22),
            );
            crate::array::entity::storage_address(array)
        };
        assert!(!storage_before.is_null());

        unsafe {
            // The array into the arena owner: same category on both sides,
            // so no escape copy is asked for.
            let slot = Object::prop_at(owner, 16);
            assert!(ref_store(
                arena_ptr,
                owner as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, array as *mut RcHeader),
            ));
            // The heap holder takes the owner: this is the escape, and the
            // only reason anything here survives.
            store_prop(arena_ptr, holder, 16, owner);
        }

        unsafe { arena_reset_full(&mut *arena_ptr) };
        crate::memory::context::set_current_context(std::ptr::null_mut());

        unsafe {
            assert_eq!(
                (*(array as *mut RcHeader)).memory_category(),
                MemoryCategory::GcHeap,
                "the array stayed behind in the dying arena"
            );
            let storage_after = crate::array::entity::storage_address(array);
            let kind = *(((storage_after as usize) & !BLOCK_MASK) as *const u32);
            assert_eq!(
                kind, BLOCK_KIND_BUFFER,
                "the storage is still arena memory the reset gave back"
            );
            assert_eq!(
                (*array).table.get(Key::Int(1)).unwrap().as_int(),
                11,
                "the carried storage lost its entries"
            );
            assert_eq!((*array).table.get(Key::Int(2)).unwrap().as_int(), 22);
        }
    }

    /// A carry the buffer arena refuses leaves the bytes where they are,
    /// and the reset keeps their block out of circulation instead. The
    /// block is then held by a payload rather than by occupants, and what
    /// hands it back is the payload's own free — the promoted array's
    /// death. Before 2026-08-08 the pin was permanent and the block was
    /// gone for the life of the process; the test was seen failing on the
    /// kind still reading retained after the array died.
    ///
    /// The refusal is aimed at one allocation rather than at the pool:
    /// `FORCE_OOM` leaves the buffer arena free to serve the carry from a
    /// block it already owns or adopts, which made this test pass 35
    /// times in 40 and prove nothing the other five. The assertion that
    /// the storage did not move is what says the refusal landed where the
    /// test needs it.
    #[test]
    fn a_refused_carry_pins_the_block_and_the_payload_frees_it() {
        use crate::array::entity::ll_array_new;
        use crate::array::table::Key;
        use crate::memory::block_pool::{BLOCK_KIND_FREE, BLOCK_MASK};
        use crate::memory::buffer_arena::FORCE_REFUSE_LONGLIVED;
        use std::sync::atomic::Ordering;
        let _g = crate::memory::block_pool::test_guard();
        let holder_cls = ClassBuilder::new("RefusedCache").prop("last", true).build();
        let owner_cls = ClassBuilder::new("RefusedOwner")
            .prop("items", true)
            .build();

        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut context = LLContext { arena: arena_ptr };
        let context_ptr: *mut LLContext = &mut context;
        crate::memory::context::set_current_context(context_ptr);

        let holder =
            unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
        let owner =
            unsafe { new_constructed(&mut *context_ptr, owner_cls, MemoryCategory::RequestArena) };
        let array = unsafe { ll_array_new(MemoryCategory::RequestArena) };

        let storage_before = unsafe {
            (*array).table.insert(
                crate::array::entity::category_of(array),
                Key::Int(1),
                Value::int(11),
            );
            crate::array::entity::storage_address(array)
        };
        assert!(!storage_before.is_null());
        let payload_block = (storage_before as usize) & !BLOCK_MASK;

        unsafe {
            let slot = Object::prop_at(owner, 16);
            assert!(ref_store(
                arena_ptr,
                owner as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, array as *mut RcHeader),
            ));
            store_prop(arena_ptr, holder, 16, owner);
        }

        FORCE_REFUSE_LONGLIVED.store(true, Ordering::Relaxed);
        unsafe { arena_reset_full(&mut *arena_ptr) };
        FORCE_REFUSE_LONGLIVED.store(false, Ordering::Relaxed);
        crate::memory::context::set_current_context(std::ptr::null_mut());

        unsafe {
            assert_eq!(
                crate::array::entity::storage_address(array),
                storage_before,
                "the carry was not refused, so this test proves nothing"
            );
            assert_eq!(
                *(payload_block as *const u32),
                crate::memory::block_pool::BLOCK_KIND_RETAINED,
                "a refused carry did not retain the block its bytes lie in"
            );

            // The promoted array dies with its holder, and its storage is
            // freed into a block that has been waiting for exactly that.
            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            crate::object::ll_object_die(holder);
            assert_eq!(
                *(payload_block as *const u32),
                BLOCK_KIND_FREE,
                "the block outlived the payload it was pinned for"
            );
        }
    }

    /// The other route out: a storage larger than a block payload is an
    /// OS-direct run the arena logged, and carrying it is making the arena
    /// forget the record rather than copying anything. Getting that wrong
    /// is not a leak but a use-after-free — the reset frees every logged
    /// run, and the promoted array would go on reading the freed memory.
    /// The address is therefore unchanged, which is the observable.
    #[test]
    fn an_over_block_storage_transfers_instead_of_being_copied() {
        use crate::array::entity::ll_array_new;
        use crate::array::table::Key;
        use crate::memory::block_pool::BLOCK_PAYLOAD;
        let _g = crate::memory::block_pool::test_guard();
        let holder_cls = ClassBuilder::new("Cache").prop("last", true).build();
        let owner_cls = ClassBuilder::new("Owner").prop("items", true).build();

        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut context = LLContext { arena: arena_ptr };
        let context_ptr: *mut LLContext = &mut context;
        crate::memory::context::set_current_context(context_ptr);

        let holder =
            unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
        let owner =
            unsafe { new_constructed(&mut *context_ptr, owner_cls, MemoryCategory::RequestArena) };
        let array = unsafe { ll_array_new(MemoryCategory::RequestArena) };

        let storage_before = unsafe {
            for i in 0..1100i64 {
                (*array).table.insert(
                    crate::array::entity::category_of(array),
                    Key::Int(i),
                    Value::int(i),
                );
            }
            crate::array::entity::storage_address(array)
        };
        assert!(
            unsafe { (*array).table.storage_and_capacity().1 } > BLOCK_PAYLOAD,
            "the table never grew past one block, so this proves nothing"
        );

        unsafe {
            let slot = Object::prop_at(owner, 16);
            assert!(ref_store(
                arena_ptr,
                owner as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, array as *mut RcHeader),
            ));
            store_prop(arena_ptr, holder, 16, owner);
        }

        unsafe { arena_reset_full(&mut *arena_ptr) };
        crate::memory::context::set_current_context(std::ptr::null_mut());

        unsafe {
            assert_eq!(
                crate::array::entity::storage_address(array),
                storage_before,
                "an OS-direct storage was copied instead of transferred"
            );
            for i in 0..1100i64 {
                assert_eq!((*array).table.get(Key::Int(i)).unwrap().as_int(), i);
            }
        }
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

    /// A class whose instance is past one block payload, which is what
    /// sends it through the arena's large-entity door: a boxed property
    /// is 16 bytes, so 4 200 of them make an object of 67 216 and a run
    /// of two blocks.
    fn wide_class(name: &str, props: usize) -> *const crate::class::Class {
        let mut builder = ClassBuilder::new(name);
        for i in 0..props {
            builder = builder.prop(&format!("p{i}"), true);
        }
        let class = builder.build();
        assert!(
            unsafe { (*class).object_size } as usize > crate::memory::block_pool::BLOCK_PAYLOAD,
            "the class still fits a shared block, so it tests nothing"
        );
        class
    }

    /// A survivor that had a block to itself keeps it, and the three
    /// rules the reset applies to one are what make that safe. The stamp
    /// is the silent one: `BLOCK_KIND_RETAINED` on a run sends a 128 KiB
    /// OS allocation to the 64 KiB block pool when the entity finally
    /// dies, and nothing between the reset and that death looks wrong.
    #[test]
    fn a_promoted_large_entity_keeps_its_block_and_leaves_the_arenas_log() {
        let _g = crate::memory::block_pool::test_guard();
        let wide = wide_class("WideSession", 4_200);
        let holder_cls = ClassBuilder::new("Cache").prop("last", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let obj = unsafe { new_constructed(&mut ctx, wide, MemoryCategory::RequestArena) };

        let block = BlockHeader::of_ptr(obj as *const u8) as usize;
        assert_eq!(
            unsafe { *(block as *const u32) },
            crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE_RUN,
            "the arena's entity door gave it a run of its own"
        );

        unsafe { store_prop(&mut arena, holder, 16, obj) };
        unsafe { arena_reset_full(&mut arena) };

        unsafe {
            assert_eq!(
                (*obj).rc.memory_category(),
                MemoryCategory::GcHeap,
                "recategorized in place, like any other survivor"
            );
            assert_eq!(
                *(block as *const u32),
                crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE_RUN,
                "stamped retained, and the death below would push a run \
                 onto the block pool"
            );
        }
        assert!(
            !crate::memory::retained::snapshot()
                .iter()
                .any(|(b, _)| *b == block),
            "a block with one computed occupant needs no inventory, and an \
             entry here is the same mistake by the other route"
        );
        assert!(
            crate::memory::large_entity::snapshot().contains(&block),
            "and the registry it was entered into at allocation is what \
             the walk finds it by now that it is a heap entity"
        );

        // The survivor is an ordinary counted object: the holder's death
        // releases it, and its own teardown is what returns the run —
        // which is also the proof that the arena stopped owning it, since
        // a record left in the log would have freed it at the reset.
        unsafe {
            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }
        assert!(
            !crate::memory::large_entity::snapshot().contains(&block),
            "the run went back with the entity"
        );
    }

    /// The other half of the door's contract: a large arena entity that
    /// nothing carries out is freed by the reset, like every other run
    /// the arena logged.
    #[test]
    fn an_unpromoted_large_arena_entity_is_freed_by_the_reset() {
        let _g = crate::memory::block_pool::test_guard();
        let wide = wide_class("WideTemp", 4_200);

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe { new_constructed(&mut ctx, wide, MemoryCategory::RequestArena) };
        let block = BlockHeader::of_ptr(obj as *const u8) as usize;
        assert!(crate::memory::large_entity::snapshot().contains(&block));

        unsafe { arena_reset_full(&mut arena) };

        assert!(
            !crate::memory::large_entity::snapshot().contains(&block),
            "the corpse's run went with the reset"
        );
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

        // Keeper dies for real, and it dies **through its holder**: the
        // `Slot` object's property is the reference keeping it alive, so
        // releasing behind the holder's back leaves a live object naming
        // freed memory. Only block reuse makes that visible — a freed
        // slot nobody reissues still reads refcount 0, which is what
        // makes the dangling property look harmless.
        unsafe {
            let slot = Object::prop_at(holder, 16);
            assert!(crate::memory::barrier::ref_store(
                &mut arena,
                holder as *mut RcHeader,
                slot,
                keeper as *mut RcHeader,
                Value::null(),
            ));
        }
        assert_eq!(
            unsafe { (*cfg).rc.refcount },
            1,
            "exactly one release at real death"
        );
        unsafe {
            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }
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
                assert!(ref_store(
                    arena,
                    holder as *mut RcHeader,
                    slot,
                    std::ptr::null_mut(),
                    Value::entity(Tag::Object, obj as *mut RcHeader),
                ));
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
            assert_eq!(
                (*a).rc.memory_category(),
                MemoryCategory::GcHeap,
                "promoted"
            );
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

        let val_cls = ClassBuilder::new("Val")
            .destructor(dtor as *const ())
            .build();
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
            assert_eq!(
                DTORS.load(Ordering::Relaxed),
                1,
                "displaced A was torn down"
            );

            // cleanup: owner death releases b's slot reference (b.rc 2 → 1),
            // then drop b's creator reference.
            assert!(crate::refcount::ll_release(owner as *mut RcHeader));
            ll_object_die(owner);
            assert!(crate::refcount::ll_release(b as *mut RcHeader));
            ll_object_die(b);
        }
    }

    /// A holder acquired **after** the survivor was promoted must survive
    /// the reconciliation. Promotion happens inside the settling loop and
    /// the release-log drain runs user destructors after it, so a
    /// destructor can store an already-promoted string into a heap object
    /// that outlives the request — a legitimate `+1` that no edge between
    /// survivors accounts for. Assigning the count from those edges alone
    /// erased it, which left the string with one count and two holders.
    #[test]
    fn a_holder_acquired_after_promotion_keeps_its_count() {
        let _g = crate::memory::block_pool::test_guard();
        static CACHE: AtomicUsize = AtomicUsize::new(0);
        static STRING: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn cache_the_string_dtor(_o: *mut Object) {
            // A dying heap entity, torn down by the release drain, puts the
            // string into a heap object: `Cache::$last = $s`.
            let cache = CACHE.load(Ordering::Relaxed) as *mut Object;
            let s = STRING.load(Ordering::Relaxed) as *mut RcHeader;
            unsafe {
                let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
                let slot = Object::prop_at(cache, 16);
                assert!(ref_store(
                    arena,
                    cache as *mut RcHeader,
                    slot,
                    std::ptr::null_mut(),
                    Value::entity(Tag::String, s),
                ));
            }
        }

        let keeper_cls = ClassBuilder::new("Keeper").prop("s", true).build();
        let holder_cls = ClassBuilder::new("Holder").prop("keep", true).build();
        let cache_cls = ClassBuilder::new("Cache").prop("last", true).build();
        let dying_cls = ClassBuilder::new("Dying")
            .destructor(cache_the_string_dtor as *const ())
            .build();

        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let ctx_ptr: *mut LLContext = &mut ctx;
        set_current_context(ctx_ptr);

        let holder = unsafe { new_constructed(ctx_ptr, holder_cls, MemoryCategory::GcHeap) };
        let cache = unsafe { new_constructed(ctx_ptr, cache_cls, MemoryCategory::GcHeap) };
        let keeper = unsafe { new_constructed(ctx_ptr, keeper_cls, MemoryCategory::RequestArena) };
        let container =
            unsafe { new_constructed(ctx_ptr, holder_cls, MemoryCategory::RequestArena) };
        let dying = unsafe { new_constructed(ctx_ptr, dying_cls, MemoryCategory::GcHeap) };
        CACHE.store(cache as usize, Ordering::Relaxed);

        let s = unsafe {
            crate::string::ll_string_new(ctx_ptr, MemoryCategory::RequestArena, b"cached")
        } as *mut RcHeader;
        STRING.store(s as usize, Ordering::Relaxed);

        unsafe {
            // The keeper holds the string and escapes, so both survive.
            let slot = Object::prop_at(keeper, 16);
            assert!(ref_store(
                arena_ptr,
                keeper as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::String, s),
            ));
            assert!(!crate::refcount::ll_release(s), "the creation reference");
            store_prop(arena_ptr, holder, 16, keeper);

            // The dying heap entity sits in an arena container, so the
            // release log tears it down — after the promotion pass.
            store_prop(arena_ptr, container, 16, dying);
            assert!(!crate::refcount::ll_release(dying as *mut RcHeader));

            arena_reset_full(arena_ptr);
        }
        set_current_context(std::ptr::null_mut());

        unsafe {
            assert_eq!(
                (*s).refcount,
                2,
                "the keeper's slot and the one the destructor added"
            );
            assert_eq!((*s).memory_category(), MemoryCategory::GcHeap);
        }
    }

    /// A COW entity's count is a value, and it stays readable through the
    /// whole fixpoint. Marking a survivor used to zero it, so a destructor
    /// releasing the same string — an ordinary `unset` — decremented from
    /// zero and underflowed inside the reset. The count is settled once
    /// instead, after the last destructor, from the edges that remain.
    #[test]
    fn a_destructor_may_release_a_cow_survivor_during_the_fixpoint() {
        let _g = crate::memory::block_pool::test_guard();

        static DROPPER: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn unset_the_string_dtor(o: *mut Object) {
            // `unset($this->s)` — the store barrier releases the string
            // this object holds, while the reset is still settling.
            unsafe {
                let slot = Object::prop_at(o, 16);
                let old = entity_checked(&*slot);
                let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
                assert!(ref_store(
                    arena,
                    o as *mut RcHeader,
                    slot,
                    old,
                    Value::null()
                ));
            }
        }

        let keeper_cls = ClassBuilder::new("Keeper").prop("s", true).build();
        let holder_cls = ClassBuilder::new("Cache").prop("keep", true).build();
        let dropper_cls = ClassBuilder::new("Dropper")
            .prop("s", true)
            .destructor(unset_the_string_dtor as *const ())
            .build();

        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let ctx_ptr: *mut LLContext = &mut ctx;
        set_current_context(ctx_ptr);

        let holder = unsafe { new_constructed(ctx_ptr, holder_cls, MemoryCategory::GcHeap) };
        let keeper = unsafe { new_constructed(ctx_ptr, keeper_cls, MemoryCategory::RequestArena) };
        let dropper =
            unsafe { new_constructed(ctx_ptr, dropper_cls, MemoryCategory::RequestArena) };
        DROPPER.store(dropper as usize, Ordering::Relaxed);

        let s = unsafe {
            crate::string::ll_string_new(ctx_ptr, MemoryCategory::RequestArena, b"shared")
        } as *mut RcHeader;

        unsafe {
            for owner in [keeper, dropper] {
                let slot = Object::prop_at(owner, 16);
                assert!(ref_store(
                    arena_ptr,
                    owner as *mut RcHeader,
                    slot,
                    std::ptr::null_mut(),
                    Value::entity(Tag::String, s),
                ));
            }
            // The creation reference goes, as it would at the end of the
            // statement that built the string.
            assert!(!crate::refcount::ll_release(s));
            assert_eq!((*s).refcount, 2, "both holders, counted as COW demands");

            // Keeper escapes: it survives, and the string with it. Dropper
            // is unheld, so its destructor runs during the fixpoint.
            store_prop(arena_ptr, holder, 16, keeper);
            arena_reset_full(arena_ptr);
        }
        set_current_context(std::ptr::null_mut());

        unsafe {
            assert_eq!(
                (*s).memory_category(),
                MemoryCategory::GcHeap,
                "the string survived with its keeper"
            );
            assert_eq!(
                (*s).refcount,
                1,
                "one surviving holder: the dead one never released twice"
            );
        }
    }

    /// An oversize arena string survives with its holder instead of being
    /// copied out. The store that put it there is arena→arena, so no
    /// barrier saw it, and the escape that follows promotes the whole
    /// subgraph — so a copy-on-write string does reach the payload carry,
    /// which until the layout split only the proved-single-owner form
    /// could (`rfc/model/memory/large-entities.md`).
    #[test]
    fn an_oversize_cow_arena_string_carries_its_payload_through_promotion() {
        let _g = crate::memory::block_pool::test_guard();

        let holder_cls = ClassBuilder::new("Cache").prop("keep", true).build();
        let keeper_cls = ClassBuilder::new("Keeper").prop("s", true).build();

        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let ctx_ptr: *mut LLContext = &mut ctx;
        set_current_context(ctx_ptr);

        let holder = unsafe { new_constructed(ctx_ptr, holder_cls, MemoryCategory::GcHeap) };
        let keeper = unsafe { new_constructed(ctx_ptr, keeper_cls, MemoryCategory::RequestArena) };
        let content = vec![b'p'; crate::memory::block_pool::BLOCK_PAYLOAD];
        let s = unsafe {
            crate::string::ll_string_new(ctx_ptr, MemoryCategory::RequestArena, &content)
        } as *mut RcHeader;
        assert!(!s.is_null());
        assert_ne!(
            unsafe { crate::refcount::header_flags(s) } & crate::refcount::STRING_OUT_OF_LINE,
            0,
            "out of line, or the payload carry is not on the path"
        );

        unsafe {
            let slot = Object::prop_at(keeper, 16);
            assert!(ref_store(
                arena_ptr,
                keeper as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::String, s),
            ));
            assert!(!crate::refcount::ll_release(s));
            store_prop(arena_ptr, holder, 16, keeper);
            arena_reset_full(arena_ptr);
        }
        set_current_context(std::ptr::null_mut());

        unsafe {
            assert_eq!(
                (*s).memory_category(),
                MemoryCategory::GcHeap,
                "promoted with its holder rather than copied at the barrier"
            );
            assert_eq!(
                crate::string::string_bytes(s as *const crate::string::LLString),
                &content[..],
                "and the payload came with it, wherever it now lives"
            );

            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            crate::object::ll_entity_die(holder as *mut RcHeader);
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
            let node = unsafe {
                new_constructed(std::ptr::null_mut(), node_cls, MemoryCategory::RequestArena)
            };
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
        let _trigger =
            unsafe { new_constructed(ctx_ptr, trigger_cls, MemoryCategory::RequestArena) };

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
            assert_eq!(
                (*s).rc.memory_category(),
                MemoryCategory::GcHeap,
                "survivor promoted"
            );
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
        let a_cls = ClassBuilder::new("A")
            .destructor(a_dtor as *const ())
            .build();
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
            assert_eq!(
                (*b).rc.refcount,
                1,
                "B's late release-log entry was drained"
            );

            assert!(ll_release(b as *mut RcHeader));
            ll_object_die(b);
        }
        set_current_context(std::ptr::null_mut());
    }
}
