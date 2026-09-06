//! What the close of an armed window takes out of the rows, and what the close
//! of an ordinary one costs while nothing is armed.

use super::*;

use crate::cycle::shadow;
use crate::memory::gc_metadata::thread_stats;
use crate::test_support::allocation_probe;

/// The shadow-row pointer of the block holding `entity`, which the sweep nulls
/// whether or not it harvested.
unsafe fn block_shadow_of(entity: *mut Object) -> *mut u8 {
    let block = entity as usize & !crate::memory::block_pool::BLOCK_MASK;
    unsafe { crate::memory::heap::block_shadow(block as *mut u8) }
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
    let _ = allocation_probe::take_allocations();
    trace_and_close(Some(MEMBER_CAPACITY));
    let drawn = allocation_probe::take_allocations();
    let after = thread_stats();

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
    trace_and_close(None);

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
#[test]
fn an_overflowed_close_still_nulls_every_pointer() {
    let _g = test_guard();
    let rings = two_rings();
    let (alpha, beta) = (rings.alpha, rings.beta);

    trace_and_close(Some(1));

    let standing = take_standing().expect("the close harvested");
    assert!(standing.overflowed(), "two rings against one record");
    assert!(standing.entities().is_empty());
    for entity in [alpha, beta] {
        assert!(
            unsafe { block_shadow_of(entity) }.is_null(),
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
    let (alpha, beta) = (outer.alpha, outer.beta);

    trace_and_close(Some(MEMBER_CAPACITY));
    let standing = take_standing().expect("the close harvested");
    assert_eq!(
        standing.entities(),
        [beta as *mut RcHeader, alpha as *mut RcHeader]
    );

    // The nested collection: it arms nothing, because the region is in use.
    let inner = two_rings();
    let before = shadow::rows_read();
    trace_and_close(None);

    assert_eq!(
        standing.entities(),
        [beta as *mut RcHeader, alpha as *mut RcHeader],
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
