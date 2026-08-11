use super::*;

fn retain(header: &mut RcHeader) {
    unsafe { ll_retain(header) }
}

fn release(header: &mut RcHeader) -> bool {
    unsafe { ll_release(header) }
}

/// The memory category decides whether the counter moves at all: a
/// heap entity counts and reports its death, an arena one is freed
/// by the reset instead, an immortal one is never touched, and a COW
/// entity counts in the arena too, the count being what separates
/// its copies. At the ceiling the counter stops moving, which
/// accepts a leak rather than the free of an entity somebody still
/// holds.
mod what_the_category_decides {
    use super::*;

    #[test]
    fn heap_entity_counts_and_dies() {
        let mut header = RcHeader::new(MemoryCategory::GcHeap, 0);
        retain(&mut header);
        assert_eq!(header.refcount, 2);
        assert!(!release(&mut header));
        assert!(release(&mut header), "second release must report death");
    }

    #[test]
    fn arena_object_is_not_counted() {
        let mut header = RcHeader::new(MemoryCategory::RequestArena, 0);
        retain(&mut header);
        assert_eq!(header.refcount, 1, "arena objects skip counting");
        assert!(!release(&mut header));
        assert_eq!(header.refcount, 1);
    }

    #[test]
    fn immortal_is_never_touched() {
        let mut header = RcHeader::new(MemoryCategory::Immortal, COW);
        retain(&mut header);
        assert!(!release(&mut header));
        assert_eq!(header.refcount, 1);
    }

    #[test]
    fn cow_in_arena_still_counts() {
        // rfc/model/values.md: refcount is part of COW value semantics,
        // maintained in every category; zero in an arena is not a death.
        let mut header = RcHeader::new(MemoryCategory::RequestArena, COW);
        retain(&mut header);
        assert_eq!(header.refcount, 2, "COW entities count everywhere");
        assert!(!release(&mut header));
        assert!(
            !release(&mut header),
            "zero in arena: no free, reset reclaims"
        );
        assert_eq!(header.refcount, 0);
    }

    #[test]
    fn cow_on_heap_dies_at_zero() {
        let mut header = RcHeader::new(MemoryCategory::GcHeap, COW);
        assert!(release(&mut header));
    }

    /// With `checked-refcount`, a count at the ceiling stops moving and
    /// the entity is effectively immortal. Without the guard the `+= 1`
    /// wraps to zero, and the next release frees an entity that is still
    /// referenced — the failure this trades a leak for.
    ///
    /// Only meaningful with the feature on:
    /// `cargo test --features checked-refcount`.
    #[cfg(feature = "checked-refcount")]
    #[test]
    fn a_saturated_refcount_never_wraps_to_zero() {
        let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
        h.refcount = u32::MAX;

        unsafe { ll_retain(&mut h) };
        assert_eq!(h.refcount, u32::MAX, "saturated, not wrapped");

        // And it stays alive: a release from the ceiling must not be able
        // to reach zero in one step either.
        let died = unsafe { ll_release(&mut h) };
        assert!(
            !died,
            "an entity at the ceiling does not die of one release"
        );
    }
}

/// Generated code stamps these bit positions, so the layout is a
/// contract with `rfc/model/lowering.md` rather than an internal
/// choice — including that `Object` is the zero kind, which is what
/// let the object bit go.
mod the_header_the_compiler_shares {
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
        assert_eq!(GC_STATE_MASK, 0b11 << 2, "gc state: bits 2-3");
        assert_eq!(
            ARENA_RESET_MARK,
            1 << 2,
            "reset mark borrows gc-state bit 2"
        );
        assert_eq!(CYCLE_COLLECTOR_BUFFERED, 1 << 6);
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
}

/// The epoch stamp sits in a byte of its own so that the collector
/// can write it while a mutator stores the counter half, and neither
/// retain, release nor the death branch may carry the other half
/// with them. Only rc-walk stamps an epoch, so the group is that
/// configuration's whole.
#[cfg(feature = "rc-walk")]
mod the_half_of_the_header_the_collector_claims {
    use super::*;

    /// The collector's one header claim: the epoch byte at header
    /// byte 6 — byte-addressable, so the collector writes it with a
    /// plain byte store while the mutator stores the whole word
    /// (`rfc/model/gc/rc-walk.md`, "The one header byte"; the condemned
    /// byte at 24-31 retired by the eager-death amendment, 2026-07-27).
    #[test]
    fn the_epoch_byte_sits_at_header_byte_six() {
        assert_eq!(EPOCH_BYTE_MASK, 0xFF << 16, "epoch byte: flags bits 16-23");
        // The byte may not reach into the mutator-owned low half.
        let low_half = MEMORY_CATEGORY_MASK
            | GC_STATE_MASK
            | (0b11 << CYCLE_COLLECTOR_COLOR_SHIFT)
            | CYCLE_COLLECTOR_BUFFERED
            | HAS_WEAK_REFERENCES
            | DESTRUCTOR_PENDING
            | DESTRUCTOR_RAN
            | COW
            | IS_ESCAPEE
            | ENTITY_KIND_MASK;
        assert_eq!(EPOCH_BYTE_MASK & low_half, 0);

        // Byte addressability on a real header: a byte store at offset
        // 6 lands exactly on the mask (little-endian, enforced at
        // compile time).
        let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
        let p = &mut h as *mut RcHeader as *mut u8;
        unsafe {
            p.add(6).write(3);
        }

        assert_eq!(h.flags & EPOCH_BYTE_MASK, 3 << EPOCH_BYTE_SHIFT);
        assert_eq!(h.refcount, 1, "the refcount bytes are untouched");
    }

    /// The narrow mutator (rfc amendment, 2026-07-27): retain and a
    /// non-final release store only the counter half — the epoch stamp
    /// passes through untouched.
    #[test]
    fn retain_and_release_leave_the_flags_half_alone() {
        let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
        h.flags |= 7 << EPOCH_BYTE_SHIFT;

        retain(&mut h);
        assert_eq!(h.refcount, 2);
        assert_eq!(
            h.flags & EPOCH_BYTE_MASK,
            7 << EPOCH_BYTE_SHIFT,
            "the stamp survives"
        );

        assert!(!release(&mut h));
        assert_eq!(h.refcount, 1);
        assert_eq!(h.flags & EPOCH_BYTE_MASK, 7 << EPOCH_BYTE_SHIFT);
    }

    /// Eager death (rfc amendment, 2026-07-27, superseding F5's
    /// deferral): the release reaching zero reports the death — there
    /// is no condemned test on the death branch, and the flags half is
    /// left exactly as loaded.
    #[test]
    fn every_death_takes_the_ordinary_path() {
        let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
        h.flags |= 7 << EPOCH_BYTE_SHIFT;
        assert!(release(&mut h), "the death is reported, stamped or not");
        assert_eq!(h.refcount, 0);
        assert_eq!(
            h.flags & EPOCH_BYTE_MASK,
            7 << EPOCH_BYTE_SHIFT,
            "flags untouched"
        );
    }
}
