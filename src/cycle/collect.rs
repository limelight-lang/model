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
use crate::cycle::reclamation::{DeferredReclamation, reclaim_before_drops};
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
/// (`rfc/runtime/exceptions.md`, and `crate::object::DestructorFn`). What
/// remains is a panic in the crate's own code, which is an assertion, and only
/// where the profile unwinds: the release profile aborts at the panic itself.
/// Reached through the two `extern "C"` ABI entries such a panic still ends the
/// process — but at their boundary and not before it, so the drops of every
/// frame below run first, this flag's among them.
///
/// **What that unwind leaves standing is the commit it was in the middle of.**
/// Every drop of the finalization chain is silent while panicking, so a member
/// the teardown had not reached keeps its guard reference and its nulled weak
/// cells, and reads as externally referenced at every later trace; a member it
/// had reached is freed or carries its true count. The children a sever had
/// already queued are rewound with the arena rather than dropped, so their
/// counted references go with them. The thread itself is clean — the window
/// closed, the returns made, the batch merged, the workspace given back — and
/// that memory is lost for the life of the process.
struct CollectingThread;

impl CollectingThread {
    /// Take the right, or answer `None` where this thread may not collect.
    ///
    /// Two states refuse it. **A collection already running**, whose rows and
    /// window a second one would take. And **a reset in flight**, which is the
    /// other place this crate runs user destructors: between
    /// `promote::retain_block` and `promote::place_survivor_lists` a promoted
    /// survivor stands in a block stamped `BLOCK_KIND_RETAINED` with no
    /// occupant list published, and `memory::retained::register` states the
    /// rule that state breaks — "no trace may address it yet". A collection
    /// there reads every such survivor as untracked and frees a member into
    /// the reset window's absorb arm, which reports a teardown that returned
    /// no memory (`memory::reset_window::absorbs_retained_free`).
    fn take() -> Option<Self> {
        if COLLECTING.with(Cell::get) || crate::memory::reset_window::is_open() {
            return None;
        }

        COLLECTING.with(|collecting| collecting.set(true));
        Some(Self)
    }
}

#[cfg(test)]
thread_local! {
    /// Pressure collections this thread has opened since
    /// [`take_pressure_collections`] last answered.
    ///
    /// It counts what `collect_under_pressure` opened rather than what called
    /// it: a call the gate above refuses opens none, and a case that read the
    /// calls would report a collection over a window never taken. Per thread
    /// because a collection is, and because the harness runs cases in
    /// parallel — which is also what makes a case that panics before it reads
    /// the count harmless: the thread it left a count on is its own.
    static PRESSURE_COLLECTIONS: Cell<usize> = const { Cell::new(0) };
}

/// Pressure collections opened on this thread since this last answered, which
/// it leaves at zero.
#[cfg(test)]
pub(crate) fn take_pressure_collections() -> usize {
    PRESSURE_COLLECTIONS.with(|count| count.replace(0))
}

impl Drop for CollectingThread {
    fn drop(&mut self) {
        struct LowerGate;
        impl Drop for LowerGate {
            fn drop(&mut self) {
                COLLECTING.with(|collecting| collecting.set(false));
            }
        }
        let _lower_gate = LowerGate;
        // This guard outlives every trace window, membership and scratch arena
        // of either collection path, including their unwind cleanup. Keep the
        // collecting gate held until the final slot returns have finished.
        unsafe { crate::cycle::queue::retire_candidates() };
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
/// whose children the arena refused. Before final owner retirement, each
/// refusal leaves the graph it was reading byte-identical and no live root
/// loses its registration. The `CollectingThread` guard then removes completed
/// deaths left by this or an earlier collection, so a zero answer does not
/// promise that the candidate queue, its dead slots or their blocks remain
/// byte-identical
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
/// nothing ends the loop, which is what makes it terminate. The trace close
/// restores every entry, then owner retirement removes completed deaths; live
/// registrations remain, so an empty queue is not the stopping condition and
/// progress is.
///
/// **A round that ends the loop without having read the whole lane arms the
/// thread.** That is a bounded round — one that overflowed at a single root,
/// or whose teardown freed nothing — and a round of either kind that an
/// allocation path refused. Only a round that traced every root and freed
/// nothing has read the whole lane; every other ending leaves a prefix behind,
/// and what stands behind it is garbage this path is handing to the poll
/// rather than garbage that is not there.
///
/// **The entity allocation path starts one**, on the refusal that would
/// otherwise be the caller's memory-exhausted
/// ([`crate::memory::heap::entity_alloc`]). Completed candidate slots return
/// after the standing membership ends, before another bounded round and before
/// this function returns to the allocation retry.
///
/// # Safety
/// As [`collect_off_the_poll`], and the caller holds no allocation in flight
/// that the destructors below could reach.
pub(crate) unsafe fn collect_under_pressure() -> usize {
    let Some(_collecting) = CollectingThread::take() else {
        return 0;
    };

    #[cfg(test)]
    PRESSURE_COLLECTIONS.with(|count| count.set(count.get() + 1));

    let mut freed = 0;
    let mut roots = ALL_ROOTS;
    loop {
        let standing = match unsafe { trace_and_harvest(roots) } {
            Traced::Harvested(standing) => standing,
            // Nothing was registered, so there is nothing this path can do and
            // nothing for a later poll to do either.
            Traced::Nothing => break,
            // The trace met a refused allocation path, which under the
            // pressure that started this collection is the ordinary answer
            // rather than a surprise (`dev/DECISIONS.md`, "under memory
            // starvation a collection ends itself and gives back everything").
            // It read no lane and proved nothing about one, so it ends the
            // loop the way a bounded round does: the poll is what tries again,
            // on whatever memory the ending itself gave back.
            Traced::AllocationFailed => {
                crate::gc::arm();
                break;
            }
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

            taken = unsafe { commit_under_pressure(standing.members, &mut arena) };
            arena.reset();
        } else {
            drop(standing);
        }

        // Always repeat after the external drops: their destructors may enter
        // an arena reset or create further completed candidate deaths. On a
        // refused or resurrected component this is the only retirement.
        unsafe { crate::cycle::queue::retire_candidates() };
        freed += taken;
        if taken > 0 && roots != ALL_ROOTS {
            // A bound was in force and it paid, so the roots past it are worth
            // another trace — on the memory this teardown just returned.
            roots = ALL_ROOTS;
            continue;
        }

        if roots != ALL_ROOTS {
            // **A bounded round that freed nothing says nothing about the
            // roots past the bound**, so this is not proof that no garbage
            // remains. Completed deaths have been retired, but a prefix of
            // externally referenced roots can still hide a ring past the
            // bound. The poll keeps its rows and has no region to overflow.
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
/// [`Traced::Nothing`] is the lane that holds nothing and the window that could
/// not be opened; [`Traced::AllocationFailed`] is the trace an allocation path refused
/// and the close that found no list armed. Each of them leaves the heap as it
/// was and every root registered, and they are told apart because one says the
/// lane is empty and the other says nothing about it at all.
///
/// The window is closed here rather than by the caller, which is what makes
/// the blocks go back before the teardown reads the list.
///
/// # Safety
/// As [`collect_under_pressure`].
unsafe fn trace_and_harvest(roots: usize) -> Traced {
    let Some(mut window) = ActiveTrace::open() else {
        return Traced::AllocationFailed;
    };

    window.detach_candidates();
    let (arena, batch) = window.rows_and_roots();
    if batch.is_empty() {
        return Traced::Nothing;
    }

    let (outcome, roots_traced) = unsafe { trace_batch(arena, batch, roots) };
    if outcome != TraceOutcome::Complete {
        return Traced::AllocationFailed;
    }

    // Armed after the trace answered and never before: a trace that gave up
    // leaves no colour that is a verdict, and a harvest of its rows would name
    // entities no scan classified (`crate::cycle::deferred_slot_reuse`).
    if !window.arm_harvest(MEMBER_CAPACITY) {
        return Traced::AllocationFailed;
    }

    drop(window);
    match crate::cycle::members::take_standing() {
        Some(members) => Traced::Harvested(HarvestedMembers {
            members,
            roots_traced,
        }),
        None => Traced::AllocationFailed,
    }
}

/// What one trace of the pressure path answered.
enum Traced {
    /// The sweep harvested a list, which may be empty.
    Harvested(HarvestedMembers),
    /// The lane held no root at all.
    Nothing,
    /// An allocation path refused, or the memory the window stands on could
    /// not be had. Nothing was read, so nothing about the lane was proved.
    AllocationFailed,
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
    match unsafe { commit_before_drops(members, arena) } {
        Some((freed, deferred)) => {
            deferred.drain();
            freed
        }
        None => 0,
    }
}

/// Commit one harvested pressure membership and retire its completed members
/// before releasing their external children.
///
/// Owning `standing` is the phase boundary: its listed membership is scoped to
/// the commit, then the standing list is released before retirement. The
/// deferred-reclamation value keeps the arena and every queued counted child
/// reference alive across that interval (`dev/DECISIONS.md`, "keep early
/// pressure retirement before external-child drops").
unsafe fn commit_under_pressure(
    mut standing: crate::cycle::members::StandingMembers,
    arena: &mut TraceScratchArena,
) -> usize {
    let outcome = {
        let members = Membership::listed(standing.entities_mut());
        unsafe { commit_before_drops(&members, arena) }
    };

    drop(standing);
    let Some((freed, deferred)) = outcome else {
        return 0;
    };

    if early_retirement_enabled() {
        #[cfg(test)]
        let candidates_before = crate::cycle::queue::candidate_count();
        unsafe { crate::cycle::queue::retire_candidates() };
        #[cfg(test)]
        EARLY_RETURNED_SLOTS.with(|returned| {
            returned.set(
                returned.get()
                    + candidates_before.saturating_sub(crate::cycle::queue::candidate_count()),
            )
        });
    }
    deferred.drain();
    freed
}

/// Run a commit through the completed member frees, leaving only its deferred
/// external drops outstanding.
unsafe fn commit_before_drops<'a>(
    members: &Membership<'_>,
    arena: &'a mut TraceScratchArena,
) -> Option<(usize, DeferredReclamation<'a>)> {
    let mut finalization = Finalization::begin();
    let confirmed = members.len() > 0
        && unsafe { finalization.confirm(members) } == ValidationResult::Unreachable;

    let mut pass = finalization.seal().destructors();
    if confirmed {
        unsafe { pass.run(members) };
    }

    let mut revalidation = pass.close();
    let mut reclaimed = None;
    if confirmed {
        match unsafe { revalidation.revalidate(members) } {
            Revalidated::Unreachable(component) => {
                if let Some(deferred) = unsafe { reclaim_before_drops(component, members, arena) } {
                    reclaimed = Some((members.len(), deferred));
                }
            }
            // The set is live: a destructor resurrected a member, and the
            // guards came off inside the reading. Every member keeps its
            // candidate bit and a later trace proposes it again.
            Revalidated::ExternallyReferenced => {}
        }
    }

    revalidation.close();
    reclaimed
}

#[cfg(not(test))]
#[inline]
fn early_retirement_enabled() -> bool {
    true
}

#[cfg(test)]
thread_local! {
    /// S39.4's in-binary A/B arm. Production has no branch: it always takes
    /// the early retirement. A measurement can restore S39.2's final-only
    /// placement without maintaining a second source tree.
    static EARLY_RETIREMENT: Cell<bool> = const { Cell::new(true) };
    static EARLY_RETURNED_SLOTS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
fn early_retirement_enabled() -> bool {
    EARLY_RETIREMENT.with(Cell::get)
}

#[cfg(test)]
fn take_early_returned_slots() -> usize {
    EARLY_RETURNED_SLOTS.with(|returned| returned.replace(0))
}

#[cfg(test)]
struct FinalOnlyRetirement(bool);

#[cfg(test)]
impl FinalOnlyRetirement {
    fn take() -> Self {
        Self(EARLY_RETIREMENT.with(|enabled| enabled.replace(false)))
    }
}

#[cfg(test)]
impl Drop for FinalOnlyRetirement {
    fn drop(&mut self) {
        EARLY_RETIREMENT.with(|enabled| enabled.set(self.0));
    }
}

#[cfg(test)]
mod tests;
