//! Generated code stamps these bit positions, so the layout is a
//! contract with `rfc/model/lowering.md` rather than an internal
//! choice — including that `Object` is the zero kind, which is what
//! let the object bit go.

use super::*;

#[test]
fn header_is_8_bytes_at_offset_zero() {
    assert_eq!(size_of::<RcHeader>(), 8);
    // 8, not 4: the header is published as one 8-byte word and the
    // wide helpers access it as one, which demands an 8-aligned
    // address. Every real slot already satisfied it.
    assert_eq!(align_of::<RcHeader>(), 8);
    assert_eq!(core::mem::offset_of!(RcHeader, refcount), 0);
    assert_eq!(core::mem::offset_of!(RcHeader, flags), 4);
}

/// The flags word layout is a contract with the compiler and the C
/// mirror in `rfc/model/lowering.md`: generated code stamps these exact
/// bit positions. Pin them so the 2026-07-22 compaction cannot drift.
#[test]
fn flags_layout_is_the_compacted_design() {
    assert_eq!(MEMORY_CATEGORY_MASK, 0b11, "category: bits 0-1");
    assert_eq!(ARENA_RESET_MARK, 1 << 2, "arena reset mark: bit 2");
    assert_eq!(HAS_WEAK_REFERENCES, 1 << 7);
    assert_eq!(DESTRUCTOR_PENDING, 1 << 8);
    assert_eq!(DESTRUCTOR_RAN, 1 << 9);
    assert_eq!(COW, 1 << 10);
    assert_eq!(STRING_OUT_OF_LINE, 1 << 15, "string layout: bit 15");
    assert_eq!(
        STRING_OUT_OF_LINE & ENTITY_KIND_MASK,
        0,
        "a wider kind field would take the layout bit"
    );
    assert_eq!(IS_ESCAPEE, 1 << 11);
    assert_eq!(ENTITY_KIND_SHIFT, 12);
    assert_eq!(ENTITY_KIND_MASK, 0b111 << 12, "entity kind: bits 12-14");

    // Bits 16 and above are unclaimed until S31 lays the collector's
    // fields there. Nothing may drift into them meanwhile, which is what
    // this asserts: every constant above is below bit 16.
    let claimed = MEMORY_CATEGORY_MASK
        | ARENA_RESET_MARK
        | HAS_WEAK_REFERENCES
        | DESTRUCTOR_PENDING
        | DESTRUCTOR_RAN
        | COW
        | IS_ESCAPEE
        | ENTITY_KIND_MASK
        | STRING_OUT_OF_LINE;
    assert_eq!(claimed & 0xFFFF_0000, 0, "nothing claims bits 16-31");
}

/// `Object` is the zero kind field, so a header built with no kind bits
/// reads as an object — the property the whole `ENTITY_OBJECT`-bit
/// removal rests on — while every other kind sits inside the field.
#[test]
fn object_is_the_zero_kind() {
    assert_eq!(EntityKind::Object.to_flags(), 0);
    assert!(is_object(0));
    assert!(
        is_object(MemoryCategory::GcHeap as u32 | COW),
        "non-kind bits do not confuse it"
    );

    for kind in [
        EntityKind::String,
        EntityKind::Array,
        EntityKind::Reference,
        EntityKind::Box,
        EntityKind::WeakRef,
        EntityKind::Lazy,
    ] {
        let bits = kind.to_flags();
        assert_ne!(bits, 0, "{kind:?} is a non-zero kind");
        assert_eq!(
            bits & !ENTITY_KIND_MASK,
            0,
            "{kind:?} lands inside the kind field"
        );
        assert!(!is_object(bits), "{kind:?} is not an object");
    }
}
