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
use crate::cycle::membership::Membership;
use crate::cycle::reclamation::{Reclaimed, reclaim};
use crate::cycle::trace::{TraceOutcome, trace_batch};
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
/// The flag falls with this value, on the ordinary exit and on an unwind out of
/// a destructor alike — which is what keeps one raising destructor from
/// stopping every later collection of the thread.
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

    if unsafe { trace_batch(arena, batch) } != TraceOutcome::Complete {
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
