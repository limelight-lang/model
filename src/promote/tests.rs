use super::*;
use crate::class::ClassBuilder;
use crate::memory::barrier::ref_store;
use crate::memory::block_pool::BLOCK_KIND_ARENA;
use crate::memory::context::{LLContext, set_current_context};
use crate::object::{ll_object_die, new_constructed};
use crate::refcount::{DESTRUCTOR_PENDING, DESTRUCTOR_RAN};
use crate::test_support::{RUN_FILLERS, entity_checked, store_prop};
use crate::value::{Tag, Value};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Nanoseconds of `rounds` runs of `one`, as (minimum, median) — the two
/// statistics the reset's measurement probes quote
/// (`dev/BENCHMARKS.md`, "the statistic that decides the answer").
fn min_and_median_nanos(rounds: usize, mut one: impl FnMut(usize) -> u128) -> (u128, u128) {
    let mut taken: Vec<u128> = (0..rounds).map(|round| one(round)).collect();
    taken.sort_unstable();
    (taken[0], taken[taken.len() / 2])
}

/// The kind stamped on the block holding `memory`, read the way every
/// concurrent reader of that word reads it.
unsafe fn block_kind(memory: *const u8) -> u32 {
    let header = BlockHeader::of_ptr(memory) as *const std::sync::atomic::AtomicU32;
    unsafe { crate::memory::block_pool::load_block_kind(header) }
}

mod the_memory_a_survivor_takes_with_it;
#[cfg(feature = "debug-journal")]
mod the_record_a_reset_closes_with;
mod the_release_log;
mod the_reset_reads_no_zero_count_member;
mod what_a_destructor_does_during_the_fixpoint;
mod what_a_sever_leaves_consistent;
mod what_the_cow_reconciliation_costs;
mod what_the_grouping_costs;
mod where_a_survivor_list_is_placed;
mod who_may_touch_the_resets_words;
mod who_survives_a_reset;
