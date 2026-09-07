//! The queue between the sever and the drops: what order it hands children
//! back in, what it costs a second component, and the one cell it is not what
//! clears.

use super::*;
use crate::weak::{LLWeakRef, ll_weakref_create, ll_weakref_get};

/// Children dropped since a case cleared it, newest last. Two entries are
/// enough to read an order off.
static DROP_ORDER: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];

/// How many children have been dropped, which is where the next entry of
/// [`DROP_ORDER`] goes.
static DROPS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn ordering_destructor(obj: *mut Object) {
    let at = DROPS.fetch_add(1, Ordering::Relaxed);
    if let Some(slot) = DROP_ORDER.get(at) {
        slot.store(obj as usize, Ordering::Relaxed);
    }
}

/// The arena a destructor of this file allocates through, which is the case's
/// own.
///
/// **The case reaches its arena through the same raw pointer**, and this is
/// where that rule is paid: a `&mut` taken from the binding beside it pops the
/// tag this one was exposed under, and the destructor's own reborrow is then
/// an aliasing violation Miri reports and an ordinary run does not
/// (`dev/WORKFLOW.md`, Miri, "A test keeps one raw pointer per object").
static AMBIENT_ARENA: AtomicUsize = AtomicUsize::new(0);

/// The cell the destructor below created, for the case to read after the free.
static RECREATED_CELL: AtomicUsize = AtomicUsize::new(0);

/// A destructor that takes a weak reference to `$this` — the one thing step 3's
/// nulling cannot cover, because it happens after it
/// (`rfc/model/weak-references.md`, "Death notification", the cycle-death
/// bullet).
unsafe extern "C" fn resurrecting_the_weak_state(obj: *mut Object) {
    let arena = AMBIENT_ARENA.load(Ordering::Relaxed) as *mut Arena;
    let mut context = LLContext {
        arena: unsafe { &mut *arena },
    };

    let cell = unsafe { ll_weakref_create(&mut context, obj as *mut RcHeader) };
    assert!(!cell.is_null(), "the fixture's second cell");
    RECREATED_CELL.store(cell as usize, Ordering::Relaxed);
}

/// A member holding two external children, which is the smallest arrangement
/// an order can be read from.
fn two_child_class(name: &str) -> *const Class {
    ClassBuilder::new(name)
        .prop("next", true)
        .prop("first_child", true)
        .prop("second_child", true)
        .build()
}

#[test]
fn the_children_are_dropped_in_the_order_the_sever_displaced_them() {
    let _g = test_guard();
    let holder = two_child_class("ReclamationTwoChildHolder");
    let peer = node_class("ReclamationOrderPeer");
    let child = ClassBuilder::new("ReclamationOrderedChild")
        .destructor(ordering_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let [first, second] = unsafe { ring(&mut arena, [holder, peer]) };
    let earlier = unsafe { object(&mut arena, child) };
    let later = unsafe { object(&mut arena, child) };

    unsafe {
        store_prop(&mut arena, first, prop_offset(1), earlier);
        store_prop(&mut arena, first, prop_offset(2), later);
        spend_creation_references(&[earlier, later]);
        read_as_unreachable(first, &[first, second]);
    }

    DROPS.store(0, Ordering::Relaxed);
    let mut scratch = open_arena();
    let mut members = headers([first, second]);
    assert_eq!(
        unsafe { commit(&mut members, &mut scratch) },
        Reclaimed::Freed
    );

    assert_eq!(DROPS.load(Ordering::Relaxed), 2, "both children died");
    assert_eq!(
        [
            DROP_ORDER[0].load(Ordering::Relaxed) as *mut Object,
            DROP_ORDER[1].load(Ordering::Relaxed) as *mut Object,
        ],
        [earlier, later],
        "the queue hands its children back in the order the sever displaced \
         them, which is the order the ordinary teardown would have released \
         the same cells in"
    );
    scratch.reset();
}

#[test]
fn a_second_component_reuses_the_segment_the_first_one_emptied() {
    let _g = test_guard();
    let plain = ClassBuilder::new("ReclamationSegmentChild").build();

    // Both components are read before the teardown's arena opens: a thread
    // bumps one workspace at a time, and the trace that reads a component
    // opens an arena of its own.
    let mut arena = Arena::new();
    let node = node_class("ReclamationSegmentNode");
    let components: [[*mut Object; 2]; 2] = std::array::from_fn(|_| {
        let [first, second] = unsafe { ring(&mut arena, [node, node]) };
        let child = unsafe { object(&mut arena, plain) };
        unsafe {
            store_prop(&mut arena, first, prop_offset(1), child);
            spend_creation_references(&[child]);
            read_as_unreachable(first, &[first, second]);
        }

        [first, second]
    });

    let mut scratch = open_arena();
    for component in components {
        let mut members = headers(component);
        assert_eq!(
            unsafe { commit(&mut members, &mut scratch) },
            Reclaimed::Freed
        );
        assert!(
            scratch.deferred_drops_are_empty(),
            "a component drains its own queue before it returns"
        );
    }

    assert_eq!(
        scratch.drop_segment_count(),
        1,
        "the second component's reservation counts the segment the first one \
         emptied instead of drawing a second"
    );
    scratch.reset();
}

#[test]
fn a_cell_a_destructor_created_is_nulled_at_the_member_s_own_free() {
    let _g = test_guard();
    let speaking = ClassBuilder::new("ReclamationWeakRecreatingNode")
        .prop("next", true)
        .prop("child", true)
        .destructor(resurrecting_the_weak_state as *const ())
        .build();
    let peer = node_class("ReclamationWeakPeer");

    let mut owned_arena = Arena::new();
    let arena = &raw mut owned_arena;
    AMBIENT_ARENA.store(arena as usize, Ordering::Relaxed);
    RECREATED_CELL.store(0, Ordering::Relaxed);

    // The arena is reached through the one raw pointer this case holds, which
    // is what keeps the destructor's own reborrow legal under Miri.
    let [first, second] = unsafe { ring(&mut *arena, [speaking, peer]) };
    unsafe { read_as_unreachable(first, &[first, second]) };

    let mut scratch = open_arena();
    let mut members = headers([first, second]);
    assert_eq!(
        unsafe { commit(&mut members, &mut scratch) },
        Reclaimed::Freed
    );

    let cell = RECREATED_CELL.swap(0, Ordering::Relaxed) as *mut LLWeakRef;
    assert!(!cell.is_null(), "the destructor took its weak reference");
    assert!(
        unsafe { ll_weakref_get(cell) }.is_null(),
        "step 3 ran before this cell existed, so what clears it is the \
         notification the member's own free delivers"
    );

    unsafe {
        assert!(ll_release(cell as *mut RcHeader));
        crate::object::ll_entity_die(cell as *mut RcHeader);
    }

    scratch.reset();
}

#[test]
fn the_queue_crosses_a_segment_boundary_on_the_room_it_reserved() {
    let _g = test_guard();
    let mut scratch = open_arena();

    // One record past a segment, which is the smallest reservation that draws
    // a second one — and the addresses are the case's own rather than
    // entities': what crosses the boundary is the chain, and a drain that
    // never drops anything is what lets the case read every record back.
    let children = crate::cycle::drops::SEGMENT_RECORDS + 1;
    assert!(scratch.reserve_drops(children));
    assert_eq!(
        scratch.drop_segment_count(),
        2,
        "the reservation draws until the room covers the bound"
    );

    for record in 0..children {
        assert!(
            scratch.push_drop(pretend_child(record)),
            "the reservation covers every push"
        );
    }

    let mut read = Vec::with_capacity(children);
    scratch.drain_drops(|child| read.push(child));
    assert_eq!(
        read,
        (0..children).map(pretend_child).collect::<Vec<_>>(),
        "the append order holds across the boundary: the full segment first, \
         then the one record above it"
    );
    assert!(scratch.deferred_drops_are_empty());
    assert_eq!(
        scratch.drop_segment_count(),
        2,
        "and the second segment is kept for the next component"
    );

    scratch.reset();
}

/// An address the queue can hold and the drain can hand back, standing for a
/// child without being one: this case reads the chain's order and never drops.
///
/// Word-aligned and never null, so nothing about it reads as an entity the
/// queue would be wrong to hold.
fn pretend_child(record: usize) -> *mut RcHeader {
    ((record + 1) * size_of::<usize>()) as *mut RcHeader
}
