use super::*;
use crate::class::ClassBuilder;
use crate::cycle::arena::TraceScratchArena;
use crate::cycle::mark::{MarkResult, mark};
use crate::cycle::scan::{ScanResult, scan};
use crate::cycle::shadow::Color;
use crate::cycle::stack::TraceStack;
use crate::cycle::testing::row_color;
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, header_refcount, ll_release, ll_retain};
use crate::test_support::{entity_checked, prop_offset, store_prop};

/// Trace the fixture from one root and assert every entity named is
/// condemned, which is the state the exact test is asked about.
///
/// The arena comes back so the caller resets it before judging: the rows
/// die at the token's release and the exact test runs after it
/// (`rfc/model/gc/rc-cycle.md`, "Concurrency").
unsafe fn condemned_from(root: *mut Object, expected: &[*mut Object]) -> TraceScratchArena {
    let mut arena = TraceScratchArena::new();
    let mut stack = TraceStack::new();
    assert_eq!(
        unsafe { mark(&mut arena, &mut stack, root as *mut RcHeader) },
        MarkResult::Complete
    );
    assert_eq!(
        unsafe { scan(&mut arena, &mut stack, root as *mut RcHeader) },
        ScanResult::Complete
    );

    for &entity in expected {
        assert_eq!(
            unsafe { row_color(entity as *mut RcHeader) },
            Color::PotentiallyUnreachable,
            "the trace condemned this entity"
        );
    }

    arena
}

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
mod what_the_corpse_rule_drops;
mod what_the_guard_discount_answers;
