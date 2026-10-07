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
//! marks.** A mark subtracts from rows (`rfc/model/gc/rc-cycle.md`,
//! "Candidate registration and trial deletion"), so one that ran after a
//! scan would leave a verdict standing on a count that was not final,
//! and a second root reaching into the first one's closure is the
//! ordinary case rather than a rare one. What holds the order is
//! `crate::cycle::trace`, which runs both phases over one batch.
//!
//! **No entity is written here either.** The colours go into the shadow
//! rows, so a scan that gives up halfway leaves the heap byte-identical
//! and owes nothing but `TraceScratchArena::reset`, exactly as the mark does.
//!
//! # Why a potentially unreachable row is not a verdict yet
//!
//! Reading a row as unreachable is a decision about one entity, and the trace's
//! unit is a component: a ring is unreachable when every member of it is. The
//! scan writes the colour per row and the exact validation reads the
//! component, so nothing here needs to know which members belong together.
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
//! Pop, load the kind, hand the entity to `cells::trace_cells_until`, answer
//! a per-child question, stop on a refusal or a recall: the loop below is
//! `crate::cycle::mark`'s with the answer and one enum changed. What the two
//! share is the visitor, `crate::cycle::mark::Expansion`, which counts the
//! positions toward the recall and stops the stride; the per-child answers
//! have nothing in common — one subtracts and one colours — so each phase
//! hands it its own. What the copy of the loop costs is that a change to
//! how a stop is answered has to be made in both files.
//!
//! **Nothing here outlives the call.** The rows, the bitmap and the worklist
//! are the caller's arena. The scan asks that arena for one thing only, a
//! worklist segment through
//! [`TraceScratchArena::push_work`](crate::cycle::arena::TraceScratchArena::push_work)
//! — it reads rows through [`find_initialized_row`], which allocates nothing —
//! and a refusal there answers [`ScanResult::AllocationFailed`] and abandons
//! the trace with the heap untouched.
//!
//! The ordering the module rests on is the token: every row this file reads
//! is read before the trace token is released (`rfc/model/gc/rc-cycle.md`,
//! "Concurrency").

use crate::cells::{self, CellReader};
use crate::cycle::arena::{TraceScratchArena, find_initialized_row};
use crate::cycle::mark::Expansion;
use crate::cycle::row::{EdgeTarget, resolve_edge_target};
use crate::cycle::shadow::{self, Color};
use crate::cycle::stack::WorklistEntry;
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
    /// The traced mutator recalled its token, as `crate::cycle::mark`'s
    /// `MarkResult::Recalled` says.
    Recalled,
}

/// Colour the closure of `root`: every entity it reaches through the
/// rows this collection met.
///
/// A row above zero is held from outside the trace, so it is coloured
/// [`Color::Live`] and so is everything reachable from it; a row at zero that
/// no live row reaches is [`Color::PotentiallyUnreachable`]. An entity the mark
/// never met is left alone, which is where an edge out of the GC heap, an
/// address the retained population cannot place, and an edge the mark pruned
/// at a mature target all end (`crate::cycle::mark`).
///
/// `arena` is the collection's and carries the worklist, as it does for the
/// mark, and every root must have been marked before the first scan runs.
///
/// # Safety
/// As `mark`: `root` is an entity header of the owning thread's heap whose
/// slot is still its own — a candidate the queue names, live or dead — and the
/// trace runs where `cells::trace_cells` may read an entity's cells through
/// `R`. A root that was torn down
/// has no met row, so the dispatch reads the block header, and for a large
/// entity the header's own flags and for a retained one its count
/// (`crate::cycle::row`) — all of them the slot's own while a queue entry names
/// it. Its cells are never read: the mark meets no row at count zero
/// (`crate::cycle::mark`, "A root at count zero is expanded by nothing"), and
/// only a met row reaches the expansion.
pub(crate) unsafe fn scan<R: CellReader>(
    arena: &mut TraceScratchArena,
    root: *mut RcHeader,
) -> ScanResult {
    if !unsafe { classify_and_schedule_entity(arena, root, false) } {
        return ScanResult::AllocationFailed;
    }

    while let Some(entry) = arena.pop_work() {
        // The colour is read here and the row pointer came off the entry:
        // the classification that queued this entity resolved its address
        // once, and resolving it again would be a second block dispatch for
        // an answer that cannot have changed — a row neither moves nor
        // unmeets inside a collection.
        let live = shadow::color(unsafe { *entry.row }) == Color::Live;
        // The kind is loaded here and passed down rather than read
        // inside the tracer, which is the contract `trace_cells` states.
        let kind = unsafe { cells::entity_kind(entry.entity) };
        let expansion = Expansion::<R, _>::new(arena, |arena, child| unsafe {
            classify_and_schedule_entity(arena, child, live)
        });
        if unsafe { cells::trace_cells_until::<R>(entry.entity, kind, expansion) }.is_break() {
            return if arena.was_recalled() {
                ScanResult::Recalled
            } else {
                ScanResult::AllocationFailed
            };
        }
    }

    ScanResult::Complete
}

/// Colour every row a collector's completed mark met from the edges that mark
/// recorded, never from the heap (`dev/design/recycler-over-counts.md`, §3.5;
/// `crate::cycle::recorded_edges` says why): a row above zero is
/// [`Color::Live`] and so is every row a recorded edge out of a live row
/// reaches; every other met row is [`Color::PotentiallyUnreachable`]. What
/// [`scan`] from each root answers over a heap no one wrote since the mark.
///
/// **Two passes.** The first walks the record in order: each row it names,
/// header or edge, is coloured by its count at its first reading, and a run's
/// header row then takes its run's index in place of the count, which has
/// answered the only question the scan asks of it — the right
/// [`shadow::write_live_index`] claims for the maturation descent after it.
/// Every live row heading a run goes on the worklist at its run. The second
/// pops the worklist and raises each potentially unreachable target of the
/// popped row's run to live, queueing it in turn where it heads a run of its
/// own. Each entry of the record is read once in the
/// first pass and each edge at most once more in the second; each counts as
/// a position toward the recall, which is read once at the start as well.
///
/// **A stop inside it leaves live rows holding a run index in place of their
/// count**, which no reader of a stopped scan takes: a live colour is final
/// there. A potentially unreachable row may hold one too, and is read as the
/// zero it was coloured at — the colour is given at zero alone, and a row
/// raised from it is live — by [`undo_the_unreachable`] and by the stop's
/// posts (`crate::cycle::worker`).
///
/// # Safety
/// As [`scan`], after a [`crate::cycle::mark::drain`] that completed, the
/// record's rows still standing.
#[cfg(feature = "gc-window")]
pub(crate) unsafe fn scan_the_recorded_edges(arena: &mut TraceScratchArena) -> ScanResult {
    use crate::cycle::recorded_edges::RUN;

    // A reading before any row is written, standing for the stride a heap
    // scan would have spent on the storage the record leaves out: the mutator
    // that recalled between the phases gets its token now, and a zero closure
    // after the stop starts a stride of its own.
    if arena.read_the_recall_as_a_stride().is_break() {
        return ScanResult::Recalled;
    }

    let entries = arena.recorded_edges().len();
    for index in 0..entries {
        if arena.inspect_position().is_break() {
            return ScanResult::Recalled;
        }

        let entry = unsafe { arena.recorded_edges().entry(index) };
        let row = (entry & !RUN) as *mut u32;
        unsafe { colour_by_the_count(row) };
        if entry & RUN == 0 {
            continue;
        }

        let color = shadow::color(unsafe { *row });
        // One past the header, which is where its first edge stands, so that
        // zero is "no run": a leaf, met and never expanded.
        unsafe { row.write(shadow::compose(color, index as u32 + 1)) };
        // Queued here, at its run, and never where an edge names it: a row
        // named before its run is queued at the run, and one never heading a
        // run is a leaf with nothing to raise — the mark's rule that leaves
        // take no entry (`crate::cycle::mark`, "Leaves are not pushed").
        if color == Color::Live && !push_a_row(arena, row) {
            return stopped(arena);
        }
    }

    while let Some(popped) = arena.pop_work() {
        let mut index = shadow::count(unsafe { *popped.row }) as usize;

        while index < entries {
            let entry = unsafe { arena.recorded_edges().entry(index) };
            if entry & RUN != 0 {
                break;
            }

            if arena.inspect_position().is_break() {
                return ScanResult::Recalled;
            }

            let target = entry as *mut u32;
            let word = unsafe { *target };
            if shadow::color(word) == Color::PotentiallyUnreachable {
                unsafe { shadow::recolor(target, Color::Live) };
                // Its run index, settled by the first pass: zero is a leaf.
                if shadow::count(word) != 0 && !push_a_row(arena, target) {
                    return stopped(arena);
                }
            }
            index += 1;
        }
    }

    ScanResult::Complete
}

/// Queue a live row whose run the second pass reads; false when both
/// allocation paths refused.
#[cfg(feature = "gc-window")]
fn push_a_row(arena: &mut TraceScratchArena, row: *mut u32) -> bool {
    arena.push_work(WorklistEntry {
        entity: std::ptr::null_mut(),
        row,
    })
}

/// What a refused push answers: a recall the growth read, or the refusal.
#[cfg(feature = "gc-window")]
fn stopped(arena: &TraceScratchArena) -> ScanResult {
    if arena.was_recalled() {
        ScanResult::Recalled
    } else {
        ScanResult::AllocationFailed
    }
}

/// Colour a met row the first time the record names it — live above zero,
/// potentially unreachable at zero. A row coloured already is left as it
/// stands.
///
/// # Safety
/// `row` is a row the record names, met by the trace and still standing.
#[cfg(feature = "gc-window")]
unsafe fn colour_by_the_count(row: *mut u32) {
    let word = unsafe { *row };
    if shadow::color(word) != Color::Unclassified {
        return;
    }

    let color = if shadow::count(word) > 0 {
        Color::Live
    } else {
        Color::PotentiallyUnreachable
    };
    unsafe { row.write(shadow::compose(color, 0)) };
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
/// As [`scan`]: `entity` is a root whose slot is still its own, live or dead,
/// or a counted child `cells::trace_cells` yielded, which is live.
unsafe fn classify_and_schedule_entity(
    arena: &mut TraceScratchArena,
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
    arena.push_work(WorklistEntry { entity, row: word })
}

/// Colour potentially unreachable the zero closure of `root` in a trace that
/// stopped short: the root, where its met row reads zero and stands
/// unclassified, and every entity reached from a row so coloured whose own met
/// row reads the same. What a stopped batch posts as its set
/// (`crate::cycle::posted_set`): the rows read zero by the stop include a live
/// state's interior, every referrer of which the mark had crossed, and the
/// closure from the proposed roots leaves it out, stopping at the first row
/// above zero — the state's entry, held from outside.
///
/// It counts positions toward the recall as the trace does and stops where a
/// reading finds it, so the mutator a recall stopped waits through at most
/// one more stride: a proposed root inside a live structure whose interior
/// rows read zero would otherwise have the closure walk that structure. What
/// it coloured by then is a sound set, any subset being one
/// (`crate::cycle::posted_set`). The worklist is the arena's, emptied of the
/// stopped mark's work first ([`TraceScratchArena::drop_the_work`]), and the
/// rows a scan cut short coloured potentially unreachable read unclassified
/// again first ([`undo_the_unreachable`]).
///
/// # Safety
/// As [`scan`], the trace's rows still standing.
pub(crate) unsafe fn colour_the_zero_closure<R: CellReader>(
    arena: &mut TraceScratchArena,
    root: *mut RcHeader,
) -> ScanResult {
    if !unsafe { colour_if_zero(arena, root) } {
        return ScanResult::AllocationFailed;
    }

    while let Some(entry) = arena.pop_work() {
        let kind = unsafe { cells::entity_kind(entry.entity) };
        let closure = ZeroClosure { arena: &mut *arena };
        if unsafe { cells::trace_cells_until::<R>(entry.entity, kind, closure) }.is_break() {
            return if arena.was_recalled() {
                ScanResult::Recalled
            } else {
                ScanResult::AllocationFailed
            };
        }
    }

    ScanResult::Complete
}

/// Colour unclassified again every row a scan cut short left potentially
/// unreachable, at count zero, which is the count every such row was coloured
/// at — written rather than kept, since a collector's scan over its record may
/// have put a run index there (`scan_the_recorded_edges`): the cut scan's
/// queue is gone, so a row it
/// coloured and had not expanded would close the zero closure early and leave
/// the rest of its component out of the set ([`colour_the_zero_closure`]).
/// The walk reads the met groups, as the reset does.
///
/// # Safety
/// As [`colour_the_zero_closure`].
pub(crate) unsafe fn undo_the_unreachable(arena: &TraceScratchArena) {
    let mut array = arena.touched_head();
    while !array.is_null() {
        let (block, population) = unsafe { ((*array).block, (*array).population) };
        let _ = unsafe {
            crate::cycle::row::for_each_proposable_met(array, block, population, |index| {
                let row = crate::cycle::row::row_at(array, block, population, index);
                row.write(shadow::compose(Color::Unclassified, 0));
                std::ops::ControlFlow::Continue(())
            })
        };
        array = unsafe { (*array).next };
    }
}

/// The visitor of [`colour_the_zero_closure`]: each counted child is offered
/// to [`colour_if_zero`], and each position counts toward the recall.
struct ZeroClosure<'a> {
    arena: &'a mut TraceScratchArena,
}

impl cells::CellVisitor for ZeroClosure<'_> {
    fn position(&mut self) -> std::ops::ControlFlow<()> {
        self.arena.inspect_position()
    }

    fn cell(&mut self, cell: cells::Cell) -> std::ops::ControlFlow<()> {
        if unsafe { colour_if_zero(self.arena, cell.child) } {
            std::ops::ControlFlow::Continue(())
        } else {
            std::ops::ControlFlow::Break(())
        }
    }
}

/// Colour `entity` potentially unreachable and queue it where its met row
/// reads zero and stands unclassified; false when both allocation paths
/// refused the worklist.
///
/// # Safety
/// As [`colour_the_zero_closure`].
unsafe fn colour_if_zero(arena: &mut TraceScratchArena, entity: *mut RcHeader) -> bool {
    let Some(word) = (unsafe { find_initialized_row_for_entity(entity) }) else {
        return true;
    };

    let row = unsafe { *word };
    if shadow::color(row) != Color::Unclassified || shadow::count(row) != 0 {
        return true;
    }

    unsafe { shadow::recolor(word, Color::PotentiallyUnreachable) };
    arena.push_work(WorklistEntry { entity, row: word })
}

/// The row this collection met for `entity`, or `None` when it has none:
/// an entity outside the GC heap, an address the retained population
/// cannot place, a mature target the mark stopped at, or a slot the mark
/// never reached.
///
/// # Safety
/// `entity` is an entity header whose slot is still its own and whose block is
/// still this collection's. A torn-down root reaches here and answers `None`,
/// its row never having been met.
#[inline]
unsafe fn find_initialized_row_for_entity(entity: *mut RcHeader) -> Option<*mut u32> {
    let EdgeTarget::Tracked(row) = (unsafe { resolve_edge_target(entity) }) else {
        return None;
    };

    unsafe { find_initialized_row(row) }
}

#[cfg(test)]
mod tests;
