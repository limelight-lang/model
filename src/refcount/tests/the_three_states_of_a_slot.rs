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
    let live = &raw mut live;
    assert_eq!(unsafe { slot_state(live) }, SlotState::Live);

    unsafe { update_header_flags(live, |flags| flags | DEAD_IN_PLACE) };
    assert_eq!(
        unsafe { slot_state(live) },
        SlotState::Live,
        "the count decides first, so a mark under a live count changes nothing"
    );
}

/// A zero count and the mark is the third state, and a zero count without it
/// is a slot no free holds: commissioned and never occupied, or handed back
/// by whoever took it.
#[test]
fn a_zero_count_is_read_apart_by_the_mark() {
    let mut slot = header(0);
    let slot = &raw mut slot;
    assert_eq!(
        unsafe { slot_state(slot) },
        SlotState::Free,
        "a dead slot no free holds is the allocator's"
    );

    assert!(
        unsafe { take_slot_for_free(slot) }.is_some(),
        "the first free takes the slot"
    );
    assert_eq!(
        unsafe { slot_state(slot) },
        SlotState::DeadInPlace,
        "and the take is what separates it from a slot no free holds"
    );

    assert!(
        unsafe { take_slot_for_free(slot) }.is_none(),
        "a second free of one slot is refused, the take finding its own bit up"
    );

    unsafe { clear_dead_in_place(slot) };
    assert_eq!(
        unsafe { slot_state(slot) },
        SlotState::Free,
        "the clear is what hands it back"
    );
    assert!(
        unsafe { take_slot_for_free(slot) }.is_some(),
        "and a free after the hand-back is taken like a first one"
    );
}

/// The take hands back the flags as they stood before it, which is the load
/// the candidate arm reads instead of making one of its own
/// (`crate::memory::stdapi::ll_free`).
#[test]
fn the_take_hands_back_the_flags_it_tested() {
    let mut slot = header(0);
    let slot = &raw mut slot;
    unsafe { update_header_flags(slot, |flags| flags | CANDIDATE_BIT) };

    let flags = unsafe { take_slot_for_free(slot) }.expect("the first free takes it");
    assert!(
        is_registered_candidate(flags),
        "the candidate bit comes back as it stood"
    );
    assert_eq!(
        flags & DEAD_IN_PLACE,
        0,
        "and the bit the take itself set is not in what it hands back"
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
    let slot = &raw mut slot;
    let collector_byte = unsafe { (slot as *mut u8).add(6) };
    unsafe { collector_byte.write(0xA5) };

    assert!(unsafe { take_slot_for_free(slot) }.is_some());
    assert_eq!(unsafe { collector_byte.read() }, 0xA5);

    unsafe { clear_dead_in_place(slot) };
    assert_eq!(unsafe { collector_byte.read() }, 0xA5);
    assert_eq!(
        unsafe { mutator_flags(slot) } & DEAD_IN_PLACE,
        0,
        "and the mutator's own half came back to where it started"
    );
}
