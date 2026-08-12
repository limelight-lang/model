//! The memory category decides whether the counter moves at all: a
//! heap entity counts and reports its death, an arena one is freed
//! by the reset instead, an immortal one is never touched, and a COW
//! entity counts in the arena too, the count being what separates
//! its copies. At the ceiling the counter stops moving, which
//! accepts a leak rather than the free of an entity somebody still
//! holds.

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
