//! PHP's rule in its three table-side clauses: a removal never
//! rewinds the cursor, an explicit key moves it past itself, and a
//! fresh table appends at 0. `i64::MAX` is the one key with no
//! successor, so the next append is refused rather than wrapped.

use super::*;

/// PHP's append rule, its three table-side clauses: removal never
/// rewinds the cursor, an explicit key moves it past itself, and a
/// fresh table appends at 0. The PHP 8.3 arm — a negative key moves
/// the cursor too, `$a[-5] = 1; $a[] = 2;` appending at −4 — is an
/// assumption, Edmond's to overturn.
#[test]
fn the_append_cursor_never_rewinds_and_an_explicit_key_moves_it() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    assert_eq!(m.append_key(), Some(0), "a fresh table appends at 0");
    for i in 0..3i64 {
        m.insert(Key::Int(i), Value::int(i));
    }

    let _ = m.remove(Key::Int(1));
    assert_eq!(m.append_key(), Some(3), "removal rewinds nothing");

    m.insert(Key::Int(9), Value::int(9));
    assert_eq!(
        m.append_key(),
        Some(10),
        "an explicit key moves the cursor past itself"
    );

    let mut n = t();
    n.insert(Key::Int(-5), Value::int(1));
    assert_eq!(
        n.append_key(),
        Some(-4),
        "PHP 8.3: a negative key moves the cursor"
    );
}

/// `i64::MAX` is the one integer key with no successor, so the next
/// append is refused rather than wrapped — the same posture as
/// `storage_bytes`' checked arithmetic.
#[test]
fn append_after_the_maximum_key_is_refused_rather_than_wrapping() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    m.insert(Key::Int(i64::MAX), Value::int(1));
    assert_eq!(m.append_key(), None, "i64::MAX + 1 must refuse, not wrap");
    m.insert(Key::Int(5), Value::int(2));
    assert_eq!(m.append_key(), None, "no later key un-exhausts the cursor");
}
