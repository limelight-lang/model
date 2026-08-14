use super::*;
use crate::class::ClassBuilder;
use crate::memory::barrier::ref_store;
use crate::memory::block_pool::BLOCK_KIND_ARENA;
use crate::memory::context::{LLContext, set_current_context};
use crate::object::{ll_object_die, new_constructed};
use crate::refcount::{DESTRUCTOR_PENDING, DESTRUCTOR_RAN};
use crate::test_support::RUN_FILLERS;
use crate::value::{Tag, Value};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Entity pointer behind a Box slot, or null for scalar/null Boxes.
fn entity_checked(v: &Value) -> *mut RcHeader {
    if v.is_refcounted() {
        v.entity_ptr()
    } else {
        std::ptr::null_mut()
    }
}

/// Store `value` into `holder`'s slot at `offset` through the real
/// barrier, as generated code would.
unsafe fn store_prop(arena: *mut Arena, holder: *mut Object, offset: u32, value: *mut Object) {
    unsafe {
        let slot = Object::prop_at(holder, offset);
        let old = entity_checked(&*slot);
        let new = if value.is_null() {
            Value::null()
        } else {
            Value::entity(Tag::Object, value as *mut RcHeader)
        };

        assert!(ref_store(arena, holder as *mut RcHeader, slot, old, new));
    }
}

/// The kind stamped on the block holding `memory`, read the way every
/// concurrent reader of that word reads it.
unsafe fn block_kind(memory: *const u8) -> u32 {
    let header = BlockHeader::of_ptr(memory) as *const std::sync::atomic::AtomicU32;
    unsafe { crate::memory::block_pool::load_block_kind(header) }
}

mod the_memory_a_survivor_takes_with_it;
mod the_release_log;
mod the_reset_reads_no_corpse;
mod what_a_destructor_does_during_the_fixpoint;
mod who_survives_a_reset;
