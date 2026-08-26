//! Generated code stamps these bit positions, so the layout is a
//! contract with `rfc/model/lowering.md` rather than an internal
//! choice — including that `Object` is the zero kind, which is what
//! let the object bit go.

use super::*;

/// Every kind in code order. The predicates below are checked against
/// this list rather than against the masks they use, so a code moved
/// without its range being reconsidered fails here instead of at the
/// first collection.
const ALL_KINDS: [EntityKind; 8] = [
    EntityKind::Object,
    EntityKind::Lazy,
    EntityKind::Array,
    EntityKind::Reference,
    EntityKind::String,
    EntityKind::StringDynamic,
    EntityKind::Box,
    EntityKind::WeakRef,
];

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
/// bit positions, and `rfc/model/classes.md`'s "Flags layout" is the
/// table both sides transcribe.
#[test]
fn flags_layout_matches_the_normative_table() {
    assert_eq!(MEMORY_CATEGORY_MASK, 0b11, "category: bits 0-1");
    assert_eq!(ENTITY_KIND_SHIFT, 2);
    assert_eq!(ENTITY_KIND_MASK, 0b1111 << 2, "entity kind: bits 2-5");
    assert_eq!(COW, 1 << 6);
    assert_eq!(ARENA_RESET_MARK, 1 << 7, "arena reset mark: bit 7");
    assert_eq!(ACYCLIC_GATE, 1 << 8);
    assert_eq!(OWNERSHIP_MARK, 1 << 9);
    assert_eq!(ENROLLED, 1 << 10);
    assert_eq!(IS_ESCAPEE, 1 << 11);
    assert_eq!(HAS_WEAK_REFERENCES, 1 << 12);
    assert_eq!(DESTRUCTOR_PENDING, 1 << 13);
    assert_eq!(DESTRUCTOR_RAN, 1 << 14);

    let claimed = MEMORY_CATEGORY_MASK
        | ENTITY_KIND_MASK
        | COW
        | ARENA_RESET_MARK
        | ACYCLIC_GATE
        | OWNERSHIP_MARK
        | ENROLLED
        | IS_ESCAPEE
        | HAS_WEAK_REFERENCES
        | DESTRUCTOR_PENDING
        | DESTRUCTOR_RAN;
    // The enrolment gate reads bits 0-1, 5 and 8-10 as one mask, so a
    // constant landing on any of them would make the gate refuse
    // candidates for a reason the design does not have. The clauses
    // themselves are `the_enrolment_gate`; this pins the positions.
    assert_eq!(
        ENROLMENT_GATE_MASK,
        MEMORY_CATEGORY_MASK | (1 << 5) | ACYCLIC_GATE | OWNERSHIP_MARK | ENROLLED,
        "the gate covers the category, the kind's top bit and the three marks"
    );
    // Bit 15 came free when the string's layout became a kind code, and
    // the normative table now calls it free.
    assert_eq!(claimed & (1 << 15), 0, "bit 15 is free");
    // Bits 16 and above are unclaimed until S31 lays the collector's
    // epoch, maturation age and reserve there. Nothing may drift into
    // them meanwhile, which is what this asserts.
    assert_eq!(claimed & 0xFFFF_0000, 0, "nothing claims bits 16-31");
}

/// The three questions `rfc/model/classes.md` turns into mask tests. Each
/// is checked with the non-kind bits set, because a predicate that read
/// the whole word rather than the field would agree on a bare kind and
/// disagree on a live header.
#[test]
fn each_predicate_answers_for_the_kind_alone() {
    for kind in ALL_KINDS {
        let flags = MemoryCategory::Immortal as u32
            | kind.to_flags()
            | COW
            | ARENA_RESET_MARK
            | IS_ESCAPEE
            | HAS_WEAK_REFERENCES
            | DESTRUCTOR_PENDING
            | DESTRUCTOR_RAN
            | ACYCLIC_GATE
            | OWNERSHIP_MARK
            | ENROLLED;

        assert_eq!(
            kind_may_close_a_cycle(flags),
            kind.closes_a_ring(),
            "{kind:?}: the mask and the classification have to agree"
        );
        assert_eq!(
            carries_a_class_word(flags),
            matches!(kind, EntityKind::Object | EntityKind::Lazy),
            "{kind:?}: a class word at +8 belongs to the object kinds"
        );
        assert_eq!(
            is_string(flags),
            matches!(kind, EntityKind::String | EntityKind::StringDynamic),
            "{kind:?}: both string layouts answer, and only they"
        );
        assert_eq!(is_object(flags), kind == EntityKind::Object);
    }
}

/// Codes 0-7 are held for kinds that can close a ring and four of them
/// stand free, so that adding such a kind is a code assignment rather
/// than a renumbering. Four codes rather than none is the whole content
/// of the reserve: a full range refuses the next kind for ever and
/// reports nothing (`rfc/model/gc/cycle/questions.md`, Y6).
#[test]
fn the_ring_reserve_holds_the_low_eight_codes_and_four_stand_free() {
    for kind in ALL_KINDS {
        assert_eq!(
            (kind as u32) < 8,
            kind.closes_a_ring(),
            "{kind:?}: the reserved range and the classification disagree"
        );
    }

    let taken = ALL_KINDS.iter().filter(|k| k.closes_a_ring()).count();
    assert_eq!(
        8 - taken,
        4,
        "four ring-closing codes stand free; a full reserve reserves nothing"
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

    for kind in ALL_KINDS {
        if kind == EntityKind::Object {
            continue;
        }

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
