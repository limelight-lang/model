//! What a second [`super::ll_free`] of one entity does, which is nothing.
//!
//! The guard is one flags bit and one branch at the head of the free
//! (`crate::refcount::DEAD_IN_PLACE`): the bit says the slot is `ll_free`'s and
//! has not been handed back, so a repeat reads it up and returns. What hands a
//! slot back is the publication of its next occupant, and, where a slot is
//! freed without ever being published, the clear the freeing path makes itself.
//!
//! **The cases here are the size-class population**, which is the one the bit
//! answers for over a span worth naming: it carries the bit until the slot is
//! handed out and published again, and a free of the old pointer past that
//! publication is a free of the new occupant. A pooled large entity's second
//! free is absorbed by the pool's re-stamp of the block kind rather than by
//! this bit
//! (`memory::large_entity::tests::an_entity_that_fills_its_own_block`), and an
//! OS-direct run's memory is the operating system's from its first free, so
//! neither is exercised here
//! (`dev/DECISIONS.md`, "a second `ll_free` of an entity is refused, and the
//! mark is the bit it is refused on").

use super::*;

use crate::memory::block_pool::BLOCK_PAYLOAD;
use crate::memory::heap::MAX_SMALL;
use crate::refcount::{EntityKind, MemoryCategory, RcHeader};

/// One size class, small enough that a block holds many of it.
const CLASS: usize = 64;

/// A published header in a slot, at the count a free demands.
///
/// The publication goes through `refcount::publish_header` rather than a
/// hand-written store, because one of the facts these cases rest on is its
/// width: it writes all eight bytes, so it hands back a slot a previous free
/// took without a clear of its own.
unsafe fn publish(slot: *mut u8) -> *mut RcHeader {
    let header = slot as *mut RcHeader;
    unsafe {
        crate::refcount::publish_header(
            header,
            RcHeader::new(MemoryCategory::GcHeap, EntityKind::Object.to_flags()),
        )
    };
    unsafe { crate::refcount::set_header_refcount(header, 0) };
    header
}

/// The refusal is counted once, and the class hands the slot out once.
///
/// The second half is the one that matters: a free list that took the same
/// address twice hands it to two owners, which is the fault this guard exists
/// to prevent and which no count of refusals would show on its own.
#[test]
fn a_second_free_of_one_entity_is_refused_and_counted() {
    let _g = crate::memory::block_pool::test_guard();
    let _ = take_refused_frees();

    let victim = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert!(!victim.is_null());
    unsafe { publish(victim) };

    unsafe { ll_free(victim) };
    assert_eq!(take_refused_frees(), 0, "the first free is made");

    unsafe { ll_free(victim) };
    assert_eq!(
        take_refused_frees(),
        1,
        "and the second is refused, the slot being one `ll_free` already holds"
    );

    let first = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    let second = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert!(!first.is_null() && !second.is_null());
    assert_ne!(first, second, "one address reached the free list twice");
    assert_eq!(
        [first, second].iter().filter(|&&s| s == victim).count(),
        1,
        "the slot is served once"
    );

    for slot in [first, second] {
        unsafe { publish(slot) };
        unsafe { ll_free(slot) };
    }
}

/// A program that frees each entity once is charged nothing: no refusal over
/// the three populations this module can build, and a slot lived in twice
/// hands itself back at the second publication.
#[test]
fn an_ordinary_life_refuses_no_free() {
    let _g = crate::memory::block_pool::test_guard();
    let _ = take_refused_frees();

    let mut lived_in = std::ptr::null_mut();
    for life in 0..2 {
        let slot = unsafe { crate::memory::heap::entity_alloc(CLASS) };
        assert!(!slot.is_null());
        let header = unsafe { publish(slot) };
        if life == 1 {
            assert_eq!(slot, lived_in, "the second life stands in the first's slot");
            assert_eq!(
                unsafe { crate::refcount::slot_state(header) },
                crate::refcount::SlotState::Free,
                "the publication handed the slot back, all eight bytes of the \
                 header going down at once"
            );
        }

        lived_in = slot;
        unsafe { ll_free(slot) };
    }

    for size in [MAX_SMALL + 16, BLOCK_PAYLOAD + 1] {
        let entity = crate::memory::large_entity::alloc(size);
        assert!(!entity.is_null());
        unsafe { publish(entity) };
        unsafe { ll_free(entity) };
    }

    assert_eq!(
        take_refused_frees(),
        0,
        "an entity freed once is a free the guard lets through, in every \
         population it stands over"
    );
}
