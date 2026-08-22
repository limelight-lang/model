use super::*;
use crate::class::ClassBuilder;
use crate::epoch::checkpoint;
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::memory::heap::for_each_entity_slot;
use crate::object::{Object, new_constructed};
use crate::refcount::{ll_release, ll_retain};
use crate::test_support::{POOLED_FILLERS, RUN_FILLERS};
use crate::value::{Tag, Value};
use std::sync::atomic::AtomicUsize;

fn walked_addresses() -> Vec<usize> {
    let mut seen = Vec::new();
    unsafe { for_each_entity_slot(|e| seen.push(e as usize)) };
    seen
}

unsafe fn tie(a: *mut Object, offset: u32, b: *mut Object) {
    unsafe {
        Object::prop_at(a, offset).write(Value::entity(Tag::Object, b as *mut RcHeader));
    }
}

static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
}

/// Step one epoch to completion on this thread, playing both actors:
/// the collector steps, with a mutator checkpoint wherever the
/// protocol needs an ack or a drain.
fn stepped_epoch() -> EpochStats {
    let mut e = Epoch::open();
    checkpoint(); // publish the activity bit
    e.snapshot();
    e.walk();
    e.judge();
    if e.stats.candidates > 0 {
        e.condemn();
        checkpoint(); // the Phase 3 ack
        e.recheck_and_post();
        checkpoint(); // the Phase 4 drain
    }

    let stats = e.close();
    checkpoint(); // the post-epoch flush of parked memory
    stats
}

mod a_ring_through_a_string_keyed_array;
mod an_edge_the_walk_may_not_record;
mod cells_a_class_keeps_outside_itself;
mod the_epoch_as_a_whole;
mod how_uniform_a_block_comes_out;
mod what_a_leaf_row_costs_the_walk;
mod what_an_array_row_costs_the_walk;
mod the_three_way_judgement;
mod what_the_snapshot_reaches;
mod what_the_young_free_exemption_removes;
