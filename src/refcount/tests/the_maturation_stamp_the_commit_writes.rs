//! The stamp byte the collector's commit writes, read back through the pair
//! of accessors that own it.
//!
//! The header is a local rather than a heap entity: what these cases are about
//! is the packing and the width, and both are the same wherever the eight
//! bytes stand. What a commit stamps, and which components it stamps, is
//! `cycle::finalization`'s.
//!
//! **One `*mut` per case, made once and used for the reads as well.** Every
//! accessor here reaches its byte through an atomic, and an atomic load
//! retags for shared read-write, which a pointer taken with `&raw const`
//! cannot grant — Miri rejects the read, and the reader looks like the
//! defect (`dev/POSTMORTEM.md`, "an atomic read needs write provenance").

use super::*;

/// The reserve above the stamp, bits 20-23, which no field claims yet. A
/// commit that stored the whole byte would carry these down with it, and the
/// day the reserve has a writer that store would be a lost field rather than a
/// visible one.
const RESERVE: u32 = 0b1111 << 20;

/// Every epoch against every age, through the byte and back. The pair is one
/// field in two parts, so a packing that crossed them would answer some
/// combinations and not others.
#[test]
fn every_stamp_reads_back_as_it_was_written() {
    for epoch in 0..=MATURATION_EPOCH_MASK >> 16 {
        for age in 0..=MATURATION_AGE_MAX {
            let mut h = RcHeader::new(MemoryCategory::GcHeap, COW);
            let written = MaturationStamp { epoch, age };

            let header: *mut RcHeader = &raw mut h;
            unsafe { write_maturation_stamp(header, written) };

            assert_eq!(unsafe { read_maturation_stamp(header) }, written);
            assert_eq!(h.refcount, 1, "the counter is not in the stamp's reach");
            assert_eq!(
                h.flags & 0x0000_FFFF,
                MemoryCategory::GcHeap as u32 | COW,
                "and neither is the mutator's half"
            );
        }
    }
}

/// The stamp is written as a read-modify-write of byte 6, so the reserve
/// beside it stands. The value is set through the field rather than through an
/// accessor because the reserve has none: nothing claims those bits yet.
#[test]
fn a_stamp_leaves_the_rest_of_its_byte_alone() {
    let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
    h.flags |= RESERVE;

    let header: *mut RcHeader = &raw mut h;
    unsafe { write_maturation_stamp(header, MaturationStamp { epoch: 2, age: 3 }) };

    assert_eq!(h.flags & RESERVE, RESERVE, "the reserve survived the stamp");
    assert_eq!(
        unsafe { read_maturation_stamp(header) },
        MaturationStamp { epoch: 2, age: 3 }
    );
}

/// An entity arrives unstamped, and a slot lived in twice arrives unstamped
/// too: the publication writes all eight bytes, so the previous occupant's
/// stamp goes down with its flags. The prune reads age 0 as no claim at all,
/// which is what makes that the safe direction.
#[test]
fn a_published_entity_carries_no_stamp_of_its_own_slot() {
    let mut slot = RcHeader::new(MemoryCategory::GcHeap, 0);
    let header: *mut RcHeader = &raw mut slot;
    unsafe {
        write_maturation_stamp(header, MaturationStamp { epoch: 3, age: 3 });
        publish_header(header, RcHeader::new(MemoryCategory::GcHeap, 0));
    }

    assert_eq!(
        unsafe { read_maturation_stamp(header) },
        MaturationStamp { epoch: 0, age: 0 },
        "the publication left no stamp behind"
    );
}

/// Byte 7's free bits, 25-31, which no field claims yet. Read with
/// [`RESERVE`]'s reasoning: the day one of them has a writer, a store of the
/// whole byte would be a lost field rather than a visible one.
const BYTE_SEVEN_FREE: u32 = 0b111_1111 << 25;

/// The reconciliation's bit is written as a read-modify-write of byte 7, so
/// the free bits beside it stand — both while the entity is in hand and after
/// it is given back. The value is set through the field, the free bits having
/// no accessor.
#[test]
fn taking_an_entity_in_hand_leaves_the_rest_of_its_byte_alone() {
    let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
    h.flags |= BYTE_SEVEN_FREE;
    let header: *mut RcHeader = &raw mut h;

    unsafe { set_reconciling(header, true) };
    assert!(unsafe { is_reconciling(header) });
    assert_eq!(
        h.flags & BYTE_SEVEN_FREE,
        BYTE_SEVEN_FREE,
        "the free bits went down with the take"
    );

    unsafe { set_reconciling(header, false) };
    assert!(!unsafe { is_reconciling(header) });
    assert_eq!(
        h.flags & BYTE_SEVEN_FREE,
        BYTE_SEVEN_FREE,
        "the free bits went down when the entity was given back"
    );
}

/// The bit the reconciliation takes an entity by is bit 24, the first of
/// byte 7 (`rfc/model/classes.md`, "Flags layout"). Held here rather than in
/// `the_header_the_compiler_shares` because the compiler never sees it: its
/// one reader is inside the reset.
#[test]
fn the_reconciliation_takes_an_entity_by_bit_24() {
    let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
    unsafe { set_reconciling(&raw mut h, true) };
    assert_eq!(h.flags, MemoryCategory::GcHeap as u32 | (1 << 24));
    // The flags are read through the field rather than through the pointer,
    // so the pointer's life ends at the call above.
}
