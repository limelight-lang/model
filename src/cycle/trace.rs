//! One trace over one detached batch: every root marked, then every root
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
//! than by a caller remembering it.
//!
//! # What it owns
//!
//! Nothing. The rows, the bitmap and the worklist are the caller's arena, and
//! the roots are the caller's batch; the phases read the batch twice and write
//! neither it nor any entity. A trace that gives up leaves the heap
//! byte-identical, and what it owes afterwards is the arena's reset and the
//! batch's merge back into the lane — both of them
//! `crate::cycle::deferred_slot_reuse::ActiveTrace`'s at its close.
//!
//! # What a root at zero costs
//!
//! Nothing here refuses one. An entry naming an entity that has since been torn
//! down is legal and expected, the entry being what keeps that slot out of the
//! allocator's hands (`rfc/model/gc/cycle/questions.md`, Y12 clause 7), and the
//! rule that reads its count before its cells is `crate::cycle::mark`'s. The
//! scan meets no row for such a root and passes over it.

use crate::cycle::arena::TraceScratchArena;
use crate::cycle::mark::{MarkResult, mark};
use crate::cycle::queue::InFlightBatch;
use crate::cycle::scan::{ScanResult, scan};

/// What a trace over a whole batch answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TraceOutcome {
    /// Every root was marked and then scanned, and every met row carries a
    /// proposal the exact test may read.
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
/// the exact test reads it, so an abandoned scan keeps none.
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
/// # Safety
/// As [`mark`]: every root is an entity header of this thread's heap whose slot
/// is still its own, and the trace runs where `cells::trace_cells` may read an
/// entity's cells plainly — on the owning thread, with no mutator running
/// beside it.
pub(crate) unsafe fn trace_batch(
    arena: &mut TraceScratchArena,
    batch: &InFlightBatch,
    roots: usize,
) -> (TraceOutcome, usize) {
    let mut traced = 0;
    let mut refused = false;
    batch.walk_roots(|root| {
        if traced == roots {
            return false;
        }

        traced += 1;
        refused = unsafe { mark(arena, root) } != MarkResult::Complete;
        !refused
    });

    if refused {
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
        refused = unsafe { scan(arena, root) } != ScanResult::Complete;
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
