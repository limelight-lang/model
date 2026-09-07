//! The obligation the deferred-drop queue puts on a class that keeps cells
//! outside its own body: its sever hands over exactly the occupants its walk
//! yields.
//!
//! The teardown counts what it will queue by walking the component's cells, and
//! for such a class the group's own walk is that count — there is nothing else
//! to read. A group that severs a cell its walk does not yield hands over a
//! child the room was never taken for; one that severs fewer leaves a counted
//! reference standing in storage about to be freed, and the member it names
//! reads as externally referenced from then on. Both are past the point where a
//! refusal is available, every cell before them being null already, so what the
//! teardown does with them is report them where they happened.

use super::*;
use crate::cells::{Cell, OutsideCarry, OutsideCells};

/// The occupant a group of this file hands over, which the case holds for it.
///
/// A static rather than a cell of the instance: a group runs after the body
/// stride, which has already emptied every cell there is (`cells::sever_cells`,
/// the object arm), so an outside cell has to stand somewhere the stride does
/// not reach — which is what the group is for.
static OUTSIDE_OCCUPANT: AtomicUsize = AtomicUsize::new(0);

/// A group whose walk yields nothing and whose sever hands one occupant over.
static SEVERS_MORE_THAN_IT_WALKS: OutsideCells = OutsideCells {
    walk_plain: yields_nothing,
    sever: hands_one_over,
    free: frees_nothing,
    carry: carries_nothing,
};

/// A group whose walk yields one occupant and whose sever hands none over.
static WALKS_MORE_THAN_IT_SEVERS: OutsideCells = OutsideCells {
    walk_plain: yields_the_occupant,
    sever: hands_nothing_over,
    free: frees_nothing,
    carry: carries_nothing,
};

unsafe fn yields_nothing(_: *mut u8, _: *const Class, _: &mut dyn FnMut(Cell)) {}

/// Yields the occupant as a Box cell of the instance's second property, which
/// is a cell the body stride has already yielded — the address is what a walk
/// answers with, and this file's cases turn on the count alone.
unsafe fn yields_the_occupant(base: *mut u8, _: *const Class, visit: &mut dyn FnMut(Cell)) {
    let child = OUTSIDE_OCCUPANT.load(Ordering::Relaxed) as *mut RcHeader;
    if child.is_null() {
        return;
    }

    visit(Cell {
        addr: unsafe { Object::prop_at(base as *mut Object, prop_offset(1)) } as usize,
        child,
        shape: crate::cells::CellShape::Box,
    });
}

unsafe fn hands_one_over(_: *mut RcHeader, displaced: &mut dyn FnMut(*mut RcHeader)) {
    let child = OUTSIDE_OCCUPANT.load(Ordering::Relaxed) as *mut RcHeader;
    if !child.is_null() {
        displaced(child);
    }
}

unsafe fn hands_nothing_over(_: *mut RcHeader, _: &mut dyn FnMut(*mut RcHeader)) {}

unsafe fn frees_nothing(_: *mut RcHeader) {}

unsafe fn carries_nothing(_: *mut Arena, _: *mut RcHeader) -> OutsideCarry {
    OutsideCarry::Nothing
}

/// A two-member ring whose first member's class carries `group`, holding one
/// external child that the group's own members read out of
/// [`OUTSIDE_OCCUPANT`].
///
/// # Safety
/// `arena` is this thread's.
unsafe fn ring_under(
    arena: &mut Arena,
    name: &str,
    group: &'static OutsideCells,
) -> [*mut Object; 2] {
    let hooked = ClassBuilder::new(name)
        .prop("next", true)
        .prop("child", true)
        .outside_cells(group)
        .build();
    let peer = node_class("ReclamationHookedPeer");
    let plain = ClassBuilder::new("ReclamationHookedChild").build();

    let first = unsafe { object(arena, hooked) };
    let second = unsafe { object(arena, peer) };
    let child = unsafe { object(arena, plain) };
    unsafe {
        store_prop(arena, first, prop_offset(0), second);
        store_prop(arena, second, prop_offset(0), first);
        store_prop(arena, first, prop_offset(1), child);
        spend_creation_references(&[first, second, child]);
        read_as_unreachable(first, &[first, second]);
    }

    OUTSIDE_OCCUPANT.store(child as usize, Ordering::Relaxed);
    [first, second]
}

#[test]
#[should_panic = "the sever displaces the children the walk ahead of it counted"]
fn a_group_that_severs_more_than_it_walks_is_refused_at_the_queue() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let ring = unsafe {
        ring_under(
            &mut arena,
            "ReclamationOverSevering",
            &SEVERS_MORE_THAN_IT_WALKS,
        )
    };

    let mut scratch = open_arena();
    let mut members = headers(ring);
    let _ = unsafe { commit(&mut members, &mut scratch) };
}

#[test]
#[should_panic = "the sever displaces every child the walk ahead of it counted"]
fn a_group_that_walks_more_than_it_severs_is_refused_at_the_close() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let ring = unsafe {
        ring_under(
            &mut arena,
            "ReclamationUnderSevering",
            &WALKS_MORE_THAN_IT_SEVERS,
        )
    };

    let mut scratch = open_arena();
    let mut members = headers(ring);
    let _ = unsafe { commit(&mut members, &mut scratch) };
}

/// The obligation over the kind the crate has rather than the one a test
/// hooks: an array's sever hands over exactly what the tracing walk yields,
/// which is one child per element and one more per string key.
///
/// The teardown counts with the walk and writes with the sever, so a
/// disagreement between the two is an abort in production for every component
/// holding an array. Nothing else in the crate reads the two counts against
/// each other: the walk has its own cases and so does the sever.
#[test]
fn the_walk_and_the_sever_agree_over_an_array() {
    use crate::array::table::Key;
    use crate::refcount::{EntityKind, ll_retain};
    use crate::value::{Tag, Value};

    let _g = test_guard();
    let mut arena = Arena::new();
    let plain = ClassBuilder::new("ReclamationArrayElement").build();
    let array = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
    assert!(!array.is_null(), "the fixture's array");

    // Two counted elements and one uncounted, which is what makes the count a
    // count rather than a length. The retains are the store barrier's work,
    // done by hand because the table below is entered raw.
    let elements = [unsafe { object(&mut arena, plain) }, unsafe {
        object(&mut arena, plain)
    }];
    for (index, element) in elements.iter().enumerate() {
        unsafe { ll_retain(*element as *mut RcHeader) };
        let value = Value::entity(Tag::Object, *element as *mut RcHeader);
        assert!(
            unsafe { crate::array::testing::insert(array, Key::Int(index as i64), value) }
                .is_some(),
            "the fixture's insert"
        );
    }

    assert!(
        unsafe { crate::array::testing::insert(array, Key::Int(2), Value::int(7)) }.is_some(),
        "the fixture's uncounted element"
    );

    let kind = EntityKind::Array as u32;
    let entity = array as *mut RcHeader;
    let mut walked = 0;
    unsafe { trace_cells::<crate::cells::PlainCells>(entity, kind, |_| walked += 1) };
    assert_eq!(
        walked,
        elements.len(),
        "the walk yields the counted elements"
    );

    let mut severed = 0;
    unsafe { sever_cells(entity, kind, |_| severed += 1) };
    assert_eq!(
        severed, walked,
        "the sever hands over what the walk counted, which is what the \
         teardown reserves against"
    );

    // The severed elements are the case's to drop: the sever hands a child over
    // undropped, so each element carries the reference the entry held beside
    // the one this case created.
    unsafe {
        for element in elements {
            assert!(
                !ll_release(element as *mut RcHeader),
                "the reference the severed entry held"
            );
            assert!(ll_release(element as *mut RcHeader));
            crate::object::ll_object_die(element);
        }

        assert!(ll_release(entity), "the array's own creation reference");
        crate::object::ll_entity_die(entity);
    }
}
