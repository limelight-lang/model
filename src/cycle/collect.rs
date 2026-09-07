//! The collection itself: what an ABI entry runs when it fires one.
//!
//! Everything below this module is a piece of one collection — the window over
//! withheld returns, the arena, the two trace phases, the exact validation, the
//! guards and destructors, the sever and the frees. This is the order they run
//! in, and the only place that knows all of them
//! (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation").
//!
//! # Two paths, and what makes them two
//!
//! Why the collection ran decides what it holds when the trace is over
//! (`rfc/model/gc/rc-cycle.md`, "When the arena goes back depends on why the
//! collection ran"). A collection off the safepoint poll — and the explicit
//! `ll_gc_collect_cycles`, which has the same freedom — keeps its rows: the
//! window stays open through the teardown, the membership **is** those rows,
//! and no list is built. A collection an allocation failure started gives its
//! blocks back before the first destructor, because the destructors it is
//! about to run allocate and there is nothing to allocate from; its membership
//! is the list its sweep harvested. The step past the trace is the same for
//! both, and takes the membership rather than either form
//! ([`crate::cycle::membership::Membership`]).
//!
//! # The commit is one component
//!
//! The scan colours rows, and nothing partitions the entities it left
//! unreachable into connected components. This driver hands the whole set to
//! the finalization chain as one, which is sound rather than merely convenient:
//! the identity the exact validation compares holds per member
//! (`crate::cycle::validation`, "The sum stands for the per-member identity"),
//! so a set that meets the sum meets it member by member and nothing is freed
//! because a neighbour balanced it. What the union costs is precision, and only
//! in the two arms that refuse: one destructor that resurrects a member, or one
//! teardown whose children the arena refuses, keeps the whole set for a later
//! collection instead of one component of it. A missed cycle stays eligible
//! (`rfc/model/gc/rc-cycle.md`, "Cost model"), so the cost is latency.
//!
//! # A collection reached from inside a collection collects nothing
//!
//! Step 4 runs user code, and user code allocates, so a poll or an allocation
//! failure can reach this module from inside a destructor of a collection
//! already running. The second collection is refused rather than served: its
//! window would be a second trace on one thread, which
//! [`ActiveTrace::open`] ends the process over, and the rows it would read are
//! the outer collection's. The refusal is the flag below, and it is the
//! runtime's half of the rfc's "Check collection eligibility before waiting".

use std::cell::Cell;

use crate::cycle::arena::TraceScratchArena;
use crate::cycle::deferred_slot_reuse::ActiveTrace;
use crate::cycle::finalization::{Finalization, Revalidated};
use crate::cycle::members::MEMBER_CAPACITY;
use crate::cycle::membership::Membership;
use crate::cycle::reclamation::{Reclaimed, reclaim};
use crate::cycle::trace::{ALL_ROOTS, TraceOutcome, trace_batch};
use crate::cycle::validation::ValidationResult;

thread_local! {
    /// Whether a collection is running on this thread, from the window's
    /// opening to the last deferred drop.
    ///
    /// Per thread because a collection is: the window, the workspace and the
    /// candidate lane it reads are all this thread's, and another thread
    /// collecting its own graph is no reason to refuse this one.
    ///
    /// `Cell<bool>` has no drop glue, which is the rule for anything a thread
    /// exit can reach (`memory::heap::ll_thread_exit`).
    static COLLECTING: Cell<bool> = const { Cell::new(false) };
}

/// The right to run one collection on this thread, taken for as long as one
/// runs.
///
/// The flag falls with this value, and an unwind that reaches its drop takes it
/// down as well. **Which unwind that is, is narrow.** A user destructor is an
/// `unsafe extern "C" fn` and a Limelight exception is caught at its own
/// boundary, so no user code unwinds into this frame
/// (`rfc/runtime/exceptions.md`, and `crate::object::DestructorFn`); the two
/// ABI entries are `extern "C"` too, so a panic raised anywhere inside one of
/// them ends the process before any drop of this module runs. What is left is
/// a panic in the crate's own code reaching the `pub(crate)` entries, which is
/// a debug build's assertion — and there the flag falling is what keeps one
/// failed collection from stopping every later one.
///
/// **What such an unwind leaves standing is the whole commit**: every drop of
/// the finalization chain is silent while panicking, so every member of the
/// union keeps its guard reference and its nulled weak cells, and a guarded
/// member reads as externally referenced at every later trace. The thread
/// itself is clean — the window closed, the returns made, the batch merged,
/// the workspace given back — and that memory is lost for the life of the
/// process.
struct CollectingThread;

impl CollectingThread {
    /// Take the right, or answer `None` where this thread is already
    /// collecting.
    fn take() -> Option<Self> {
        if COLLECTING.with(Cell::get) {
            return None;
        }

        COLLECTING.with(|collecting| collecting.set(true));
        Some(Self)
    }
}

impl Drop for CollectingThread {
    fn drop(&mut self) {
        COLLECTING.with(|collecting| collecting.set(false));
    }
}

/// Collect this thread's candidates, keeping the rows through the teardown, and
/// answer how many entities were freed.
///
/// This is the collection an explicit `ll_gc_collect_cycles` and an armed
/// safepoint poll both run: neither is short of memory, so the arena stays and
/// the teardown reads the rows themselves ([`crate::cycle::membership`]).
///
/// **Zero is every answer short of a teardown**, and they are not
/// distinguished here: a thread already collecting, a workspace the memory
/// manager refused, an empty candidate lane, a trace that met a refused
/// allocation path, a set the exact validation read as live, and a teardown
/// whose children the arena refused. What each of them costs is the collection
/// and nothing else — no root loses its registration, and the heap is
/// byte-identical wherever the trace gave up
/// (`dev/DECISIONS.md`, "under memory starvation a collection ends itself and
/// gives back everything").
///
/// # Safety
/// Called at a safepoint of this mutator: refcounts and edges consistent, and
/// no other thread reading this thread's entities
/// (`rfc/model/gc/strategies.md`, "Collection requests and triggers").
pub(crate) unsafe fn collect_off_the_poll() -> usize {
    let Some(_collecting) = CollectingThread::take() else {
        // The arming a poll spent to reach this stays spent, which is the
        // ruling and its cost together: the flag is an event, and a thread
        // that stayed armed inside its own collection would fire at every poll
        // of the teardown. What the thread loses is one collection — the
        // registrations step 4 made stand in the lane until something arms it
        // again, which the next draw does.
        return 0;
    };

    let Some(mut window) = ActiveTrace::open() else {
        return 0;
    };

    window.detach_candidates();
    let (arena, batch) = window.rows_and_roots();
    if batch.is_empty() {
        return 0;
    }

    if unsafe { trace_batch(arena, batch, ALL_ROOTS) }.0 != TraceOutcome::Complete {
        return 0;
    }

    // The rows this trace wrote, read as the commit's membership. They stand
    // until the window's close sweeps them, which is after everything below.
    let touched = window.arena().touched_head();
    let Some(members) = (unsafe { Membership::rows(touched) }) else {
        return 0;
    };

    unsafe { commit(&members, window.arena()) }
}

/// Collect this thread's candidates for a caller that has run out of memory,
/// giving every block back before the first destructor, and answer how many
/// entities were freed.
///
/// The path an allocation failure starts, and the difference from the ordinary
/// one is the memory rather than the graph: the destructors this is about to
/// run allocate, and there is nothing to allocate from, so the sweep that ends
/// the trace writes the unreachable rows into a fixed region of the thread's
/// workspace and every block goes back before the teardown reads that list
/// (`rfc/model/gc/rc-cycle.md`, "When the arena goes back depends on why the
/// collection ran").
///
/// **What does not fit is traced again over fewer roots.** The region's
/// capacity never grows, a growth here being a request to the very path that
/// refused, and a part of a set closed under its in-edges is not a set that can
/// be torn down ([`crate::cycle::members`]). So an overflow halves the roots
/// and traces again, on the same graph and with the same registrations; at one
/// root still overflowing, the collection ends and arms the thread, that
/// component being past what this path can hold and the poll's to collect.
///
/// **A teardown that freed something under a bound is followed by another
/// trace**, on the memory it just returned, because the roots the bound left
/// out are exactly the garbage this call was asked for. A round that freed
/// nothing ends the loop, which is what makes it terminate: the lane it traces
/// does not shrink — every entry goes back at the close, freed slot or not
/// (`dev/DECISIONS.md`, "the commit clears no candidate bit") — so the
/// stopping condition is progress rather than an empty queue.
///
/// **A bounded round that ends the loop arms the thread**, whether it ended on
/// an overflow at one root or on a teardown that freed nothing. Only a round
/// that traced every root and freed nothing has read the whole lane; a bounded
/// one has read a prefix of it, and what stands behind that prefix is garbage
/// this path is leaving to the poll rather than garbage that is not there.
///
/// **Nothing in the crate starts one yet.** The allocation slow path is where
/// a refusal becomes a collection, and `PLAN.md` S36.15 is the step that puts
/// the call there.
///
/// # Safety
/// As [`collect_off_the_poll`], and the caller holds no allocation in flight
/// that the destructors below could reach.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the allocation slow path that starts one is `PLAN.md` S36.15's"
    )
)]
pub(crate) unsafe fn collect_under_pressure() -> usize {
    let Some(_collecting) = CollectingThread::take() else {
        return 0;
    };

    let mut freed = 0;
    let mut roots = ALL_ROOTS;
    loop {
        let Some(mut standing) = (unsafe { trace_and_harvest(roots) }) else {
            break;
        };

        if standing.overflowed() {
            // The list is empty after an overflow, so nothing is torn down and
            // nothing is owed; the roots keep their registration and the next
            // trace of this loop is over fewer of them.
            let traced = standing.roots_traced;
            drop(standing);
            if traced <= 1 {
                // One root reaches more than the region holds, so no bound
                // makes this component fit. The poll's collection keeps its
                // rows and has no region to overflow — though it does have
                // blocks to be refused, and under the pressure that started
                // this it may meet the same refusal.
                crate::gc::arm();
                break;
            }

            roots = traced / 2;
            continue;
        }

        let mut taken = 0;
        if !standing.members.entities().is_empty() {
            // The trace's own arena went back with its blocks, so the queue
            // the sever's displaced children wait in stands in a second one —
            // over the same workspace, which this thread holds whether or not
            // the pool has anything (`crate::cycle::reclamation`).
            let Some(mut arena) = TraceScratchArena::open() else {
                break;
            };

            let members = Membership::listed(standing.members.entities_mut());
            taken = unsafe { commit(&members, &mut arena) };
            arena.reset();
        }

        drop(standing);
        freed += taken;
        if taken > 0 && roots != ALL_ROOTS {
            // A bound was in force and it paid, so the roots past it are worth
            // another trace — on the memory this teardown just returned.
            roots = ALL_ROOTS;
            continue;
        }

        if roots != ALL_ROOTS {
            // **A bounded round that freed nothing says nothing about the
            // roots past the bound**, so this is not "there is no more
            // garbage" and must not be read as one. Two things produce it and
            // neither is rare: a prefix that names only slots this loop has
            // already freed — an entry is never retired, so the dead stand in
            // the lane where the next bound re-selects them (`dev/DECISIONS.md`,
            // "the commit clears no candidate bit") — and a prefix whose roots
            // are live. What the collection can still do for its caller is
            // hand the rest to the poll, whose own collection keeps its rows
            // and has no region to overflow.
            crate::gc::arm();
        }

        break;
    }

    freed
}

/// One trace of the pressure path: open the window, take the batch, trace the
/// first `roots` roots of it, and close the window so that its sweep harvests
/// the unreachable rows into the thread's member list.
///
/// `None` is every end short of a list: a workspace the manager refused, an
/// empty lane, a trace an allocation path refused, and a close that found no
/// list armed. Each of them leaves the heap as it was and every root
/// registered.
///
/// The window is closed here rather than by the caller, which is what makes
/// the blocks go back before the teardown reads the list.
///
/// # Safety
/// As [`collect_under_pressure`].
unsafe fn trace_and_harvest(roots: usize) -> Option<HarvestedMembers> {
    let mut window = ActiveTrace::open()?;
    window.detach_candidates();

    let (arena, batch) = window.rows_and_roots();
    if batch.is_empty() {
        return None;
    }

    let (outcome, roots_traced) = unsafe { trace_batch(arena, batch, roots) };
    if outcome != TraceOutcome::Complete {
        return None;
    }

    // Armed after the trace answered and never before: a trace that gave up
    // leaves no colour that is a verdict, and a harvest of its rows would name
    // entities no scan classified (`crate::cycle::deferred_slot_reuse`).
    if !window.arm_harvest(MEMBER_CAPACITY) {
        return None;
    }

    drop(window);
    let members = crate::cycle::members::take_standing()?;
    Some(HarvestedMembers {
        members,
        roots_traced,
    })
}

/// What one trace of the pressure path left behind: the harvested list, and
/// the roots the trace read to produce it.
///
/// The count travels with the list because the bound that produced it is the
/// caller's next decision, and the batch it was taken from is gone by then —
/// the close merged it back into the lane.
struct HarvestedMembers {
    members: crate::cycle::members::StandingMembers,
    roots_traced: usize,
}

impl HarvestedMembers {
    /// Whether the trace met more unreachable entities than the region holds.
    fn overflowed(&self) -> bool {
        self.members.overflowed()
    }
}

/// Run the commit over one membership and answer how many entities it freed.
///
/// The order is the design's and the types hold most of it: the guards and the
/// weak nulling are one act, the destructor pass takes the invalidation by
/// value, and the second reading takes the pass
/// (`crate::cycle::finalization`). What this function adds is the one thing no
/// type below states — that a commit reads one membership once
/// (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation").
///
/// A commit is counted even where the validation refuses the set, because the
/// epoch a maturation stamp carries counts commits rather than teardowns
/// (`crate::cycle::epoch`).
///
/// `arena` carries the queue the sever's displaced children wait in, and on the
/// pressure path it is a second arena rather than the trace's: the trace's went
/// back with its blocks.
///
/// # Safety
/// Every member of `members` is an entity of this thread's GC heap whose slot
/// is still its own, the membership is valid for the whole call, and the call
/// runs on the owning thread with no mutator beside it.
unsafe fn commit(members: &Membership<'_>, arena: &mut TraceScratchArena) -> usize {
    let mut finalization = Finalization::begin();
    let confirmed = members.len() > 0
        && unsafe { finalization.confirm(members) } == ValidationResult::Unreachable;

    let mut pass = finalization.seal().destructors();
    if confirmed {
        unsafe { pass.run(members) };
    }

    let mut revalidation = pass.close();
    let mut freed = 0;
    if confirmed {
        match unsafe { revalidation.revalidate(members) } {
            Revalidated::Unreachable(component) => {
                if unsafe { reclaim(component, members, arena) } == Reclaimed::Freed {
                    freed = members.len();
                }
            }
            // The set is live: a destructor resurrected a member, and the
            // guards came off inside the reading. Every member keeps its
            // candidate bit and a later trace proposes it again.
            Revalidated::ExternallyReferenced => {}
        }
    }

    revalidation.close();
    freed
}

#[cfg(test)]
mod tests;
