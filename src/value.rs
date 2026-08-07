//! The 16-byte Box: tagged value for dynamically-typed storage sites.
//!
//! Layout per `rfc/model/values.md`: payload (8) + type tag (1) +
//! flags (1) + 6 reserved. Unboxed representations (raw i64/f64/ptr
//! for declared types) are a compiler contract and have no runtime
//! type here. `false` and `true` are separate tags so truth tests
//! never read the payload; there is deliberately no `undef` *tag* —
//! the uninitialized state of a Box property slot is a `flags` bit
//! ([`VALUE_UNDEF`]), not a type.

use crate::refcount::{RcHeader, ll_release, ll_retain};

/// Type tags. Pointer-carrying tags point to entities beginning with
/// `RcHeader`.
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

/// Box flag: the payload is a counted entity pointer. Duplicates what
/// the tag implies so retain/release on Box copy is one bit test, no
/// tag decoding (`rfc/model/values.md`).
pub const VALUE_REFCOUNTED: u8 = 1 << 0;

/// Box flag: this is an **uninitialized** property slot (`mixed` /
/// untyped, declared without a default). Reading it throws `Error`, per
/// PHP's uninitialized-typed-property semantics; any store clears it,
/// because the barrier writes all 16 bytes ([`crate::memory::barrier`]);
/// `unset()` restores it (a [`Value::undef`] store, then `drop_ref` of
/// the displaced entity). It is confined to property slots and never set
/// on a Box in a local, parameter, return, array element or reference
/// box, so it cannot flow into a value context — unlike Zend's
/// `IS_UNDEF` (`rfc/model/values.md`). Raw typed slots have no room for
/// it and use the per-object init bitmap instead. The factory stamps it
/// over the class's `undef_runs` after the zero-fill
/// ([`crate::class::Class::undef_runs`]).
pub const VALUE_UNDEF: u8 = 1 << 1;

/// Box flag: the slot is mid-write — the `rc-satb` concurrent marker's
/// torn-16-byte-read lock (`rfc/model/gc/satb.md`). `store_box` on that
/// strategy sets it before the payload store and clears it with the
/// final tag store; the marker skips a slot whose flag is set. Every
/// other build leaves the bit permanently clear. Reserved here until
/// rc-satb lands; pinned now so the flags byte's assignments are fixed.
pub const VALUE_WRITING: u8 = 1 << 2;

/// The Box. `#[repr(C)]`: generated code addresses the fields by
/// offset (payload +0, tag +8, flags +9).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Value {
    payload: u64,
    tag: Tag,
    flags: u8,
    _reserved: [u8; 6],
}

impl Value {
    #[inline]
    const fn raw(payload: u64, tag: Tag, flags: u8) -> Self {
        Value {
            payload,
            tag,
            flags,
            _reserved: [0; 6],
        }
    }

    pub const fn null() -> Self {
        Self::raw(0, Tag::Null, 0)
    }

    /// The uninitialized property slot: all-zero but for [`VALUE_UNDEF`]
    /// (an all-zero Box is `null`, not undefined). This is what the
    /// factory stamps and what an `unset()` stores back.
    pub const fn undef() -> Self {
        Self::raw(0, Tag::Null, VALUE_UNDEF)
    }

    pub const fn bool(v: bool) -> Self {
        Self::raw(0, if v { Tag::True } else { Tag::False }, 0)
    }

    pub const fn int(v: i64) -> Self {
        Self::raw(v as u64, Tag::Int, 0)
    }

    pub fn float(v: f64) -> Self {
        Self::raw(v.to_bits(), Tag::Float, 0)
    }

    /// A counted entity reference (object, string, array, …).
    pub fn entity(tag: Tag, ptr: *mut RcHeader) -> Self {
        debug_assert!(!ptr.is_null());
        Self::raw(ptr as u64, tag, VALUE_REFCOUNTED)
    }

    #[inline]
    pub fn tag(&self) -> Tag {
        self.tag
    }

    /// One bit test — the fast path retain/release asks exactly this.
    #[inline]
    pub fn is_refcounted(&self) -> bool {
        self.flags & VALUE_REFCOUNTED != 0
    }

    /// The same test for a caller holding the Box's **second raw word**
    /// rather than a `Value` — a walker reading slot memory as two `u64`s
    /// because it may be racing a mutator and must read atomically.
    ///
    /// It lives here rather than at the walkers because it is this type's
    /// layout contract and nothing else: the second word is
    /// `tag | flags | reserved`, so the flags byte is bits 8-15. Written
    /// out at a walker, that arithmetic is a copy of the contract in a
    /// module that cannot be told when the contract moves, and there were
    /// two such copies before this existed.
    #[inline]
    pub(crate) fn refcounted_in_meta_word(meta: u64) -> bool {
        (meta >> 8) as u8 & VALUE_REFCOUNTED != 0
    }

    /// The Box as the two machine words a slot holds it in: payload first,
    /// then `tag | flags << 8` with the reserved bytes above them. This is
    /// the form the store barrier publishes and every walker reads back,
    /// and it lives here for the reason
    /// [`refcounted_in_meta_word`](Self::refcounted_in_meta_word) does —
    /// the arithmetic is this type's layout contract, and a copy of it in
    /// another module cannot be told when the contract moves.
    #[inline]
    pub(crate) fn into_words(self) -> [u64; 2] {
        // `repr(C)`, sixteen bytes, every field a plain integer: the pair
        // is the same bits under another name.
        unsafe { core::mem::transmute::<Value, [u64; 2]>(self) }
    }

    /// The same Box with its reserved bytes cleared.
    ///
    /// A container that keeps state of its own in those bytes — the array
    /// table keeps an entry's chain link there — hands the Box out through
    /// this, so the link never travels in a copy and lands in another
    /// container's entry (`array/entry.rs`).
    #[inline]
    pub(crate) fn without_reserved(mut self) -> Self {
        self._reserved = [0; 6];
        self
    }

    /// Uninitialized property slot? Generated code tests this on a
    /// tracked property read and throws `Error` when set; `isset()`
    /// reads it inverted.
    #[inline]
    pub fn is_undef(&self) -> bool {
        self.flags & VALUE_UNDEF != 0
    }

    #[inline]
    pub fn as_int(&self) -> i64 {
        debug_assert_eq!(self.tag, Tag::Int);
        self.payload as i64
    }

    #[inline]
    pub fn as_float(&self) -> f64 {
        debug_assert_eq!(self.tag, Tag::Float);
        f64::from_bits(self.payload)
    }

    /// The entity header behind a refcounted Box.
    #[inline]
    pub fn entity_ptr(&self) -> *mut RcHeader {
        debug_assert!(self.is_refcounted());
        self.payload as *mut RcHeader
    }

    /// Truth test never reads the payload for null/false/true.
    #[inline]
    pub fn is_truthy_tag(&self) -> Option<bool> {
        match self.tag {
            Tag::Null | Tag::False => Some(false),
            Tag::True => Some(true),
            _ => None,
        }
    }
}

/// Retain the entity behind a Box copy, if any.
///
/// # Safety
/// A refcounted `v` must point to a live entity.
#[inline]
pub unsafe fn value_retain(v: &Value) {
    if v.is_refcounted() {
        unsafe { ll_retain(v.entity_ptr()) };
    }
}

/// Release the entity behind a dying Box. Returns `true` when the
/// entity died with it and the caller must run its teardown.
///
/// # Safety
/// A refcounted `v` must point to a live entity.
#[inline]
pub unsafe fn value_release(v: &Value) -> bool {
    if v.is_refcounted() {
        unsafe { ll_release(v.entity_ptr()) }
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refcount::MemoryCategory;

    #[test]
    fn box_is_16_bytes_with_fixed_offsets() {
        assert_eq!(size_of::<Value>(), 16);
        assert_eq!(core::mem::offset_of!(Value, payload), 0);
        assert_eq!(core::mem::offset_of!(Value, tag), 8);
        assert_eq!(core::mem::offset_of!(Value, flags), 9);
    }

    /// The flags byte's bit assignments are ABI (generated code tests
    /// them by constant), so the values are pinned, not just the names.
    #[test]
    fn flag_bits_are_pinned_and_undef_is_not_null() {
        assert_eq!(VALUE_REFCOUNTED, 1);
        assert_eq!(VALUE_UNDEF, 2);
        assert_eq!(VALUE_WRITING, 4);

        let u = Value::undef();
        assert!(u.is_undef());
        assert!(!u.is_refcounted(), "undef is never traced or counted");
        assert!(
            !Value::null().is_undef(),
            "an all-zero Box is null, not undef"
        );
    }

    #[test]
    fn scalar_round_trips() {
        assert_eq!(Value::int(-42).as_int(), -42);
        assert_eq!(Value::float(2.5).as_float(), 2.5);
        assert_eq!(Value::int(i64::MIN).as_int(), i64::MIN, "full 64-bit ints");
    }

    #[test]
    fn truth_never_reads_payload_for_bool_tags() {
        assert_eq!(Value::null().is_truthy_tag(), Some(false));
        assert_eq!(Value::bool(false).is_truthy_tag(), Some(false));
        assert_eq!(Value::bool(true).is_truthy_tag(), Some(true));
        assert_eq!(Value::int(0).is_truthy_tag(), None, "ints decode payload");
    }

    #[test]
    fn entity_boxes_count_scalars_do_not() {
        let mut e = RcHeader::new(MemoryCategory::GcHeap, 0);
        let v = Value::entity(Tag::Object, &mut e);
        assert!(v.is_refcounted());
        unsafe { value_retain(&v) };
        assert_eq!(e.refcount, 2);
        assert!(!unsafe { value_release(&v) });
        assert_eq!(e.refcount, 1);

        let i = Value::int(7);
        assert!(!i.is_refcounted());
        assert!(!unsafe { value_release(&i) }, "scalars are no-ops");
    }
}
