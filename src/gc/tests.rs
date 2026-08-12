use super::*;
use crate::class::ClassBuilder;
use crate::memory::arena::Arena;
use crate::memory::barrier::ref_store;
use crate::memory::context::LLContext;
use crate::object::new_constructed;
use crate::refcount::ll_release;
use crate::test_support::{POOLED_FILLERS, RUN_FILLERS, wide_class};
use crate::value::Tag;

/// Real store through the barrier: retain + whole-value slot write.
unsafe fn link(arena: *mut Arena, from: *mut Object, offset: u32, to: *mut Object) {
    unsafe {
        let slot = Object::prop_at(from, offset);
        assert!(
            ref_store(
                arena,
                from as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Object, to as *mut RcHeader),
            ),
            "the barrier refused the link this test is built on"
        );
    }
}

fn node_class() -> *const crate::class::Class {
    ClassBuilder::new("CycleNode").prop("next", true).build()
}

mod a_ring_with_no_object_in_it;
mod the_candidate_buffer;
mod trial_deletion_over_a_ring;
mod what_a_destructor_does_to_the_white_set;
mod what_the_free_of_the_white_set_owes;
mod where_a_collection_may_fire;
