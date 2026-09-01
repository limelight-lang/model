//! The mark: trial deletion from one candidate root, over the shadow
//! rows and never over the heap.
//!
//! Per edge the trace subtracts one from the child's working count; per
//! entity it meets a row once, and the count that row starts from is the
//! entity's own refcount. A row that still reads above zero when the
//! scan arrives is therefore held from outside the traced component
//! (`rfc/model/gc/rc-cycle.md`, "Candidate registration and trial
//! deletion").
//!
//! **No entity is written.** Mark and scan touch shadow rows, the met
//! bitmap and this module's worklist, all three of them in the
//! collection's arena, so a collection that gives up halfway leaves the
//! heap byte-identical and owes nothing but `TraceScratchArena::reset`
//! (`crate::cycle::arena`).
//!
//! # The descent turns on the meeting
//!
//! An edge into an entity this collection has already expanded takes the
//! decrement and stops there. That is the whole of what terminates the
//! trace, and a ring re-entered at every in-edge would not terminate at
//! all. The bit saying which reach this was is `RowLookup::first_visit`,
//! carried out of the meeting because the meeting is what destroys it
//! (`crate::cycle::arena::TraceScratchArena::ensure_row`).
//!
//! The descent carries an explicit worklist rather than the machine
//! stack, and why is `crate::cycle::stack`.
//!
//! # What it owns, and what a refusal costs
//!
//! Nothing outlives the call: the rows, the bitmap and the worklist are the
//! caller's arena, and the mark holds a `&mut` to it for one trace. Two
//! things it can ask that arena for — a row array, through
//! [`TraceScratchArena::ensure_row`](crate::cycle::arena::TraceScratchArena::ensure_row),
//! and a worklist segment, through
//! [`TraceStack::push`](crate::cycle::stack::TraceStack::push). Either refused
//! answers [`MarkResult::AllocationFailed`], which abandons the trace where it
//! stands. Abandoning is free precisely because no entity was written.
//!
//! One ordering matters and it is this module's: the row is ensured before
//! the count is subtracted, so an entity reached for the first time starts
//! from its refcount rather than from a subtraction against a row that does
//! not exist yet.

use crate::cells::{self, PlainCells};
use crate::cycle::arena::{RowLookup, TraceScratchArena};
use crate::cycle::row::{EdgeTarget, resolve_edge_target};
use crate::cycle::shadow;
use crate::cycle::stack::TraceStack;
use crate::refcount::{RcHeader, header_refcount};

/// What a mark from one root answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MarkResult {
    /// The closure is exhausted: every entity the root reaches through
    /// the GC heap has been met, and every internal edge the trace found
    /// has been subtracted from the row it points at.
    Complete,
    /// Both allocation paths refused, so the collection aborts. The heap is
    /// byte-identical and the arena's reset is the whole of the debt.
    AllocationFailed,
}

/// Trial-delete the component reachable from `root`, leaving the verdict
/// to the scan.
///
/// Every entity reached through the GC heap is met once, its row
/// initialised from its refcount; every edge the trace finds between two
/// met entities is subtracted from the target's row. Edges leaving the
/// GC heap are counted as external references and followed no further,
/// which is what keeps a ring through the arena — broken by the arena's
/// own reset — out of the collector's reach (`crate::cycle::row`).
///
/// `arena` and `stack` belong to the collection rather than to the root:
/// a second root inside the first one's closure meets rows that already
/// say met, expands nothing twice, and reuses the segments the first
/// root's depth drew.
///
/// **Nothing is written into any entity**, so [`MarkResult::AllocationFailed`]
/// leaves the heap byte-identical and the caller's whole duty is
/// `TraceScratchArena::reset`.
///
/// # Safety
/// `root` is a live entity header of this thread's heap, and the trace
/// runs where `cells::trace_cells` may read an entity's cells plainly —
/// on the owning thread, with no mutator running beside it.
pub(crate) unsafe fn mark(
    arena: &mut TraceScratchArena,
    stack: &mut TraceStack,
    root: *mut RcHeader,
) -> MarkResult {
    if !unsafe { schedule_root_if_unvisited(arena, stack, root) } {
        return MarkResult::AllocationFailed;
    }

    while let Some(entity) = stack.pop() {
        // The kind is loaded here and passed down rather than read
        // inside the tracer, which is the contract `trace_cells` states:
        // a collector holds the kind from its own reading of the header
        // and does not go back to a word the mutator may be writing.
        let kind = unsafe { cells::entity_kind(entity) };
        let mut refused = false;
        unsafe {
            cells::trace_cells::<PlainCells>(entity, kind, |cell| {
                // The refusal cannot break out of the tracer, so the
                // remaining cells of this entity are read and dropped.
                // They cost a load each and nothing else: the collection
                // is over, and every row it wrote dies with the arena.
                if refused {
                    return;
                }

                refused = !visit_child(arena, stack, cell.child);
            })
        };

        if refused {
            return MarkResult::AllocationFailed;
        }
    }

    MarkResult::Complete
}

/// Meet the root's own row and queue it for expansion. False when both
/// allocation paths refused.
///
/// **The root takes no subtraction.** The row starts at the entity's
/// refcount and the trace subtracts the edges it finds; the queue entry
/// that named this root is not one of them, and subtracting for it would
/// read a component held by a single external reference as unreachable.
///
/// # Safety
/// As [`mark`].
unsafe fn schedule_root_if_unvisited(
    arena: &mut TraceScratchArena,
    stack: &mut TraceStack,
    root: *mut RcHeader,
) -> bool {
    let EdgeTarget::Tracked(row) = (unsafe { resolve_edge_target(root) }) else {
        // The candidate gate admits none: an entity outside the GC heap
        // never reaches the queue (`rfc/model/gc/rc-cycle.md`,
        // "Zero-count entities pending slot reuse"). Answered rather than asserted, because the
        // collection's cost of being wrong here is one root that traces
        // nothing.
        return true;
    };

    match unsafe { arena.ensure_row(row, header_refcount(root)) } {
        RowLookup::AllocationFailed => false,
        RowLookup::Untracked => true,
        RowLookup::Ready { first_visit, .. } => {
            if first_visit {
                stack.push(arena, root)
            } else {
                true
            }
        }
    }
}

/// Take one out-edge of an entity being expanded: subtract it from the
/// child's working count, and queue the child when this collection has
/// not seen it before. False when both allocation paths refused.
///
/// An edge the row dispatch cannot place — a child outside the GC heap, or a
/// retained block whose object index does not name it — is counted as an
/// external live reference and followed no further, which keeps the referent
/// alive rather than reading it as unreachable on a row the trace guessed.
///
/// **S37.1's maturation prune belongs at the head of this function**, above the
/// block dispatch: a matured child is read as an opaque live external, which is
/// the answer this function already gives an edge leaving the heap, so the
/// prune adds a header test and no second dispatch. It cannot live in
/// `resolve_edge_target`, because the prune is evaluated on the target of an
/// edge and never on a root, and [`schedule_root_if_unvisited`] asks
/// `resolve_edge_target` the same question (`rfc/model/gc/rc-cycle.md`,
/// "Candidate registration and trial deletion").
///
/// # Safety
/// As [`mark`], and `child` is a counted child `cells::trace_cells`
/// yielded, hence a live entity header.
unsafe fn visit_child(
    arena: &mut TraceScratchArena,
    stack: &mut TraceStack,
    child: *mut RcHeader,
) -> bool {
    let EdgeTarget::Tracked(row) = (unsafe { resolve_edge_target(child) }) else {
        return true;
    };

    match unsafe { arena.ensure_row(row, header_refcount(child)) } {
        RowLookup::AllocationFailed => false,
        RowLookup::Untracked => true,
        RowLookup::Ready { row, first_visit } => {
            unsafe { shadow::subtract(row, 1) };
            if first_visit {
                stack.push(arena, child)
            } else {
                true
            }
        }
    }
}

#[cfg(test)]
mod tests;
