//! The key is the position, so a vector stores no key and its length is
//! both the element count and the next append key. Growth doubles and
//! moves the elements, which is why it runs inside the head's window.
//!
//! Every test here works through an array rather than a bare `Vector`,
//! and not for the reason the tests in `the_entity_over_a_vector` do:
//! the head is the entity's (`array::head`), so a `Vector` on its own
//! has no version, no chunk and no count, and there is nothing about it
//! to measure without the array in front of it.

use super::*;

/// Give an array whose elements are integers back: the last reference
/// goes, then the teardown frees the storage and the slot. A test holding
/// entities releases them itself instead — the drain would take them a
/// second time.
fn discard(a: *mut LLArray) {
    unsafe {
        assert!(
            ll_release(a as *mut RcHeader),
            "the test was the only holder"
        );
        crate::object::ll_entity_die(a as *mut RcHeader);
    }
}

#[test]
fn a_fresh_vector_is_strategy_two_and_holds_nothing() {
    let _g = crate::memory::block_pool::test_guard();
    let a = vector_array(MemoryCategory::GcHeap);
    let (v, head) = unsafe { as_vector(a) };
    assert_eq!(head.tag(), StorageTag::Vector);
    assert_eq!(head.used(), 0);
    assert_eq!(
        Vector::append_key(head),
        Some(0),
        "the first append takes key 0"
    );
    assert!(v.get(head, 0).is_none());
    discard(a);
}

#[test]
fn pushing_keeps_order_and_the_length_is_the_next_key() {
    let _g = crate::memory::block_pool::test_guard();
    let a = vector_array(MemoryCategory::GcHeap);
    let (v, head) = unsafe { as_vector_mut(a) };
    for i in 0..5i64 {
        assert_eq!(Vector::append_key(head), Some(i));
        assert!(v.push(head, MemoryCategory::GcHeap, Value::int(i * 10)));
    }

    assert_eq!(head.used(), 5);
    for i in 0..5usize {
        assert_eq!(v.get(head, i).unwrap().as_int(), i as i64 * 10);
    }

    assert!(
        v.get(head, 5).is_none(),
        "past the end is absent, not a hole"
    );
    discard(a);
}

/// Growth moves every element into a fresh chunk, so it opens the
/// head's window: a walker that strode the old chunk against the new
/// count would read past its end. The version is the instrument —
/// it moves across the growth and the elements survive it in order.
#[test]
fn growth_moves_the_elements_inside_the_window() {
    let _g = crate::memory::block_pool::test_guard();
    let a = vector_array(MemoryCategory::GcHeap);
    let (v, head) = unsafe { as_vector_mut(a) };
    for i in 0..FIRST_CAP {
        assert!(v.push(head, MemoryCategory::GcHeap, Value::int(i as i64)));
    }

    let before = head.version();
    let chunk = head.storage();
    assert!(v.push(head, MemoryCategory::GcHeap, Value::int(FIRST_CAP as i64)));
    assert_ne!(head.storage(), chunk, "the ninth element needs a new chunk");
    assert_eq!(
        head.version(),
        before + 2,
        "one window opened and closed around the move"
    );
    assert_eq!(head.version() % 2, 0, "and it is closed");

    for i in 0..=FIRST_CAP {
        assert_eq!(
            v.get(head, i).unwrap().as_int(),
            i as i64,
            "order survived the move"
        );
    }

    discard(a);
}

/// Releasing the storage publishes the same words growth does, so it
/// takes the same window: a collector whose snapshot holds the slot
/// of a dying array must not be handed the chunk with the counts of
/// the empty state. The mixture this
/// representation could offer is narrower than the ordered hash's —
/// it has no index region to stride — and the bracket is what says
/// so rather than the order of two stores.
#[test]
fn the_release_of_the_storage_is_one_window() {
    let _g = crate::memory::block_pool::test_guard();
    let a = vector_array(MemoryCategory::GcHeap);
    let (v, head) = unsafe { as_vector_mut(a) };
    for i in 0..4i64 {
        assert!(v.push(head, MemoryCategory::GcHeap, Value::int(i)));
    }

    let before = head.version();
    v.dispose(head, MemoryCategory::GcHeap);
    assert_eq!(
        head.version(),
        before + 2,
        "one window opened and closed around the release"
    );
    assert!(head.storage().is_null() && head.used() == 0);
    discard(a);
}

/// `set` overwrites a published element and hands the displaced value
/// back for the caller to release; past the end it refuses, because
/// what a key outside the range means is a migration and that is not
/// this type's answer to give.
#[test]
fn set_hands_the_displaced_value_back_and_refuses_past_the_end() {
    let _g = crate::memory::block_pool::test_guard();
    let a = vector_array(MemoryCategory::GcHeap);
    let (v, head) = unsafe { as_vector_mut(a) };
    assert!(v.push(head, MemoryCategory::GcHeap, Value::int(1)));
    assert_eq!(v.set(head, 0, Value::int(2)).unwrap().as_int(), 1);
    assert_eq!(v.get(head, 0).unwrap().as_int(), 2);
    assert!(
        v.set(head, 1, Value::int(3)).is_none(),
        "past the end refuses"
    );
    assert_eq!(head.used(), 1, "and the refusal appended nothing");
    discard(a);
}
