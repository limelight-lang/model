use super::*;
use crate::array::entity::{
    LLArray, as_vector, as_vector_mut, new_vector_array, sever_counted_children, storage_head,
};
use crate::refcount::ll_release;
use crate::value::Tag;

/// An array whose storage is a vector, at count one, empty.
///
/// Through `new_vector_array` rather than `ll_array_new`: the two stamp
/// the same representation today, and the named door is what keeps these
/// tests about the vector rather than about what a fresh array happens
/// to be. They build the entity rather than the bare representation
/// because the head is the entity's, so a `Vector` alone has no chunk
/// and no count to measure.
fn vector_array(category: MemoryCategory) -> *mut LLArray {
    let a = unsafe { new_vector_array(category) };
    assert!(!a.is_null(), "allocation refused in a test");
    a
}

mod the_dense_range;
mod the_entity_over_a_vector;
