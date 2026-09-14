//! Whatever the table did to an element — an insert, an overwrite, a
//! replacement by null, a removal, a growth, a sever — the element's
//! discriminating word, the one at +8, is a live pointer, a tag word with
//! bit 0 set, or zero. A collector reads that word alone and follows every
//! even non-zero one, so a composing writer that spelled a null as
//! `(0, link << 32)` would hand it an entry index as an entity address
//! (`rfc/model/arrays-hashtable.md`, "The collision link lives inside the
//! element's ValueBox").

use super::*;
use crate::array::entry::ELEMENT_OFFSET;
use crate::value::Tag;
use std::collections::HashSet;

/// The +8 word of every element inside `used`, holes included: a hole's
/// element is read by a walker too, since the key word is the only thing
/// that says it is one.
fn discriminating_words(m: &Owned) -> Vec<u64> {
    (0..m.used())
        .map(|i| unsafe {
            (m.entries().add(i) as *const u8)
                .add(ELEMENT_OFFSET + 8)
                .cast::<u64>()
                .read()
        })
        .collect()
}

fn assert_every_discriminating_word_is_a_pointer_a_tag_word_or_zero(
    m: &Owned,
    live: &HashSet<u64>,
) {
    for (i, w8) in discriminating_words(m).into_iter().enumerate() {
        if w8 != 0 && w8 & 1 == 0 {
            assert!(
                live.contains(&w8),
                "entry {i}: the +8 word {w8:#x} is even and non-zero and no live pointer"
            );
        }
    }
}

#[test]
fn no_discriminating_word_a_collector_reads_is_an_even_non_zero_non_pointer() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    let strings: Vec<*mut LLString> = (0..8u8).map(|i| mk(&[b's', b'0' + i])).collect();
    let live: HashSet<u64> = strings.iter().map(|&s| s as u64).collect();

    // Three arms interleaved, across two growths of the chunk
    // (`FIRST_CAP` is 8): the immediate arm, the pointer arm and null.
    for k in 0..24i64 {
        let value = match k % 3 {
            0 => Value::int(k),
            1 => Value::entity(Tag::String, strings[(k / 3) as usize] as *mut RcHeader),
            _ => Value::null(),
        };
        let (added, old) = m.insert(Key::Int(k), value).unwrap();
        assert!(added && old.is_none());
    }

    assert!(
        m.used() > FIRST_CAP,
        "the chunk grew, so `rebuild_index` ran"
    );
    assert_every_discriminating_word_is_a_pointer_a_tag_word_or_zero(&m, &live);

    // The overwrite arm of `insert`: a pointer replaced by null, and an
    // int by a pointer.
    let (added, old) = m.insert(Key::Int(1), Value::null()).unwrap();
    assert!(!added);
    assert_eq!(old.unwrap().entity_ptr(), strings[0] as *mut RcHeader);
    let (added, old) = m
        .insert(
            Key::Int(0),
            Value::entity(Tag::String, strings[0] as *mut RcHeader),
        )
        .unwrap();
    assert!(!added);
    assert_eq!(old.unwrap().as_int(), 0);
    assert!(m.get(Key::Int(1)).unwrap().is_null());
    assert_every_discriminating_word_is_a_pointer_a_tag_word_or_zero(&m, &live);

    // A removal leaves a hole; the hole's element is read by a walker,
    // and it is the container's null with the chain cut — never undef,
    // which is confined to property slots.
    let (removed, key) = m.remove(Key::Int(4)).unwrap();
    assert_eq!(removed.entity_ptr(), strings[1] as *mut RcHeader);
    assert!(key.is_null());
    assert!(m.remove(Key::Int(2)).unwrap().0.is_null());
    for hole in [2usize, 4] {
        assert!(m.entry(hole).is_hole());
        assert_eq!(discriminating_words(&m)[hole], 0x0001 | (NONE as u64) << 32);
    }

    assert_every_discriminating_word_is_a_pointer_a_tag_word_or_zero(&m, &live);

    // A compaction drops the holes and rebuilds every chain.
    assert_eq!(m.compact(), Some(2));
    assert_every_discriminating_word_is_a_pointer_a_tag_word_or_zero(&m, &live);

    let displaced = m.sever();
    let severed: HashSet<u64> = displaced.iter().map(|&c| c as u64).collect();
    // Seven strings stood in the table: eight were stored once each,
    // `strings[0]` moved from key 1 to key 0 by the two overwrites, and
    // `strings[1]` went with the removal.
    assert_eq!(displaced.len(), 7);
    assert_eq!(
        severed,
        live.iter()
            .copied()
            .filter(|&s| s != strings[1] as u64)
            .collect()
    );
    assert_every_discriminating_word_is_a_pointer_a_tag_word_or_zero(&m, &live);

    // The table retains nothing, so every string is still at the count
    // its creation gave it.
    for s in strings {
        unsafe {
            assert!(crate::refcount::ll_release(s as *mut RcHeader));
            crate::string::string_die(s);
        }
    }
}
