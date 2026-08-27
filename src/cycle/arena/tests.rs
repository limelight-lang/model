use super::*;
use crate::memory::block_pool::{FORCE_OOM, test_guard};
use std::sync::atomic::Ordering;

/// A block the arena may stamp a shadow pointer on: a commissioned
/// entity block, reached through the slot its heap hands out.
///
/// The heap comes back with it, because the block goes home when the
/// heap is dropped and a test that let it go early would be nulling a
/// shadow pointer in memory the pool had handed to someone else.
fn an_entity_block() -> (crate::memory::Heap, *mut u8, *mut u8) {
    let mut heap = crate::memory::Heap::new_entity();
    let slot = heap.alloc(crate::memory::heap::SIZE_CLASSES[0]);
    let block = ((slot as usize) & !crate::memory::block_pool::BLOCK_MASK) as *mut u8;
    (heap, slot, block)
}

mod what_the_arena_gives_back;
