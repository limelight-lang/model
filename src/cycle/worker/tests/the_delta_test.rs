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

unsafe extern "C" fn no_destructor_body(_object: *mut Object) {}

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

/// A ring nobody touched since the consent is proved by its tags, and the
/// collector frees it itself (S68.6b): the owner's poll applies what it left
/// and counts the two members freed, with no collection over a set.
#[test]
fn an_untouched_garbage_ring_is_proved_by_its_tags() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let _ = unsafe { a_garbage_ring(&mut arena) };
    let _ = crate::cycle::trace::take_sets_proved_by_tags_validated();
    let freed_before = crate::cycle::collector_frees::frees_counts();

    let counts = served_and_counted();
    assert_eq!(
        (counts.proved, counts.touched, counts.no_checkpoint),
        (1, 0, 0)
    );
    let freed = crate::cycle::collector_frees::frees_counts();
    assert_eq!(
        (
            freed.sets - freed_before.sets,
            freed.members - freed_before.members
        ),
        (1, 2),
        "the collector freed the ring"
    );
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "the poll applied its frees"
    );
    assert_eq!(
        crate::cycle::trace::take_sets_proved_by_tags_validated(),
        0,
        "no set reached the owner"
    );
    reset_lanes();
}

/// The explicit fire applies what a collector freed before it reads P, as the
/// poll does, and counts it.
#[test]
fn the_explicit_fire_applies_the_collectors_frees() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let _ = unsafe { a_garbage_ring(&mut arena) };
    let freed_before = crate::cycle::collector_frees::frees_counts();

    let counts = served_and_counted();
    assert_eq!((counts.proved, counts.touched), (1, 0));
    assert_eq!(
        crate::cycle::collector_frees::frees_counts().members - freed_before.members,
        2
    );
    assert_eq!(
        unsafe { crate::gc::ll_gc_collect_cycles() },
        2,
        "the fire applied the frees"
    );
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        0,
        "nothing stood after it"
    );
    reset_lanes();
}

/// A proved ring the collector does not free itself — a member with a
/// destructor — reaches the owner marked: the owner meets its members with no
/// trace of their cells, confirms the set by the counts' sum against the edges
/// the collector recorded between them, and frees the ring (S68.6a).
#[test]
fn a_proved_ring_the_collector_keeps_from_is_taken_by_the_owner_without_a_trace() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let kept = ClassBuilder::new("DeltaTestDestructedNode")
        .prop("next", true)
        .destructor(no_destructor_body as *const ())
        .build();
    let mut context = LLContext { arena: &mut arena };
    let a = unsafe { new_constructed(&mut context, kept, MemoryCategory::GcHeap) };
    let b = unsafe { new_constructed(&mut context, kept, MemoryCategory::GcHeap) };
    unsafe {
        store_prop(&mut arena, a, prop_offset(0), b);
        store_prop(&mut arena, b, prop_offset(0), a);
        assert!(!ll_release(a as *mut RcHeader));
        assert!(!ll_release(b as *mut RcHeader));
    }
    let _ = crate::cycle::trace::take_sets_proved_by_tags_validated();

    let counts = served_and_counted();
    assert_eq!(counts.proved, 1);
    let _ = crate::cycle::finalization::take_confirmed_by_the_sum();
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2, "the ring was freed");
    assert_eq!(
        crate::cycle::trace::take_sets_proved_by_tags_validated(),
        1,
        "the owner took the proved set without a trace of its cells"
    );
    assert_eq!(
        crate::cycle::finalization::take_confirmed_by_the_sum(),
        1,
        "the commit confirmed it by the counts' sum against the recorded edges"
    );
    reset_lanes();
}

/// Touch `member` with the open window between the next batch's mark and its
/// Δ-test, as the mutator's own count write or slot store would.
fn touch_between_the_phases(member: *mut Object) {
    let token = unsafe { &raw const (*record()).token } as usize;
    let member = member as usize;
    testing::between_the_next_phases(Box::new(move || unsafe {
        let window = (*(token as *const crate::cycle::token::TraceToken)).window();
        crate::refcount::set_window(window);
        crate::refcount::tag_with_the_window(member as *mut RcHeader);
        crate::refcount::set_window(0);
    }));
}

/// A member tagged with the open window between the mark and the Δ-test — a
/// count write or a slot store of the mutator's — refuses what its recorded
/// edges reach, here the whole ring, which goes back to R unwalked; the next
/// batch, untouched, proves it and the collector frees it.
#[test]
fn a_member_touched_in_the_window_sends_what_it_reaches_back_once() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let (a, _) = unsafe { a_garbage_ring(&mut arena) };
    touch_between_the_phases(a);
    let requeued = crate::cycle::split::split_counts()[1];

    let counts = served_and_counted();
    assert_eq!((counts.proved, counts.touched), (0, 1));
    assert_eq!(
        crate::cycle::split::split_counts()[1] - requeued,
        1,
        "the touched closure went back to R"
    );
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        0,
        "nothing reached the owner"
    );

    let freed_before = crate::cycle::collector_frees::frees_counts();
    let counts = served_and_counted();
    assert_eq!((counts.proved, counts.touched), (1, 0));
    assert_eq!(
        crate::cycle::collector_frees::frees_counts().members - freed_before.members,
        2,
        "the second batch's collector freed the ring"
    );
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2);
    reset_lanes();
}

/// A root whose closure a touch refused once, touched again, is refused no
/// second time: its closure joins S, unmarked, and goes the owner's exact way.
#[test]
fn a_closure_touched_twice_goes_the_exact_way() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let (a, _) = unsafe { a_garbage_ring(&mut arena) };
    touch_between_the_phases(a);
    let _ = served_and_counted();
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0);

    touch_between_the_phases(a);
    let second = crate::cycle::split::split_counts()[2];
    let _ = crate::cycle::trace::take_sets_proved_by_tags_validated();
    let counts = served_and_counted();
    assert_eq!(counts.touched, 1);
    assert_eq!(
        crate::cycle::split::split_counts()[2] - second,
        1,
        "the second refusal kept the closure in W"
    );
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "the exact validation freed it"
    );
    assert_eq!(
        crate::cycle::trace::take_sets_proved_by_tags_validated(),
        0,
        "unmarked"
    );
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
/// before its own checkpoint answers it lands in the window and refuses what
/// the member reaches: the answer's release is what puts the tag before the
/// collector's reads (`dev/design/recycler-over-counts.md`, §4.7).
#[test]
fn a_write_before_the_checkpoint_answers_refuses_what_it_reaches() {
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
        0,
        "the touched ring went back to R"
    );
    let _ = served_and_counted();
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "the next batch freed it"
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

/// A member with weak references is one the collector does not free: an
/// upgrade after T can make it live again, which no tag records. It and what
/// it reaches, here the ring, go the owner's exact way, unmarked.
#[test]
fn a_weakly_held_member_sends_what_it_reaches_the_exact_way() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let (a, _) = unsafe { a_garbage_ring(&mut arena) };
    let weak = {
        let mut context = LLContext { arena: &mut arena };
        unsafe { crate::weak::ll_weakref_create(&mut context, a as *mut RcHeader) }
    };
    assert!(!weak.is_null());

    let _ = crate::cycle::trace::take_sets_proved_by_tags_validated();
    let counts = served_and_counted();
    assert_eq!((counts.proved, counts.weakly_held), (1, 1));
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2);
    assert_eq!(
        crate::cycle::trace::take_sets_proved_by_tags_validated(),
        0,
        "unmarked"
    );
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
    // A destructor keeps the set from the collector's own free (S68.6b), so
    // that it reaches the owner as a set, marked or not.
    let small = ClassBuilder::new("DeltaTestKeptNode")
        .prop("next", true)
        .destructor(no_destructor_body as *const ())
        .build();
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

static DESTRUCTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

unsafe extern "C" fn count_the_destruction(_object: *mut Object) {
    DESTRUCTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// A clean ring `c` holding a ring `d` whose class has a destructor, every
/// one garbage, `c1` and `d1` registered first so that a batch of two takes
/// a root in each and the mark reaches all four: W, whose split frees `c` and
/// posts `d` to the owner, C's one edge into it held.
unsafe fn a_clean_ring_over_a_destructed_one(arena: &mut Arena) -> [*mut Object; 4] {
    unsafe { a_clean_ring_over_a_destructed_one_registered(arena, [0, 2, 1, 3]) }
}

/// [`a_clean_ring_over_a_destructed_one`], registered in `order` of
/// `[c1, c2, d1, d2]`.
unsafe fn a_clean_ring_over_a_destructed_one_registered(
    arena: &mut Arena,
    order: [usize; 4],
) -> [*mut Object; 4] {
    let clean = ClassBuilder::new("DeltaTestHolder")
        .prop("next", true)
        .prop("held", true)
        .build();
    let destructed = ClassBuilder::new("DeltaTestCountedNode")
        .prop("next", true)
        .destructor(count_the_destruction as *const ())
        .build();
    let mut context = LLContext { arena: &mut *arena };
    let c1 = unsafe { new_constructed(&mut context, clean, MemoryCategory::GcHeap) };
    let c2 = unsafe { new_constructed(&mut context, clean, MemoryCategory::GcHeap) };
    let d1 = unsafe { new_constructed(&mut context, destructed, MemoryCategory::GcHeap) };
    let d2 = unsafe { new_constructed(&mut context, destructed, MemoryCategory::GcHeap) };
    unsafe {
        store_prop(arena, c1, prop_offset(0), c2);
        store_prop(arena, c2, prop_offset(0), c1);
        store_prop(arena, c1, prop_offset(1), d1);
        store_prop(arena, d1, prop_offset(0), d2);
        store_prop(arena, d2, prop_offset(0), d1);
        let members = [c1, c2, d1, d2];
        for index in order {
            assert!(!ll_release(members[index] as *mut RcHeader));
        }
    }
    [c1, c2, d1, d2]
}

/// The split: the collector frees the clean ring, and the destructed ring it
/// holds reaches the owner marked proved, C's edge into it held — the owner
/// confirms it by the sum with that edge counted, before any drop into it has
/// run, and frees it, its destructors run once each.
#[test]
fn a_clean_ring_is_freed_by_the_collector_and_the_ring_it_holds_by_the_owner() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let _ = unsafe { a_clean_ring_over_a_destructed_one(&mut arena) };
    let _ = crate::cycle::trace::take_sets_proved_by_tags_validated();
    let _ = crate::cycle::finalization::take_confirmed_by_the_sum();
    let freed_before = crate::cycle::collector_frees::frees_counts();
    let split_before = crate::cycle::split::split_counts()[0];
    let destructed = DESTRUCTED.load(std::sync::atomic::Ordering::Relaxed);

    let counts = served_and_counted();
    assert_eq!((counts.proved, counts.touched), (1, 0));
    assert_eq!(crate::cycle::split::split_counts()[0] - split_before, 1);
    assert_eq!(
        crate::cycle::collector_frees::frees_counts().members - freed_before.members,
        2,
        "the collector freed the clean ring"
    );
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        4,
        "the clean ring applied, the destructed ring freed by the owner"
    );
    assert_eq!(crate::cycle::trace::take_sets_proved_by_tags_validated(), 1);
    assert_eq!(
        crate::cycle::finalization::take_confirmed_by_the_sum(),
        1,
        "confirmed by the sum, C's edge counted"
    );
    assert_eq!(
        DESTRUCTED.load(std::sync::atomic::Ordering::Relaxed) - destructed,
        2
    );
    reset_lanes();
}

/// A proved S given back unread — the explicit fire collects over R whole —
/// has C's held drops applied before that collection's trace, so S reads its
/// own counts there and is freed with the rest.
#[test]
fn held_drops_given_back_unread_are_applied_before_the_trace_over_r() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let _ = unsafe { a_clean_ring_over_a_destructed_one(&mut arena) };
    let destructed = DESTRUCTED.load(std::sync::atomic::Ordering::Relaxed);

    let _ = served_and_counted();
    assert_eq!(
        unsafe { crate::gc::ll_gc_collect_cycles() },
        4,
        "the clean ring applied, the destructed ring collected over R"
    );
    assert_eq!(
        DESTRUCTED.load(std::sync::atomic::Ordering::Relaxed) - destructed,
        2
    );
    assert!(!unsafe { &*record() }.collectors_frees_stand());
    reset_lanes();
}

/// A clean ring `d` whose first member holds the first of a ring `a`, every
/// one garbage, `d1` and `a1` registered first: a batch of two reaches all
/// four, and a touch of `a1` refuses `a` alone.
unsafe fn a_clean_ring_over_a_ring(arena: &mut Arena) -> [*mut Object; 4] {
    let holder = ClassBuilder::new("DeltaTestOuterHolder")
        .prop("next", true)
        .prop("held", true)
        .build();
    let node = node_class();
    let mut context = LLContext { arena: &mut *arena };
    let d1 = unsafe { new_constructed(&mut context, holder, MemoryCategory::GcHeap) };
    let d2 = unsafe { new_constructed(&mut context, holder, MemoryCategory::GcHeap) };
    let a1 = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    let a2 = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    unsafe {
        store_prop(arena, d1, prop_offset(0), d2);
        store_prop(arena, d2, prop_offset(0), d1);
        store_prop(arena, d1, prop_offset(1), a1);
        store_prop(arena, a1, prop_offset(0), a2);
        store_prop(arena, a2, prop_offset(0), a1);
        for member in [d1, a1, d2, a2] {
            assert!(!ll_release(member as *mut RcHeader));
        }
    }
    [d1, d2, a1, a2]
}

/// A touch refuses what it reaches and no more: the ring the touched member
/// stands in goes back to R, the clean ring holding it is freed by the
/// collector, its edge into the refused ring a drop like any other; the next
/// batch proves the refused ring alone.
#[test]
fn a_touch_refuses_its_closure_and_the_rest_is_freed() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let [_, _, a1, _] = unsafe { a_clean_ring_over_a_ring(&mut arena) };
    touch_between_the_phases(a1);
    let freed_before = crate::cycle::collector_frees::frees_counts();
    let requeued = crate::cycle::split::split_counts()[1];

    let counts = served_and_counted();
    assert_eq!(counts.touched, 1);
    assert_eq!(crate::cycle::split::split_counts()[1] - requeued, 1);
    assert_eq!(
        crate::cycle::collector_frees::frees_counts().members - freed_before.members,
        2,
        "the clean ring freed beside the refused one"
    );
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "the clean ring applied"
    );

    let counts = served_and_counted();
    assert_eq!((counts.proved, counts.touched), (1, 0));
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "the refused ring, proved"
    );
    reset_lanes();
}

/// At a second refusal the touched closure stays in W as a seed of S, which
/// goes the owner's exact way unmarked, while the collector frees C beside
/// it; the owner applies C's drop into S before it reads P.
#[test]
fn a_second_refusal_beside_a_clean_ring_frees_both() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let [_, _, a1, _] = unsafe { a_clean_ring_over_a_ring(&mut arena) };
    touch_between_the_phases(a1);
    let _ = served_and_counted();
    // The clean ring is freed at the first batch, and the refused ring's roots
    // go back to R.
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2);

    touch_between_the_phases(a1);
    let second = crate::cycle::split::split_counts()[2];
    let counts = served_and_counted();
    assert_eq!(counts.touched, 1);
    assert_eq!(crate::cycle::split::split_counts()[2] - second, 1);
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "the refused ring, the exact way"
    );
    reset_lanes();
}

/// A weakly-held member beside a clean ring: the ring it stands in seeds S,
/// unmarked, and the collector frees the clean ring holding it; the owner
/// applies the drop into S, then frees S the exact way.
#[test]
fn a_weakly_held_ring_beside_a_clean_one_is_split_and_unmarked() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let [_, _, a1, _] = unsafe { a_clean_ring_over_a_ring(&mut arena) };
    let weak = {
        let mut context = LLContext { arena: &mut arena };
        unsafe { crate::weak::ll_weakref_create(&mut context, a1 as *mut RcHeader) }
    };
    let freed_before = crate::cycle::collector_frees::frees_counts();
    let _ = crate::cycle::trace::take_sets_proved_by_tags_validated();

    let counts = served_and_counted();
    assert_eq!((counts.proved, counts.weakly_held), (1, 1));
    assert_eq!(
        crate::cycle::collector_frees::frees_counts().members - freed_before.members,
        2
    );
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 4);
    assert_eq!(
        crate::cycle::trace::take_sets_proved_by_tags_validated(),
        0,
        "S unmarked"
    );
    unsafe {
        assert!(ll_release(weak as *mut RcHeader));
        crate::object::ll_entity_die(weak as *mut RcHeader);
    }
    reset_lanes();
}

/// An S no root of the batch lands in is not posted: C's drops into it go to
/// the owner with the rest, and the poll's application lowers its counts; the
/// next batch, over its own roots, proves it alone.
#[test]
fn an_s_no_root_lands_in_gets_its_drops_at_the_poll() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let _ = unsafe { a_clean_ring_over_a_destructed_one_registered(&mut arena, [0, 1, 2, 3]) };
    let destructed = DESTRUCTED.load(std::sync::atomic::Ordering::Relaxed);

    let _ = served_and_counted();
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "the clean ring applied"
    );
    assert!(!unsafe { &*record() }.collectors_frees_stand());
    assert_eq!(
        DESTRUCTED.load(std::sync::atomic::Ordering::Relaxed),
        destructed
    );

    let _ = served_and_counted();
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2, "the destructed ring");
    assert_eq!(
        DESTRUCTED.load(std::sync::atomic::Ordering::Relaxed) - destructed,
        2
    );
    reset_lanes();
}
