//! Thirty-two bytes with the collision link inside the element's
//! reserved word, and a key cap the index type imposes.

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
    assert_eq!(std::mem::offset_of!(Entry, key_word), 8);
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
