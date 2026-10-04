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

/// What the destructor of [`a_publishing_class`] publishes into: the target
/// and how many drops, set by the case before the slice.
static PUBLISH_INTO: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

unsafe extern "C" fn publish_at_the_death(_object: *mut crate::object::Object) {
    let live = PUBLISH_INTO.swap(0, std::sync::atomic::Ordering::Relaxed) as *mut RcHeader;
    if !live.is_null() {
        unsafe { publish_drops_into(live, 3) };
    }
}

/// A class whose destructor publishes three drops on the record, as a
/// collector's grant consented to from inside a slice's cascade does.
fn a_publishing_class() -> *const crate::class::Class {
    ClassBuilder::new("PublishingAtItsDeath")
        .destructor(publish_at_the_death as *const ())
        .build()
}

/// A publication made while a slice holds the stack in its hands — the word
/// empty, the rest not yet back — is kept beside the rest: both are
/// applied. Red with the rest stored back over the word.
#[test]
fn a_publication_during_a_slice_is_kept_beside_its_rest() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let dying =
        unsafe { new_constructed(&mut context, a_publishing_class(), MemoryCategory::GcHeap) }
            as *mut RcHeader;
    let live = unsafe { drops_into_a_live_object(&mut arena, APPLY_STRIDE + 9) };
    // The dying object's drop, its last count, on top of the stack.
    let mut frees = Frees {
        drops: Chain::empty(),
        chains: Chain::empty(),
        registered: 0,
        members: 0,
        held: Chain::empty(),
        held_count: 0,
    };
    assert!(frees.drops.push(dying as usize));
    let record = crate::cycle::mutator_record::this_thread_record();
    publish(frees, unsafe { &*record });
    PUBLISH_INTO.store(live as usize, std::sync::atomic::Ordering::Relaxed);

    let held = || unsafe { header_refcount(live) } as usize;
    assert_eq!(held(), 1 + APPLY_STRIDE + 9);
    while unsafe { apply_a_slice_of_this_threads() }.1 {}
    assert_eq!(
        PUBLISH_INTO.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "it published"
    );
    assert_eq!(held(), 1, "the rest and the publication both applied");
    unsafe { ll_release(live) };
}

/// A chain longer than a metadata block goes in slices across the block's
/// end, every drop applied once; the members it freed are answered by the
/// first slice alone.
#[test]
fn a_chain_past_one_block_goes_in_slices_and_counts_its_members_once() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let drops = ENTRIES_PER_BLOCK + APPLY_STRIDE + 3;
    let class = ClassBuilder::new("SlicedAcrossBlocks").build();
    let mut context = LLContext { arena: &mut arena };
    let live =
        unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) } as *mut RcHeader;
    let mut frees = Frees {
        drops: Chain::empty(),
        chains: Chain::empty(),
        registered: 0,
        members: 7,
        held: Chain::empty(),
        held_count: 0,
    };
    for _ in 0..drops {
        unsafe { ll_retain(live) };
        assert!(frees.drops.push(live as usize));
    }
    assert!(frees.drops.blocks >= 2, "past one block");
    let record = crate::cycle::mutator_record::this_thread_record();
    publish(frees, unsafe { &*record });

    let mut answered = Vec::new();
    loop {
        let (members, stand) = unsafe { apply_a_slice_of_this_threads() };
        answered.push(members);
        if !stand {
            break;
        }
    }
    assert_eq!(answered.len(), drops.div_ceil(APPLY_STRIDE));
    assert_eq!(answered[0], 7);
    assert!(answered[1..].iter().all(|&members| members == 0));
    assert_eq!(unsafe { header_refcount(live) }, 1);
    unsafe { ll_release(live) };
}

/// A poll that leaves drops standing reads no posted set and keeps its
/// arming, which the poll that applies the last of them fires.
#[test]
fn a_poll_with_drops_standing_keeps_its_arming() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let live = unsafe { drops_into_a_live_object(&mut arena, APPLY_STRIDE + 1) };
    crate::gc::arm_to_retire();
    let _ = unsafe { crate::gc::ll_gc_maybe_collect() };
    let record = crate::cycle::mutator_record::this_thread_record();
    assert!(
        unsafe { &*record }.collectors_frees_stand(),
        "a drop stands"
    );
    assert!(crate::gc::is_armed(), "the arming kept");
    let _ = unsafe { crate::gc::ll_gc_maybe_collect() };
    assert!(!unsafe { &*record }.collectors_frees_stand());
    assert!(!crate::gc::is_armed(), "fired once none stood");
    assert_eq!(unsafe { header_refcount(live) }, 1);
    unsafe { ll_release(live) };
}
