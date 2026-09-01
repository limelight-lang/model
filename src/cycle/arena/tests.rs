use super::*;
use crate::memory::block_pool::{force_oom, test_guard};

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

/// The row of slot `index` of an entity block, built the way
/// `row::resolve_edge_target` would build it from an entity's address. The
/// tests here drive the arena rather than the dispatch, so the row is written
/// out: an entity block's slot index is its position under the block's stride,
/// and slot 0 is the first address the payload holds.
fn slot_row(block: *mut u8, index: u32) -> crate::cycle::row::RowKey {
    crate::cycle::row::RowKey {
        block: block as usize,
        index,
        population: crate::cycle::row::Population::Slotted,
    }
}

/// The row `ensure_row` handed back, or a panic naming what it answered
/// instead. Every test here asks for a row it expects to get.
fn met(answer: RowLookup) -> *mut u32 {
    match answer {
        RowLookup::Ready { row, .. } => row,
        other => panic!("the arena refused a row: {other:?}"),
    }
}

/// The row and whether this was the collection's first reach of the
/// entity — the bit the mark's descent turns on, which only the meeting
/// can answer.
fn met_first(answer: RowLookup) -> (*mut u32, bool) {
    match answer {
        RowLookup::Ready { row, first_visit } => (row, first_visit),
        other => panic!("the arena refused a row: {other:?}"),
    }
}

mod the_rows_a_block_gets_at_its_first_touch;
mod what_a_met_row_answers;
mod what_the_arena_gives_back;
