//! The set a collector's batch proves unreachable and posts beside its
//! verdicts (`crate::cycle::posted_set`), through a real serve: the owner's
//! collection over P frees a garbage cycle no root of which closes it, and
//! reads nothing of the live state the cycle points into; a stopped trace
//! posts the zero closure of its proposed roots, a scan cut short included,
//! and the closure reads the recall.

use super::the_batch::served_by_a_collector;
use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::arena::RECALL_STRIDE;
use crate::cycle::queue::candidate_count;
use crate::cycle::testing::move_prop;
use crate::gc::ll_gc_maybe_collect;
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, SlotState, ll_release, slot_state};
use crate::test_support::{prop_offset, store_prop};

/// Objects of the live state behind the cycle: far more positions than the
/// owner's reading of the cycle takes.
const STATE: usize = 1_000;

/// A destructor that does nothing.
unsafe extern "C" fn nothing_to_dispose(_object: *mut Object) {}

/// A class of `props` counted Box properties.
fn class_of(name: &str, props: usize) -> *const Class {
    let names: Vec<String> = (0..props).map(|index| format!("p{index}")).collect();
    let mut builder = ClassBuilder::new(name).destructor(nothing_to_dispose as *const ());
    for prop in &names {
        builder = builder.prop(prop, true);
    }
    builder.build()
}

/// One object of `class` at count one, the caller's.
unsafe fn object(arena: &mut Arena, class: *const Class) -> *mut Object {
    let mut context = LLContext { arena };
    unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) }
}

/// A live chain of [`STATE`] objects, no member registered, each link the
/// next one's creation reference: the head is the case's.
unsafe fn live_state(arena: &mut Arena) -> *mut Object {
    let link = class_of("PostedSetStateLink", 1);
    let head = unsafe { object(arena, link) };
    let mut tail = head;
    for _ in 1..STATE {
        let next = unsafe { object(arena, link) };
        unsafe { move_prop(tail, prop_offset(0), next) };
        tail = next;
    }
    head
}

/// A garbage triangle r, a, b, every pair linked both ways, with an edge from
/// b into `state`; r registered, a and b not, so that every cycle through a
/// and b avoids the batch's one root. Answers the three.
unsafe fn a_triangle_into(arena: &mut Arena, state: *mut Object) -> [*mut Object; 3] {
    let node = class_of("PostedSetTriangleNode", 3);
    let arena_ptr: *mut Arena = arena;
    let [r, a, b] = [(); 3].map(|_| unsafe { object(&mut *arena_ptr, node) });
    unsafe {
        // a's and b's creation references go into r, so neither ever takes
        // a non-final decrement.
        move_prop(r, prop_offset(0), a);
        move_prop(r, prop_offset(1), b);
        store_prop(arena_ptr, a, prop_offset(0), r);
        store_prop(arena_ptr, a, prop_offset(1), b);
        store_prop(arena_ptr, b, prop_offset(0), r);
        store_prop(arena_ptr, b, prop_offset(1), a);
        store_prop(arena_ptr, b, prop_offset(2), state);
        assert!(
            !ll_release(r as *mut RcHeader),
            "a and b hold r, and the release registers it"
        );
    }
    [r, a, b]
}

/// A garbage triangle whose cycles avoid the batch's one root is freed by the
/// collection over P from the set the batch posted, and the owner reads the
/// triangle and not the live state it points into: its mark expands fewer
/// entities than a tenth of the state. Red on proposed roots alone with the owner's own walk
/// (its final drain expands the state), and red on the zero-only rule, which
/// proves nothing here.
#[test]
fn a_cycle_that_avoids_the_root_is_freed_without_reading_the_state() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let state = unsafe { live_state(&mut arena) };
    let triangle = unsafe { a_triangle_into(&mut arena, state) };
    assert_eq!(candidate_count(), 1, "r alone is registered");
    unsafe { &*record() }.set_batch_size(1);

    assert!(matches!(
        served_by_a_collector(),
        Served::Batch {
            roots: 1,
            complete: true,
            ..
        }
    ));
    assert!(
        !unsafe { &*record() }.posted_set().is_null(),
        "the batch posted the triangle"
    );
    // A turn of the epoch first, so that no stamp of the batch's stands and
    // no prune is what keeps the owner off the state: it reads the set alone.
    crate::cycle::epoch::turn_this_threads_cell();
    crate::cycle::mark::record_expansions();
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 3, "the triangle is freed");
    let expanded = crate::cycle::mark::take_expansions().len();
    assert!(
        expanded < STATE / 10,
        "the owner expanded {expanded} entities"
    );
    for member in triangle {
        assert_ne!(
            unsafe { slot_state(member as *const RcHeader) },
            SlotState::Live
        );
    }

    unsafe {
        assert!(
            ll_release(state as *mut RcHeader),
            "the case held the state alone"
        );
        ll_object_die(state);
    }
    reset_lanes();
}

/// A trace stopped inside the mark posts the zero closure of its proposed
/// roots: a garbage ring of two met and expanded before the stop is posted,
/// and the owner frees it from the set; the root over a wide region, cut
/// inside its own, stays. R holds the wide root first, so the worklist expands
/// the ring first and the first reading falls inside the wide region.
#[test]
fn a_stopped_trace_posts_its_zero_closure_and_the_owner_frees_it() {
    use crate::array::entity::ll_array_new;
    use crate::array::testing::push;
    use crate::memory::barrier::write_value_slot;
    use crate::value::{Tag, Value};

    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let element = class_of("PostedSetStopElement", 0);
    let holder = class_of("PostedSetStopHolder", 1);
    let array = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    for _ in 0..3 * RECALL_STRIDE {
        let item = unsafe { object(&mut arena, element) };
        assert!(unsafe { push(array, Value::entity(Tag::Object, item as *mut RcHeader)) });
    }
    let wide = unsafe { object(&mut arena, holder) };
    unsafe {
        write_value_slot(
            Object::prop_at(wide, prop_offset(0)),
            Value::entity(Tag::Array, array as *mut RcHeader),
        );
        crate::refcount::ll_retain(wide as *mut RcHeader);
        assert!(
            !ll_release(wide as *mut RcHeader),
            "the case holds the wide root"
        );
    }
    let node = class_of("PostedSetStopRingNode", 1);
    let ring = unsafe { crate::cycle::testing::long_ring(&mut arena, node, 2) };
    assert_eq!(candidate_count(), 3);
    unsafe { &*record() }.set_batch_size(3);

    testing::recall_at_the_reading(1);
    let served = served_by_a_collector();
    unsafe { &(*record()).token }.recall_for_test(false);
    assert!(
        matches!(
            served,
            Served::Batch {
                complete: false,
                ..
            }
        ),
        "{served:?}"
    );
    assert!(
        !unsafe { &*record() }.posted_set().is_null(),
        "the stop posted the ring"
    );

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2, "the ring is freed");
    for member in ring {
        assert_ne!(
            unsafe { slot_state(member as *const RcHeader) },
            SlotState::Live
        );
    }
    assert_eq!(
        unsafe { slot_state(wide as *const RcHeader) },
        SlotState::Live
    );

    unsafe {
        assert!(
            ll_release(wide as *mut RcHeader),
            "the case held the wide root alone"
        );
        ll_object_die(wide);
    }
    reset_lanes();
}

/// A garbage ring of `members`, its first member registered and holding the
/// last's edge, every other member holding the creation reference of the next
/// and never decremented: one root of R, the rest reached only through it.
unsafe fn a_ring_behind_one_root(arena: &mut Arena, members: usize) -> *mut Object {
    let node = class_of("PostedSetLongRingNode", 1);
    let arena_ptr: *mut Arena = arena;
    let root = unsafe { object(&mut *arena_ptr, node) };
    let mut tail = root;
    for _ in 1..members {
        let next = unsafe { object(&mut *arena_ptr, node) };
        unsafe { move_prop(tail, prop_offset(0), next) };
        tail = next;
    }
    unsafe {
        store_prop(arena_ptr, tail, prop_offset(0), root);
        assert!(
            !ll_release(root as *mut RcHeader),
            "the last member holds the root"
        );
    }
    root
}

/// Serve this thread's record once with the recall raised between the mark
/// and the scan, clear it, and answer the members the stop posted.
fn a_serve_recalled_after_the_mark() -> Option<usize> {
    let token = unsafe { &raw const (*record()).token } as usize;
    testing::between_the_next_phases(Box::new(move || {
        unsafe { &*(token as *const crate::cycle::token::TraceToken) }.recall_for_test(true)
    }));
    let _ = crate::cycle::posted_set::testing::take_members_posted();
    let served = served_by_a_collector();
    unsafe { &(*record()).token }.recall_for_test(false);
    assert!(
        matches!(
            served,
            Served::Batch {
                complete: false,
                ..
            }
        ),
        "{served:?}"
    );
    crate::cycle::posted_set::testing::take_members_posted()
}

/// Members of a ring the mark crosses whole and the scan cuts: the mark reads
/// one position a member, so the first reading after the mark falls inside the
/// scan, which has coloured part of the ring potentially unreachable by then;
/// the closure, reading the recall a stride further on, reaches the rest.
const CUT_IN_THE_SCAN: usize = RECALL_STRIDE * 3 / 4;

/// A stop inside the scan posts the whole ring: the rows the cut scan coloured
/// potentially unreachable read unclassified again before the zero closure, so
/// the closure from the root walks past them. Red without that: the set holds
/// the scan's prefix, the owner reads the root live, and nothing is freed.
#[test]
fn a_stop_inside_the_scan_posts_the_whole_ring() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let root = unsafe { a_ring_behind_one_root(&mut arena, CUT_IN_THE_SCAN) };
    assert_eq!(candidate_count(), 1);
    unsafe { &*record() }.set_batch_size(1);

    assert_eq!(a_serve_recalled_after_the_mark(), Some(CUT_IN_THE_SCAN));
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        CUT_IN_THE_SCAN,
        "the ring is freed"
    );
    assert_ne!(
        unsafe { slot_state(root as *const RcHeader) },
        SlotState::Live
    );
    reset_lanes();
}

/// The closure reads the recall: behind a ring of many strides the stop posts
/// at most a stride or two of it, the mutator waiting through no more, and the
/// owner, proving nothing of a cut ring, writes the root back into R for a
/// later batch. Red with a closure that reads no recall: it posts the ring
/// whole.
#[test]
fn a_stopped_trace_posts_a_closure_cut_at_the_recall() {
    const MEMBERS: usize = 8 * RECALL_STRIDE;
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let root = unsafe { a_ring_behind_one_root(&mut arena, MEMBERS) };
    unsafe { &*record() }.set_batch_size(1);

    let posted = a_serve_recalled_after_the_mark().expect("the stop posted a set");
    assert!(
        posted <= 2 * RECALL_STRIDE,
        "the closure posted {posted} members"
    );
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        0,
        "a cut ring proves nothing"
    );
    assert_eq!(candidate_count(), 1, "the root went back into R");

    assert_eq!(unsafe { crate::gc::ll_gc_collect_cycles() }, MEMBERS);
    let _ = root;
    reset_lanes();
}
