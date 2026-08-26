//! `canonical_key` turns the numeric strings PHP means as integers
//! into integer keys and leaves every other spelling a string key —
//! a leading zero, a plus sign, a leading space, a value past what
//! `i64` holds.

use super::*;

/// The three canonical spellings of the done criterion, each finding
/// what the integer key stored — one table, one lookup per pair.
#[test]
fn a_canonical_numeric_string_finds_what_the_integer_key_stored() {
    let _g = crate::memory::block_pool::test_guard();
    let a = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
    for (i, k) in [1i64, -1, i64::MAX, i64::MIN].into_iter().enumerate() {
        unsafe {
            crate::array::testing::insert(a, Key::Int(k), Value::int(i as i64));
        }
    }

    for (i, spelling) in [
        &b"1"[..],
        b"-1",
        b"9223372036854775807",
        b"-9223372036854775808",
    ]
    .into_iter()
    .enumerate()
    {
        let s = mk(spelling);
        let key = unsafe { canonical_key(s) };
        assert!(
            matches!(key, Key::Int(_)),
            "{:?} must canonicalise",
            std::str::from_utf8(spelling).unwrap()
        );
        unsafe {
            assert_eq!(
                crate::array::testing::get(a, key).unwrap().as_int(),
                i as i64,
                "{:?} missed the integer key's entry",
                std::str::from_utf8(spelling).unwrap()
            );
        }

        free(s);
    }

    unsafe {
        crate::array::entity::dispose_storage(a, category_of(a));
        crate::refcount::set_header_refcount(a as *mut crate::refcount::RcHeader, 0);
        ll_free(a as *mut u8);
    }
}

/// The five non-canonical spellings of the done criterion stay
/// string keys, plus the two cheap boundaries beside them.
#[test]
fn a_non_canonical_spelling_stays_a_string_key() {
    let _g = crate::memory::block_pool::test_guard();
    for spelling in [&b"011"[..], b"1.0", b" 1", b"-0", b"9223372036854775808"] {
        let s = mk(spelling);
        let key = unsafe { canonical_key(s) };
        assert!(
            matches!(key, Key::Str(p) if p == s),
            "{:?} must stay a string key",
            std::str::from_utf8(spelling).unwrap()
        );
        free(s);
    }

    for spelling in [&b""[..], b"+1", b"-"] {
        let s = mk(spelling);
        assert!(
            matches!(unsafe { canonical_key(s) }, Key::Str(_)),
            "{:?} must stay a string key",
            std::str::from_utf8(spelling).unwrap()
        );
        free(s);
    }
}
