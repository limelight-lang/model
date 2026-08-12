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

mod the_dense_range;
mod the_entity_over_a_vector;
