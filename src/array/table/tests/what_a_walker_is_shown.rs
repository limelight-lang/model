//! Enumeration covers the dense prefix and skips holes, and the hole
//! marker lives in the key word rather than in the element: a store
//! barrier writes all sixteen bytes of a `Value`, so a marker inside
//! one would be erased and the tracer would then walk a dead
//! element. A string key is enumerated beside the elements; whether
//! it is counted is `array::entity`'s to measure.

use super::*;

// ---- what the memory manager is owed -----------------------------

#[test]
fn enumeration_is_complete_over_the_dense_prefix_and_skips_holes() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    for i in 0..100i64 {
        m.insert(Key::Int(i), Value::int(i));
    }

    for i in (0..100i64).filter(|i| i % 3 == 0) {
        let _ = m.remove(Key::Int(i));
    }

    let mut seen = Vec::new();
    m.for_each_value(|v| seen.push(v.as_int()));
    let expected: Vec<i64> = (0..100).filter(|i| i % 3 != 0).collect();
    assert_eq!(seen, expected, "every live element exactly once, in order");
    assert_eq!(seen.len(), m.len());
}

/// The reason the hole marker lives in `key`: a store barrier writes
/// all sixteen bytes of a `Value`, so a marker inside it would be
/// erased and the tracer would then walk a dead element.
#[test]
fn a_value_store_over_a_hole_does_not_resurrect_it() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    for i in 0..8i64 {
        m.insert(Key::Int(i), Value::int(i));
    }

    let _ = m.remove(Key::Int(3));
    // The store the table really performs, and the link it has to
    // carry through: the element field is private, so a whole `Value`
    // written into the slot by hand does not compile.
    let link_before = m.entry(3).link();
    unsafe { Entry::store_element(m.entries().add(3), Value::int(0xDEAD)) };
    assert!(
        m.entry(3).is_hole(),
        "an element store cleared the hole marker"
    );
    assert_eq!(
        m.entry(3).link(),
        link_before,
        "an element store moved the chain link"
    );
    let mut seen = Vec::new();
    m.for_each_value(|v| seen.push(v.as_int()));
    assert!(
        !seen.contains(&0xDEAD),
        "the hole survived a full value write"
    );
    assert_eq!(seen.len(), 7);
}

#[test]
fn string_keys_are_enumerated_as_counted_children() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    let a = mk(b"alpha");
    let b = mk(b"beta");
    m.insert(Key::Str(a), Value::int(1));
    m.insert(Key::Int(9), Value::int(2));
    m.insert(Key::Str(b), Value::int(3));
    // Ownership unmodelled here too; `array::entity` measures it.
    let _ = m.remove(Key::Str(a));

    let mut keys = Vec::new();
    m.for_each_string_key(|s| keys.push(s));
    assert_eq!(
        keys,
        vec![b],
        "an integer key and a hole are not string children"
    );
}
