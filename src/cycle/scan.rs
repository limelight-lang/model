//! The scan: which met rows are candidates for teardown, and which are
//! held from outside the traced component.
//!
//! The mark left every met row carrying the entity's refcount less the
//! internal edges the trace found, and the scan is what reads that
//! count. A row above zero is held by a reference the trace never saw,
//! so it survives and so does everything reachable from it; a row at
//! zero that no such reference reaches is condemned. A colour is a
//! proposal and never a verdict: what judges the set is
//! `crate::cycle::exact`, which re-reads a component's current fields
//! on the owning thread before any free.
//!
//! **It runs after every root has been marked, never between two
//! marks.** A mark subtracts from rows, so one that ran after a scan
//! would leave a verdict standing on a count that was not final — and a
//! second root reaching into the first one's closure is the ordinary
//! case rather than a rare one (`rfc/model/gc/rc-cycle.md`, "What it
//! is").
//!
//! **No entity is written here either.** The colours go into the shadow
//! rows, so a scan that gives up halfway leaves the heap byte-identical
//! and owes nothing but `ShadowArena::reset`, exactly as the mark does.
//!
//! # Why a condemned row is not a verdict yet
//!
//! Condemning is a decision about one entity, and the trace's unit is a
//! component: a ring is condemned when every member of it is. The scan
//! writes the colour per row and the exact test reads the component, so
//! nothing here needs to know which members belong together.
//!
//! # The colour is re-read at expansion
//!
//! An entity is queued when its colour changes and expanded when it is
//! popped, and between the two another path into it can raise it from
//! condemned to live. So the expansion reads the colour again rather
//! than carrying it on the worklist: what decides the children is the
//! colour the entity holds now (`dev/DECISIONS.md`, "the scan re-reads a
//! colour it may have written").
//!
//! # The descent is the mark's, written twice
//!
//! Pop, load the kind, hand the entity to `cells::trace_cells`, answer a
//! per-child question, abort on a refusal: the loop below is
//! `crate::cycle::mark`'s with one bool and one enum changed. Sharing it
//! would take a trait over the per-child answer for two callers, and the
//! two answers have nothing in common — one subtracts and one colours.
//! What the copy costs is that a change to the refusal handling has to
//! be made in both files.

use crate::cells::{self, PlainCells};
use crate::cycle::arena::{ShadowArena, met_row};
use crate::cycle::row::{Edge, edge_to};
use crate::cycle::shadow::{self, Colour};
use crate::cycle::stack::TraceStack;
use crate::refcount::RcHeader;

/// What a scan from one root answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Scanned {
    /// Every entity the root reaches through a met row carries a
    /// verdict: [`Colour::Condemned`] or [`Colour::Live`].
    Complete,
    /// Both memory doors refused the worklist a segment, so the
    /// collection aborts. The heap is byte-identical and the arena's
    /// reset is the whole of the debt.
    Refused,
}

/// Colour the closure of `root`: every entity it reaches through the
/// rows this collection met.
///
/// A row above zero is held from outside the trace, so it is coloured
/// [`Colour::Live`] and so is everything reachable from it; a row at
/// zero that no live row reaches is [`Colour::Condemned`]. An entity the
/// mark never met is left alone, which is where an edge out of the GC
/// heap and an address the retained population cannot place both end.
///
/// `arena` and `stack` are the collection's, as they are for the mark,
/// and every root must have been marked before the first scan runs.
///
/// # Safety
/// As `mark`: `root` is a live entity header of this thread's heap, and
/// the trace runs where `cells::trace_cells` may read an entity's cells
/// plainly — on the owning thread, with no mutator running beside it.
pub(crate) unsafe fn scan(
    arena: &mut ShadowArena,
    stack: &mut TraceStack,
    root: *mut RcHeader,
) -> Scanned {
    if !unsafe { decide(arena, stack, root, false) } {
        return Scanned::Refused;
    }

    while let Some(entity) = stack.pop() {
        let Some(word) = (unsafe { met_row_of(entity) }) else {
            // Queued only after its row answered, and a row neither
            // moves nor unmeets inside a collection.
            debug_assert!(false, "a queued entity has no met row");
            continue;
        };

        let live = shadow::colour(unsafe { *word }) == Colour::Live;
        // The kind is loaded here and passed down rather than read
        // inside the tracer, which is the contract `trace_cells` states.
        let kind = unsafe { cells::entity_kind(entity) };
        let mut refused = false;
        unsafe {
            cells::trace_cells::<PlainCells>(entity, kind, |cell| {
                // The refusal cannot break out of the tracer, so the
                // remaining cells of this entity are read and dropped.
                // They cost a load each and nothing else: the collection
                // is over, and every row it coloured dies with the arena.
                if refused {
                    return;
                }

                refused = !decide(arena, stack, cell.child, live);
            })
        };

        if refused {
            return Scanned::Refused;
        }
    }

    Scanned::Complete
}

/// Colour one entity the scan has reached and queue it when the colour
/// changed, `from_live` saying whether the edge came from a row already
/// known to be held from outside. False when both memory doors refused.
///
/// The three colours a met row can carry answer differently. `Met` is
/// undecided, and the count decides it — an edge from a live parent
/// decides it live whatever the count says. `Condemned` is decided and
/// not final: a live parent raises it. `Live` is final, and stopping
/// there is what terminates the scan.
///
/// # Safety
/// As [`scan`], and `entity` is a root or a counted child
/// `cells::trace_cells` yielded, hence a live entity header.
unsafe fn decide(
    arena: &mut ShadowArena,
    stack: &mut TraceStack,
    entity: *mut RcHeader,
    from_live: bool,
) -> bool {
    let Some(word) = (unsafe { met_row_of(entity) }) else {
        return true;
    };

    let row = unsafe { *word };
    // `Colour::Untouched` does not reach here: an unmet row is what
    // `met_row` answers `None` for.
    let colour = shadow::colour(row);
    if colour == Colour::Live || (colour == Colour::Condemned && !from_live) {
        return true;
    }

    // A saturated count is a floor rather than a total, and a floor of
    // `COUNT_MAX` is above zero, so this test keeps such a row live
    // without asking about saturation separately
    // (`crate::cycle::shadow::is_saturated`).
    let verdict = if from_live || shadow::count(row) > 0 {
        Colour::Live
    } else {
        Colour::Condemned
    };

    unsafe { shadow::recolour(word, verdict) };
    stack.push(arena, entity)
}

/// The row this collection met for `entity`, or `None` when it has none:
/// an entity outside the GC heap, an address the retained population
/// cannot place, or a slot the mark never reached.
///
/// # Safety
/// `entity` is a live entity header whose block is still this
/// collection's.
#[inline]
unsafe fn met_row_of(entity: *mut RcHeader) -> Option<*mut u32> {
    let Edge::Interior(row) = (unsafe { edge_to(entity) }) else {
        return None;
    };

    unsafe { met_row(row) }
}

#[cfg(test)]
mod tests;
