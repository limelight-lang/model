//! A string key is matched by content rather than by address, and
//! through the layout-agnostic accessor, so a key past what the heap
//! packs in one slot is found too: the inline accessor would build a
//! slice over the entity and compare the payload pointer instead of
//! the bytes. `7` and `"7"` are two keys, told apart by the entry's
//! key kind rather than by where they land.

use super::*;

#[test]
fn string_keys_round_trip_and_compare_by_content_not_by_pointer() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();

    let names: Vec<*mut LLString> = (0..200)
        .map(|i| mk(format!("key-{i}").as_bytes()))
        .collect();
    for (i, s) in names.iter().enumerate() {
        let (added, _) = m.insert(Key::Str(*s), Value::int(i as i64)).unwrap();
        assert!(added);
    }

    assert_eq!(m.len(), 200);

    // A *different* entity with the same bytes must find the entry:
    // the table compares content, since only interned names have
    // pointer identity.
    for i in 0..200usize {
        let other = mk(format!("key-{i}").as_bytes());
        assert_eq!(
            m.get(Key::Str(other)).unwrap().as_int(),
            i as i64,
            "a string key is matched by content"
        );
    }

    assert!(m.get(Key::Str(mk(b"absent"))).is_none());
}

/// The same, with a key past what the heap packs in one slot. Such a
/// key is out of line, so every comparison has to reach its bytes
/// through the layout-agnostic accessor: the inline one would build a
/// 16 KiB slice over a 32-byte entity, comparing the payload pointer
/// and whatever follows it, and a key that is present would read as
/// absent.
#[test]
fn an_oversize_string_key_is_matched_by_content() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    let content = vec![b'k'; crate::memory::heap::MAX_SMALL * 2];
    let stored = mk(&content);
    assert!(
        crate::string::bytes_are_out_of_line(unsafe {
            crate::refcount::header_flags(stored as *const crate::refcount::RcHeader)
        }),
        "the key is out of line, or this test proves nothing"
    );
    m.insert(Key::Str(stored), Value::int(9));

    let other = mk(&content);
    assert_ne!(other, stored, "a second entity with the same bytes");
    assert_eq!(
        m.get(Key::Str(other)).unwrap().as_int(),
        9,
        "an equal oversize key finds the element"
    );

    let mut different = content.clone();
    *different.last_mut().unwrap() = b'x';
    let absent = mk(&different);
    assert!(
        m.get(Key::Str(absent)).is_none(),
        "and one that differs in its last byte does not"
    );

    // Unlike the small-key tests above, these three own payloads in
    // the buffer arena, and a leaked payload is not local to this
    // test: the block-adoption tests read the same thread's arena and
    // assume nothing is holding a block open.
    unsafe {
        for s in [stored, other, absent] {
            // Down to zero before the kill, whatever the table's
            // insert took: the free path asserts a dead header.
            while !crate::refcount::ll_release(s as *mut crate::refcount::RcHeader) {}
            crate::string::string_die(s);
        }
    }
}

#[test]
fn integer_and_string_keys_coexist_without_aliasing() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    m.insert(Key::Int(7), Value::int(700));

    // The string's hash is written rather than hashed, so that the
    // one thing separating the two keys is the entry's key kind:
    // below `strong` a string's slot is its cached hash, so 7 puts
    // it on the integer's chain, and 7 is also what the entry stores
    // as its identity. Left to the real hash, the two share a slot
    // on about one run in sixteen — the seed is drawn per process —
    // and never share an identity at all, so the arm this is named
    // for went untested either way.
    let s = mk(b"7");
    unsafe { (*s).hash = 7 };
    m.insert(Key::Str(s), Value::int(77));

    assert_eq!(
        chain(&m, 7).len(),
        2,
        "the two keys are on one chain, which is where aliasing would show"
    );
    assert_eq!(m.len(), 2, "an integer key and a string key are two keys");
    assert_eq!(m.get(Key::Int(7)).unwrap().as_int(), 700);
    assert_eq!(m.get(Key::Str(s)).unwrap().as_int(), 77);
}
