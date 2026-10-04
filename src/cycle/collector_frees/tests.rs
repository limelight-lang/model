//! The owner's application of what the collector freed, sliced and whole;
//! the cases that serve a batch stand in
//! `crate::cycle::worker::tests::the_delta_test`.

use super::*;
use crate::class::ClassBuilder;
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::memory::context::LLContext;
use crate::object::new_constructed;
use crate::refcount::{header_refcount, ll_release, ll_retain};

/// One live object, and a chain of `drops` drops into it, each a count it
/// holds, published on this thread's record as a collector publishes.
///
/// # Safety
/// The calling thread has a record.
unsafe fn drops_into_a_live_object(arena: &mut Arena, drops: usize) -> *mut RcHeader {
    let class = ClassBuilder::new("SlicedDrops").build();
    let mut context = LLContext { arena };
    let live =
        unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) } as *mut RcHeader;
    unsafe { publish_drops_into(live, drops) };
    live
}

/// Publish `drops` drops into `live`, its counts taken first.
///
/// # Safety
/// As [`drops_into_a_live_object`].
unsafe fn publish_drops_into(live: *mut RcHeader, drops: usize) {
    let mut frees = Frees {
        drops: Chain::empty(),
        chains: Chain::empty(),
        registered: 0,
        members: 0,
        held: Chain::empty(),
        held_count: 0,
    };
    for _ in 0..drops {
        unsafe { ll_retain(live) };
        assert!(frees.drops.push(live as usize), "the pool served the chain");
    }
    let record = crate::cycle::mutator_record::this_thread_record();
    publish(frees, unsafe { &*record });
}

/// A poll's slice applies at most the stride, leaves the rest standing on
/// the record and says so; the slices after it go on from there until none
/// stands. Red with the application whole.
#[test]
fn a_slice_applies_the_stride_and_leaves_the_rest_standing() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let drops = 2 * APPLY_STRIDE + 5;
    let live = unsafe { drops_into_a_live_object(&mut arena, drops) };
    let held = |live| unsafe { header_refcount(live) } as usize;
    assert_eq!(held(live), drops + 1);

    assert_eq!(unsafe { apply_a_slice_of_this_threads() }, (0, true));
    assert_eq!(held(live), drops + 1 - APPLY_STRIDE, "one stride applied");
    assert_eq!(unsafe { apply_a_slice_of_this_threads() }, (0, true));
    assert_eq!(held(live), 6);
    assert_eq!(unsafe { apply_a_slice_of_this_threads() }, (0, false));
    assert_eq!(held(live), 1, "every drop applied once");
    assert_eq!(unsafe { apply_a_slice_of_this_threads() }, (0, false));
    unsafe { ll_release(live) };
}

/// What a collector publishes while a slice's rest stands is pushed beside
/// it, not over it: both are applied whole. Red with the rest stored back
/// over the word.
#[test]
fn a_publication_beside_a_standing_rest_loses_no_drop() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let drops = APPLY_STRIDE + 7;
    let live = unsafe { drops_into_a_live_object(&mut arena, drops) };
    assert_eq!(unsafe { apply_a_slice_of_this_threads() }, (0, true));
    unsafe { publish_drops_into(live, 3) };
    let held = || unsafe { header_refcount(live) } as usize;
    assert_eq!(held(), 1 + 7 + 3);

    assert_eq!(unsafe { apply_a_slice_of_this_threads() }, (0, false));
    assert_eq!(held(), 1, "the rest and the publication both applied");
    unsafe { ll_release(live) };
}

/// The whole application takes every entry of the stack.
#[test]
fn the_whole_application_takes_every_entry_of_the_stack() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let live = unsafe { drops_into_a_live_object(&mut arena, 5) };
    unsafe { publish_drops_into(live, 4) };
    assert_eq!(unsafe { header_refcount(live) }, 10);
    assert_eq!(unsafe { apply_this_threads() }, 0);
    assert_eq!(unsafe { header_refcount(live) }, 1);
    let record = crate::cycle::mutator_record::this_thread_record();
    assert!(!unsafe { &*record }.collectors_frees_stand());
    unsafe { ll_release(live) };
}
