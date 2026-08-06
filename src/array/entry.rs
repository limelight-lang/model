//! The table's storage: `u32` index slots followed by a dense array of
//! entries in insertion order (`rfc/model/arrays-hashtable.md`).
//!
//! One entry is 40 bytes and its field order is load-bearing rather than
//! arbitrary:
//!
//! ```text
//! +0   hash_or_key  u64   full hash of a string key, or the integer key
//! +8   key          ptr   string key; 0 = integer key, 1 = hole
//! +16  next         u32   index of the next entry in this bucket's chain
//! +20  meta         u32   per-entry state
//! +24  value        Value (16 B)
//! ```
//!
//! **The `Value` sits last on purpose.** The store barrier writes all
//! sixteen bytes of a `Value` (`rfc/model/values.md`, the `+10` row: bytes
//! 10..15 are padding and explicitly not per-slot state), so anything the
//! table keeps beside a value has to sit outside those bytes. Zend threads
//! its collision chain through the element's own padding (`zval.u2.next`);
//! here that link would be severed by the first ordinary value store, so
//! it gets a field of its own at +16, and every write the barrier performs
//! lands inside 24..40.
//!
//! **`key` doubles as the state discriminant**, which keeps the hole
//! marker outside the `Value` for the same reason: an aligned pointer is a
//! string key, `0` is an integer key whose value is in `hash_or_key`, and
//! `1` is a hole left by deletion. The arena reset's tracer enumerates
//! elements by scanning `0..used` and skipping holes, and that enumeration
//! has to be complete rather than conservative (`dev/DECISIONS.md`,
//! 2026-08-04), so the marker must survive any value store.
//!
//! **Every link is an index, never a pointer.** Promotion copies an arena
//! survivor's out-of-line storage into the heap with the entity header
//! fixed in place (`rfc/model/strings.md`, and the same obligation for
//! arrays), so a self-referential pointer inside the storage would have to
//! be fixed up. `u32` indices move without fixing up anything.

use crate::string::LLString;
use crate::value::Value;

/// End of a chain, and the empty index slot.
pub const NONE: u32 = u32::MAX;

/// A table holds at most `NONE - 1` entries: the index is a `u32` and one
/// value is reserved for "no entry". Language-visible, like the 4 GiB
/// string cap, and checked through one gate rather than cast at each use.
pub const MAX_ENTRIES: usize = (NONE - 1) as usize;

/// `key` values that are not a string pointer. Both are below the
/// alignment of any real `LLString`, so a pointer can never collide.
pub(crate) const KEY_INT: usize = 0;
/// Anything above this in the `key` field is a real string pointer, which
/// is the test a walker makes on the raw word it read.
pub(crate) const KEY_HOLE: usize = 1;

/// One element of the table, in insertion order. See the module comment
/// for why the fields sit where they do.
#[repr(C)]
pub struct Entry {
    /// The full 64-bit hash for a string key, or the integer key itself.
    pub hash_or_key: u64,
    /// String key, or [`KEY_INT`] / [`KEY_HOLE`]. Raw rather than
    /// `Option<NonNull<…>>` because the two sentinels carry state that an
    /// `Option` cannot.
    pub key: *mut LLString,
    /// Next entry in this bucket's chain, or [`NONE`].
    pub next: u32,
    /// Reserved for per-entry state the design has not needed yet.
    pub meta: u32,
    /// The element. Written by the store barrier, which touches all 16
    /// bytes and nothing before them.
    pub value: Value,
}

impl Entry {
    /// True when this entry was deleted and its slot is waiting for
    /// compaction. Iteration, the tracer and every lookup skip it.
    #[inline]
    pub fn is_hole(&self) -> bool {
        self.key as usize == KEY_HOLE
    }

    /// True when the key is an integer, whose value is in `hash_or_key`.
    #[inline]
    pub fn is_int_key(&self) -> bool {
        self.key as usize == KEY_INT
    }

    /// The string key, or null for an integer key or a hole.
    #[inline]
    pub fn string_key(&self) -> *mut LLString {
        if (self.key as usize) <= KEY_HOLE {
            std::ptr::null_mut()
        } else {
            self.key
        }
    }

    /// Mark as deleted. The value is *not* cleared here: releasing it is
    /// the caller's, because the order matters to the collector.
    #[inline]
    pub fn make_hole(&mut self) {
        self.key = KEY_HOLE as *mut LLString;
    }

    #[inline]
    pub fn set_int_key(&mut self, k: i64) {
        self.hash_or_key = k as u64;
        self.key = KEY_INT as *mut LLString;
    }

    #[inline]
    pub fn set_string_key(&mut self, s: *mut LLString, hash: u64) {
        debug_assert!(s as usize > KEY_HOLE, "a string key is a real pointer");
        self.hash_or_key = hash;
        self.key = s;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout the design fixes. This test fails if a field is
    /// reordered or the entry grows, which is the point: the `Value`
    /// being last is what keeps the store barrier off the key and the
    /// chain link.
    #[test]
    fn entry_layout_is_the_one_the_design_fixes() {
        assert_eq!(std::mem::size_of::<Entry>(), 40);
        assert_eq!(std::mem::align_of::<Entry>(), 8);
        assert_eq!(std::mem::offset_of!(Entry, hash_or_key), 0);
        assert_eq!(std::mem::offset_of!(Entry, key), 8);
        assert_eq!(std::mem::offset_of!(Entry, next), 16);
        assert_eq!(std::mem::offset_of!(Entry, meta), 20);
        assert_eq!(std::mem::offset_of!(Entry, value), 24);
    }

    /// The whole reason for the field order: a 16-byte write at the
    /// value's offset must not reach the key or the link.
    #[test]
    fn a_full_value_write_cannot_reach_the_key_or_the_link() {
        let value_start = std::mem::offset_of!(Entry, value);
        let value_end = value_start + std::mem::size_of::<Value>();
        assert_eq!(value_end, std::mem::size_of::<Entry>());

        for f in [
            std::mem::offset_of!(Entry, hash_or_key),
            std::mem::offset_of!(Entry, key),
            std::mem::offset_of!(Entry, next),
            std::mem::offset_of!(Entry, meta),
        ] {
            assert!(f < value_start, "field at +{f} is inside the value's write");
        }
    }

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
            next: NONE,
            meta: 0,
            value: Value::int(7),
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

        e.make_hole();
        assert!(e.is_hole());
        assert!(!e.is_int_key());
        assert!(e.string_key().is_null());
    }

    /// The cap exists so a `usize` count is never truncated into the
    /// index's `u32`, the same shape of gate as the string length's.
    #[test]
    fn the_entry_cap_leaves_none_free() {
        assert_eq!(MAX_ENTRIES, NONE as usize - 1);
        assert!(MAX_ENTRIES < NONE as usize);
    }
}
