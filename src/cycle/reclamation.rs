//! Step 6 of the commit: the internal edges of a confirmed component are cut,
//! its members are freed, and the children the cut displaced out of the
//! component are dropped after the last of those frees.
//!
//! The order is what makes the property structural rather than argued: between
//! the first null and the last free no user code runs at all, so nothing can
//! store a member into a root while the component is half torn down
//! (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation", step 6).
//! A child inside the component is released where it is met — it stops at its
//! own guard, every member carrying one until this call ends — and a child
//! outside it is held in [`DeferredDrops`](crate::cycle::drops::DeferredDrops) until the frees are behind, because
//! its release runs its destructor.
//!
//! [`reclaim_before_drops`] is the teardown, and [`DeferredReclamation`] is
//! what it hands back: the queued children, held until the driver has ended
//! the membership and, under pressure, returned the completed member slots —
//! only then does its drain run the child destructors. Both production paths
//! reach it through `cycle::collect`'s commit; the test-only `reclaim` is
//! that call and the drain in one.
//!
//! # What a refusal costs, and where it is taken
//!
//! The queue's memory is the collection arena's, and the arena answers null
//! when both of its allocation paths refuse. A refusal met halfway through a
//! sever has no answer: the cells are already null, the members are not yet
//! freed, and dropping a child inline is the one thing step 6 forbids. So the
//! room for the whole component is taken **before the first cell is emptied**,
//! and a component whose room is
//! refused keeps every field it had: [`reclaim_before_drops`] answers `None`,
//! the guards come off through the counted
//! release, and the members stand as floating garbage with their candidate bits
//! up, which a later trace proposes again (`dev/DECISIONS.md`, "under memory
//! starvation a collection ends itself and gives back everything, and each
//! thread frees its own").
//!
//! **The room asked for is exactly the children the sever will queue**,
//! counted
//! by a walk of the same cells the sever is about to empty, each child tested
//! against the membership the way the sever tests it. That is a second stride
//! over the component, and it buys two things a figure read off the layouts
//! does not. A component holding an array of a million integers asks for
//! nothing rather than for eight megabytes of records it would never write —
//! and asking for them is a refusal on the pressure path, where the memory the
//! teardown would release is the memory it is being refused. And the count
//! being exact makes the sever's own obligation checkable: the children queued
//! equal the children counted, in every build, which is what stands under the
//! contract a class with cells outside its body carries
//! ([`crate::cells::OutsideCells::sever`]).
//!
//! # What the drain leaves in the queue of candidates
//!
//! Entries. Every deferred drop is a counted release, so a child that survives
//! it takes a non-final decrement and the candidate gate admits it: the drop
//! writes the thread's live lane. On the ordinary path the teardown runs inside
//! its own trace, whose batch was detached before the mark, so the lane the
//! close finds is not the lane the detach emptied — which is why the close
//! joins the two chains rather than writing one over the other
//! (`cycle::queue::merge_candidates`). That is the design's own clause, "the
//! releases the sever performs \[being\] non-final decrements", read from the
//! side of the entries it produces (`rfc/model/gc/rc-cycle.md`,
//! "Concurrency").
//!
//! # What a member the queue still names costs
//!
//! Its slot, until somebody retires the entry. A member registered as a
//! candidate carries `CANDIDATE_BIT`, and `memory::stdapi::ll_free` withholds
//! such a slot rather than returning it: the entry is a raw pointer, and the
//! trace that pops it reads a refcount out of the body to apply the zero-count
//! rule (`crate::cycle::mark`). **This teardown clears no bit** — the entity is
//! torn down, its children released and its weak cells nulled, and the address
//! stays readable and out of the allocator's hands. After the collection's
//! membership and shadow readers end, `queue::retire_candidates` removes the
//! completed deaths and returns their slots through `ll_free`.
//!
//! # Why the queue is the arena's
//!
//! A component's external children are unbounded — one member can be an array
//! of a million elements — so the queue is a chain of segments rather than a
//! region ([`crate::cycle::records`]), and the segments come from the bump the
//! trace's worklist draws from. The arena owns the chain for the reason it owns
//! the worklist: the segments die at its reset, and a value holding a chain
//! over memory it does not own would make that lifetime a promise its drop
//! cannot check.
//!
//! **Which arena is the driver's to say** (`crate::cycle::collect`). A collection off
//! the safepoint poll runs its teardown inside its own trace and hands that
//! trace's arena over; a collection an allocation failure started has given
//! every block back before the teardown, and what it hands over is a second
//! arena opened over the same workspace.

use crate::cells::{entity_kind, sever_cells};
use crate::cycle::arena::TraceScratchArena;
use crate::cycle::finalization::{GuardedComponent, release_guards};
use crate::cycle::membership::Membership;
use crate::memory::barrier::drop_ref;
use crate::refcount::{MemoryCategory, RcHeader, severed_edge_release};

/// What [`reclaim`] did with one component: the two answers of
/// [`reclaim_before_drops`], named.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg(test)]
pub(crate) enum Reclaimed {
    /// Every member was severed, freed and un-guarded, and every displaced
    /// child outside the component was dropped.
    Freed,
    /// The reservation refusal, in the trace's name for the same event
    /// (`crate::cycle::mark`): both allocation paths answered null, and what
    /// follows is the collection's own end rather than the process's.
    AllocationFailed,
}

/// External children held past a completed sever and every member free.
///
/// The component and its membership are finished before this value exists.
/// What remains is owned by `arena`: one counted reference per queued child,
/// kept until [`DeferredReclamation::drain`] releases it. This split is the
/// successful pressure path's safe point for mutator candidate retirement — no
/// member address is read afterwards, while a child's destructor may allocate
/// from a member slot the retirement returned.
#[must_use = "the severed external children still carry counted references"]
pub(crate) struct DeferredReclamation<'a> {
    arena: &'a mut TraceScratchArena,
    drained: bool,
}

impl DeferredReclamation<'_> {
    /// Release every external child after the caller's between-phase work.
    pub(crate) fn drain(mut self) {
        self.drained = true;
        self.arena.drain_drops(|child| unsafe {
            // The mutator category is `GcHeap` for every member: the validation
            // answers about counted entities alone, so the drop needs no read
            // of a header that is no longer there.
            drop_ref(MemoryCategory::GcHeap, child);
        });
    }
}

impl Drop for DeferredReclamation<'_> {
    fn drop(&mut self) {
        if !self.drained && !std::thread::panicking() {
            panic!("a completed sever's external children were not drained");
        }
    }
}

/// [`reclaim_before_drops`] and the drain in one call, for a case that has no
/// between-phase work.
///
/// # Safety
/// As [`reclaim_before_drops`].
#[cfg(test)]
pub(crate) unsafe fn reclaim(
    component: GuardedComponent<'_>,
    members: &Membership<'_>,
    arena: &mut TraceScratchArena,
) -> Reclaimed {
    let Some(deferred) = (unsafe { reclaim_before_drops(component, members, arena) }) else {
        return Reclaimed::AllocationFailed;
    };

    deferred.drain();
    Reclaimed::Freed
}

/// Tear one confirmed component down — sever its internal edges, free every
/// member, take the guards off — and hand back the children the sever
/// displaced out of it, held for a drain the caller times.
///
/// `members` is the membership the revalidation answered about, in whichever
/// of the two forms the path that produced it holds ([`Membership`]): the
/// pressure path's harvested list, or the rows a collection off the poll keeps
/// through its teardown (`rfc/model/gc/rc-cycle.md`, "When the arena goes back
/// depends on why the collection ran"). When this returns a value the
/// membership names freed entities, and nothing may read it again.
///
/// `component` is that answer, and consuming it here is what states the
/// teardown happened: this call is the only discharge of it that tears down
/// (`GuardedComponent::guards_released`).
///
/// **`None` is the reservation refusal**, answered before the first cell is
/// emptied: the component keeps its edges and gets its true counts back, which
/// is the state a component the revalidation reads as externally referenced is
/// left in — and, as on that arm, a member whose guard was its last reference
/// is freed inside the release rather than kept, so the membership can name a
/// freed entity afterwards.
///
/// A returned value names no member and reads the membership no further, so
/// the caller may end the membership and, under pressure, retire the completed
/// candidate entries before it drains — a child's destructor may allocate from
/// a member slot that retirement returned.
///
/// # Safety
/// Every member is an entity of this thread's GC heap carrying exactly one
/// guard reference, named once in `members`, and `arena` is the collection's,
/// live until the returned value is drained. The call runs on the owning
/// thread with no mutator beside it, and the caller reads no other component
/// until the teardown, the drain included, is behind it (`dev/DECISIONS.md`,
/// "the revalidation of a component and its teardown are adjacent").
pub(crate) unsafe fn reclaim_before_drops<'a>(
    component: GuardedComponent<'_>,
    members: &Membership<'_>,
    arena: &'a mut TraceScratchArena,
) -> Option<DeferredReclamation<'a>> {
    // The most a count without identity can check: the answer and the
    // membership describe the same component. In every build, because a caller
    // that paired the wrong two would tear down a component nothing read again.
    assert_eq!(
        component.members(),
        members.len(),
        "the component read again and the membership severed are the same"
    );

    let external_children = unsafe { members.external_children() };

    if !arena.reserve_drops(external_children) {
        unsafe { component.release(members) };
        return None;
    }

    debug_assert!(
        arena.deferred_drops_are_empty(),
        "a component's teardown starts with the queue the last one drained"
    );

    // Counted against the walk above rather than against the room the chain
    // happens to hold: a segment's slack would absorb a small over-sever and
    // report nothing.
    let mut queued = 0;
    unsafe {
        members.for_each(|member| {
            let kind = entity_kind(member);
            let displaced = |child: *mut RcHeader| {
                if members.contains(child) {
                    // The count cannot reach zero here: the guard stands under
                    // every member until the release below. Why the decrement
                    // is the narrow store and not `ll_release` is
                    // `severed_edge_release`'s own contract.
                    let left = severed_edge_release(child);
                    debug_assert!(
                        left > 0,
                        "a member's guard stands until its component is freed"
                    );
                } else {
                    queued += 1;
                    assert!(
                        queued <= external_children,
                        "the sever displaces the children the walk ahead of it counted"
                    );
                    // The room was reserved for exactly this many, so a push
                    // that refuses is the reservation's own defect — and one
                    // that a release build must see, because the child's
                    // counted reference is what the queue holds.
                    let pushed = arena.push_drop(child);
                    assert!(pushed, "a reserved deferred drop is never refused");
                }
            };

            sever_cells(member, kind, displaced);
        })
    };

    // The other half of the same obligation: a sever that hands over fewer
    // children than the walk counted has left a counted reference standing in
    // the storage of an entity about to be freed, and nothing later reports it
    // — the member it names reads as externally referenced from then on.
    assert_eq!(
        queued, external_children,
        "the sever displaces every child the walk ahead of it counted"
    );

    // Every internal edge is off, so each member stands at its guard alone and
    // the release below is its last reference. The ordinary death path is what
    // frees it: phase 1 finds `DESTRUCTOR_RAN` and runs nothing, phase 2's
    // first act clears a weak cell a destructor of step 4 re-created, its child
    // releases find every cell null, and the slot goes back through the window
    // that withholds it while a trace can still address its row
    // (`crate::cycle::deferred_slot_reuse`, through `memory::stdapi::ll_free`).
    unsafe { release_guards(members) };
    unsafe { component.guards_released() };
    Some(DeferredReclamation {
        arena,
        drained: false,
    })
}

#[cfg(test)]
mod tests;
