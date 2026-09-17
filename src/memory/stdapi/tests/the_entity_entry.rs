//! What [`super::ll_free_entity`] does for a slot, which is what
//! [`super::ll_free`] does for the same slot: the slot comes back on the
//! class's free list once, and a repeat is refused on the same bit.

use super::*;

use crate::refcount::{EntityKind, MemoryCategory, RcHeader};

const CLASS: usize = 64;

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

#[test]
fn the_entity_entry_returns_the_slot_as_the_general_one_does() {
    let _g = crate::memory::block_pool::test_guard();
    let _ = take_refused_frees();

    let by_the_general = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    let by_the_entity = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert!(!by_the_general.is_null() && !by_the_entity.is_null());
    unsafe { publish(by_the_general) };
    unsafe { publish(by_the_entity) };

    unsafe { ll_free(by_the_general) };
    unsafe { ll_free_entity(by_the_entity) };
    assert_eq!(take_refused_frees(), 0, "both first frees are made");

    // The repeat through either entry is refused on the same bit.
    unsafe { ll_free_entity(by_the_entity) };
    unsafe { ll_free_entity(by_the_general) };
    assert_eq!(take_refused_frees(), 2);

    // Both slots come back, each once: the free list is one list.
    let served: Vec<*mut u8> = (0..2)
        .map(|_| unsafe { crate::memory::heap::entity_alloc(CLASS) })
        .collect();
    assert_eq!(
        served.iter().filter(|&&s| s == by_the_general).count(),
        1,
        "the general entry's slot is served once"
    );
    assert_eq!(
        served.iter().filter(|&&s| s == by_the_entity).count(),
        1,
        "the entity entry's slot is served once"
    );
    for slot in served {
        unsafe { publish(slot) };
        unsafe { ll_free_entity(slot) };
    }
}
