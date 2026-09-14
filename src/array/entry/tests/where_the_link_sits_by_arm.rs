//! The collision link sits in the top four bytes of whichever word is the
//! element's tag word — +8 on the immediate arm, +0 on the pointer arm —
//! and a null element is spelled with a tag word rather than all-zero, so
//! the +8 word a collector reads is a pointer, a tag word with bit 0 set,
//! or zero (`rfc/model/arrays-hashtable.md`, "The collision link lives
//! inside the element's ValueBox").

use super::*;
use crate::refcount::{MemoryCategory, RcHeader};
use crate::value::Tag;

/// The two machine words of a box, as the entry holds them and a
/// collector reads them.
fn words(v: Value) -> [u64; 2] {
    v.into_words()
}

fn entry() -> Entry {
    Entry {
        hash_or_key: 0,
        key_word: KEY_INT,
        element: Value::int(0),
    }
}

/// On the pointer arm the +8 word is the pointer and nothing else, so the
/// link goes into the +0 tag word — and every composing writer finds it
/// there.
#[test]
fn a_pointer_element_keeps_its_link_in_the_first_word() {
    let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
    let p = &raw mut h;
    let mut e = entry();
    let at = &raw mut e;

    unsafe { Entry::store_element_and_link(at, Value::entity(Tag::Object, p), 42) };
    assert_eq!(words(e.element), [0x0701 | 42 << 32, p as u64]);
    assert_eq!(e.link(), 42);

    unsafe { Entry::store_link(at, 7) };
    assert_eq!(words(e.element), [0x0701 | 7 << 32, p as u64]);

    unsafe { Entry::store_element(at, Value::entity(Tag::String, p)) };
    assert_eq!(words(e.element), [0x0501 | 7 << 32, p as u64]);
    assert_eq!(e.link(), 7, "the element store moved the link");
    assert_eq!(
        words(e.value()),
        [0x0501, p as u64],
        "the box handed out carries no link"
    );
}

/// A null element is `(0, 0x0001 | link << 32)`: `(0, link << 32)` would
/// be an even non-zero +8 word, which is a pointer to a collector.
#[test]
fn a_null_element_is_spelled_with_a_tag_word() {
    let mut e = entry();
    let at = &raw mut e;

    unsafe { Entry::store_element_and_link(at, Value::null(), 42) };
    assert_eq!(words(e.element), [0, 0x0001 | 42 << 32]);
    assert_eq!(e.link(), 42);

    unsafe { Entry::store_link(at, 9) };
    assert_eq!(words(e.element), [0, 0x0001 | 9 << 32]);

    unsafe { Entry::store_element(at, Value::null()) };
    assert_eq!(words(e.element), [0, 0x0001 | 9 << 32]);
    assert_eq!(e.link(), 9, "the element store moved the link");
}

/// The container's null reads back as `(0, 0x0001)`, the second spelling
/// every null test accepts (`rfc/model/values.md`, "Type tests, by tag").
#[test]
fn a_null_element_reads_back_as_the_container_null() {
    let mut e = entry();
    unsafe { Entry::store_element_and_link(&raw mut e, Value::null(), 42) };

    let handed_out = e.value();
    assert_eq!(words(handed_out), [0, 0x0001]);
    assert!(handed_out.is_null());
    assert!(!handed_out.is_undef());
    assert!(!handed_out.is_pointer());
}

/// An element store that changes the arm moves the link with the tag
/// word and leaves no link bits behind in the word that stops being the
/// tag word.
#[test]
fn the_link_follows_the_tag_word_across_arms() {
    let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
    let p = &raw mut h;
    let mut e = entry();
    let at = &raw mut e;

    unsafe { Entry::store_element_and_link(at, Value::int(5), 42) };
    assert_eq!(words(e.element), [5, 0x0301 | 42 << 32]);

    unsafe { Entry::store_element(at, Value::entity(Tag::Object, p)) };
    assert_eq!(words(e.element), [0x0701 | 42 << 32, p as u64]);
    assert_eq!(e.link(), 42);

    unsafe { Entry::store_element(at, Value::int(6)) };
    assert_eq!(words(e.element), [6, 0x0301 | 42 << 32]);
    assert_eq!(e.link(), 42);

    unsafe { Entry::store_element(at, Value::null()) };
    assert_eq!(words(e.element), [0, 0x0001 | 42 << 32]);
    assert_eq!(e.link(), 42);
}
