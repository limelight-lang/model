//! A heap out of blocks sweeps its owned blocks for frees other threads
//! posted only when the sweep is due (`Heap::a_sweep_is_due`): where no
//! thread frees into it, a share of its blocks apart; where one does, at
//! once; and wherever the pool refuses it a block. A full block another
//! thread emptied comes back within that share of new blocks.

use super::*;
use crate::memory::block_pool::{BLOCK_SIZE, budget_blocks};

/// The class a 24-byte request lands in.
fn the_class() -> usize {
    size_class_index(24).expect("a small size")
}

/// Allocate from `heap` until it owns `blocks` blocks of the class, and
/// answer the slots.
fn fill(heap: &mut Heap, blocks: u32) -> Vec<usize> {
    let ci = the_class();
    let mut slots = Vec::new();
    while heap.owned_count[ci] < blocks {
        let p = heap.alloc(24);
        assert!(!p.is_null());
        slots.push(p as usize);
    }
    slots
}

/// Free every slot of `slots` that lies in the block of the first one from
/// another thread, onto that block's remote stack; answer the block's
/// address, the slots freed and the slots left.
fn empty_the_first_block_from_another_thread(slots: Vec<usize>) -> (usize, usize, Vec<usize>) {
    let mask = !(BLOCK_SIZE - 1);
    let base = slots[0] & mask;
    let (first, kept): (Vec<usize>, Vec<usize>) =
        slots.into_iter().partition(|&p| p & mask == base);
    let freed = first.len();
    std::thread::spawn(move || {
        assert!(ll_thread_init(), "the runtime started this thread");
        for p in first {
            unsafe { with_thread_heap(|h| h.free(p as *mut u8)) };
        }
    })
    .join()
    .unwrap();
    (base, freed, kept)
}

/// Whether `heap` owns the block at `base` with frees still pending on it.
fn still_pending(heap: &Heap, base: usize) -> bool {
    let mut block = heap.owned[the_class()];
    while !block.is_null() {
        if block as usize == base {
            return unsafe { !(*block).remote.remote_free.load(Ordering::Relaxed).is_null() };
        }
        block = unsafe { (*block).links.owned_next };
    }
    false
}

#[test]
#[cfg_attr(miri, ignore = "fills 200 blocks")]
fn a_heap_no_thread_frees_into_sweeps_a_share_of_its_blocks_apart() {
    let _g = crate::memory::block_pool::test_guard();
    const BLOCKS: u32 = 200;
    let mut heap = Heap::new();
    let slots = fill(&mut heap, BLOCKS);

    // Below 16 blocks every call sweeps; past them a sweep waits for an
    // eighth of the owned blocks — some 38 sweeps over 200 blocks, not 200.
    let sweeps = heap.sweeps[the_class()];
    assert!(sweeps * 4 < BLOCKS, "{sweeps} sweeps over {BLOCKS} blocks gained");
    for p in slots {
        unsafe { heap.free(p as *mut u8) };
    }
}

#[test]
#[cfg_attr(miri, ignore = "fills 64 blocks")]
fn a_full_block_another_thread_emptied_comes_back_within_a_share() {
    let _g = crate::memory::block_pool::test_guard();
    const BLOCKS: u32 = 64;
    let ci = the_class();
    let mut heap = Heap::new();
    let slots = fill(&mut heap, BLOCKS);
    let (base, _, mut kept) = empty_the_first_block_from_another_thread(slots);
    assert!(still_pending(&heap, base));

    // The first call out of blocks is not due — the last sweep is fewer
    // than an eighth of the owned blocks behind — and draws a block; within
    // that share a due sweep takes the emptied block's frees back.
    let sweeps = heap.sweeps[ci];
    let owned = heap.owned_count[ci];
    let mut drawn_before_the_sweep = false;
    while heap.sweeps[ci] == sweeps {
        kept.push(heap.alloc(24) as usize);
        drawn_before_the_sweep |= heap.owned_count[ci] > owned;
        assert!(
            heap.owned_count[ci] <= owned + BLOCKS / SWEEP_SHARE + 1,
            "no sweep within a share of new blocks"
        );
    }
    assert!(drawn_before_the_sweep, "the first call out of blocks was gated");
    assert!(!still_pending(&heap, base), "the sweep took the frees back");
    for p in kept {
        unsafe { heap.free(p as *mut u8) };
    }
}

#[test]
#[cfg_attr(miri, ignore = "fills 64 blocks")]
fn a_heap_the_pool_refuses_sweeps_before_it_answers_null() {
    let _g = crate::memory::block_pool::test_guard();
    const BLOCKS: u32 = 64;
    let ci = the_class();
    let mut heap = Heap::new();
    let slots = fill(&mut heap, BLOCKS);
    let (base, freed, mut kept) = empty_the_first_block_from_another_thread(slots);

    // No block from the pool and none adopted: once the block in hand is
    // full the next call out of blocks is not due, and serves from the
    // emptied block all the same, until that too is full.
    let _budgeted = budget_blocks(0);
    let owned = heap.owned_count[ci];
    let per_block = BLOCK_PAYLOAD / SIZE_CLASSES[ci];
    let mut served = 0;
    loop {
        let p = heap.alloc(24);
        if p.is_null() {
            break;
        }
        kept.push(p as usize);
        served += 1;
        assert!(served <= 2 * per_block, "the pool refused nothing");
    }
    assert!(served >= freed, "the frees in the heap's own block served: {served} of {freed}");
    assert_eq!(heap.owned_count[ci], owned, "no block came from the pool");
    assert!(!still_pending(&heap, base));
    for p in kept {
        unsafe { heap.free(p as *mut u8) };
    }
}
