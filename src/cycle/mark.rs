//! The mark: trial deletion from one candidate root, over the shadow
//! rows and never over the heap.
//!
//! Per edge the trace subtracts one from the child's working count; per
//! entity it meets a row once, and the count that row starts from is the
//! entity's own refcount. A row that still reads above zero when the
//! scan arrives is therefore held from outside the traced component
//! (`rfc/model/gc/rc-cycle.md`, "What it is").
//!
//! **No entity is written.** Mark and scan touch shadow rows, the met
//! bitmap and this module's worklist, all three of them in the
//! collection's arena, so a collection that gives up halfway leaves the
//! heap byte-identical and owes nothing but `ShadowArena::reset`
//! (`crate::cycle::arena`).
//!
//! # The descent turns on the meeting
//!
//! An edge into an entity this collection has already expanded takes the
//! decrement and stops there. That is the whole of what terminates the
//! trace, and a ring re-entered at every in-edge would not terminate at
//! all. The bit saying which reach this was is `Met::first_reach`,
//! carried out of the meeting because the meeting is what destroys it
//! (`crate::cycle::arena::ShadowArena::meet`).
//!
//! The descent carries an explicit worklist rather than the native
//! stack, and why is `crate::cycle::stack`.

use crate::cells::{self, PlainCells};
use crate::cycle::arena::{Met, ShadowArena};
use crate::cycle::row::{Edge, edge_to};
use crate::cycle::shadow;
use crate::cycle::stack::TraceStack;
use crate::refcount::{RcHeader, header_refcount};

/// What a mark from one root answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Marked {
    /// The closure is exhausted: every entity the root reaches through
    /// the GC heap has been met, and every internal edge the trace found
    /// has been subtracted from the row it points at.
    Complete,
    /// Both memory doors refused, so the collection aborts. The heap is
    /// byte-identical and the arena's reset is the whole of the debt.
    Refused,
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
/// **Nothing is written into any entity**, so [`Marked::Refused`] leaves
/// the heap byte-identical and the caller's whole duty is
/// `ShadowArena::reset`.
///
/// # Safety
/// `root` is a live entity header of this thread's heap, and the trace
/// runs where `cells::trace_cells` may read an entity's cells plainly —
/// on the owning thread, with no mutator running beside it.
pub(crate) unsafe fn mark(
    arena: &mut ShadowArena,
    stack: &mut TraceStack,
    root: *mut RcHeader,
) -> Marked {
    if !unsafe { meet_root(arena, stack, root) } {
        return Marked::Refused;
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
            return Marked::Refused;
        }
    }

    Marked::Complete
}

/// Meet the root's own row and queue it for expansion. False when both
/// memory doors refused.
///
/// **The root takes no subtraction.** The row starts at the entity's
/// refcount and the trace subtracts the edges it finds; the queue entry
/// that named this root is not one of them, and subtracting for it would
/// condemn a component held by a single external reference.
///
/// # Safety
/// As [`mark`].
unsafe fn meet_root(arena: &mut ShadowArena, stack: &mut TraceStack, root: *mut RcHeader) -> bool {
    let Edge::Interior(row) = (unsafe { edge_to(root) }) else {
        // The enrolment gate admits none: an entity outside the GC heap
        // never reaches the queue (`rfc/model/gc/rc-cycle.md`, "Death
        // while enrolled"). Answered rather than asserted, because the
        // collection's cost of being wrong here is one root that traces
        // nothing.
        return true;
    };

    match unsafe { arena.meet(row, header_refcount(root)) } {
        Met::Refused => false,
        Met::Unplaced => true,
        Met::Row { first_reach, .. } => {
            if first_reach {
                stack.push(arena, root)
            } else {
                true
            }
        }
    }
}

/// Take one out-edge of an entity being expanded: subtract it from the
/// child's working count, and queue the child when this collection has
/// not seen it before. False when both memory doors refused.
///
/// An edge the row dispatch cannot place — a child outside the GC heap,
/// or a retained block whose object index does not name it — is counted
/// as an external live reference and followed no further, which keeps
/// the referent alive rather than condemning it on a guessed row.
///
/// **S37.1's maturation prune belongs at the head of this function**,
/// above the block dispatch: a matured child is read as an opaque live
/// external, which is the answer this function already gives an edge
/// leaving the heap, so the prune adds a header test and no second
/// dispatch. It cannot live in `edge_to`, because the prune is evaluated
/// on the target of an edge and never on a root, and [`meet_root`] asks
/// `edge_to` the same question (`rfc/model/gc/rc-cycle.md`, "What it
/// is").
///
/// # Safety
/// As [`mark`], and `child` is a counted child `cells::trace_cells`
/// yielded, hence a live entity header.
unsafe fn visit_child(
    arena: &mut ShadowArena,
    stack: &mut TraceStack,
    child: *mut RcHeader,
) -> bool {
    let Edge::Interior(row) = (unsafe { edge_to(child) }) else {
        return true;
    };

    match unsafe { arena.meet(row, header_refcount(child)) } {
        Met::Refused => false,
        Met::Unplaced => true,
        Met::Row { row, first_reach } => {
            unsafe { shadow::subtract(row, 1) };
            if first_reach {
                stack.push(arena, child)
            } else {
                true
            }
        }
    }
}

#[cfg(test)]
mod tests;
