use super::*;
use crate::class::ClassBuilder;
use crate::cycle::shadow::Color;
use crate::cycle::testing::{row_color, traced_unreachable_from};
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, header_refcount, ll_release, ll_retain};
use crate::test_support::{entity_checked, prop_offset, store_prop};

/// The entity a property holds, or null.
///
/// # Safety
/// `holder` is a live object and `offset` one of its property slots.
unsafe fn prop_entity(holder: *mut Object, offset: u32) -> *mut RcHeader {
    unsafe { entity_checked(&*Object::prop_at(holder, offset)) }
}

mod what_a_mutation_racing_the_verdict_costs;
mod what_a_ring_through_an_array_reads_as;
mod what_an_edge_out_of_the_component_counts_for;
mod what_the_guard_references_answer;
// The one case of this module is a debug-build case, the counter it reads
// standing behind a `debug_assert`. A release test build compiles neither, so
// the module goes with them and the accessor it calls is gated alike.
#[cfg(debug_assertions)]
mod what_the_premise_check_costs;
mod what_the_zero_count_rule_drops;
