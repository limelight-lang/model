//! The 2 → 3 transition: the vector's elements become the hash's entries
//! under the keys their positions were, and the order they were appended
//! in is the order the hash iterates.
//!
//! What the elements' order rests on is `Table::insert` being called at
//! `0..used` in that order, and what the append cursor rests on is the
//! same call advancing it past each integer key. Both are properties of
//! the loop rather than of a field copied across, which is why a test that
//! reads the entries in order is the whole proof.

use super::*;

use std::sync::atomic::Ordering;

use crate::array::entity::{migrate_to_hash, new_vector_array};
use crate::array::head::StorageTag;
use crate::array::testing;
use crate::memory::block_pool::test_guard;

/// A vector array holding `0, 10, 20, …`, at count one.
fn vector_of(n: i64) -> *mut LLArray {
    let a = unsafe { new_vector_array(MemoryCategory::GcHeap) };
    assert!(!a.is_null(), "allocation refused in a test");
    for i in 0..n {
        assert!(
            unsafe { testing::push(a, Value::int(i * 10)) },
            "the vector refused an append"
        );
    }

    a
}

/// The head as a reference, which is how a walker reads it and the only
/// way to ask the tag and the counts of an array whose representation
/// this test does not want to name.
fn head_of(a: *mut LLArray) -> &'static crate::array::head::StorageHead {
    unsafe { &*crate::array::entity::storage_head(a) }
}

fn discard(a: *mut LLArray) {
    unsafe {
        assert!(
            crate::refcount::ll_release(a as *mut RcHeader),
            "the test was the only holder"
        );
        crate::object::ll_entity_die(a as *mut RcHeader);
    }
}

#[test]
fn every_element_arrives_under_the_key_its_position_was() {
    let _g = test_guard();
    let a = vector_of(5);
    assert!(unsafe { migrate_to_hash(a, MemoryCategory::GcHeap) });

    assert_eq!(
        head_of(a).tag(),
        StorageTag::Hash,
        "the tag names the representation the bytes now are"
    );
    assert_eq!(unsafe { testing::used(a) }, 5);
    for i in 0..5i64 {
        assert_eq!(
            unsafe { testing::get(a, Key::Int(i)) }.unwrap().as_int(),
            i * 10,
            "position {i} became key {i}"
        );
    }

    discard(a);
}

/// The hash iterates its entries in insertion order and reads no index to
/// do it, so this is the same question `foreach` asks.
#[test]
fn the_entries_are_in_the_order_the_vector_held_them() {
    let _g = test_guard();
    let a = vector_of(4);
    assert!(unsafe { migrate_to_hash(a, MemoryCategory::GcHeap) });

    let order: Vec<(i64, i64)> = unsafe { testing::iter(a) }
        .map(|e| (e.hash_or_key as i64, e.value().as_int()))
        .collect();
    assert_eq!(order, vec![(0, 0), (1, 10), (2, 20), (3, 30)]);

    discard(a);
}

/// The cursor the migrated array appends at is the length the vector
/// would have appended at, which the insert loop leaves behind without
/// copying a field: `next_free` ends one past the highest integer key.
#[test]
fn the_append_cursor_is_the_vectors_length() {
    let _g = test_guard();
    let a = vector_of(3);
    assert_eq!(
        crate::array::vector::Vector::append_key(head_of(a)),
        Some(3),
        "the vector would have appended at 3"
    );

    assert!(unsafe { migrate_to_hash(a, MemoryCategory::GcHeap) });
    assert_eq!(
        unsafe { testing::table(a) }.append_key(),
        Some(3),
        "and so does the hash"
    );

    discard(a);
}

/// An empty vector has nothing to insert, so the cursor stays at "no
/// integer key yet" — which answers 0, the same key the empty vector
/// would have taken.
#[test]
fn an_empty_vector_becomes_an_empty_hash_that_appends_at_zero() {
    let _g = test_guard();
    let a = vector_of(0);
    assert!(unsafe { migrate_to_hash(a, MemoryCategory::GcHeap) });

    let table = unsafe { testing::table(a) };
    assert!(table.is_empty());
    assert_eq!(table.append_key(), Some(0));
    assert_eq!(unsafe { testing::used(a) }, 0);

    discard(a);
}

/// The table's allocation is the migration's only refusal, and it leaves
/// the array a vector holding everything it held.
///
/// The vector is built before the switch goes on, so the only allocation
/// inside the refusal window is the table's chunk — which is what makes
/// this a test of the migration's refusal rather than of the append's.
#[test]
fn a_refused_table_leaves_the_vector_serving() {
    let _g = test_guard();
    let a = vector_of(3);

    crate::memory::buffer_arena::FORCE_REFUSE_LONGLIVED.store(true, Ordering::Relaxed);
    let migrated = unsafe { migrate_to_hash(a, MemoryCategory::GcHeap) };
    crate::memory::buffer_arena::FORCE_REFUSE_LONGLIVED.store(false, Ordering::Relaxed);

    assert!(!migrated, "the table's chunk was refused");
    let head = head_of(a);
    assert_eq!(
        head.tag(),
        StorageTag::Vector,
        "the array is still a vector"
    );
    assert_eq!(head.used(), 3);
    let (vector, head) = unsafe { crate::array::entity::as_vector(a) };
    for i in 0..3usize {
        assert_eq!(
            vector.get(head, i).unwrap().as_int(),
            i as i64 * 10,
            "element {i} survived the refusal"
        );
    }

    discard(a);
}
