use super::*;

/// Thirty-two bytes with the collision link inside the element's
/// reserved word, and a key cap the index type imposes.
mod the_layout_the_design_fixes {
    use super::*;

    /// The layout the design fixes. This test fails if a field is
    /// reordered or the entry grows, which is the point: the key keeping a
    /// word of its own is what lets a hole outlive an element store, and
    /// the element's own reserved bytes are where the chain link lives.
    #[test]
    fn entry_layout_is_the_one_the_design_fixes() {
        assert_eq!(std::mem::size_of::<Entry>(), 32);
        assert_eq!(std::mem::align_of::<Entry>(), 8);
        assert_eq!(std::mem::offset_of!(Entry, hash_or_key), 0);
        assert_eq!(std::mem::offset_of!(Entry, key), 8);
        assert_eq!(std::mem::offset_of!(Entry, element), 16);
        assert_eq!(ELEMENT_OFFSET, std::mem::offset_of!(Entry, element));
    }

    /// The cap exists so a `usize` count is never truncated into the
    /// index's `u32`, the same shape of gate as the string length's.
    #[test]
    fn the_entry_cap_leaves_none_free() {
        assert_eq!(MAX_ENTRIES, NONE as usize - 1);
        assert!(MAX_ENTRIES < NONE as usize);
    }
}

/// The link shares its word with the element and the key sits below
/// both, so an element store has to carry the link through and leave
/// the key alone — and a value handed out may not carry the link with
/// it into another entry.
mod what_a_write_to_the_element_may_not_disturb {
    use super::*;

    /// The property the private element field exists to guarantee: an
    /// element store leaves the chain link and the key exactly as they
    /// were. Written as a round trip rather than as an offset assertion,
    /// because what matters is the behaviour a caller can rely on.
    #[test]
    fn an_element_store_keeps_the_link_and_the_key() {
        let mut e = Entry {
            hash_or_key: 0xDEAD_BEEF,
            key: KEY_INT as *mut LLString,
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
            key: KEY_INT as *mut LLString,
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
            key: 0x1000 as *mut LLString,
            element: Value::int(0),
        };

        let at = &raw mut e;
        unsafe { Entry::store_element_and_link(at, Value::int(3), 11) };

        unsafe { Entry::make_hole(at) };
        assert!(e.is_hole());
        assert_eq!(e.link(), 11);
        assert_eq!(e.value().as_int(), 3);
    }
}

/// One word carries the string key and the two states that are not a
/// key, which works only because both sentinels sit below the
/// alignment of any real string.
mod the_key_word_as_a_discriminant {
    use super::*;

    /// The sentinels have to be unreachable as real string pointers, or a
    /// key could read as a hole.
    #[test]
    fn key_sentinels_cannot_collide_with_a_string_pointer() {
        assert!(KEY_INT < std::mem::align_of::<LLString>());
        assert!(KEY_HOLE < std::mem::align_of::<LLString>());
    }

    #[test]
    fn key_states_are_distinguished() {
        let mut e = Entry {
            hash_or_key: 0,
            key: std::ptr::null_mut(),
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
        assert_eq!(e.hash_or_key, 0xDEAD_BEEF);

        unsafe { Entry::make_hole(&raw mut e) };
        assert!(e.is_hole());
        assert!(!e.is_int_key());
        assert!(e.string_key().is_null());
    }
}
