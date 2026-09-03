use super::*;
use crate::memory::block_pool::{BLOCK_MASK, test_guard};
use crate::refcount::MemoryCategory;
use std::sync::atomic::Ordering;

mod an_entity_that_fills_its_own_block;
mod the_commissioning_rule;
mod the_registry_the_runs_carry;
mod the_two_halves_are_separate_populations;
mod what_the_registry_asks_the_allocator;
