use super::*;
use crate::cycle::arena::RowLookup;
use crate::cycle::arena::WORKSPACE_BUMP_BYTES;
use crate::cycle::epoch;
use crate::cycle::loads::{self, Load, Population as LoadPopulation};
use crate::cycle::queue::{deferred_count, release_queue_segments, reoffer_deferred_candidates};
use crate::cycle::row::RowKey;
use crate::cycle::testing::{on_a_fresh_thread, open_arena};
use crate::gc::ll_gc_collect_cycles;
use crate::memory::block_pool::{BLOCK_MASK, BLOCK_PAYLOAD, test_guard};
use crate::memory::gc_metadata::{self, GcMemoryStats};
use crate::memory::heap::SIZE_CLASSES;

/// One collection's report with what the test reads around it: the entities
/// freed, and this thread's GC blocks and bytes at current and at peak over
/// the collection, the peak lowered to current before it.
struct Collected {
    freed: usize,
    report: CollectionReport,
    memory: GcMemoryStats,
}

/// The report of one collection over a built load, with the queue's spare
/// cells refilled as the poll refills them, the deferred lane re-offered and
/// the dispatch count zeroed, which is what every reading here prices one
/// collection from.
///
/// The refill is what lets a root read live stand in the deferred lane at
/// the close: with no spare cell the lane cannot take it and it rejoins the
/// active lane instead, and the re-offer then moves nothing. The poll
/// refills before it fires, so a collection here is offered the same lanes
/// one off the poll is.
///
/// The re-offer counts nothing itself, so the lane is read before it and
/// the count handed to the census from here.
///
/// The keeper holds the ring, so a collection frees nothing; a case that let
/// the ring go reads `freed` itself.
fn collect_once() -> Collected {
    crate::cycle::queue::refill_and_drain();
    note_reoffered(deferred_count());
    reoffer_deferred_candidates();
    let _ = crate::cycle::row::take_edge_dispatches();
    gc_metadata::lower_thread_peak_to_current();
    let freed = unsafe { ll_gc_collect_cycles() };
    Collected {
        freed,
        report: take(),
        memory: gc_metadata::thread_stats(),
    }
}

/// The identity every arena lifetime keeps: what the bump had to grant is
/// what it granted, abandoned, or still holds.
fn the_bump_balances(report: &CollectionReport) {
    let close = report.close.expect("the collection reached its close");
    let counters = report.counters;
    let capacity = WORKSPACE_BUMP_BYTES
        + BLOCK_PAYLOAD * (counters.drawn_from_pool + counters.drawn_from_reserve);
    let granted = counters.granted.rows
        + counters.granted.worklist
        + counters.granted.components
        + counters.granted.drops;
    assert_eq!(
        capacity,
        granted + counters.tail_bytes_abandoned + close.remainder,
        "the base bump plus the payloads drawn equals the bytes granted plus the tails plus the remainder"
    );
}

/// The lines a touched list covers, computed here from the arrays' own
/// addresses rather than by the instrument: the header, the bitmap and each
/// initialised group, as one union.
unsafe fn lines_by_hand(arena: &TraceScratchArena) -> usize {
    let mut lines = HashSet::new();
    let mut cover = |start: usize, bytes: usize| {
        if bytes == 0 {
            return;
        }

        for line in start / LINE_BYTES..=(start + bytes - 1) / LINE_BYTES {
            lines.insert(line);
        }
    };

    // The layout as literals rather than through `shadow`'s constants, so the
    // two computations share only the arrays' addresses: a header of 24, a
    // group of 32, one bitmap bit per group.
    let mut array = arena.touched_head();
    while !array.is_null() {
        let base = array as usize;
        let row_count = unsafe { (*array).row_count };
        let groups = (row_count as usize).div_ceil(8);
        cover(base, 24);
        for group in 0..groups {
            if unsafe { shadow::group_is_initialized(array, group as u32 * 8) } {
                cover(base + 24 + group * 32, 32);
            }
        }

        cover(base + 24 + groups * 32, groups.div_ceil(8));
        array = unsafe { (*array).next };
    }

    lines.len()
}

/// An entity block's row key at `index`, for a case that meets rows by hand.
fn slotted_row(block: *mut u8, index: u32) -> RowKey {
    RowKey {
        block: block as usize,
        index,
        population: Population::Slotted,
    }
}

/// The line union of one array is the header, the met groups and the
/// bitmap, and no unmet group: one row met reads two or three lines, and
/// eight rows spread over eight groups read the same header and bitmap plus
/// eight groups.
///
/// The count is computed twice, by the instrument and by hand off the
/// array's address, because the union depends on where the arena placed the
/// array and no constant states it.
#[test]
fn the_line_union_covers_the_header_the_met_groups_and_the_bitmap() {
    let _g = test_guard();
    // The heap comes back with the block: the block goes home when the heap
    // is dropped, and the arena must reset before that.
    let mut heap = crate::memory::Heap::new_entity();
    let slot = heap.alloc(SIZE_CLASSES[0]);
    let block = ((slot as usize) & !BLOCK_MASK) as *mut u8;
    let mut arena = open_arena();

    let answer = unsafe { arena.ensure_row(slotted_row(block, 0), 1) };
    assert!(matches!(answer, RowLookup::Ready { .. }));
    let one_group = unsafe { distinct_lines(&arena) };
    assert_eq!(one_group, unsafe { lines_by_hand(&arena) });
    // 24 bytes of header and 32 of group are contiguous: one line, or two
    // when a line boundary falls inside them; the bitmap is one more, far
    // behind the rows.
    assert!(
        (2..=3).contains(&one_group),
        "header and one group span one or two lines, the bitmap one: {one_group}"
    );

    for group in 1..8 {
        let answer = unsafe { arena.ensure_row(slotted_row(block, group * shadow::GROUP), 1) };
        assert!(matches!(answer, RowLookup::Ready { .. }));
    }

    let eight_groups = unsafe { distinct_lines(&arena) };
    assert_eq!(eight_groups, unsafe { lines_by_hand(&arena) });
    // Eight groups of 32 bytes are 256 contiguous bytes: four lines, or five
    // across a boundary, in place of the one group's one or two.
    assert!(
        eight_groups > one_group,
        "eight met groups cover more lines than one: {eight_groups} against {one_group}"
    );
    assert!(
        eight_groups - one_group <= 4,
        "and at most four lines more, 256 bytes being four lines"
    );

    arena.reset();
    drop(heap);
}

/// A collection nobody armed writes nothing, and the report of an armed one
/// is taken once: the second take reads an empty report.
#[test]
fn a_report_stands_only_while_armed_and_is_taken_once() {
    let _g = test_guard();
    on_a_fresh_thread(|| {
        release_queue_segments();
        let _epoch = epoch::pin(0);
        let mut built = unsafe {
            loads::build(Load {
                class_bytes: 64,
                members: 2,
                fillers: 0,
                population: LoadPopulation::Ordinary,
                second_edge: false,
            })
        };

        assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
        assert_eq!(
            take(),
            CollectionReport::default(),
            "nothing armed, nothing written"
        );

        let armed = arm();
        let report = collect_once().report;
        assert!(report.scan.is_some() && report.close.is_some());
        assert_eq!(take(), CollectionReport::default(), "the take cleared it");
        drop(armed);

        assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
        assert_eq!(
            take(),
            CollectionReport::default(),
            "disarmed, nothing written"
        );

        unsafe { loads::release_ring(&mut built) };
        let _armed = arm();
        assert_eq!(
            collect_once().freed,
            2,
            "the ring went with the keeper's edge"
        );
    });
}

/// A registered ring of `n` under a keeper reads as its construction: `n`
/// roots, `n` rows met in one block, `n` internal edges, `2n` mark
/// dispatches and `4n` for the trace, one worklist segment and one component
/// segment out of the bump, nothing drawn, nothing validated, and the bump
/// balancing to the byte.
///
/// At class 256, where the array's 1,052 bytes are granted as 1,056: the
/// request and the grant are two figures, and a class whose array is a
/// multiple of eight would read them as one.
#[test]
fn a_registered_ring_under_a_keeper_reads_as_constructed() {
    let _g = test_guard();
    on_a_fresh_thread(|| {
        release_queue_segments();
        let _epoch = epoch::pin(0);
        let members = 16;
        let mut built = unsafe {
            loads::build(Load {
                class_bytes: 256,
                members,
                fillers: 0,
                population: LoadPopulation::Ordinary,
                second_edge: false,
            })
        };
        let _armed = arm();

        for collection in 1..=3 {
            let Collected {
                freed,
                report,
                memory,
            } = collect_once();
            assert_eq!(freed, 0);
            let scan = report.scan.clone().expect("the scan ended");
            let close = report.close.expect("the close was reached");
            let context = format!("collection {collection}");
            assert_eq!(
                close.ending,
                crate::cycle::collect::Ending::NothingProposed,
                "{context}: a live ring proposes nothing"
            );
            assert_eq!(
                scan.blocks.len(),
                1,
                "{context}: one block, read on its own"
            );
            assert_eq!(
                (scan.blocks[0].groups, scan.blocks[0].groups_met),
                (32, 2),
                "{context}: the block's own G and T"
            );
            assert_eq!(
                memory.peak_blocks(),
                memory.current_blocks(),
                "{context}: no block drawn past what stands"
            );
            assert_eq!(
                memory.peak_bytes_in_use(),
                1056 + 2 * crate::cycle::stack::SEGMENT_BYTES + 64,
                "{context}: the rows, two segments and the control line at the peak"
            );
            assert_eq!(scan.roots, members, "{context}: every member is a root");
            assert_eq!(scan.density.slotted.blocks, 1, "{context}: one block");
            assert_eq!(
                scan.density.slotted.rows_met as usize, members,
                "{context}: rows met"
            );
            assert_eq!(
                scan.density.slotted.groups_met, 2,
                "{context}: sixteen rows, two groups"
            );
            assert_eq!(
                scan.density.retained.blocks + scan.density.single_entity.blocks,
                0
            );
            assert_eq!(
                scan.internal_edges.recoverable_internal_edges as usize, members,
                "{context}: one internal edge per member"
            );
            assert_eq!(
                scan.mark_dispatches,
                2 * members,
                "{context}: a root and an edge per member"
            );
            assert_eq!(
                scan.trace_dispatches,
                4 * members,
                "{context}: the scan repeats the mark's"
            );
            assert_eq!(
                scan.blocks_held, 0,
                "{context}: nothing drawn past the workspace"
            );
            assert!(scan.lines >= 2, "{context}: a header and a group at least");

            assert_eq!(
                close.worklist_segments, 1,
                "{context}: one segment holds sixteen entries"
            );
            assert_eq!(
                close.component_segments, 1,
                "{context}: and one holds the component"
            );
            assert_eq!(
                close.drop_segments, 0,
                "{context}: a live ring tears nothing down"
            );
            assert_eq!((close.blocks_held, close.from_reserve), (0, 0));
            assert_eq!(
                close.descent.vertices, members,
                "{context}: the descent opened every member"
            );
            assert_eq!(
                close.descent.components, 1,
                "{context}: and closed one component"
            );

            let counters = report.counters;
            assert_eq!(counters.row_arrays, 1);
            assert_eq!(
                (counters.row_bytes_requested, counters.granted.rows),
                (1052, 1056),
                "{context}: 255 rows ask for 1,052 bytes and the bump grants eight-aligned"
            );
            assert_eq!(
                counters.granted.worklist,
                crate::cycle::stack::SEGMENT_BYTES
            );
            assert_eq!(
                counters.granted.components,
                crate::cycle::stack::SEGMENT_BYTES
            );
            assert_eq!(counters.granted.drops, 0);
            assert_eq!(
                (
                    counters.drawn_from_pool,
                    counters.drawn_from_reserve,
                    counters.returned
                ),
                (0, 0, 0)
            );
            assert_eq!(
                (counters.tails_abandoned, counters.tail_bytes_abandoned),
                (0, 0)
            );
            assert_eq!(
                counters.reoffered,
                if collection == 1 { 0 } else { members },
                "{context}: the roots read live were deferred and re-offered"
            );
            assert_eq!(
                (counters.validations, counters.members_validated),
                (0, 0),
                "{context}: nothing proposed"
            );
            the_bump_balances(&report);
        }

        unsafe { loads::release_ring(&mut built) };
        let Collected { freed, report, .. } = collect_once();
        assert_eq!(freed, members, "the ring is garbage without the keeper");
        assert_eq!(
            report.close.expect("the close was reached").ending,
            crate::cycle::collect::Ending::TornDown
        );
        let counters = report.counters;
        assert_eq!(
            (counters.validations, counters.members_validated),
            (1, members),
            "one exact validation over the whole ring, and no second: no destructor ran"
        );
        assert_eq!(
            report.close.expect("the close was reached").drop_segments,
            0,
            "the sever displaces no child out of a ring that holds only its members"
        );
        the_bump_balances(&report);
    });
}

/// A ring placed one member per block at the narrowest class fills the bump
/// with row arrays and grows past it: the blocks drawn come back at the
/// close, each growth abandons the tail it left, and the identity holds with
/// them in it.
#[test]
fn a_sparse_ring_past_the_bump_draws_blocks_and_returns_them() {
    let _g = test_guard();
    on_a_fresh_thread(|| {
        release_queue_segments();
        let _epoch = epoch::pin(0);
        // Eight arrays of 8,440 bytes at class 32 are 67,520 bytes: past the
        // 56,960-byte bump by one block's worth.
        let members = 8;
        let class_bytes = 32;
        let mut built = unsafe {
            loads::build(Load {
                class_bytes,
                members,
                fillers: loads::slots_per_block(class_bytes) - 1,
                population: LoadPopulation::Ordinary,
                second_edge: false,
            })
        };
        let _armed = arm();

        let Collected { freed, report, .. } = collect_once();
        assert_eq!(freed, 0);
        let scan = report.scan.clone().expect("the scan ended");
        let close = report.close.expect("the close was reached");
        assert_eq!(
            scan.density.slotted.blocks as usize, members,
            "one block per member"
        );
        assert_eq!(scan.blocks.len(), members, "and one reading per block");
        assert_eq!(scan.density.slotted.rows_met as usize, members);
        assert_eq!(
            scan.density.slotted.groups_met as usize, members,
            "one group per block"
        );
        assert_eq!(
            scan.blocks_held, 1,
            "the rows alone grew past the bump once"
        );
        let counters = report.counters;
        assert_eq!(counters.row_arrays, members);
        assert_eq!(counters.drawn_from_pool, 1);
        assert_eq!(counters.drawn_from_reserve, 0);
        assert_eq!(counters.returned, 1, "and it went back at the close");
        assert_eq!(counters.tails_abandoned, 1);
        assert!(
            counters.tail_bytes_abandoned < shadow::bytes_for(2040).next_multiple_of(8),
            "the tail is what one array did not fit in"
        );
        assert_eq!((close.blocks_held, close.from_reserve), (1, 0));
        the_bump_balances(&report);

        unsafe { loads::release_ring(&mut built) };
        assert_eq!(collect_once().freed, members);
    });
}

// The loads S40.3 records, ignored in the ordinary suite: the widest builds a
// ring of 2,040 and the sparse arm a block per member.
mod the_loads;
