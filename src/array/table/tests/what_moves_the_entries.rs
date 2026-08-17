//! Growth and compaction both move entries, so both are bracketed by
//! the version counter — odd while the move runs — which is how a
//! walker tells that its reading of `storage`, `nslots` and `used`
//! came from one arrangement. Order and every key survive either,
//! and no link inside the storage is a pointer into it, which is
//! what lets promotion copy the whole block flat.

use super::*;

/// Every operation that moves entries has to be visible to a walker
/// that is reading them, and the version is how: odd while the move
/// runs, changed afterwards. A walker validates its reading of
/// `storage`, `nslots` and `used` against two readings of this.
///
/// **What forces a counter rather than a double read of `storage`
/// has changed, and the counter stays.** The original argument was
/// compaction sliding entries inside one chunk, which no reading of
/// the pointer can see, and compaction allocates, so that
/// case is gone. What remains is the 2 → 3 migration, which changes
/// what the bytes *mean* at an address that may not move, and the
/// three words themselves: `storage` and `used` are published
/// separately, and pairing them is exactly what a validated reading
/// is for.
#[test]
fn moving_the_entries_shows_up_as_a_version_change() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    let at_rest = m.version();
    assert_eq!(at_rest % 2, 0, "a table nobody is rearranging reads even");

    // Enough inserts to cross several growths.
    for i in 0..200i64 {
        assert!(m.insert(Key::Int(i), Value::int(i)).is_some());
    }

    let after_growth = m.version();
    assert!(after_growth > at_rest, "growth moved entries silently");
    assert_eq!(after_growth % 2, 0, "the window was left open");

    let before_compaction = m.storage();
    assert!(m.compact().is_some(), "the compaction was refused");
    let after_compaction = m.version();
    assert!(
        after_compaction > after_growth,
        "compaction moved entries silently"
    );
    assert_ne!(
        m.storage(),
        before_compaction,
        "compaction is a move into a fresh chunk"
    );
    assert_eq!(after_compaction % 2, 0, "the window was left open");
}

/// Compacting a table that has no chunk allocates nothing.
///
/// A chunk for `cap == 0` holds no entry, and the state it leaves —
/// `storage` non-null with `mask` set — is what the next insert reads
/// as room for one: it wrote a 32-byte entry into sixteen granted
/// bytes, and published the count for the walker to stride
/// The insert below is half the test.
#[test]
fn compacting_a_table_with_no_chunk_allocates_nothing() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    assert_eq!(m.compact(), Some(0), "there was nothing to reclaim");
    assert!(
        m.storage().is_null(),
        "a compaction of nothing produced a chunk"
    );
    assert!(m.insert(Key::Int(1), Value::int(1)).is_some());
    assert_eq!(m.get(Key::Int(1)).unwrap().as_int(), 1);
    assert_eq!(m.used(), 1);
}

#[test]
fn growth_preserves_every_key_and_the_order() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    // Enough to cross several doublings from the initial capacity.
    for i in 0..5000i64 {
        assert!(m.insert(Key::Int(i), Value::int(i)).is_some());
    }

    assert_eq!(m.len(), 5000);
    let order: Vec<i64> = m.iter().map(|e| e.hash_or_key as i64).collect();
    assert_eq!(order, (0..5000).collect::<Vec<i64>>());
    for i in 0..5000i64 {
        assert_eq!(m.get(Key::Int(i)).unwrap().as_int(), i);
    }
}

#[test]
fn compaction_reclaims_holes_and_preserves_order() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    for i in 0..100i64 {
        m.insert(Key::Int(i), Value::int(i));
    }

    for i in (0..100i64).filter(|i| i % 2 == 0) {
        let _ = m.remove(Key::Int(i));
    }

    let before: Vec<i64> = m.iter().map(|e| e.hash_or_key as i64).collect();
    assert_eq!(m.used(), 100, "holes still occupy their slots");

    let moved = m.compact().expect("the compaction was refused");
    assert!(moved > 0);
    assert_eq!(m.used(), 50, "compaction reclaimed the holes");

    let after: Vec<i64> = m.iter().map(|e| e.hash_or_key as i64).collect();
    assert_eq!(before, after, "compaction preserves insertion order");
    for i in (0..100i64).filter(|i| i % 2 == 1) {
        assert_eq!(m.get(Key::Int(i)).unwrap().as_int(), i);
    }
}

#[test]
fn a_string_key_survives_growth_and_compaction() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    let keys: Vec<*mut LLString> = (0..300).map(|i| mk(format!("k{i}").as_bytes())).collect();
    for (i, s) in keys.iter().enumerate() {
        m.insert(Key::Str(*s), Value::int(i as i64));
    }

    for (i, s) in keys.iter().enumerate() {
        if i % 3 == 0 {
            // Table tests leave key ownership unmodelled; the pair is
            // waived here and measured in `array::entity`'s tests.
            let _ = m.remove(Key::Str(*s));
        }
    }

    m.compact();
    for (i, s) in keys.iter().enumerate() {
        if i % 3 == 0 {
            assert!(m.get(Key::Str(*s)).is_none());
        } else {
            assert_eq!(m.get(Key::Str(*s)).unwrap().as_int(), i as i64);
        }
    }
}

/// Promotion copies the storage as one contiguous block, so nothing
/// inside it may point into it. Every link is an index; this pins it.
#[test]
fn the_storage_holds_no_pointer_into_itself() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    for i in 0..500i64 {
        m.insert(Key::Int(i), Value::int(i));
    }

    let base = m.storage() as usize;
    let bytes = super::storage_bytes(m.nslots(), m.cap).unwrap();
    for k in 0..m.used() {
        let e = m.entry(k);
        for word in [e.hash_or_key, e.key_word as u64, e.value().as_int() as u64] {
            let w = word as usize;
            assert!(
                w < base || w >= base + bytes,
                "entry {k} holds a word pointing into the storage"
            );
        }
    }
}
