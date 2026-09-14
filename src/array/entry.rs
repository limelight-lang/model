//! The table's storage: `u32` index slots followed by a dense array of
//! 32-byte entries in insertion order (`rfc/model/arrays-hashtable.md`,
//! "The shape: an index array over a dense insertion-ordered entry
//! array").
//!
//! ```text
//! +0   hash_or_key  u64   full hash of a string key, or the integer key
//! +8   key_word     usize string key, tagged in its low three bits;
//!                         0 = integer key, 1 = hole
//! +16  element      Box   the value; the top four bytes of its tag word
//!                         carry the entry's collision link, a u32 at the
//!                         entry's +28 on the immediate arm and +20 on the
//!                         pointer arm (`rfc/model/arrays-hashtable.md`,
//!                         "The collision link lives inside the element's
//!                         ValueBox")
//! ```
//!
//! Two rules the code here exists to hold. Every write to either word of
//! the element, and to `key_word`, is one relaxed atomic store of the width
//! the collector loads (`cells::trace_cells`): change one width and change
//! the other. And every link is an index rather than a pointer, so
//! promotion copies the storage without fixing anything up.

use crate::string::LLString;
use crate::value::{DISCRIMINATING_WORD_OFFSET, TAG_WORD_BIT, TAG_WORD_MASK, Value};
use std::sync::atomic::{AtomicU64, Ordering};

/// End of a chain, and the empty index slot.
pub const NONE: u32 = u32::MAX;

/// A table holds at most `NONE - 1` entries: the index is a `u32` and one
/// value is reserved for "no entry". Language-visible, like the 4 GiB
/// string cap, and checked through one gate rather than cast at each use.
pub const MAX_ENTRIES: usize = (NONE - 1) as usize;

/// An integer key, whose value is in `hash_or_key`. It and
/// [`KEY_HOLE`] are the key words that are not a pointer, and both sit
/// below [`KEY_SENTINEL_LIMIT`], where no tagged pointer can land.
pub(crate) const KEY_INT: usize = 0;
/// A removed entry; the state it stands for is on [`Entry::is_hole`].
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

/// Where the link sits inside the element's tag word: the top four bytes.
/// The four below it are the flags, the tag and two bytes still spare.
const LINK_SHIFT: u32 = 32;
/// The `+8` word an entry spells a null element with, before the link goes
/// in: `(0, link << 32)` would be an even non-zero `+8` word, which is a
/// pointer to a collector reading that word alone. Both element writers
/// normalise a zero `+8` word to this one; `store_link` requires it done.
/// What a stored tag word keeps of the box is its low sixteen bits
/// ([`TAG_WORD_MASK`]), so a box handed out of an entry is bit-identical
/// to one built by a constructor — except a null, which comes out as
/// `(0, 0x0001)`.
const NULL_ELEMENT_WORD: u64 = TAG_WORD_BIT;

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
    /// The element, and the chain link in the top bytes of its tag word.
    /// Private: a flat assignment would publish a zeroed tag word over
    /// the link, and zero is a legal entry index rather than an end of
    /// chain, so the corruption would be a self-referencing entry rather
    /// than a crash — and it would publish a null as `(0, 0)`, which the
    /// next `store_link` would turn into a pointer.
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

    /// The element, with the container bits of its tag word cleared, so
    /// no caller ever holds this entry's chain link.
    #[inline]
    pub fn value(&self) -> Value {
        self.element.without_container_bits()
    }

    /// The next entry in this bucket's chain, or [`NONE`]: the top bytes
    /// of whichever word is the tag word, selected by the `+8` word's arm.
    #[inline]
    pub fn link(&self) -> u32 {
        (self.element.tag_word() >> LINK_SHIFT) as u32
    }

    /// Mark as deleted. The element is *not* cleared here: releasing it is
    /// the caller's, because the order matters to the collector.
    ///
    /// Published atomically because the collector reads this word to
    /// decide whether the key is a counted child (`cells::trace_cells`),
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
    /// already carries. The link moves with the tag word when the arm
    /// changes, and the word that stops being the tag word loses it.
    ///
    /// Two relaxed atomic stores, one per word, because a collector
    /// reads the `+8` word relaxed and an access of another width — or a
    /// plain store — against that is a data race rather than a torn
    /// value. Torn, the pair is still one the collector reads correctly:
    /// it interprets `+8` alone, and each spelling of it is one store's.
    ///
    /// # Safety
    /// `e` addresses a live entry of a live table.
    #[inline]
    pub unsafe fn store_element(e: *mut Entry, v: Value) {
        let link = unsafe { Self::link_bits(e) };
        unsafe { Self::store_words(e, Self::compose(v, link)) };
    }

    /// Publish `v` as the element **and** `link` as the chain link, which
    /// is what a fresh entry needs: it has no link to keep.
    ///
    /// # Safety
    /// `e` addresses a live entry of a live table.
    #[inline]
    pub unsafe fn store_element_and_link(e: *mut Entry, v: Value, link: u32) {
        unsafe { Self::store_words(e, Self::compose(v, (link as u64) << LINK_SHIFT)) };
    }

    /// Repoint this entry's chain link, keeping the element: one store,
    /// into the tag word of the element's arm.
    ///
    /// # Safety
    /// `e` addresses a live entry of a live table, whose element was
    /// published by one of the two writers above.
    #[inline]
    pub unsafe fn store_link(e: *mut Entry, link: u32) {
        unsafe {
            let w8 = (*Self::word(e, DISCRIMINATING_WORD_OFFSET)).load(Ordering::Relaxed);
            debug_assert_ne!(w8, 0, "an entry never holds the barrier's all-zero null");
            let tag_word = Self::word(e, Self::tag_word_offset(w8));
            let kept = (*tag_word).load(Ordering::Relaxed) & TAG_WORD_MASK;
            (*tag_word).store(kept | ((link as u64) << LINK_SHIFT), Ordering::Relaxed);
        }
    }

    /// The two words `v` is published as with `link` — already shifted
    /// into the top bytes — in its tag word: a null is spelled with
    /// [`NULL_ELEMENT_WORD`] first, so that the link never makes an even
    /// non-zero `+8` word.
    ///
    /// Selects rather than indexes the pair: an index into `[w0, w8]`
    /// compiles to a spill of both words and a read-modify-write through
    /// the stack, on the store path as on the chain walk
    /// (`dev/BENCHMARKS.md`, "S48.2 the box's price after the relayout").
    #[inline]
    fn compose(v: Value, link: u64) -> [u64; 2] {
        let [w0, w8] = v.into_words();
        let w8 = if w8 == 0 { NULL_ELEMENT_WORD } else { w8 };
        let pointer = Value::is_pointer_word(w8);
        let tag_word = if pointer { w0 } else { w8 };
        debug_assert_eq!(
            tag_word & !TAG_WORD_MASK,
            0,
            "a box entering an entry carries no container bits"
        );
        let composed = (tag_word & TAG_WORD_MASK) | link;
        if pointer {
            [composed, w8]
        } else {
            [w0, composed]
        }
    }

    /// The link this entry carries, in place in its tag word and with the
    /// box's own bits cleared.
    ///
    /// # Safety
    /// As [`store_link`](Self::store_link).
    #[inline]
    unsafe fn link_bits(e: *mut Entry) -> u64 {
        unsafe {
            let w8 = (*Self::word(e, DISCRIMINATING_WORD_OFFSET)).load(Ordering::Relaxed);
            debug_assert_ne!(w8, 0, "an entry never holds the barrier's all-zero null");
            (*Self::word(e, Self::tag_word_offset(w8))).load(Ordering::Relaxed) & !TAG_WORD_MASK
        }
    }

    /// Publish both words of the element, `+0` first.
    ///
    /// # Safety
    /// `e` addresses a live entry of a live table.
    #[inline]
    unsafe fn store_words(e: *mut Entry, words: [u64; 2]) {
        unsafe {
            (*Self::word(e, 0)).store(words[0], Ordering::Relaxed);
            (*Self::word(e, DISCRIMINATING_WORD_OFFSET)).store(words[1], Ordering::Relaxed);
        }
    }

    /// The offset inside the element of its tag word, given its `+8`
    /// word: `+0` on the pointer arm, `+8` on the immediate arm.
    #[inline]
    fn tag_word_offset(w8: u64) -> usize {
        if Value::is_pointer_word(w8) {
            0
        } else {
            DISCRIMINATING_WORD_OFFSET
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

    /// The element's word at `offset` — 0 or [`DISCRIMINATING_WORD_OFFSET`]
    /// — as the atomic every store and load of it goes through.
    #[inline]
    unsafe fn word(e: *mut Entry, offset: usize) -> *const AtomicU64 {
        unsafe { ((&raw mut (*e).element) as *mut u8).add(offset) as *const AtomicU64 }
    }
}

#[cfg(test)]
mod tests;
