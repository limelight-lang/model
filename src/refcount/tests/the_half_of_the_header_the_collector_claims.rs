//! The epoch stamp sits in a byte of its own so that the collector
//! can write it while a mutator stores the counter half, and neither
//! retain, release nor the death branch may carry the other half
//! with them. Only rc-walk stamps an epoch, so the group is that
//! configuration's whole.

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
