//! The table's storage: `u32` index slots followed by a dense array of
//! entries in insertion order (`rfc/model/arrays-hashtable.md`).
//!
//! One entry is 32 bytes:
//!
//! ```text
//! +0   hash_or_key  u64   full hash of a string key, or the integer key
//! +8   key          ptr   string key; 0 = integer key, 1 = hole
//! +16  element      Box   the value; its reserved bytes carry the entry's
//!                         collision link, a u32 at +28
//! ```
//!
//! **The collision link lives inside the element's Box**, in the six bytes
//! `rfc/model/values.md` reserves at +10, and that is the one fact to hold
//! before reading anything else here. Zend threads its link through
//! `zval.u2.next` and pays for it with a rule no macro may break: a value
//! copy never carries `u2`. The rule here has to be stronger, because the
//! concurrent collector reads the element's second word while a mutator
//! writes it. It is enforced three ways:
//!
//! - the element field is **private**, so `entry.element = v` does not
//!   compile outside this module, and every write goes through
//!   [`Entry::store_element`] or [`Entry::store_link`];
//! - those two compose the whole second word — tag, flags and link — and
//!   publish it as **one relaxed atomic store**, matching the eight-byte
//!   relaxed load the collector performs (`walk::trace_cells`);
//! - every read hands the Box out through `Value::without_reserved`, so a
//!   link can never travel in a copy and land in another entry.
//!
//! **The key keeps a word of its own**, which is what makes the hole
//! marker survive an element store: writing an element touches +16..32 and
//! nothing below it. `key` doubles as the state discriminant — an aligned
//! pointer is a string key, `0` is an integer key whose value is in
//! `hash_or_key`, and `1` is a hole left by deletion. The arena reset's
//! tracer enumerates elements by scanning `0..used` and skipping holes, and
//! that enumeration has to be complete rather than conservative
//! (`dev/DECISIONS.md`, 2026-08-04), so the marker must survive any value
//! store. The collector reads this word too, so [`Entry::make_hole`]
//! publishes it atomically.
//!
//! **Every link is an index, never a pointer.** Promotion copies an arena
//! survivor's out-of-line storage into the heap with the entity header
//! fixed in place (`rfc/model/strings.md`, and the same obligation for
//! arrays), so a self-referential pointer inside the storage would have to
//! be fixed up. `u32` indices move without fixing up anything.

use crate::string::LLString;
use crate::value::Value;
use std::sync::atomic::{AtomicU64, Ordering};

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

/// Where the link sits inside the element's second word: the top four
/// bytes, which is the Box's reserved offset +12 and the entry's +28. The
/// four below it are the tag, the flags and two bytes still spare — the
/// room the old `meta` field held, now inside the same atomic word.
const LINK_SHIFT: u32 = 32;
/// What a stored second word keeps of the Box: the tag and the flags. The
/// two spare bytes above them read back as zero, so a Box handed out of an
/// entry is bit-identical to one built by a constructor.
const BOX_META_MASK: u64 = 0x0000_0000_0000_FFFF;

/// Where the element begins inside an entry. The walkers stride the
/// storage by raw offsets rather than through `&Entry`, because they may
/// be racing a mutator and must read each word atomically; this is the one
/// place that says where those words are.
pub const ELEMENT_OFFSET: usize = 16;

/// One element of the table, in insertion order. See the module comment
/// for why the link lives where it does.
#[repr(C)]
pub struct Entry {
    /// The full 64-bit hash for a string key, or the integer key itself.
    /// The collector never reads it, so it is written plainly.
    pub hash_or_key: u64,
    /// String key, or [`KEY_INT`] / [`KEY_HOLE`]. Raw rather than
    /// `Option<NonNull<…>>` because the two sentinels carry state that an
    /// `Option` cannot.
    pub key: *mut LLString,
    /// The element, and the chain link in its reserved bytes. Private:
    /// a flat assignment would publish zeroed reserved bytes over the
    /// link, and zero is a legal entry index rather than an end of chain,
    /// so the corruption would be a self-referencing entry rather than a
    /// crash.
    element: Value,
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

    /// The element, with the reserved bytes cleared, so no caller ever
    /// holds this entry's chain link.
    #[inline]
    pub fn value(&self) -> Value {
        self.element.without_reserved()
    }

    /// The next entry in this bucket's chain, or [`NONE`].
    #[inline]
    pub fn link(&self) -> u32 {
        (self.element.into_words()[1] >> LINK_SHIFT) as u32
    }

    /// Mark as deleted. The element is *not* cleared here: releasing it is
    /// the caller's, because the order matters to the collector.
    ///
    /// Published atomically because the collector reads this word to
    /// decide whether the key is a counted child (`walk::trace_cells`),
    /// and it may be reading while this runs.
    ///
    /// # Safety
    /// `e` addresses a live entry of a live table.
    #[inline]
    pub unsafe fn make_hole(e: *mut Entry) {
        unsafe { Self::store_key_word(e, KEY_HOLE as u64) };
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

    /// Publish `v` as the element, keeping the chain link this entry
    /// already carries.
    ///
    /// Two relaxed atomic stores, one per word, because the collector
    /// reads both words relaxed and an access of another width — or a
    /// plain store — against that is a data race rather than the torn
    /// value the epoch repairs.
    ///
    /// # Safety
    /// `e` addresses a live entry of a live table.
    #[inline]
    pub unsafe fn store_element(e: *mut Entry, v: Value) {
        let words = v.into_words();
        unsafe {
            let payload = Self::payload_word(e);
            let meta = Self::meta_word(e);
            let link = (*meta).load(Ordering::Relaxed) & !BOX_META_MASK;
            (*payload).store(words[0], Ordering::Relaxed);
            (*meta).store((words[1] & BOX_META_MASK) | link, Ordering::Relaxed);
        }
    }

    /// Publish `v` as the element **and** `link` as the chain link, which
    /// is what a fresh entry needs: it has no link to keep.
    ///
    /// # Safety
    /// `e` addresses a live entry of a live table.
    #[inline]
    pub unsafe fn store_element_and_link(e: *mut Entry, v: Value, link: u32) {
        let words = v.into_words();
        unsafe {
            (*Self::payload_word(e)).store(words[0], Ordering::Relaxed);
            (*Self::meta_word(e)).store(
                (words[1] & BOX_META_MASK) | ((link as u64) << LINK_SHIFT),
                Ordering::Relaxed,
            );
        }
    }

    /// Repoint this entry's chain link, keeping the element.
    ///
    /// # Safety
    /// `e` addresses a live entry of a live table.
    #[inline]
    pub unsafe fn store_link(e: *mut Entry, link: u32) {
        unsafe {
            let meta = Self::meta_word(e);
            let kept = (*meta).load(Ordering::Relaxed) & BOX_META_MASK;
            (*meta).store(kept | ((link as u64) << LINK_SHIFT), Ordering::Relaxed);
        }
    }

    /// The key word, published atomically for the reason
    /// [`make_hole`](Self::make_hole) gives.
    ///
    /// # Safety
    /// `e` addresses a live entry of a live table.
    #[inline]
    pub unsafe fn store_key_word(e: *mut Entry, word: u64) {
        unsafe {
            let at = (&raw mut (*e).key) as *const AtomicU64;
            (*at).store(word, Ordering::Relaxed);
        }
    }

    #[inline]
    unsafe fn payload_word(e: *mut Entry) -> *const AtomicU64 {
        unsafe { (&raw mut (*e).element) as *const AtomicU64 }
    }

    #[inline]
    unsafe fn meta_word(e: *mut Entry) -> *const AtomicU64 {
        unsafe { ((&raw mut (*e).element) as *mut u8).add(8) as *const AtomicU64 }
    }
}

#[cfg(test)]
mod tests {
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

    /// The cap exists so a `usize` count is never truncated into the
    /// index's `u32`, the same shape of gate as the string length's.
    #[test]
    fn the_entry_cap_leaves_none_free() {
        assert_eq!(MAX_ENTRIES, NONE as usize - 1);
        assert!(MAX_ENTRIES < NONE as usize);
    }
}
