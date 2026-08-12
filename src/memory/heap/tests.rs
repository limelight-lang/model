use super::*;
use crate::memory::block_pool::BLOCK_PAYLOAD;
use std::sync::atomic::Ordering;

mod blocks_going_home_with_nobody_asking;
mod frees_arriving_from_another_thread;
mod the_allocation_itself;
mod the_block_under_the_slots;
mod what_a_walker_reads_between_the_slots;
