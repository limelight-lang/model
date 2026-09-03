//! The probe read against a path whose answer is fixed outside it.
//!
//! A counter is only evidence once it has been seen agreeing with a
//! known number. Read for the first time on the path it was written to
//! instrument, a miscounting probe and a misbehaving path are the same
//! observation.

use super::*;

/// One `Box` is one allocation while it lives and one free when it
/// drops, which is the standard library's contract rather than this
/// crate's, so a disagreement here is the probe's.
#[test]
fn a_dropped_box_is_one_allocation_and_one_free() {
    let _ = take_allocations();
    let _ = take_heap_deallocations();

    let boxed = std::hint::black_box(Box::new(0u64));
    let (allocated, _pool) = take_allocations();
    let while_alive = take_heap_deallocations();

    drop(boxed);
    let after_drop = take_heap_deallocations();
    let taken_twice = take_heap_deallocations();

    assert_eq!(allocated, 1, "the box came from the global allocator");
    assert_eq!(while_alive, 0, "and nothing was given back while it lived");
    assert_eq!(after_drop, 1, "the drop reached `dealloc`");
    assert_eq!(taken_twice, 0, "and the count is taken, not read");
}

/// A reallocation is one allocation and no free, which is the rule
/// [`CountingAlloc`] states and the `Box` above cannot reach. Whether
/// `System` extends the block in place or moves it is not the subject:
/// either way the caller comes out holding one live block, and the probe
/// charges no free.
#[test]
fn a_growth_is_one_allocation_and_no_free() {
    let mut grown: Vec<u64> = Vec::with_capacity(1);
    grown.push(0);

    let _ = take_allocations();
    let _ = take_heap_deallocations();
    grown.push(1);
    let (allocated, _pool) = take_allocations();
    let freed = take_heap_deallocations();

    assert_eq!(allocated, 1, "the growth reallocated once");
    assert_eq!(freed, 0, "and the release inside it is not counted as one");
    assert_eq!(grown.len(), 2, "the vector really grew");
}
