//! Generated code stamps these bit positions, so the layout is a
//! contract with `rfc/model/lowering.md` rather than an internal
//! choice — including that `Object` is the zero kind, which is what
//! let the object bit go.

use super::*;

#[test]
fn header_is_8_bytes_at_offset_zero() {
    assert_eq!(size_of::<RcHeader>(), 8);
    // 8, not 4, since the rc-walk groundwork: the header is published
    // and (under rc-walk) accessed as one 8-byte word, which demands
    // an 8-aligned address. Every real slot already satisfied it.
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
    // The string layout bit deliberately overlaps the candidate
    // index's lowest bit, and the whole safety of that rests on two
    // facts nothing else asserts. `encode_index(0)` is `1 << 15`, so
    // a string admitted to the buffer at position zero would acquire
    // the layout bit and every later byte access would read its hash
    // field as a payload pointer.
    assert_eq!(STRING_OUT_OF_LINE, 1 << CANDIDATE_INDEX_SHIFT);
    assert_eq!(
        STRING_OUT_OF_LINE & ENTITY_KIND_MASK,
        0,
        "a wider kind field would take the layout bit"
    );
    assert_eq!(
        CANDIDATE_KINDS & (1 << EntityKind::String as u32),
        0,
        "String must never take a candidate index: bit 15 is its layout"
    );
    assert_eq!(IS_ESCAPEE, 1 << 11);
    assert_eq!(ENTITY_KIND_SHIFT, 12);
    assert_eq!(ENTITY_KIND_MASK, 0b111 << 12, "entity kind: bits 12-14");
    assert_eq!(CANDIDATE_INDEX_SHIFT, 15);
    assert_eq!(
        CANDIDATE_INDEX_MASK,
        0x0001_FFFF << 15,
        "candidate index: bits 15-31, 17 wide"
    );
    assert_eq!(CANDIDATE_INDEX_MAX, 131_070);

    // The kind field and the candidate index must not overlap, and the
    // whole word must stay 32 bits wide.
    assert_eq!(
        ENTITY_KIND_MASK & CANDIDATE_INDEX_MASK,
        0,
        "kind and index are disjoint"
    );
    assert_eq!(
        CANDIDATE_INDEX_MASK >> 15 << 15,
        CANDIDATE_INDEX_MASK,
        "index reaches the top bit"
    );
    assert_eq!(
        0x8000_0000u32 & CANDIDATE_INDEX_MASK,
        0x8000_0000,
        "and includes bit 31"
    );
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
