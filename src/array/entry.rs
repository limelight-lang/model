//! The table's storage: `u32` index slots followed by a dense array of
//! 32-byte entries in insertion order (`rfc/model/arrays-hashtable.md`,
//! "The shape: an index array over a dense insertion-ordered entry
//! array").
//!
//! ```text
//! +0   hash_or_key  u64   full hash of a string key, or the integer key
//! +8   key          ptr   string key; 0 = integer key, 1 = hole
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

/// `key` values that are not a string pointer. Both are below the
/// alignment of any real `LLString`, so a pointer can never collide.
pub(crate) const KEY_INT: usize = 0;
/// Anything above this in the `key` field is a real string pointer, which
/// is the test a walker makes on the raw word it read.
pub(crate) const KEY_HOLE: usize = 1;

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
mod tests;
