//! The collector's Δ-test of the set its scan over the record proved, under
//! `recycler-over-counts` (`crate::cycle::delta_test`;
//! `dev/design/recycler-over-counts.md`, §4): a garbage ring nobody touched
//! since the consent is proved by its tags, and the owner's exact validation
//! agrees; a member tagged with the open window refuses the set; a stale tag
//! is cleared as it is read.

use super::the_batch::served_by_a_collector;
use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::delta_test::tag_reading_counts;
use crate::gc::ll_gc_maybe_collect;
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::{Object, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, ll_release, window_tag};
use crate::test_support::{prop_offset, store_prop};

fn node_class() -> *const Class {
    ClassBuilder::new("DeltaTestNode")
        .prop("next", true)
        .build()
}

/// A garbage ring of two, its first member registered: one root of R.
unsafe fn a_garbage_ring(arena: &mut Arena) -> (*mut Object, *mut Object) {
    let node = node_class();
    let mut context = LLContext { arena: &mut *arena };
    let a = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    let b = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    unsafe {
        store_prop(arena, a, prop_offset(0), b);
        store_prop(arena, b, prop_offset(0), a);
        assert!(!ll_release(a as *mut RcHeader), "b's edge holds a");
        assert!(!ll_release(b as *mut RcHeader), "a's edge holds b");
    }
    (a, b)
}

/// Serve the ring's batch and answer how the Δ-test's counts moved.
fn served_and_counted() -> crate::cycle::delta_test::TagReadingCounts {
    let before = tag_reading_counts();
    unsafe { &*record() }.set_batch_size(2);
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { complete: true, .. }
    ));
    let after = tag_reading_counts();
    crate::cycle::delta_test::TagReadingCounts {
        proved: after.proved - before.proved,
        touched: after.touched - before.touched,
        weakly_held: after.weakly_held - before.weakly_held,
        no_checkpoint: after.no_checkpoint - before.no_checkpoint,
        ..after
    }
}

/// A ring nobody touched since the consent is proved by its tags; the owner's
/// exact validation of the proved set reads every member unreachable, as a
/// debug build asserts, and frees the ring.
#[test]
fn an_untouched_garbage_ring_is_proved_by_its_tags() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let _ = unsafe { a_garbage_ring(&mut arena) };
    let _ = crate::cycle::trace::take_sets_proved_by_tags_validated();

    let counts = served_and_counted();
    assert_eq!(
        (counts.proved, counts.touched, counts.no_checkpoint),
        (1, 0, 0)
    );
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2, "the ring was freed");
    assert_eq!(
        crate::cycle::trace::take_sets_proved_by_tags_validated(),
        1,
        "the owner validated the proved set"
    );
    reset_lanes();
}

/// A member tagged with the open window between the mark and the Δ-test — a
/// count write or a slot store of the mutator's — refuses the set, which the
/// owner's exact validation then reads as it reads any set.
#[test]
fn a_member_touched_in_the_window_refuses_the_set() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let (a, _) = unsafe { a_garbage_ring(&mut arena) };
    let token = unsafe { &raw const (*record()).token } as usize;
    let member = a as usize;
    testing::between_the_next_phases(Box::new(move || unsafe {
        // The mutator's tag, written here as its own thread would write it.
        let window = (*(token as *const crate::cycle::token::TraceToken)).window();
        crate::refcount::set_window(window);
        crate::refcount::tag_with_the_window(member as *mut RcHeader);
        crate::refcount::set_window(0);
    }));
    let _ = crate::cycle::trace::take_sets_proved_by_tags_validated();

    let counts = served_and_counted();
    assert_eq!((counts.proved, counts.touched), (0, 1));
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "the exact validation freed it"
    );
    assert_eq!(crate::cycle::trace::take_sets_proved_by_tags_validated(), 0);
    reset_lanes();
}

/// A member carrying a stale number — a window before the consent — does not
/// refuse the set, and the Δ-test clears it, so that the number coming round
/// again cannot refuse the next attempt.
#[test]
fn a_stale_tag_is_cleared_and_refuses_nothing() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let (a, _) = unsafe { a_garbage_ring(&mut arena) };
    // Any number but the one this thread's next consent opens.
    let open = crate::refcount::this_threads_window();
    let stale = crate::refcount::the_next_window() % 255 + 1;
    crate::refcount::set_window(stale);
    unsafe { crate::refcount::tag_with_the_window(a as *mut RcHeader) };
    crate::refcount::set_window(open);

    let counts = served_and_counted();
    assert_eq!((counts.proved, counts.touched), (1, 0));
    assert_eq!(
        unsafe { window_tag(a as *const RcHeader) },
        0,
        "the stale tag was cleared"
    );
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2);
    reset_lanes();
}

/// Serve the ring's batch with this thread standing in as the mutator, its
/// poll's answer to a checkpoint ask left to `at_the_poll`, which runs at each
/// reading the harness makes; and answer how the counts moved.
fn served_with(mut at_the_poll: impl FnMut()) -> crate::cycle::delta_test::TagReadingCounts {
    let before = tag_reading_counts();
    unsafe { &*record() }.set_batch_size(2);
    unsafe { &*record() }.clear_posted_for_test();
    let sent = crate::cycle::testing::Sent(record());
    let collector = std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the collector thread"
        );
        unsafe { testing::serve_alone(sent.into_inner()) }
    });
    while !collector.is_finished() {
        crate::cycle::token::read_and_act_on_this_thread();
        at_the_poll();
        std::thread::yield_now();
    }
    assert!(matches!(
        collector.join().expect("the collector finished"),
        Served::Batch { complete: true, .. }
    ));
    let after = tag_reading_counts();
    crate::cycle::delta_test::TagReadingCounts {
        proved: after.proved - before.proved,
        touched: after.touched - before.touched,
        weakly_held: after.weakly_held - before.weakly_held,
        no_checkpoint: after.no_checkpoint - before.no_checkpoint,
        ..after
    }
}

/// A count write the mutator makes on a member after the collector's ask and
/// before its own checkpoint answers it lands in the window and refuses the
/// set: the answer's release is what puts the tag before the collector's
/// reads (`dev/design/recycler-over-counts.md`, §4.7).
#[test]
fn a_write_before_the_checkpoint_answers_refuses_the_set() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let (a, _) = unsafe { a_garbage_ring(&mut arena) };
    let token = unsafe { &(*record()).token };
    let mut written = false;

    let counts = served_with(|| {
        if !written && token.checkpoint_is_asked() {
            // The mutator's own count write, under the window its consent
            // opened: a retain and a release that leave the count as it was.
            unsafe {
                crate::refcount::ll_retain(a as *mut RcHeader);
                let header = a as *mut RcHeader;
                let count = crate::refcount::header_refcount(header);
                crate::refcount::set_header_refcount(header, count - 1);
            }
            written = true;
            token.reach_the_checkpoint();
        }
    });
    assert!(written, "the ask stood at a reading");
    assert_eq!(
        (counts.proved, counts.touched, counts.no_checkpoint),
        (0, 1, 0)
    );
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "the exact validation freed it"
    );
    reset_lanes();
}

/// A mutator that reaches no checkpoint costs the set its Δ-test: the wait
/// ends at its bound, and the set goes the exact way.
#[test]
fn a_mutator_that_never_answers_costs_the_test_and_nothing_else() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let _ = unsafe { a_garbage_ring(&mut arena) };
    let _ = crate::cycle::trace::take_sets_proved_by_tags_validated();

    let counts = served_with(|| {});
    assert_eq!((counts.proved, counts.no_checkpoint), (0, 1));
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2);
    assert_eq!(crate::cycle::trace::take_sets_proved_by_tags_validated(), 0);
    reset_lanes();
}

/// A member with weak references is not proved by the tags: an upgrade after
/// T can make it live again, which no tag records.
#[test]
fn a_weakly_held_member_keeps_the_set_from_its_proof() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let (a, _) = unsafe { a_garbage_ring(&mut arena) };
    let weak = {
        let mut context = LLContext { arena: &mut arena };
        unsafe { crate::weak::ll_weakref_create(&mut context, a as *mut RcHeader) }
    };
    assert!(!weak.is_null());

    let counts = served_and_counted();
    assert_eq!((counts.proved, counts.weakly_held), (0, 1));
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2);
    unsafe {
        assert!(ll_release(weak as *mut RcHeader));
        crate::object::ll_entity_die(weak as *mut RcHeader);
    }
    reset_lanes();
}

/// A set the pool closed short of every member is not marked proved, though
/// its tags pass: a part of a garbage set may not be freed while the rest
/// still names it. The ring's two members stand in two blocks, of two size
/// classes, and the set keeps the first block's member alone.
#[test]
fn a_set_cut_short_is_not_marked_proved() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let small = node_class();
    let wide = ClassBuilder::new("DeltaTestWideNode")
        .prop("next", true)
        .prop("a", false)
        .prop("b", false)
        .prop("c", false)
        .prop("d", false)
        .prop("e", false)
        .build();
    let (a, b) = {
        let mut context = LLContext { arena: &mut arena };
        unsafe {
            (
                new_constructed(&mut context, small, MemoryCategory::GcHeap),
                new_constructed(&mut context, wide, MemoryCategory::GcHeap),
            )
        }
    };
    let block_of = |entity: *mut Object| entity as usize & !crate::memory::block_pool::BLOCK_MASK;
    assert_ne!(
        block_of(a),
        block_of(b),
        "the two members stand in two blocks"
    );
    unsafe {
        store_prop(&mut arena, a, prop_offset(0), b);
        store_prop(&mut arena, b, prop_offset(0), a);
        assert!(!ll_release(a as *mut RcHeader));
        assert!(!ll_release(b as *mut RcHeader));
    }
    let _ = crate::cycle::trace::take_sets_proved_by_tags_validated();
    crate::cycle::posted_set::testing::refuse_the_second_block();

    let counts = served_and_counted();
    assert_eq!(counts.proved, 1, "the tags passed");
    assert_eq!(
        crate::cycle::posted_set::testing::take_members_posted(),
        Some(1),
        "the set kept one member"
    );
    let _ = unsafe { ll_gc_maybe_collect() };
    assert_eq!(
        crate::cycle::trace::take_sets_proved_by_tags_validated(),
        0,
        "the set reached the owner unmarked"
    );
    unsafe { crate::gc::ll_gc_collect_cycles() };
    reset_lanes();
}
