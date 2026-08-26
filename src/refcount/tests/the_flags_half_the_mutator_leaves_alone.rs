//! The mutator writes the counter and its own two bytes of the flags,
//! so a byte a collector puts at +6 survives every hot-path operation
//! and every flags update.
//!
//! Nothing writes the flags half from another thread yet — the collector
//! that will is S38's — so what these tests pin is the separation the
//! mutator owes, which outlives the collector that first asked for it:
//! `rc-walk`'s epoch stamp is where the marker below comes from.

use super::*;

/// A marker in the flags half, in bits no constant claims. Any value
/// there would do — what the tests read is whether it comes back.
const FOREIGN_MARK: u32 = 7 << 16;

/// Byte 6 of the header is addressable on its own: a byte store there
/// lands in the flags half and leaves the refcount untouched. The
/// little-endian assumption behind that is a `compile_error!` in
/// `refcount.rs`; this is the run-time half of the same claim.
#[test]
fn a_byte_store_at_offset_six_misses_the_counter() {
    let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
    let p = &mut h as *mut RcHeader as *mut u8;
    unsafe {
        p.add(6).write(3);
    }

    assert_eq!(h.flags & 0x00FF_0000, 3 << 16, "byte 6 is flags bits 16-23");
    assert_eq!(h.refcount, 1, "the refcount bytes are untouched");
}

/// The narrow mutator (rfc amendment, 2026-07-27): retain and a
/// non-final release store only the counter half — a foreign mark in
/// the flags half passes through untouched.
#[test]
fn retain_and_release_leave_the_flags_half_alone() {
    let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
    h.flags |= FOREIGN_MARK;

    retain(&mut h);
    assert_eq!(h.refcount, 2);
    assert_eq!(h.flags & FOREIGN_MARK, FOREIGN_MARK, "the mark survives");

    assert!(!release(&mut h));
    assert_eq!(h.refcount, 1);
    assert_eq!(h.flags & FOREIGN_MARK, FOREIGN_MARK);
}

/// Eager death (rfc amendment, 2026-07-27, superseding F5's deferral):
/// the release reaching zero reports the death — there is no condemned
/// test on the death branch, and the flags half is left exactly as
/// loaded.
#[test]
fn every_death_takes_the_ordinary_path() {
    let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
    h.flags |= FOREIGN_MARK;
    assert!(release(&mut h), "the death is reported, marked or not");
    assert_eq!(h.refcount, 0);
    assert_eq!(h.flags & FOREIGN_MARK, FOREIGN_MARK, "flags untouched");
}

/// A flags update that **started before** the collector's byte landed
/// leaves that byte alone.
///
/// The order is the test. A store made before the update passes on a
/// whole-word writer too, because such a writer loads the byte and puts
/// it back unchanged; only a store made *after* the mutator's load and
/// before the mutator's store separates the two widths. The closure runs
/// in exactly that gap, so the interleaving is fixed rather than raced
/// for.
#[test]
fn a_collector_byte_survives_an_update_that_read_before_it() {
    let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
    // One raw pointer, cast rather than re-derived: two `&raw mut`
    // borrows of the same local would invalidate the first under Stacked
    // Borrows, and the byte write below happens while the update holds
    // its own pointer.
    let base = &raw mut h as *mut u8;

    unsafe {
        update_header_flags(base as *mut RcHeader, |flags| {
            base.add(6).write(3);
            flags | COW
        });
    }

    assert_eq!(
        h.flags & 0x00FF_0000,
        3 << 16,
        "the collector's byte survived an update that had already read"
    );
    assert_ne!(h.flags & COW, 0, "and the mutator's own bit landed");
    assert_eq!(h.refcount, 1, "with the counter untouched");
}

/// A mark in the collector's bytes passes through both halves of a
/// destructor's guard.
///
/// **This pins the outcome, not the width.** The mark is set before the
/// call, so a whole-word writer that loads it and puts it back passes
/// too — the two functions take no closure, so there is no gap to place
/// a store in the way the update test does. What holds their width is
/// `the_widths_the_mutator_uses`, which reads the source.
#[test]
fn the_teardown_guard_leaves_the_flags_half_alone() {
    let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
    h.flags |= FOREIGN_MARK;

    unsafe { crate::refcount::mutator_guard_retain(&raw mut h) };
    assert_eq!(h.refcount, 2);
    assert_eq!(h.flags & FOREIGN_MARK, FOREIGN_MARK, "the mark survives +1");

    let left = unsafe { crate::refcount::mutator_unguard_release(&raw mut h) };
    assert_eq!(left, 1);
    assert_eq!(h.flags & FOREIGN_MARK, FOREIGN_MARK, "and survives -1");
}
