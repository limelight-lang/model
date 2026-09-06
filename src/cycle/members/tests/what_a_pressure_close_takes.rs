//! What the close of an armed window takes out of the rows, and what the close
//! of an ordinary one costs while nothing is armed.

use super::*;

use crate::cycle::shadow;
use crate::memory::gc_metadata::thread_stats;
use crate::test_support::allocation_probe;

/// What this collection left stamped on the block holding `entity`, zero once
/// the sweep has been over it.
///
/// Two words rather than one, because the two populations of the fixture keep
/// their rows in different places: an entity block names its array from the
/// collector line, and a large entity's block carries the row itself in its
/// own header. Reading the first word for both would answer zero for the
/// large entity whatever the sweep did.
unsafe fn stamp_of(entity: *mut Object) -> usize {
    let block = (entity as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8;
    let header = crate::memory::block_pool::BlockHeader::of_ptr(block as *const u8);
    let kind = unsafe { crate::memory::block_pool::load_block_kind(&raw const (*header).kind) };
    if crate::memory::large_entity::is_large_entity(kind) {
        let row = unsafe { *crate::memory::large_entity::shadow_row(block) };
        row as usize
    } else {
        let array = unsafe { crate::memory::heap::block_shadow(block) };
        array as usize
    }
}

/// The harvest takes the entities the scan left unreachable and nothing else, and
/// takes them out of memory the manager was never asked for: the region is the
/// thread's workspace, so an armed close moves neither counter.
#[test]
fn an_armed_close_takes_the_entities_the_scan_left_unreachable() {
    let _g = test_guard();
    let rings = two_rings();
    let (alpha, beta) = (rings.alpha, rings.beta);

    let before = thread_stats();
    let rows_before = shadow::rows_read();
    let _ = allocation_probe::take_allocations();
    assert!(
        trace_and_close(Some(MEMBER_CAPACITY)),
        "the region was free"
    );
    let drawn = allocation_probe::take_allocations();
    let after = thread_stats();
    let rows = shadow::rows_read() - rows_before;

    let standing = take_standing().expect("the close harvested");
    assert!(!standing.overflowed());
    // The order the module documents, unsorted: block by block of the touched
    // list, which is newest first, and by ascending row inside each block. The
    // trace meets `alpha` first, so `beta`'s block is the newer array and its
    // one row comes out first.
    assert_eq!(
        standing.entities(),
        [beta as *mut RcHeader, alpha as *mut RcHeader],
        "both rings, in the order the walk met them, and nothing besides"
    );

    // What the harvest read, derived rather than observed: the eight rows of
    // the one group the trace met in `alpha`'s block — the bitmap is what
    // keeps the other 4,072 unread — and the single header word `beta`'s block
    // carries, which is the row of a block with no array.
    assert_eq!(rows, 8 + 1, "one met group and one large entity's own word");
    assert_eq!(drawn, (0, 0), "the region is the thread's own workspace");
    assert_eq!(
        (after.current_blocks(), after.current_bytes_in_use()),
        (before.current_blocks(), before.current_bytes_in_use()),
        "and it is charged to no ledger, being memory the thread holds anyway"
    );

    drop(standing);
    tear_down(rings);
}

/// The ordinary path pays nothing for the pressure path's list: with no
/// harvest armed the close reads no row at all, which is the whole of what a
/// collection off the poll owes here.
#[test]
fn an_ordinary_close_reads_no_row() {
    let _g = test_guard();
    let rings = two_rings();

    let before = shadow::rows_read();
    assert!(!trace_and_close(None), "this close asked for no harvest");

    assert_eq!(
        shadow::rows_read() - before,
        0,
        "an unarmed sweep nulls pointers and reads nothing"
    );
    assert!(take_standing().is_none(), "and leaves no list behind");

    tear_down(rings);
}

/// The sweep's own duty is unconditional: a harvest that overflowed still
/// nulls every block's shadow pointer, because a row left standing is a slot
/// the next collection can hand out under this one's rows.
///
/// **The capacity is zero so that the refusal lands on the first array of the
/// two**, which is what puts a block past it: at a capacity of one the walk
/// refuses on its last array and the claim would be made over the block that
/// refused.
#[test]
fn an_overflowed_close_still_nulls_every_pointer() {
    let _g = test_guard();
    let rings = two_rings();
    let (alpha, beta) = (rings.alpha, rings.beta);

    assert!(trace_and_close(Some(0)), "the region was free");

    let standing = take_standing().expect("the close harvested");
    assert!(standing.overflowed(), "two rings against one record");
    assert!(standing.entities().is_empty());
    for entity in [alpha, beta] {
        assert_eq!(
            unsafe { stamp_of(entity) },
            0,
            "the sweep finished its own work past the refusal"
        );
    }

    drop(standing);
    tear_down(rings);
}

/// The high-water figure the close enters is the trace's own residue, and the
/// harvest adds nothing to it: the region is outside the bump, so the same
/// trace with a harvest armed and without one enters the same bytes.
///
/// Two arms over the same fixture rather than one arm against a constant: the
/// residue is the rows and the worklist of these two rings, and a figure
/// written down here would pin the fixture rather than the harvest.
#[test]
fn the_harvest_enters_what_an_unarmed_close_enters() {
    let _g = test_guard();

    let rings = two_rings();
    crate::memory::gc_metadata::lower_thread_peak_to_current();
    trace_and_close(None);
    let unarmed = thread_stats();
    tear_down(rings);

    let rings = two_rings();
    crate::memory::gc_metadata::lower_thread_peak_to_current();
    trace_and_close(Some(MEMBER_CAPACITY));
    let armed = thread_stats();
    let standing = take_standing().expect("the close harvested");
    assert_eq!(standing.entities().len(), 2, "the arm under test harvested");
    drop(standing);
    tear_down(rings);

    assert_eq!(armed, unarmed, "the same trace, the same ledger");
}

/// The rule the region exists to keep: a collection a destructor of the
/// teardown starts appends nothing to the list that teardown is reading.
///
/// The list stands from the arming until the driver releases it, which is
/// across the whole teardown, so the sweep cannot gate on "a list is armed on
/// this thread" — it gates on the flag its own arena carries. A nested close
/// that appended here would hand the outer driver entities its own path has
/// already torn down.
#[test]
fn a_nested_close_appends_nothing_to_a_standing_list() {
    let _g = test_guard();
    let outer = two_rings();

    trace_and_close(Some(MEMBER_CAPACITY));
    let standing = take_standing().expect("the close harvested");
    let harvested: Vec<*mut RcHeader> = standing.entities().to_vec();
    assert_eq!(harvested.len(), 2, "both rings");

    // The nested collection asks for a harvest of its own and is refused,
    // which is the sequence a destructor of the teardown drives.
    let inner = two_rings();
    let before = shadow::rows_read();
    assert!(
        !trace_and_close(Some(MEMBER_CAPACITY)),
        "the region is the outer driver's"
    );

    assert_eq!(
        standing.entities(),
        harvested.as_slice(),
        "the outer driver's list is what it was"
    );
    assert!(!standing.overflowed());
    assert_eq!(
        shadow::rows_read() - before,
        0,
        "and the nested close read no row at all"
    );

    drop(standing);
    tear_down(inner);
    tear_down(outer);
}

/// A sweep that unwinds out of its harvest leaves neither half of its work
/// half-done: the second sweep of the same close unstamps every block the
/// first did not reach, and the records the first wrote are given up rather
/// than handed to a driver as a whole set.
///
/// The unwind is injected, because the state has no other way in: what raises
/// it in production is a retained block whose survivor list no longer holds a
/// row's position, and a debug build ends on `entity_at`'s assertion before
/// the arm that answers `None` is reached
/// (`crate::cycle::arena::InjectedHarvestFailure`).
#[test]
fn a_harvest_that_unwinds_gives_up_its_records_and_still_sweeps() {
    let _g = test_guard();
    let rings = two_rings();
    let (alpha, beta) = (rings.alpha, rings.beta);

    let mut active = ActiveTrace::open().expect("the guard drew this thread's workspace");
    active.detach_candidates();
    let outcome = {
        let (arena, batch) = active.rows_and_roots();
        unsafe { trace_batch(arena, batch) }
    };
    assert_eq!(outcome, TraceOutcome::Complete);
    assert!(active.arm_harvest(MEMBER_CAPACITY), "the region was free");

    let armed = crate::cycle::arena::InjectedHarvestFailure::arm();
    let raised = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(active);
    }));
    assert!(raised.is_err(), "the harvest was expected to raise");
    drop(armed);

    for entity in [alpha, beta] {
        assert_eq!(
            unsafe { stamp_of(entity) },
            0,
            "the second sweep of the close unstamped what the first left"
        );
    }

    let standing = take_standing().expect("the harvest was armed");
    assert!(
        standing.overflowed(),
        "a walk that did not finish is a reading short of the whole set"
    );
    assert!(standing.entities().is_empty());

    drop(standing);
    tear_down(rings);
}
