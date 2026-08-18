//! One word carries the tagged string key and the two states that are
//! not a key, which works only because both sentinels sit below
//! [`KEY_SENTINEL_LIMIT`], where no 8-aligned pointer can land once its
//! low bits carry the tag.

use super::*;

/// The sentinels have to be unreachable as real string pointers, or a
/// key could read as a hole.
#[test]
fn key_sentinels_cannot_collide_with_a_string_pointer() {
    assert!(KEY_INT < std::mem::align_of::<LLString>());
    assert!(KEY_HOLE < std::mem::align_of::<LLString>());
    // The whole encoding rests on this: every real string address has
    // its low three bits clear, or the tag would eat address bits.
    assert!(std::mem::align_of::<LLString>() >= KEY_SENTINEL_LIMIT);
}

#[test]
fn key_states_are_distinguished() {
    let mut e = Entry {
        hash_or_key: 0,
        key_word: KEY_INT,
        element: Value::int(7),
    };

    e.set_int_key(-3);
    assert!(e.is_int_key());
    assert!(!e.is_hole());
    assert!(e.string_key().is_null());
    assert_eq!(e.hash_or_key as i64, -3);

    // A string key is any aligned pointer; a real one is not needed to
    // pin the discriminant.
    let fake = 0x1000 as *mut LLString;
    e.set_string_key(fake, 0xDEAD_BEEF);
    assert!(!e.is_int_key());
    assert!(!e.is_hole());
    assert_eq!(e.string_key(), fake);
    assert_eq!(
        e.key_word,
        0x1000 | KEY_TAG_STRING,
        "the stored word carries the string tag"
    );
    assert_eq!(e.hash_or_key, 0xDEAD_BEEF);

    unsafe { Entry::make_hole(&raw mut e) };
    assert!(e.is_hole());
    assert!(!e.is_int_key());
    assert!(e.string_key().is_null());
}
