//! What one ordinary collection cost, read at two boundaries and counted
//! across it (`PLAN.md` S40.3). Test builds only.
//!
//! # Two boundaries, and why one reading is too late
//!
//! The rows, the bump and every chain are gone by the time
//! `ll_gc_collect_cycles` returns: the window's close sweeps the rows, the
//! reset rewinds the bump and hands the drawn blocks back, and the chains are
//! emptied with it. So the report is assembled where the state still stands.
//! The first reading is at the scan's end, before the commit, which is the
//! last instant the rows carry what the trace left — past it the descent takes
//! every live row's count for a component index (`crate::cycle::maturation`),
//! and `crate::cycle::density` reads nothing after that either. The second is
//! at the close, before the window drops: the chains still hold the segments
//! they drew, since a chain keeps a segment it emptied, so the count at the
//! close is the high-water of the whole commit.
//!
//! Events no final state records are counted where they happen — a block
//! drawn and by which path, a tail the bump abandoned, a segment granted and to
//! which consumer, a deferred record re-offered, an exact validation run —
//! and every such site is a call whose release body is empty, the shape
//! `crate::cycle::row::note_phase_boundary` has.
//!
//! # An armed observer, one collection at a time
//!
//! Nothing here runs unless a load [`arm`]ed the observer on its thread, and
//! the seams write only while it stands armed, so the ordinary suite's
//! collections cost one thread-local read per seam and record nothing. A
//! second collection reaching the scan's end while a report stands unread is
//! a nested collection — a destructor's, or a fixture that forgot to
//! [`take`] — and the observer ends the case rather than overwriting the outer
//! report: the loads this reads are stated to nest nothing.
//!
//! # What the module owns
//!
//! The report, in a thread-local. The line union at the scan's end is a
//! `HashSet` of the harness's own, allocated through the test binary's global
//! allocator and never through the collector's arena or the crate's heap; the
//! walk itself reads rows through `shadow::row` and meets none.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;

// The arena's grants replayed over a report, for the flat form and for the
// specified chunked one.
pub(crate) mod replay;

use crate::cycle::arena::{Consumer, Funding, TraceScratchArena};
use crate::cycle::collect::Ending;
use crate::cycle::density::{self, BlockDensity, InternalEdgeCensus, TraceDensity};
use crate::cycle::maturation::DescentCounts;
use crate::cycle::row::Population;
use crate::cycle::shadow::{self, RowArray};

/// A cache line, which is what the flat form's first touch is counted in.
pub(crate) const LINE_BYTES: usize = 64;

/// Bytes the bump granted, per consumer, since the last [`take`].
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct GrantedBytes {
    pub(crate) rows: usize,
    pub(crate) worklist: usize,
    pub(crate) components: usize,
    pub(crate) drops: usize,
}

/// Events counted since the last [`take`], which a load makes one
/// collection.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct Counters {
    /// Bytes granted, by consumer.
    pub(crate) granted: GrantedBytes,
    /// Row arrays reserved, and what they asked for before the grant's
    /// rounding to eight.
    pub(crate) row_arrays: usize,
    pub(crate) row_bytes_requested: usize,
    /// Bytes the bump left in a block when it grew past it, and how many
    /// such tails.
    pub(crate) tail_bytes_abandoned: usize,
    pub(crate) tails_abandoned: usize,
    /// Blocks the bump grew into, by path, and blocks the reset returned.
    pub(crate) drawn_from_pool: usize,
    pub(crate) drawn_from_reserve: usize,
    pub(crate) returned: usize,
    /// Deferred records the re-offer moved back to the active lane.
    pub(crate) reoffered: usize,
    /// Exact validations run, and the members their two walks read.
    pub(crate) validations: usize,
    pub(crate) members_validated: usize,
}

/// What the rows say at the scan's end, before the commit.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ScanReading {
    /// Root records the trace attempted. A record names an entity once, since
    /// the candidate bit admits one registration, so this is the unique roots
    /// of the batch as well.
    pub(crate) roots: usize,
    /// Rows met, saturated rows, touched blocks, `G` and `T`, per population.
    pub(crate) density: TraceDensity,
    /// The same per touched block, in the touched list's order, newest first.
    pub(crate) blocks: Vec<BlockDensity>,
    /// Internal in-edges the mark subtracted, with saturated rows apart.
    pub(crate) internal_edges: InternalEdgeCensus,
    /// Row resolutions the mark made, and the mark and the scan together.
    /// `E`, the edges the mark followed, is the first less `roots`.
    pub(crate) mark_dispatches: usize,
    pub(crate) trace_dispatches: usize,
    /// Distinct [`LINE_BYTES`] lines the touched list covers: each array's
    /// header, its bitmap and its initialised groups, and a large entity's row
    /// word in its own block header, as one address union.
    pub(crate) lines: usize,
    /// Blocks the bump held past the workspace at this instant.
    pub(crate) blocks_held: usize,
}

/// What the chains and the bump say at the close, before the window drops.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct CloseReading {
    /// Where the collection ended.
    pub(crate) ending: Ending,
    /// Segments each chain holds, emptied ones included: the deepest each
    /// stood in this collection.
    pub(crate) worklist_segments: usize,
    pub(crate) component_segments: usize,
    pub(crate) drop_segments: usize,
    /// Blocks the bump held past the workspace, and how many of them came
    /// through the reserve.
    pub(crate) blocks_held: usize,
    pub(crate) from_reserve: usize,
    /// Bytes the bump could still grant out of the block under its cursor.
    pub(crate) remainder: usize,
    /// The maturation descent's own counts.
    pub(crate) descent: DescentCounts,
}

/// One collection's report. A boundary the collection never reached is
/// `None`, which a load reads as absent rather than as zero.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct CollectionReport {
    pub(crate) scan: Option<ScanReading>,
    pub(crate) close: Option<CloseReading>,
    pub(crate) counters: Counters,
}

thread_local! {
    /// Whether a load is reading this thread's collections. `const`, and no
    /// drop glue, for the reason `crate::cycle::epoch`'s pin has none.
    static ARMED: Cell<bool> = const { Cell::new(false) };
    /// The report under assembly. `RefCell` rather than `Cell`, so that a
    /// seam can update one field in place; no drop glue in the `const`
    /// initialiser, the `Vec` being empty until a seam fills it and cleared
    /// at every take.
    static REPORT: RefCell<CollectionReport> = const { RefCell::new(CollectionReport {
        scan: None,
        close: None,
        counters: Counters {
            granted: GrantedBytes { rows: 0, worklist: 0, components: 0, drops: 0 },
            row_arrays: 0,
            row_bytes_requested: 0,
            tail_bytes_abandoned: 0,
            tails_abandoned: 0,
            drawn_from_pool: 0,
            drawn_from_reserve: 0,
            returned: 0,
            reoffered: 0,
            validations: 0,
            members_validated: 0,
        },
    }) };
}

/// Start reading this thread's collections, clearing whatever stood, until
/// the guard is dropped.
pub(crate) fn arm() -> Armed {
    ARMED.with(|armed| armed.set(true));
    REPORT.with(|report| *report.borrow_mut() = CollectionReport::default());
    Armed
}

/// The guard [`arm`] opened; dropping it stops the seams and clears the
/// report.
pub(crate) struct Armed;

impl Drop for Armed {
    fn drop(&mut self) {
        ARMED.with(|armed| armed.set(false));
        REPORT.with(|report| *report.borrow_mut() = CollectionReport::default());
    }
}

/// The report of the collection since the last take, which this leaves
/// empty. Reading and clearing together, because every load prices the
/// collection it drove and what stands before it is another's.
pub(crate) fn take() -> CollectionReport {
    REPORT.with(|report| std::mem::take(&mut *report.borrow_mut()))
}

/// Whether a load is reading this thread's collections, which is what a
/// site whose count costs a walk asks before walking.
pub(crate) fn armed() -> bool {
    ARMED.with(Cell::get)
}

fn with_counters(update: impl FnOnce(&mut Counters)) {
    if armed() {
        REPORT.with(|report| update(&mut report.borrow_mut().counters));
    }
}

/// The scan has ended and the commit has not begun: read the rows.
///
/// `roots` is what `trace_batch` answered as records attempted. The reading
/// walks the touched list through `density`'s readers and a walk of its own
/// for the lines, and meets no row.
///
/// # Panics
/// When a scan reading already stands: the collection is nested inside one
/// whose report was not taken (module doc).
///
/// # Safety
/// As `density::totals`: the trace completed, the window is open, and the
/// call is on the owning thread.
pub(crate) unsafe fn note_scan_end(arena: &TraceScratchArena, roots: usize) {
    if !armed() {
        return;
    }

    let reading = ScanReading {
        roots,
        density: unsafe { density::totals(arena) },
        blocks: unsafe { density::per_block(arena) },
        internal_edges: unsafe { density::internal_edges(arena) },
        mark_dispatches: crate::cycle::row::take_dispatches_in_mark_phase(),
        trace_dispatches: crate::cycle::row::edge_dispatches_so_far(),
        lines: unsafe { distinct_lines(arena) },
        blocks_held: arena.blocks_held(),
    };
    REPORT.with(|report| {
        let mut report = report.borrow_mut();
        assert!(
            report.scan.is_none(),
            "a collection ended its scan inside one whose report stands unread"
        );
        report.scan = Some(reading);
    });
}

/// The collection is about to close its window with `ending`: read the
/// chains and the bump.
pub(crate) fn note_close(arena: &TraceScratchArena, ending: Ending) {
    if !armed() {
        return;
    }

    let reading = CloseReading {
        ending,
        worklist_segments: arena.worklist_segment_count(),
        component_segments: arena.component_segment_count(),
        drop_segments: arena.drop_segment_count(),
        blocks_held: arena.blocks_held(),
        from_reserve: arena.blocks_from_reserve(),
        remainder: arena.room_left(),
        descent: crate::cycle::maturation::take_descent_counts(),
    };
    REPORT.with(|report| report.borrow_mut().close = Some(reading));
}

/// The bump granted `bytes` to `consumer`.
pub(crate) fn note_grant(consumer: Consumer, bytes: usize) {
    with_counters(|counters| {
        let field = match consumer {
            Consumer::Rows => &mut counters.granted.rows,
            Consumer::Worklist => &mut counters.granted.worklist,
            Consumer::Components => &mut counters.granted.components,
            Consumer::Drops => &mut counters.granted.drops,
        };
        *field += bytes;
    });
}

/// A row array was reserved for `requested` bytes; the grant is the
/// rounding's and is counted through [`note_grant`].
pub(crate) fn note_row_array(requested: usize) {
    with_counters(|counters| {
        counters.row_arrays += 1;
        counters.row_bytes_requested += requested;
    });
}

/// The bump grew past a block and left `bytes` of it ungranted.
pub(crate) fn note_tail_abandoned(bytes: usize) {
    with_counters(|counters| {
        counters.tails_abandoned += 1;
        counters.tail_bytes_abandoned += bytes;
    });
}

/// The bump grew into a block that came through `funding`.
pub(crate) fn note_block_drawn(funding: Funding) {
    with_counters(|counters| match funding {
        Funding::Pool => counters.drawn_from_pool += 1,
        Funding::Reserve => counters.drawn_from_reserve += 1,
    });
}

/// The reset handed one drawn block back.
pub(crate) fn note_block_returned() {
    with_counters(|counters| counters.returned += 1);
}

/// A re-offer is about to move `records` from the deferred lane to the
/// active one; the load reads the lane and says so, since the re-offer
/// itself counts nothing.
pub(crate) fn note_reoffered(records: usize) {
    with_counters(|counters| counters.reoffered += records);
}

/// One exact validation ran over `members`.
pub(crate) fn note_validation(members: usize) {
    with_counters(|counters| {
        counters.validations += 1;
        counters.members_validated += members;
    });
}

/// Distinct lines the touched list covers, as one address union.
///
/// An array contributes its header, its bitmap and every group whose bit
/// stands, each as the whole of its bytes: a met group is zeroed whole at its
/// first touch, so its eight rows are written whatever the trace met among
/// them. A large entity contributes its header-only array and the row word
/// in its own block header.
///
/// # Safety
/// As [`note_scan_end`].
unsafe fn distinct_lines(arena: &TraceScratchArena) -> usize {
    let mut lines = HashSet::new();
    let mut cover = |start: usize, bytes: usize| {
        let first = start / LINE_BYTES;
        let last = (start + bytes - 1) / LINE_BYTES;
        for line in first..=last {
            lines.insert(line);
        }
    };

    let mut array = arena.touched_head();
    while !array.is_null() {
        let header = array as usize;
        cover(header, size_of::<RowArray>());
        let population = unsafe { (*array).population };
        if population == Population::SingleEntity {
            let block = unsafe { (*array).block };
            let word = unsafe { crate::memory::large_entity::shadow_row(block) };
            cover(word as usize, size_of::<u32>());
        } else {
            let row_count = unsafe { (*array).row_count };
            let groups = shadow::group_count(row_count);
            let rows_start = unsafe { shadow::row(array, 0) } as usize;
            let group_bytes = shadow::GROUP as usize * size_of::<u32>();
            for group in 0..groups {
                if unsafe { shadow::group_is_initialized(array, group * shadow::GROUP) } {
                    cover(rows_start + group as usize * group_bytes, group_bytes);
                }
            }

            let bitmap_start = rows_start + groups as usize * group_bytes;
            cover(
                bitmap_start,
                shadow::bytes_for(row_count) - (bitmap_start - header),
            );
        }

        array = unsafe { (*array).next };
    }

    lines.len()
}

#[cfg(test)]
mod tests;
