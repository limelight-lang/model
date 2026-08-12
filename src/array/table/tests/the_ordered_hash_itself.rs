//! Insertion order is the enumeration order, so an overwrite keeps a
//! key's position while a delete and a reinsert put it at the end. A
//! removed key stays removed and the table goes on answering for
//! every other, and a removal of a key that was never there changes
//! nothing.

use super::*;

#[test]
fn an_empty_table_finds_nothing_and_does_not_allocate() {
    let _g = crate::memory::block_pool::test_guard();
    let m = t();
    assert!(m.is_empty());
    assert!(m.get(Key::Int(0)).is_none());
    assert!(!m.contains(Key::Int(1)));
    assert_eq!(m.used(), 0);
    let _ = ctx();
}

#[test]
fn insert_then_get_round_trips_integer_keys() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    for i in 0..500i64 {
        let (added, old) = m.insert(Key::Int(i), Value::int(i * 3)).unwrap();
        assert!(added && old.is_none(), "a fresh key is an addition");
    }

    assert_eq!(m.len(), 500);
    for i in 0..500i64 {
        assert_eq!(m.get(Key::Int(i)).unwrap().as_int(), i * 3);
    }

    assert!(m.get(Key::Int(500)).is_none());
    assert!(m.get(Key::Int(-1)).is_none());
}

#[test]
fn overwriting_a_key_keeps_its_position_and_returns_the_old_value() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    m.insert(Key::Int(1), Value::int(10));
    m.insert(Key::Int(2), Value::int(20));

    let (added, old) = m.insert(Key::Int(1), Value::int(11)).unwrap();
    assert!(!added, "an overwrite is not a new key");
    assert_eq!(old.unwrap().as_int(), 10);
    assert_eq!(m.len(), 2);

    let order: Vec<i64> = m.iter().map(|e| e.hash_or_key as i64).collect();
    assert_eq!(order, vec![1, 2], "an overwrite must not move the key");
}

#[test]
fn iteration_is_insertion_order_and_skips_holes() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    for i in 0..10i64 {
        m.insert(Key::Int(i), Value::int(i));
    }

    for i in [1i64, 4, 7] {
        assert!(m.remove(Key::Int(i)).is_some());
    }

    let order: Vec<i64> = m.iter().map(|e| e.hash_or_key as i64).collect();
    assert_eq!(order, vec![0, 2, 3, 5, 6, 8, 9]);
    assert_eq!(m.len(), 7);
}

#[test]
fn a_deleted_key_reinserts_at_the_end() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    for i in 0..3i64 {
        m.insert(Key::Int(i), Value::int(i));
    }

    let _ = m.remove(Key::Int(1));
    m.insert(Key::Int(1), Value::int(99));

    let order: Vec<i64> = m.iter().map(|e| e.hash_or_key as i64).collect();
    assert_eq!(order, vec![0, 2, 1], "PHP appends a re-inserted key");
    assert_eq!(m.get(Key::Int(1)).unwrap().as_int(), 99);
}

#[test]
fn removing_shortens_the_chain_rather_than_leaving_a_marker() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();

    // Three keys a multiple of the table's sixteen slots apart share
    // slot 0, a fresh table indexing an integer key by its value.
    // The stride is what builds a chain at all: a dense set puts
    // every key in a slot of its own, and a removal there repairs
    // nothing.
    let stride = 1024i64;
    for i in 0..3i64 {
        m.insert(Key::Int(i * stride), Value::int(i));
    }

    assert_eq!(
        chain_keys(&m, 0),
        vec![2 * stride, stride, 0],
        "insertion is at the head, so the newest key comes first"
    );

    assert_eq!(m.remove(Key::Int(stride)).unwrap().0.as_int(), 1);
    assert_eq!(
        chain_keys(&m, 0),
        vec![2 * stride, 0],
        "the removed entry left the chain instead of staying on it"
    );
    assert!(m.entry(1).is_hole(), "and its entry is a hole");
    assert!(m.get(Key::Int(stride)).is_none());
    for i in [0i64, 2] {
        assert_eq!(m.get(Key::Int(i * stride)).unwrap().as_int(), i);
    }

    // The same over a dense set, kept as a sweep and not as the
    // instrument: every key there is alone in its slot, so every
    // removal takes the head-of-chain arm and a tombstoning table
    // satisfies these assertions too.
    let mut dense = t();
    for i in 0..64i64 {
        dense.insert(Key::Int(i), Value::int(i));
    }

    for i in 0..64i64 {
        assert_eq!(dense.remove(Key::Int(i)).unwrap().0.as_int(), i);
        assert!(
            dense.get(Key::Int(i)).is_none(),
            "a removed key stays removed"
        );
    }

    assert_eq!(dense.len(), 0);
    for i in 0..64i64 {
        assert!(!dense.contains(Key::Int(i)));
    }
}

#[test]
fn removing_a_missing_key_reports_rather_than_disturbing_the_table() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    m.insert(Key::Int(1), Value::int(1));
    assert!(m.remove(Key::Int(2)).is_none());
    assert_eq!(m.len(), 1);
    assert_eq!(m.get(Key::Int(1)).unwrap().as_int(), 1);
}

#[test]
fn negative_and_extreme_integer_keys_round_trip() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    let keys = [0i64, -1, 1, i64::MIN, i64::MAX, -1024, 1024];
    for (n, k) in keys.iter().enumerate() {
        m.insert(Key::Int(*k), Value::int(n as i64));
    }

    for (n, k) in keys.iter().enumerate() {
        assert_eq!(m.get(Key::Int(*k)).unwrap().as_int(), n as i64);
    }

    assert_eq!(m.len(), keys.len());
}
