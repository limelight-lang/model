//! The wrapper supplies the `RcHeader` and nothing else: no
//! per-instance class pointer, the same construction as a string,
//! because `array` is final and the entity kind already says what
//! this is.

use super::*;

#[test]
fn a_fresh_array_is_a_cow_entity_of_the_array_kind_at_count_one() {
    let _g = crate::memory::block_pool::test_guard();
    let a = arr();
    assert!(!a.is_null());
    unsafe {
        assert_eq!(crate::refcount::entity_refcount(a), 1);
        assert_eq!(
            crate::refcount::entity_flags(a) & crate::refcount::COW,
            crate::refcount::COW
        );
        assert_eq!(
            crate::refcount::entity_flags(a) & crate::refcount::ENTITY_KIND_MASK,
            EntityKind::Array.to_flags()
        );
        assert_eq!(category_of(a), MemoryCategory::GcHeap);
        // Strategy 2, which is what the factory stamps: a fresh array
        // holds the mixed vector and reaches the ordered hash only by
        // migrating (`rfc/model/arrays.md`, "Transition Rules").
        assert_eq!((*storage_head(a)).tag(), StorageTag::Vector);
        assert_eq!(crate::array::testing::used(a), 0);
        crate::array::entity::dispose_storage(a, category_of(a));
    }
}

/// The layout the design fixes: no per-instance class pointer, the
/// same construction as a string. `array` is final, so the entity
/// kind already says what this is.
///
/// The head sits between the header and the representation, and that
/// is what the 40 bytes between them are. It costs the entity nothing:
/// the words were the table's before, so an array is the same 112
/// bytes either way — which is the figure the placement was chosen
/// against.
#[test]
fn an_array_carries_no_class_pointer() {
    assert_eq!(std::mem::offset_of!(LLArray, rc), 0);
    assert_eq!(
        std::mem::offset_of!(LLArray, head),
        8,
        "the walker's words start straight after the header"
    );
    assert_eq!(
        std::mem::offset_of!(LLArray, storage),
        8 + size_of::<StorageHead>(),
        "the representation follows the head — nothing between"
    );
    assert_eq!(size_of::<LLArray>(), 112);
}
