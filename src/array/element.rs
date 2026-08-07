//! The generic element layer over the table (`PLAN.md`, stage S2).
//!
//! The five element operations — read, store, append, unset, take a
//! reference — land here with S2.5; what lives here already is the key
//! constructor, because every one of them starts by settling what the
//! key *is*. Canonicalisation sits in this layer rather than inside
//! `Table` on purpose: `Map` is the table's second customer and keys a
//! map exactly, so a table that canonicalised would be unusable there.

use crate::array::table::Key;
use crate::string::LLString;

/// The key a PHP subscript denotes: an integer for the canonical
/// decimal spelling of an `i64`, the string itself for everything else.
///
/// PHP's rule, and each clause is a pinned test: the spelling is an
/// optional `-` followed by digits, with no leading zero (`"0"` is the
/// one zero), no `"-0"`, no sign `+`, no spaces, no fraction — `$a["1"]`
/// and `$a[1]` are one key while `$a["011"]`, `$a[" 1"]` and
/// `$a["1.0"]` are string keys. A spelling past the `i64` range stays a
/// string key too.
///
/// # Safety
/// `s` is a live string entity. The returned `Key::Str` borrows no
/// reference: key ownership starts where the key is stored
/// (`Table::insert`'s contract), not here.
pub unsafe fn canonical_key(s: *mut LLString) -> Key {
    match canonical_int(unsafe { LLString::bytes(s) }) {
        Some(n) => Key::Int(n),
        None => Key::Str(s),
    }
}

/// The integer whose canonical decimal spelling `bytes` is, or `None`.
fn canonical_int(bytes: &[u8]) -> Option<i64> {
    let (digits, negative) = match bytes {
        [b'-', rest @ ..] => (rest, true),
        _ => (bytes, false),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    // No leading zero — "0" is the one zero — and no "-0".
    if digits[0] == b'0' && (negative || digits.len() > 1) {
        return None;
    }
    // `str::parse` refuses overflow, where a hand-rolled accumulator
    // wraps through it: `i64::MAX + 1` must stay a string key. The
    // bytes are ASCII by the digit test above, so the UTF-8 view cannot
    // fail.
    std::str::from_utf8(bytes).ok()?.parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::array::table::Key;
    use crate::memory::stdapi::ll_free;
    use crate::refcount::MemoryCategory;
    use crate::string::{LLString, ll_string_new};
    use crate::value::Value;

    fn mk(bytes: &[u8]) -> *mut LLString {
        let s = unsafe { ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, bytes) };
        assert!(!s.is_null());
        s
    }

    fn free(s: *mut LLString) {
        unsafe {
            (*s).rc.refcount = 0;
            ll_free(s as *mut u8);
        }
    }

    /// The three canonical spellings of the done criterion, each finding
    /// what the integer key stored — one table, one lookup per pair.
    #[test]
    fn a_canonical_numeric_string_finds_what_the_integer_key_stored() {
        let _g = crate::memory::block_pool::test_guard();
        let a = unsafe { crate::array::entity::ll_array_new(MemoryCategory::GcHeap) };
        let owner = a as *const crate::refcount::RcHeader;
        for (i, k) in [1i64, -1, i64::MAX, i64::MIN].into_iter().enumerate() {
            unsafe {
                (*a).table.insert(owner, Key::Int(k), Value::int(i as i64));
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
                    (*a).table.get(key).unwrap().as_int(),
                    i as i64,
                    "{:?} missed the integer key's entry",
                    std::str::from_utf8(spelling).unwrap()
                );
            }
            free(s);
        }
        unsafe {
            (*a).table.dispose(owner);
            (*a).rc.refcount = 0;
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
}
