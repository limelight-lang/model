//! The table's storage: `u32` index slots followed by a dense array of
//! 32-byte entries in insertion order (`rfc/model/arrays-hashtable.md`,
//! "The shape: an index array over a dense insertion-ordered entry
//! array").
//!
//! ```text
//! +0   hash_or_key  u64   full hash of a string key, or the integer key
//! +8   key          ptr   string key, tagged in its low three bits;
//!                         0 = integer key, 1 = hole
//! +16  element      Box   the value; its reserved bytes carry the entry's
//!                         collision link, a u32 at +28
//! ```
//!
//! Two rules the code here exists to hold. Every write to the element's
//! second word, and to `key`, is one relaxed atomic store of the width
//! the collector loads (`walk::trace_cells`): change one width and change
//! the other. And every link is an index rather than a pointer, so
//! promotion copies the storage without fixing anything up.

use crate::string::LLString;
use crate::value::Value;
use std::sync::atomic::{AtomicU64, Ordering};

/// End of a chain, and the empty index slot.
pub const NONE: u32 = u32::MAX;

/// A table holds at most `NONE - 1` entries: the index is a `u32` and one
/// value is reserved for "no entry". Language-visible, like the 4 GiB
/// string cap, and checked through one gate rather than cast at each use.
pub const MAX_ENTRIES: usize = (NONE - 1) as usize;

/// `key` values that are not a pointer. Both sit below
/// [`KEY_SENTINEL_LIMIT`], where no tagged pointer can land.
pub(crate) const KEY_INT: usize = 0;
pub(crate) const KEY_HOLE: usize = 1;

/// The key word's encoding, one for every owner (`rfc/model/maps.md`,
/// "The key word gains a tag, for every owner"): a word below this
/// limit is a sentinel and is tested first; at or above it, the low
/// three bits carry the key's kind and the pointer is the word with
/// them off. A reader — the walker included — makes the sentinel test
/// on the raw word it loaded and masks only what passes it.
pub(crate) const KEY_SENTINEL_LIMIT: usize = 8;
/// The low three bits of a tagged key word.
pub(crate) const KEY_TAG_MASK: usize = 7;
/// The string kind — the one kind an array produces; a map adds object
/// `2` and array `3` when it arrives.
pub(crate) const KEY_TAG_STRING: usize = 1;

/// Where the link sits inside the element's second word: the top four
/// bytes, which is the Box's reserved offset +12 and the entry's +28. The
/// four below it are the tag, the flags and two bytes still spare.
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
    /// The key word: a tagged string pointer, or [`KEY_INT`] /
    /// [`KEY_HOLE`]. An integer rather than a pointer type, because the
    /// word is never a dereferenceable address — the tag is in it — and
    /// the sentinels carry state no `Option` could; the pointer edge is
    /// [`string_key`](Self::string_key) alone.
    pub key_word: usize,
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
        self.key_word == KEY_HOLE
    }

    /// True when the key is an integer, whose value is in `hash_or_key`.
    #[inline]
    pub fn is_int_key(&self) -> bool {
        self.key_word == KEY_INT
    }

    /// The string key, or null for an integer key or a hole. The tag
    /// comes off here and goes on in
    /// [`set_string_key`](Self::set_string_key), nowhere else.
    #[inline]
    pub fn string_key(&self) -> *mut LLString {
        let word = self.key_word;
        if word < KEY_SENTINEL_LIMIT {
            std::ptr::null_mut()
        } else {
            debug_assert_eq!(
                word & KEY_TAG_MASK,
                KEY_TAG_STRING,
                "an array produces string keys only"
            );
            (word & !KEY_TAG_MASK) as *mut LLString
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
        self.key_word = KEY_INT;
    }

    /// Set a string key. `hash` is the string's **own cached hash**, never
    /// a slot hash: an escalated table mixes the salt into that one, and
    /// the key would then be unfindable by its own identity.
    ///
    /// Both words are written plainly, so the entry must be one no walker
    /// can reach — outside `used`, or inside a version bracket. Nothing is
    /// retained here; the reference the table owes per stored key is the
    /// caller's ([`Table::insert`](crate::array::table::Table::insert)).
    #[inline]
    pub fn set_string_key(&mut self, s: *mut LLString, hash: u64) {
        debug_assert!(
            s as usize >= KEY_SENTINEL_LIMIT && s as usize & KEY_TAG_MASK == 0,
            "a string key is a real 8-aligned pointer: the mask would \
             otherwise hand back an address inside the previous slot"
        );
        self.hash_or_key = hash;
        self.key_word = s as usize | KEY_TAG_STRING;
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
            let at = (&raw mut (*e).key_word) as *const AtomicU64;
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
mod tests;
