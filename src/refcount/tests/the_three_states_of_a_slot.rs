//! What a walker of a slot's first eight bytes reads, and in which order
//! the two halves are asked ([`crate::refcount::slot_state`]).

use super::*;

/// A header on the stack at `count`, which is all these cases need:
/// `slot_state` reads the first eight bytes and nothing else, and the
/// crate builds stack headers wherever a case is about the word rather
/// than about the slot under it.
fn header(count: u32) -> RcHeader {
    let mut header = RcHeader::new(MemoryCategory::GcHeap, EntityKind::Object.to_flags());
    unsafe { set_header_refcount(&raw mut header, count) };
    header
}

/// A count above zero answers before the flags are read at all.
///
/// This is what keeps a live slot at one load, and it is the order rather
/// than the answer: a header
/// carrying a stale mark under a live count reads live, and a walk that
/// asked the flags first would skip an entity.
#[test]
fn a_live_slot_answers_from_the_count_alone() {
    let mut live = header(1);
    assert_eq!(unsafe { slot_state(&raw const live) }, SlotState::Live);

    unsafe { update_header_flags(&raw mut live, |flags| flags | DEAD_IN_PLACE) };
    assert_eq!(
        unsafe { slot_state(&raw const live) },
        SlotState::Live,
        "the count decides first, so a mark under a live count changes nothing"
    );
}

/// A zero count and the mark is the third state, and a zero count without
/// it is what the allocator may hand out.
#[test]
fn a_zero_count_is_read_apart_by_the_mark() {
    let mut slot = header(0);
    assert_eq!(
        unsafe { slot_state(&raw const slot) },
        SlotState::Free,
        "a dead slot with no mark is the allocator's"
    );

    unsafe { mark_dead_in_place(&raw mut slot) };
    assert_eq!(
        unsafe { slot_state(&raw const slot) },
        SlotState::DeadInPlace,
        "and the mark takes it out of both the live set and the free one"
    );

    unsafe { clear_dead_in_place(&raw mut slot) };
    assert_eq!(
        unsafe { slot_state(&raw const slot) },
        SlotState::Free,
        "the clear is what gives it back"
    );
}

/// A byte standing at +6 comes through the mark and the clear unchanged.
///
/// **This pins the outcome, not the width**: a store made before the
/// update survives a whole-word writer too, which loads the byte and puts
/// it back. What holds the width is
/// `the_widths_the_mutator_uses::the_mutators_header_helpers_are_narrow`,
/// over the two accessors `update_header_flags` calls.
#[test]
fn the_mark_leaves_the_collectors_byte_where_it_stands() {
    let mut slot = header(0);
    let collector_byte = unsafe { (&raw mut slot as *mut u8).add(6) };
    unsafe { collector_byte.write(0xA5) };

    unsafe { mark_dead_in_place(&raw mut slot) };
    assert_eq!(unsafe { collector_byte.read() }, 0xA5);

    unsafe { clear_dead_in_place(&raw mut slot) };
    assert_eq!(unsafe { collector_byte.read() }, 0xA5);
    assert_eq!(
        unsafe { mutator_flags(&raw const slot) } & DEAD_IN_PLACE,
        0,
        "and the mutator's own half came back to where it started"
    );
}
