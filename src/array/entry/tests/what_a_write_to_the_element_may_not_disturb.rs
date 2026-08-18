//! The link shares its word with the element and the key sits below
//! both, so an element store has to carry the link through and leave
//! the key alone — and a value handed out may not carry the link with
//! it into another entry.

use super::*;

/// The property the private element field exists to guarantee: an
/// element store leaves the chain link and the key exactly as they
/// were. Written as a round trip rather than as an offset assertion,
/// because what matters is the behaviour a caller can rely on.
#[test]
fn an_element_store_keeps_the_link_and_the_key() {
    let mut e = Entry {
        hash_or_key: 0xDEAD_BEEF,
        key_word: KEY_INT,
        element: Value::int(1),
    };

    let at = &raw mut e;

    unsafe { Entry::store_element_and_link(at, Value::int(7), 42) };
    assert_eq!(e.link(), 42);
    assert_eq!(e.value().as_int(), 7);

    unsafe { Entry::store_element(at, Value::int(9)) };
    assert_eq!(e.link(), 42, "the element store moved the link");
    assert_eq!(e.value().as_int(), 9);
    assert_eq!(e.hash_or_key, 0xDEAD_BEEF);
    assert!(e.is_int_key());

    unsafe { Entry::store_link(at, NONE) };
    assert_eq!(e.link(), NONE);
    assert_eq!(e.value().as_int(), 9, "the link store moved the element");
}

/// A Box handed out of an entry carries no trace of the link, so it
/// can be stored into another container without corrupting it.
#[test]
fn a_value_read_out_of_an_entry_has_no_link_in_it() {
    let mut e = Entry {
        hash_or_key: 0,
        key_word: KEY_INT,
        element: Value::int(0),
    };

    unsafe { Entry::store_element_and_link(&raw mut e, Value::int(5), 7) };

    let handed_out = e.value();
    assert_eq!(handed_out.into_words(), Value::int(5).into_words());
}

/// A hole is published without touching the element, which is what
/// keeps the chain walkable until compaction rebuilds it.
#[test]
fn making_a_hole_leaves_the_element_and_the_link_alone() {
    let mut e = Entry {
        hash_or_key: 0,
        key_word: 0x1000 | KEY_TAG_STRING,
        element: Value::int(0),
    };

    let at = &raw mut e;
    unsafe { Entry::store_element_and_link(at, Value::int(3), 11) };

    unsafe { Entry::make_hole(at) };
    assert!(e.is_hole());
    assert_eq!(e.link(), 11);
    assert_eq!(e.value().as_int(), 3);
}
