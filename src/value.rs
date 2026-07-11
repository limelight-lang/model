//! The 16-byte Box: tagged value for dynamically-typed storage sites.
//!
//! Layout per `rfc/model/values.md`: payload (8) + type tag (1) +
//! flags (1) + 6 reserved. Unboxed representations (raw i64/f64/ptr
//! for declared types) are a compiler contract and have no runtime
//! type here. `false` and `true` are separate tags so truth tests
//! never read the payload; there is deliberately no `undef` tag.

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
