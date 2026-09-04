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
//! batch's restore — both of them
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

/// Trace `batch`: mark from every root, then scan from every root.
///
/// A refusal in either phase ends the whole trace rather than the root that met
/// it. In the mark that is forced — a partial mark leaves rows subtracted by an
/// incomplete closure, and no colour drawn from them means anything. In the
/// scan the same rule applies one phase later: a colour is a proposal until
/// the exact test reads it, so an abandoned scan keeps none.
///
/// # Safety
/// As [`mark`]: every root is an entity header of this thread's heap whose slot
/// is still its own, and the trace runs where `cells::trace_cells` may read an
/// entity's cells plainly — on the owning thread, with no mutator running
/// beside it.
pub(crate) unsafe fn trace_batch(
    arena: &mut TraceScratchArena,
    batch: &InFlightBatch,
) -> TraceOutcome {
    let marked = batch.walk_roots(|root| unsafe { mark(arena, root) } == MarkResult::Complete);
    if !marked {
        return TraceOutcome::AllocationFailed;
    }

    // Between the phases and only here: both dispatch over the same
    // edges, so a measurement that wants the mark's own count has no
    // other place to read it (`crate::cycle::row::note_phase_boundary`).
    // The body is empty without `cfg(test)`.
    crate::cycle::row::note_phase_boundary();

    let scanned = batch.walk_roots(|root| unsafe { scan(arena, root) } == ScanResult::Complete);
    if !scanned {
        return TraceOutcome::AllocationFailed;
    }

    TraceOutcome::Complete
}

#[cfg(test)]
mod tests;
