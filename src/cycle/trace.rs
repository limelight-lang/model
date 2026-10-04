//! One trace over one batch read out of the ring: every root marked, then every root
//! scanned.
//!
//! The order is the whole of this module, and it is a correctness requirement
//! rather than a convenience. It follows from the arithmetic trial deletion
//! rests on: the mark subtracts each internal edge from the row it points at
//! (`rfc/model/gc/rc-cycle.md`, "Candidate registration and trial deletion"),
//! so a row is final only once every root has been marked, and a scan run
//! before that reads a count still owed subtractions. A second root reaching
//! into the first one's closure is what makes the case ordinary rather than
//! rare, and the rfc states neither the ordering nor that frequency. Both
//! phases run here, in one function, so the rule holds by construction rather
//! than by a caller remembering it — with one other keeper of the same order:
//! the collector thread's batch runs the two phases over `AtomicCells` in
//! `crate::cycle::worker` (`worker::trace`), and it is that function's
//! obligation to mark every root before any root scans.
//!
//! # What it owns
//!
//! Nothing. The rows, the bitmap and the worklist are the caller's arena, and
//! the roots are the caller's batch; the phases read the batch twice and write
//! neither it nor any entity. A trace that gives up leaves the heap
//! byte-identical, and what it owes afterwards is the arena's reset and the
//! batch's disposition in place — both of them
//! `crate::cycle::deferred_slot_reuse::ActiveTrace`'s at its close.
//!
//! # What a root at zero costs
//!
//! Nothing here refuses one. An entry naming an entity that has since been torn
//! down is legal and expected, the entry being what keeps that slot out of the
//! allocator's hands (`rfc/model/gc/cycle/questions.md`, Y12 clause 7), and the
//! rule that reads its count before its cells is `crate::cycle::mark`'s. The
//! scan meets no row for such a root and passes over it.

use crate::cells::CellReader;
use crate::cycle::arena::TraceScratchArena;
use crate::cycle::mark::{MarkResult, drain, drain_within_the_met, schedule_root_if_unvisited};
use crate::cycle::posted_set::PostedSet;
use crate::cycle::queue::Batch;
use crate::cycle::scan::{ScanResult, scan};

/// What a trace over a whole batch answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TraceOutcome {
    /// Every root was marked and then scanned, and every met row carries a
    /// proposal the exact validation may read.
    Complete,
    /// An allocation path refused, in either phase, and the trace was
    /// abandoned where it stood. The heap is byte-identical and no colour is a
    /// verdict; the roots keep their registration because nothing here disposes
    /// of one, and their records go back with the window's close.
    AllocationFailed,
}

/// Every root of the batch, which is what a collection with room for the
/// answer traces.
pub(crate) const ALL_ROOTS: usize = usize::MAX;

/// Trace the first `roots` roots of `batch`: mark from each, then scan from
/// each.
///
/// A refusal in either phase ends the whole trace rather than the root that met
/// it. In the mark that is forced — a partial mark leaves rows subtracted by an
/// incomplete closure, and no colour drawn from them means anything. In the
/// scan the same rule applies one phase later: a colour is a proposal until
/// the exact validation reads it, so an abandoned scan keeps none.
///
/// **A bound on the roots is conservative and not a partial answer.** A root
/// left untraced subtracts no edge from any row, so every row this trace reads
/// stands at or above the count the whole batch would have left it at, and a
/// row read as potentially unreachable under the bound would read the same
/// without it. What the bound loses is the garbage the untraced roots name,
/// which keeps its registration and is the next trace's
/// (`rfc/model/gc/rc-cycle.md`, "Cost model": trace precision affects cost and
/// latency rather than safety). [`ALL_ROOTS`] is the collection that wants
/// none of it; the bound is the pressure path's, whose answer has to fit a
/// region of fixed size ([`crate::cycle::members`]).
///
/// **Both phases take the same prefix**, which is the same requirement as the
/// order between them: a root marked and not scanned leaves its closure's rows
/// subtracted and uncoloured.
///
/// The roots traced come back with the answer, because the caller that bounds
/// them needs to know what the bound was worth: the batch carries no count and
/// the walk is the only place one is taken.
///
/// `R` is how the cells are read: `PlainCells` on the owning thread,
/// `AtomicCells` from a collector thread holding the mutator's token
/// (`cells::CellReader`).
///
/// # Safety
/// As [`drain`]: every root is an entity header of the owning thread's heap
/// whose slot is still its own, and the trace runs where `cells::trace_cells`
/// may read an entity's cells through `R`.
pub(crate) unsafe fn trace_batch<R: CellReader>(
    arena: &mut TraceScratchArena,
    batch: &Batch,
    roots: usize,
) -> (TraceOutcome, usize) {
    let mut traced = 0;
    let mut refused = false;
    batch.walk_roots(|root| {
        if traced == roots {
            return false;
        }

        traced += 1;
        refused = !unsafe { schedule_root_if_unvisited(arena, root) };
        !refused
    });

    // Every root met before any is expanded, so no edge into a batch root is
    // a first visit and none is held (`crate::cycle::mark`, "The held stack").
    if refused || unsafe { drain::<R>(arena) } != MarkResult::Complete {
        return (TraceOutcome::AllocationFailed, traced);
    }

    // Between the phases and only here: both dispatch over the same
    // edges, so a measurement that wants the mark's own count has no
    // other place to read it (`crate::cycle::row::note_phase_boundary`).
    // The body is empty without `cfg(test)`.
    crate::cycle::row::note_phase_boundary();

    let mut scanned = 0;
    batch.walk_roots(|root| {
        if scanned == traced {
            return false;
        }

        scanned += 1;
        refused = unsafe { scan::<R>(arena, root) } != ScanResult::Complete;
        !refused
    });

    if refused {
        return (TraceOutcome::AllocationFailed, traced);
    }

    // The scan's end is the trace's last row read off the poll, and the token
    // has to stand through it; the probe is empty without `cfg(test)`.
    crate::cycle::token::note_last_row_read();
    (TraceOutcome::Complete, traced)
}

#[cfg(test)]
mod tests;

// The traces within a set that found it garbage whole and skipped the scan
// (tests only). Per thread, as a collection is.
#[cfg(test)]
thread_local! {
    static SETS_GARBAGE_WHOLE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The traces within a set on this thread that skipped the scan since this
/// last answered, which it leaves at zero.
#[cfg(test)]
pub(crate) fn take_sets_garbage_whole() -> usize {
    SETS_GARBAGE_WHOLE.with(|count| count.replace(0))
}

/// Trace the set a collection over P validates: every member of `set` and
/// every root of `batch` met first, a slot whose count reads zero met by
/// nobody, then [`drain_within_the_met`], then a scan from each — trial
/// deletion restricted to the set, whose potentially unreachable rows are
/// closed under referrers whatever the set holds
/// (`crate::cycle::posted_set`, "Why the set and not its roots"), the scan
/// skipped where every met row reads zero
/// ([`crate::cycle::row::colour_unreachable_where_every_met_row_reads_zero`]).
/// Without a set the roots are the set. Answers the roots of `batch` traced, as
/// [`trace_batch`] does.
///
/// # Safety
/// As [`trace_batch`], on the owning thread through `PlainCells`: every
/// member of `set` is an address the collector listed in this thread's heap,
/// no block of which went back to the pool since (`posted_set`'s hooks).
pub(crate) unsafe fn trace_within_the_set<R: CellReader>(
    arena: &mut TraceScratchArena,
    batch: &Batch,
    set: Option<&PostedSet>,
) -> (TraceOutcome, usize) {
    let members = || set.into_iter().flat_map(PostedSet::members);
    let mut refused = members().any(|member| !unsafe { schedule_root_if_unvisited(arena, member) });
    let mut traced = 0;
    if !refused {
        batch.walk_roots(|root| {
            traced += 1;
            refused = !unsafe { schedule_root_if_unvisited(arena, root) };
            !refused
        });
    }

    if refused || unsafe { drain_within_the_met::<R>(arena) } != MarkResult::Complete {
        return (TraceOutcome::AllocationFailed, traced);
    }

    crate::cycle::row::note_phase_boundary();
    // A set none of whose met rows any edge from outside holds is garbage
    // whole, and its colours need no scan of its cells.
    if unsafe {
        crate::cycle::row::colour_unreachable_where_every_met_row_reads_zero(arena.touched_head())
    } {
        #[cfg(test)]
        SETS_GARBAGE_WHOLE.with(|count| count.set(count.get() + 1));
        arena.keep_the_cells_left_out_as_external_children();
        #[cfg(feature = "recycler-over-counts")]
        if let Some(set) = set {
            unsafe { check_a_set_proved_by_its_tags(set) };
        }
        crate::cycle::token::note_last_row_read();
        return (TraceOutcome::Complete, traced);
    }

    if members().any(|member| unsafe { scan::<R>(arena, member) } != ScanResult::Complete) {
        return (TraceOutcome::AllocationFailed, traced);
    }

    batch.walk_roots(|root| {
        refused = unsafe { scan::<R>(arena, root) } != ScanResult::Complete;
        !refused
    });
    if refused {
        return (TraceOutcome::AllocationFailed, traced);
    }

    #[cfg(feature = "recycler-over-counts")]
    if let Some(set) = set {
        unsafe { check_a_set_proved_by_its_tags(set) };
    }
    crate::cycle::token::note_last_row_read();
    (TraceOutcome::Complete, traced)
}

#[cfg(all(test, feature = "recycler-over-counts"))]
thread_local! {
    /// Sets the collector proved garbage by their tags whose exact
    /// validation this thread ran since this last answered, which it leaves
    /// at zero.
    static SETS_PROVED_BY_TAGS_VALIDATED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The sets proved by their tags [`trace_within_the_set`] validated on this thread since
/// this last answered.
#[cfg(all(test, feature = "recycler-over-counts"))]
pub(crate) fn take_sets_proved_by_tags_validated() -> usize {
    SETS_PROVED_BY_TAGS_VALIDATED.with(|count| count.replace(0))
}

/// The exact validation checked against the collector's Δ-test: no member
/// of a set the collector proved garbage by its tags may read live here
/// (`crate::cycle::delta_test`). A debug build asserts it; the release build
/// reads nothing.
///
/// # Safety
/// As [`trace_within_the_set`], after its scan, the rows still standing.
#[cfg(feature = "recycler-over-counts")]
unsafe fn check_a_set_proved_by_its_tags(set: &PostedSet) {
    if !set.proved_by_its_tags() {
        return;
    }

    #[cfg(test)]
    SETS_PROVED_BY_TAGS_VALIDATED.with(|count| count.set(count.get() + 1));
    #[cfg(debug_assertions)]
    for member in set.members() {
        if unsafe { crate::refcount::slot_state(member) } != crate::refcount::SlotState::Live {
            continue;
        }
        let crate::cycle::row::EdgeTarget::Tracked(key) =
            (unsafe { crate::cycle::row::resolve_edge_target(member) })
        else {
            continue;
        };
        if let Some(row) = unsafe { crate::cycle::arena::find_initialized_row(key) } {
            assert_ne!(
                crate::cycle::shadow::color(unsafe { *row }),
                crate::cycle::shadow::Color::Live,
                "a member of a set the collector proved garbage reads live in the exact \
                 validation: {member:p}"
            );
        }
    }
}
