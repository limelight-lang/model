use super::*;

use crate::memory::block_pool::{BLOCK_KIND_FREE, BLOCK_KIND_RETAINED, load_block_kind};

/// Bytes between the fixture's cells: the smallest entity a bump block
/// holds, so the list a fixture places past them is placed where a
/// reset would place it.
const CELL_STRIDE: usize = 16;

/// A retained block and occupants a walk may dereference, which is
/// what the module doc requires of anything published here: a pool
/// block commissioned the way the reset commissions one
/// ([`bare_retained_block`]), with `n` cells at the start of its
/// payload, every cell's refcount word zeroed.
///
/// The cells come back as raw pointers beside their addresses, and a
/// test that occupies one writes through **the pointer its address
/// was taken from**: an address that has been through `usize` carries
/// no provenance to write with, which Miri rejects.
///
/// The block is the test's to return, through [`give_back`], once
/// nothing holds it.
fn walkable_index(n: usize) -> (usize, Vec<usize>, Vec<*mut u64>) {
    let block = bare_retained_block();
    let base = BlockHeader::payload_start(block as *mut BlockHeader);
    let pointers: Vec<*mut u64> = (0..n)
        .map(|i| unsafe { base.add(i * CELL_STRIDE) } as *mut u64)
        .collect();
    for &cell in &pointers {
        unsafe { cell.write(0) };
    }

    let addresses: Vec<usize> = pointers.iter().map(|&p| p as usize).collect();
    (block, addresses, pointers)
}

/// Where the fixture's block has room for a list of `n` addresses: its
/// own tail, past the cells — the placement the reset makes first.
fn list_room(block: usize, n: usize) -> *mut usize {
    let base = BlockHeader::payload_start(block as *mut BlockHeader);
    unsafe { base.add(n * CELL_STRIDE) as *mut usize }
}

/// Return the fixture's block to the pool, which a test that took one
/// owes whether or not its assertions held. The caller has spent every
/// count the block was held for.
fn give_back(block: usize) {
    unsafe { release_emptied(block) };
}

/// The kind stamped on `block`, read the way its owner reads it.
fn kind_of(block: usize) -> u32 {
    unsafe { load_block_kind(&raw const (*(block as *mut BlockHeader)).kind) }
}

mod a_block_pinned_for_a_payload;
mod the_index_a_walker_reads;
mod what_the_list_asks_the_allocator;
mod when_a_retained_block_goes_home;
