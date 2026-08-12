use super::*;
use crate::array::entity::{
    LLArray, as_vector, as_vector_mut, new_vector_array, sever_counted_children, storage_head,
};
use crate::refcount::ll_release;
use crate::value::Tag;

/// An array whose storage is a vector, at count one, empty.
///
/// Through `new_with_storage` rather than `ll_array_new`, which stamps
/// the ordered hash until the element layer reads the tag. That is what
/// makes these tests the vector's only producer for
/// one step, and it is why they build the entity rather than the bare
/// representation: what the criterion asks about is an array.
fn vector_array(category: MemoryCategory) -> *mut LLArray {
    let a = unsafe { new_vector_array(category) };
    assert!(!a.is_null(), "allocation refused in a test");
    a
}

/// Give an array whose elements are integers back: the last reference
/// goes, then the teardown frees the storage and the slot. A test holding
/// entities releases them itself instead — the drain would take them a
/// second time.
fn discard(a: *mut LLArray) {
    unsafe {
        assert!(
            ll_release(a as *mut RcHeader),
            "the test was the only holder"
        );
        crate::object::ll_entity_die(a as *mut RcHeader);
    }
}

/// A child array, handed to the caller at the +1 its factory returns.
fn child() -> *mut LLArray {
    let c = unsafe { crate::array::entity::ll_array_new(MemoryCategory::GcHeap) };
    assert!(!c.is_null());
    c
}

mod the_dense_range;
mod the_entity_over_a_vector;
