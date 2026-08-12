//! Arena death with promotion: the reset-time consumer of the escapee list
//! and its hold-counts (`rfc/model/memory/arena-reset.md`).
//!
//! Phase 1 implements **retention only**, which the RFC makes the whole of
//! the first implementation: no copying, no identity machinery, no
//! reference fixup. Sparse-block evacuation is additive and lands later.
//!
//! The algorithm:
//!
//! 1. **Fixpoint** over the destructor log and the escapee list: from the
//!    escapees whose hold-count is still non-zero, mark the surviving
//!    subgraph, then run pre-destructors of dying, unescaped objects.
//!    Destructors run PHP code and may create new escapes or track new
//!    destructors, hence the loop. No holder slot is ever dereferenced, so
//!    a holder that died before now cannot dangle the reset.
//! 2. **Count.** External references are already each root's `refcount`,
//!    its `IS_ESCAPEE` hold-count kept live by the barrier and by holder
//!    teardown, so this pass adds only internal edges between survivors
//!    and one compensating retain per heap entity a survivor holds: that
//!    entity's release-at-reset record assumed the holder would die, and
//!    the survivor now owes its own release at its real death. A **COW**
//!    survivor is counted apart, in [`reconcile_cow_counts`] after the
//!    fixpoint, its count being a value the mutator reads and destructors
//!    being mutator code.
//! 3. **Retain blocks** carrying survivors: rewrite each survivor's
//!    category to GcHeap in place, stamp the blocks `BLOCK_KIND_RETAINED`
//!    and keep them out of the pool. The pointer-tag alternative was
//!    rejected exactly because this rewrite must be possible. A survivor
//!    that had a block to itself is the exemption: its block is a large
//!    entity's own allocation, which the arena took through
//!    `Arena::alloc_entity` and hands over here instead of retaining, out
//!    of the arena's large-run log and into nothing else, the run registry
//!    having held it since it was allocated
//!    (`rfc/model/memory/large-entities.md`).
//! 4. **Release-at-reset log**: one release per record, with real teardown
//!    dispatch for entities that die of it.
//!
//! Every traversal here — the mark, the re-trace, the count and the COW
//! reconciliation — goes through `walk::trace_entity`, the crate's one
//! kind-dispatched tracer, and never through a kind test of promotion's
//! own (`dev/DECISIONS.md`, "the reset traces through one tracer").

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
                                &raw const (*header).kind,
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
            // Omitting this arm is silent: nothing between the reset and
            // the entity's death looks wrong, which is why it is the one
            // of `large-entities.md`'s four rules for a surviving run
            // that carries a test of its own.
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
                        &raw const (*(block as *mut BlockHeader)).kind,
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

    // COW counts settle here, for the reason they were left alone until
    // now: the fixpoint is where mutator code runs, and on a COW entity
    // the count is what that code reads to decide whether a write may go
    // in place.
    unsafe { reconcile_cow_counts(&survivors, &cow_at_promotion) };

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
    crate::memory::large_entity::is_large_entity(unsafe {
        crate::memory::block_pool::load_block_kind(&raw const (*block).kind)
    })
}

/// Entity teardown dispatch from a bare header — the uniform kind
/// switch. The release log can hold weak cells and reference boxes, and
/// a bare `ll_object_die` on one of those would read a class pointer
/// that is not there.
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
/// request, and that reference belongs to nobody the edge walk can see
/// (`dev/DECISIONS.md`, 2026-08-04).
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
mod tests;
