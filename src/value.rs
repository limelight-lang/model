//! The 16-byte ValueBox: tagged value for dynamically-typed storage sites.
//!
//! Layout per `rfc/model/values.md`, "ValueBox Layout": two words. `+0` is
//! the immediate value — a full `i64`, an `f64` bit pattern, zero for
//! `null`, `false` and `true` — or, on the pointer arm, the tag word. `+8`
//! is the counted pointer, bit 0 clear because every entity begins with an
//! 8-aligned `RcHeader`, or the tag word with bit 0 set. The `+8` word
//! alone decides the arm ([`DISCRIMINATING_WORD_OFFSET`]), which is what
//! lets a collector on another thread read it with one load and never
//! interpret `+0`: bit 0 set is an immediate value, zero is null, anything
//! else is a pointer. A tag word is the flags byte — bit 0 fixed to 1, bit
//! 1 `undef` — below the tag byte, with bits 16–63 zero outside a
//! container.
//!
//! `Tag` is a code the accessors decode and never a field: a Rust enum at
//! `+8` is undefined the moment a pointer byte lands there. Unboxed
//! representations (raw i64/f64/ptr for declared types) are a compiler
//! contract and have no runtime type here. `false` and `true` are separate
//! tags so truth tests never read the payload; there is deliberately no
//! `undef` *tag* — the uninitialized state of a ValueBox property slot is
//! the [`TAG_WORD_UNDEF`] bit of its tag word, not a type.

use crate::refcount::{RcHeader, ll_release, ll_retain};

/// Type tags: the tag byte's codes. Pointer-carrying tags point to
/// entities beginning with `RcHeader`. A code above `Reference` is
/// corruption, and [`Value::tag`] aborts on one.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tag {
    Null = 0,
    False = 1,
    True = 2,
    Int = 3,
    Float = 4,
    String = 5,
    Array = 6,
    Object = 7,
    Resource = 8,
    Reference = 9,
}

/// Where the word that decides the arm sits inside a ValueBox. Every
/// reader and writer of a box cell that addresses the word by offset
/// spells it through this constant (`cells.rs`, `memory/barrier.rs`,
/// `array/entry.rs`, `array/vector.rs`).
pub const DISCRIMINATING_WORD_OFFSET: usize = 8;

/// Tag-word flag, bit 0: this word is a tag word and not a pointer. Fixed
/// to 1 in every tag word; a pointer's bit 0 is 0 by alignment.
pub const TAG_WORD_BIT: u64 = 1 << 0;

/// Tag-word flag, bit 1: this is an **uninitialized** property slot
/// (`mixed` / untyped, declared without a default). Reading it throws
/// `Error`, per PHP's uninitialized-typed-property semantics; any store
/// clears it, because the barrier writes all 16 bytes
/// ([`crate::memory::barrier`]); `unset()` restores it (a [`Value::undef`]
/// store, then `drop_ref` of the displaced entity). It is confined to
/// property slots and never set on a box in a local, parameter, return,
/// array element or reference box, so it cannot flow into a value
/// context — unlike Zend's
/// `IS_UNDEF` (`rfc/model/values.md`). Raw typed slots have no room for
/// it and use the per-object init bitmap instead. The factory stamps it
/// over the class's `undef_runs` after the zero-fill
/// ([`crate::class::Class::undef_runs`]).
pub const TAG_WORD_UNDEF: u64 = 1 << 1;

/// The tag byte's position inside a tag word.
const TAG_CODE_SHIFT: u32 = 8;

/// The low sixteen bits of a tag word — flags byte and tag byte — which
/// is what a type test compares against a constant: a container's bytes
/// above are excluded, and no pointer's low bytes can match because bit 0
/// of every tag-word constant is 1. A container that keeps state in the
/// bytes above masks with this too (`array/entry.rs`).
pub(crate) const TAG_WORD_MASK: u64 = 0xFFFF;

/// The tag word of an initialized value of `tag`.
#[inline]
const fn tag_word_of(tag: Tag) -> u64 {
    TAG_WORD_BIT | (tag as u64) << TAG_CODE_SHIFT
}

/// The ValueBox. `#[repr(C)]`: generated code addresses the words by
/// offset, and a collector reads `w8` alone.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Value {
    w0: u64,
    w8: u64,
}

impl Value {
    /// An immediate-arm box: the value at `+0`, the tag word at `+8`.
    #[inline]
    const fn immediate(value: u64, tag: Tag) -> Self {
        Value {
            w0: value,
            w8: tag_word_of(tag),
        }
    }

    /// PHP `null` as this constructor and the factory's zero-fill spell
    /// it: all-zero. A null read out of a container is `(0, 0x0001)` and
    /// keeps that spelling wherever it is stored next — the barrier writes
    /// the box it is given — and every null test accepts both.
    pub const fn null() -> Self {
        Value { w0: 0, w8: 0 }
    }

    /// The uninitialized property slot: a Null tag word with
    /// [`TAG_WORD_UNDEF`] (an all-zero box is `null`, not undefined). This
    /// is what the factory stamps and what an `unset()` stores back.
    pub const fn undef() -> Self {
        Value {
            w0: 0,
            w8: TAG_WORD_BIT | TAG_WORD_UNDEF,
        }
    }

    /// PHP `true` or `false`: separate tags, the payload zero.
    pub const fn bool(v: bool) -> Self {
        Self::immediate(0, if v { Tag::True } else { Tag::False })
    }

    /// PHP `int`: the full 64 bits at `+0`.
    pub const fn int(v: i64) -> Self {
        Self::immediate(v as u64, Tag::Int)
    }

    /// PHP `float`: the IEEE bit pattern at `+0`, untouched.
    pub fn float(v: f64) -> Self {
        Self::immediate(v.to_bits(), Tag::Float)
    }

    /// A counted entity reference (object, string, array, …): the tag
    /// word at `+0`, the pointer at `+8`.
    pub fn entity(tag: Tag, ptr: *mut RcHeader) -> Self {
        debug_assert!(!ptr.is_null());
        debug_assert!(
            ptr as usize % 8 == 0,
            "a pointer's bit 0 is the arm test, and an unaligned one reads as a tag word"
        );
        Value {
            w0: tag_word_of(tag),
            w8: ptr as u64,
        }
    }

    /// The box the two machine words `[w0, w8]` spell — the inverse of
    /// [`into_words`](Self::into_words), for a test that spells a box the
    /// way a slot holds it. No production reader builds a `Value` from
    /// words: the walkers decide on the `+8` word and never form the box.
    #[cfg(test)]
    pub(crate) fn from_words(words: [u64; 2]) -> Self {
        debug_assert!(
            !Self::is_pointer_word(words[1]) || words[1] % 8 == 0,
            "an even non-zero +8 word is a pointer, and every entity is 8-aligned"
        );
        Value {
            w0: words[0],
            w8: words[1],
        }
    }

    /// The box as the two machine words a slot holds it in, `+0` first.
    /// This is the form the store barrier publishes and every walker reads
    /// back.
    #[inline]
    pub(crate) fn into_words(self) -> [u64; 2] {
        [self.w0, self.w8]
    }

    /// Whether a box's `+8` word, read on its own, is the counted pointer:
    /// non-zero with bit 0 clear. The one test a collector makes, on the
    /// one word it loads, and the test a container selects the tag word
    /// by ([`crate::array::entry`]); it lives here because the arithmetic
    /// is this type's layout contract, and a copy at a walker cannot be
    /// told when the contract moves.
    #[inline]
    pub(crate) fn is_pointer_word(w8: u64) -> bool {
        w8 != 0 && w8 & TAG_WORD_BIT == 0
    }

    /// The tag word: `w8` on the immediate arm, `w0` on the pointer arm.
    /// One test and one `cmov` on words a consumer of the value loads
    /// anyway; `(0, 0)` yields zero, whose tag byte is `Null`. A container
    /// reads its own bits above the tag byte off this word
    /// ([`crate::array::entry::Entry::link`]); a select rather than an
    /// index into the pair, for what the index compiled to on the chain
    /// walk (`dev/BENCHMARKS.md`, "S48.2 the box's price after the
    /// relayout").
    #[inline]
    pub(crate) fn tag_word(&self) -> u64 {
        if self.w8 & TAG_WORD_BIT != 0 {
            self.w8
        } else {
            self.w0
        }
    }

    /// The decode: the tag byte of whichever word is the tag word. Aborts
    /// on a code no tag has, in every build — such a byte is corruption
    /// rather than a value, and reading it as one would follow a pointer
    /// that is not there.
    #[inline]
    pub fn tag(&self) -> Tag {
        match (self.tag_word() >> TAG_CODE_SHIFT) as u8 {
            0 => Tag::Null,
            1 => Tag::False,
            2 => Tag::True,
            3 => Tag::Int,
            4 => Tag::Float,
            5 => Tag::String,
            6 => Tag::Array,
            7 => Tag::Object,
            8 => Tag::Resource,
            9 => Tag::Reference,
            _ => corrupt_tag_code(),
        }
    }

    /// The same box with bits 16–63 of its tag word cleared.
    ///
    /// A container that keeps state of its own in those bits — the array
    /// table keeps an entry's chain link there — hands the box out through
    /// this, so the link never travels in a copy and lands in another
    /// container's entry (`array/entry.rs`). A null read out of a
    /// container is `(0, 0x0001)` afterwards, the second spelling of null
    /// every null test accepts.
    #[inline]
    pub(crate) fn without_container_bits(mut self) -> Self {
        if self.w8 & TAG_WORD_BIT != 0 {
            self.w8 &= TAG_WORD_MASK;
        } else {
            self.w0 &= TAG_WORD_MASK;
        }

        self
    }

    /// Uninitialized property slot? Generated code tests this on a
    /// tracked property read and throws `Error` when set; `isset()`
    /// reads it inverted. One test of `+8`, sound on both arms: an
    /// 8-aligned pointer has bits 0–2 clear.
    #[inline]
    pub fn is_undef(&self) -> bool {
        self.w8 & TAG_WORD_UNDEF != 0
    }

    /// The `int` payload. Asserted `Int` in a debug build; in release the
    /// `+0` word of whatever the box is.
    #[inline]
    pub fn as_int(&self) -> i64 {
        debug_assert!(self.is_int());
        self.w0 as i64
    }

    /// The `float` payload, under the same rule as [`Self::as_int`].
    #[inline]
    pub fn as_float(&self) -> f64 {
        debug_assert_eq!(self.tag(), Tag::Float);
        f64::from_bits(self.w0)
    }

    /// The entity header behind a pointer-arm box.
    #[inline]
    pub fn entity_ptr(&self) -> *mut RcHeader {
        debug_assert!(self.is_pointer());
        self.w8 as *mut RcHeader
    }

    /// The entity header behind a pointer-arm box, and null for any other
    /// value — the form a slot walker wants, where a non-entity is "nothing
    /// to release".
    #[inline]
    pub fn entity_or_null(&self) -> *mut RcHeader {
        if self.is_pointer() {
            self.entity_ptr()
        } else {
            std::ptr::null_mut()
        }
    }

    /// PHP `null`: the barrier's `(0, 0)` and a container's `(0, 0x0001)`
    /// alike, in one test of `+8`. An undef slot (`0x0003`) answers false
    /// — reading one is an error, not a null ([`Self::is_undef`]) — and so
    /// does every other tag word and every pointer.
    #[inline]
    pub fn is_null(&self) -> bool {
        self.w8 & !TAG_WORD_BIT == 0
    }

    /// PHP `int`, without reading the payload: the low sixteen bits of
    /// `+8` against the Int tag word.
    #[inline]
    pub fn is_int(&self) -> bool {
        self.w8 & TAG_WORD_MASK == tag_word_of(Tag::Int)
    }

    /// The box is on the pointer arm — the fast-path retain and release
    /// ask exactly this, and it is one test of `+8`.
    #[inline]
    pub fn is_pointer(&self) -> bool {
        Self::is_pointer_word(self.w8)
    }

    /// The truth test for the tags whose truth is fixed: null and false
    /// are false, true is true, and every other tag answers `None` because
    /// its truth is the payload's or the entity's. Reads `+8` alone: the
    /// arm test, then a byte match on the tag — the least costly of three
    /// forms measured (`dev/BENCHMARKS.md`, "S48.2 the box's price after
    /// the relayout"). What an undef slot answers is unspecified: the
    /// undef test runs before any truth test can.
    #[inline]
    pub fn is_truthy_tag(&self) -> Option<bool> {
        const NULL: u8 = Tag::Null as u8;
        const FALSE: u8 = Tag::False as u8;
        const TRUE: u8 = Tag::True as u8;
        let w8 = self.w8;
        if w8 & TAG_WORD_BIT == 0 {
            return if w8 == 0 { Some(false) } else { None };
        }

        match (w8 >> TAG_CODE_SHIFT) as u8 {
            NULL | FALSE => Some(false),
            TRUE => Some(true),
            _ => None,
        }
    }
}

/// A tag byte no tag has was read: the box is corrupt, and there is no
/// frame to raise through from inside a type test.
#[cold]
#[inline(never)]
fn corrupt_tag_code() -> ! {
    std::process::abort()
}

/// Retain the entity behind a box copy, if any.
///
/// # Safety
/// A pointer-arm `v` must point to a live entity.
#[inline]
pub unsafe fn value_retain(v: &Value) {
    if v.is_pointer() {
        unsafe { ll_retain(v.entity_ptr()) };
    }
}

/// Release the entity behind a dying box. Returns `true` when the
/// entity died with it and the caller must run its teardown.
///
/// # Safety
/// A pointer-arm `v` must point to a live entity.
#[inline]
pub unsafe fn value_release(v: &Value) -> bool {
    if v.is_pointer() {
        unsafe { ll_release(v.entity_ptr()) }
    } else {
        false
    }
}

#[cfg(test)]
mod tests;
