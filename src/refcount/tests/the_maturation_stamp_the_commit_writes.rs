//! The stamp byte the collector's commit writes, read back through the pair
//! of accessors that own it.
//!
//! The header is a local rather than a heap entity: what these cases are about
//! is the packing and the width, and both are the same wherever the eight
//! bytes stand. What a commit stamps, and which components it stamps, is
//! `cycle::finalization`'s.

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

            unsafe { write_maturation_stamp(&raw mut h, written) };

            assert_eq!(unsafe { read_maturation_stamp(&raw const h) }, written);
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

    unsafe { write_maturation_stamp(&raw mut h, MaturationStamp { epoch: 2, age: 3 }) };

    assert_eq!(h.flags & RESERVE, RESERVE, "the reserve survived the stamp");
    assert_eq!(
        unsafe { read_maturation_stamp(&raw const h) },
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
    unsafe {
        write_maturation_stamp(&raw mut slot, MaturationStamp { epoch: 3, age: 3 });
        publish_header(&raw mut slot, RcHeader::new(MemoryCategory::GcHeap, 0));
    }

    assert_eq!(
        unsafe { read_maturation_stamp(&raw const slot) },
        MaturationStamp { epoch: 0, age: 0 },
        "the publication left no stamp behind"
    );
}
