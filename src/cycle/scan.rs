//! The scan: which met rows are candidates for teardown, and which are
//! held from outside the traced component.
//!
//! The mark left every met row carrying the entity's refcount less the
//! internal edges the trace found, and the scan is what reads that
//! count. A row above zero is held by a reference the trace never saw,
//! so it survives and so does everything reachable from it; a row at
//! zero that no such reference reaches is unreachable. A colour is a
//! proposal and never a verdict: what validates the set is
//! `crate::cycle::validation`, which re-reads a component's current fields
//! on the owning thread before any free.
//!
//! **It runs after every root has been marked, never between two
//! marks.** A mark subtracts from rows, so one that ran after a scan
//! would leave a verdict standing on a count that was not final — and a
//! second root reaching into the first one's closure is the ordinary
//! case rather than a rare one (`rfc/model/gc/rc-cycle.md`, "Candidate
//! registration and trial deletion").
//!
//! **No entity is written here either.** The colours go into the shadow
//! rows, so a scan that gives up halfway leaves the heap byte-identical
//! and owes nothing but `TraceScratchArena::reset`, exactly as the mark does.
//!
//! # Why a potentially unreachable row is not a verdict yet
//!
//! Reading a row as unreachable is a decision about one entity, and the trace's
//! unit is a component: a ring is unreachable when every member of it is. The
//! scan writes the colour per row and the exact test reads the component, so
//! nothing here needs to know which members belong together.
//!
//! # The colour is re-read at expansion
//!
//! An entity is queued when its colour changes and expanded when it is
//! popped, and between the two another path into it can raise it from
//! unreachable to live. So the expansion reads the colour again rather
//! than carrying it on the worklist: what decides the children is the
//! colour the entity holds now (`dev/DECISIONS.md`, "the scan re-reads a
//! colour it may have written"). What the entry does carry is the row's
//! address, which is fixed for the collection, so the re-read costs one
//! load rather than a second dispatch on the child's block
//! (`crate::cycle::stack::WorklistEntry`).
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
//!
//! **Nothing here outlives the call.** The rows, the bitmap and the worklist
//! are the caller's arena. The scan asks that arena for one thing only, a
//! worklist segment through
//! [`TraceStack::push`](crate::cycle::stack::TraceStack::push) — it reads rows
//! through
//! [`find_initialized_row`](crate::cycle::arena::find_initialized_row), which
//! allocates nothing — and a refusal there answers
//! [`ScanResult::AllocationFailed`] and abandons the trace with the heap
//! untouched.
//!
//! The ordering the module rests on is the sweep: every row this file reads is
//! read before the trace token is released, and the clearing of the shadow
//! pointers is the last of those reads (`rfc/model/gc/rc-cycle.md`,
//! "Concurrency").

use crate::cells::{self, PlainCells};
use crate::cycle::arena::{TraceScratchArena, find_initialized_row};
use crate::cycle::row::{EdgeTarget, resolve_edge_target};
use crate::cycle::shadow::{self, Color};
use crate::cycle::stack::{TraceStack, WorklistEntry};
use crate::refcount::RcHeader;

/// What a scan from one root answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ScanResult {
    /// Every entity the root reaches through a met row carries a
    /// verdict: [`Color::PotentiallyUnreachable`] or [`Color::Live`].
    Complete,
    /// Both allocation paths refused the worklist a segment, so the
    /// collection aborts. The heap is byte-identical and the arena's
    /// reset is the whole of the debt.
    AllocationFailed,
}

/// Colour the closure of `root`: every entity it reaches through the
/// rows this collection met.
///
/// A row above zero is held from outside the trace, so it is coloured
/// [`Color::Live`] and so is everything reachable from it; a row at zero that
/// no live row reaches is [`Color::PotentiallyUnreachable`]. An entity the mark
/// never met is left alone, which is where an edge out of the GC heap and an
/// address the retained population cannot place both end.
///
/// `arena` and `stack` are the collection's, as they are for the mark,
/// and every root must have been marked before the first scan runs.
///
/// # Safety
/// As `mark`: `root` is a live entity header of this thread's heap, and
/// the trace runs where `cells::trace_cells` may read an entity's cells
/// plainly — on the owning thread, with no mutator running beside it.
pub(crate) unsafe fn scan(
    arena: &mut TraceScratchArena,
    stack: &mut TraceStack,
    root: *mut RcHeader,
) -> ScanResult {
    if !unsafe { classify_and_schedule_entity(arena, stack, root, false) } {
        return ScanResult::AllocationFailed;
    }

    while let Some(entry) = stack.pop() {
        // The colour is read here and the row pointer came off the entry:
        // the classification that queued this entity resolved its address
        // once, and resolving it again would be a second block dispatch for
        // an answer that cannot have changed — a row neither moves nor
        // unmeets inside a collection.
        let live = shadow::color(unsafe { *entry.row }) == Color::Live;
        // The kind is loaded here and passed down rather than read
        // inside the tracer, which is the contract `trace_cells` states.
        let kind = unsafe { cells::entity_kind(entry.entity) };
        let mut refused = false;
        unsafe {
            cells::trace_cells::<PlainCells>(entry.entity, kind, |cell| {
                // The refusal cannot break out of the tracer, so the
                // remaining cells of this entity are read and dropped.
                // They cost a load each and nothing else: the collection
                // is over, and every row it coloured dies with the arena.
                if refused {
                    return;
                }

                refused = !classify_and_schedule_entity(arena, stack, cell.child, live);
            })
        };

        if refused {
            return ScanResult::AllocationFailed;
        }
    }

    ScanResult::Complete
}

/// Colour one entity the scan has reached and queue it when the colour
/// changed, `reached_from_live` saying whether the edge came from a row already
/// known to be held from outside. False when both allocation paths refused.
///
/// The three colours a met row can carry answer differently.
/// `Color::Unclassified` is undecided, and the count decides it — an edge
/// from a live parent decides it live whatever the count says.
/// `Color::PotentiallyUnreachable` is decided and not final: a live parent
/// raises it. `Live` is final, and stopping
/// there is what terminates the scan.
///
/// # Safety
/// As [`scan`], and `entity` is a root or a counted child
/// `cells::trace_cells` yielded, hence a live entity header.
unsafe fn classify_and_schedule_entity(
    arena: &mut TraceScratchArena,
    stack: &mut TraceStack,
    entity: *mut RcHeader,
    reached_from_live: bool,
) -> bool {
    let Some(word) = (unsafe { find_initialized_row_for_entity(entity) }) else {
        return true;
    };

    let row = unsafe { *word };
    // `Color::Untouched` does not reach here: an unmet row is what
    // `find_initialized_row` answers `None` for.
    let color = shadow::color(row);
    if color == Color::Live || (color == Color::PotentiallyUnreachable && !reached_from_live) {
        return true;
    }

    // A saturated count is a lower bound rather than a total, and a lower bound
    // of `COUNT_MAX` is above zero, so this test keeps such a row live without
    // asking about saturation separately
    // (`crate::cycle::shadow::is_saturated`).
    let verdict = if reached_from_live || shadow::count(row) > 0 {
        Color::Live
    } else {
        Color::PotentiallyUnreachable
    };

    unsafe { shadow::recolor(word, verdict) };
    stack.push(arena, WorklistEntry { entity, row: word })
}

/// The row this collection met for `entity`, or `None` when it has none:
/// an entity outside the GC heap, an address the retained population
/// cannot place, or a slot the mark never reached.
///
/// # Safety
/// `entity` is a live entity header whose block is still this
/// collection's.
#[inline]
unsafe fn find_initialized_row_for_entity(entity: *mut RcHeader) -> Option<*mut u32> {
    let EdgeTarget::Tracked(row) = (unsafe { resolve_edge_target(entity) }) else {
        return None;
    };

    unsafe { find_initialized_row(row) }
}

#[cfg(test)]
mod tests;
