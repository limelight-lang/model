//! A slot whose entity died while a queue entry named it, and what the
//! allocator is allowed to do with it.
//!
//! The rule is one sentence and the whole of the parking:
//! [`super::ll_free`] withholds such a slot instead of returning it, so
//! the entry — a raw pointer carrying nothing of its own — still names a
//! body whose count can be read (`rfc/model/gc/rc-cycle.md`, "Death while
//! enrolled"). Nothing is recorded anywhere: the entry is the record.
//!
//! What that buys is stated as an accounting invariant, and it is what
//! this module checks: **a block's `used` falls at the slot's return and
//! never at the parking**. A block holding a parked corpse therefore
//! never reads empty, never reaches the pool, and cannot be handed out
//! under the entry that names one of its slots.

use super::*;

use crate::memory::block_pool::{BLOCK_MASK, BLOCK_PAYLOAD, BlockPool};
use crate::refcount::{EntityKind, MemoryCategory, RcHeader};

/// A published header in a slot, at the count a free demands.
///
/// Zero, because that is what a dying entity reaches and what
/// [`super::ll_free`]'s own assertion requires of every entity slot it
/// takes.
unsafe fn publish(slot: *mut u8) -> *mut RcHeader {
    let header = slot as *mut RcHeader;
    unsafe {
        header.write(RcHeader::new(
            MemoryCategory::GcHeap,
            EntityKind::Object.to_flags(),
        ))
    };
    unsafe { crate::refcount::set_header_refcount(header, 0) };
    header
}

/// Whether the pool is holding this block, asked by drawing until it
/// appears — the instrument `heap::tests::the_block_under_the_slots`
/// uses, and for its reason: `blocks_out` and `regions_carved` are
/// process-global and move under a test that holds no lock over them.
fn pool_holds(block: usize) -> bool {
    let pool = BlockPool::global();
    let mut drawn = Vec::new();
    let mut found = false;
    for _ in 0..16 {
        let b = pool.get();
        assert!(!b.is_null(), "the pool refused mid-search");
        drawn.push(b);
        if b as usize == block {
            found = true;
            break;
        }
    }

    for b in drawn {
        pool.put(b);
    }

    found
}

/// The whole rule in one arrangement: a block emptied around a parked
/// corpse stays out of the pool, and reaches it at the return and not
/// before.
///
/// **Two blocks are filled, and the second is the one watched**, because
/// `Heap::retire_empty` keeps the first emptied block of a class as that
/// class's one spare and returns only the next — a test that filled a
/// single block would watch a block that never leaves the thread
/// (`heap::tests::the_block_under_the_slots::empty_block_returns_to_pool`
/// says the same of the raw population).
#[test]
fn a_block_emptied_around_a_parked_corpse_reaches_the_pool_at_the_return() {
    let _g = crate::memory::block_pool::test_guard();

    const SIZE: usize = 64;
    let slots = BLOCK_PAYLOAD / SIZE;
    let cells: Vec<*mut u8> = (0..2 * slots)
        .map(|_| unsafe { crate::memory::heap::entity_alloc(SIZE) })
        .collect();
    assert!(
        cells.iter().all(|c| !c.is_null()),
        "the pool served the fill"
    );

    let watched = cells[2 * slots - 1] as usize & !BLOCK_MASK;
    assert_ne!(
        cells[0] as usize & !BLOCK_MASK,
        watched,
        "the fill has to span two blocks, the first being kept as the spare"
    );

    // The corpse: a slot of the watched block, enrolled and then freed.
    // Its death is not simulated — what a real one does before it frees
    // is `ll_object_die`'s business and is unchanged by the parking.
    let corpse = cells
        .iter()
        .rev()
        .find(|c| **c as usize & !BLOCK_MASK == watched)
        .copied()
        .expect("the watched block holds slots");
    let header = unsafe { publish(corpse) };
    unsafe { crate::refcount::update_header_flags(header, |f| f | crate::refcount::CANDIDATE_BIT) };
    unsafe { ll_free(corpse) };

    for cell in &cells {
        if *cell == corpse {
            continue;
        }

        unsafe { publish(*cell) };
        unsafe { ll_free(*cell) };
    }

    assert!(
        !pool_holds(watched),
        "a block holding a parked corpse reached the pool: its `used` fell \
         when the free was withheld rather than at the return"
    );

    // The return, which is the retirement's last act: the bit comes down
    // and the same door takes the slot.
    unsafe { crate::refcount::clear_candidate_bit(header) };
    unsafe { ll_free(corpse) };

    assert!(
        pool_holds(watched),
        "the block did not reach the pool once its last slot returned"
    );
}

/// The parking is a withholding and nothing else: the body is left as
/// the death wrote it, so the count the retirement reads is still there.
#[test]
fn a_parked_slot_keeps_the_body_the_death_left() {
    let _g = crate::memory::block_pool::test_guard();

    let cell = unsafe { crate::memory::heap::entity_alloc(64) };
    assert!(!cell.is_null());
    let header = unsafe { publish(cell) };
    unsafe { crate::refcount::update_header_flags(header, |f| f | crate::refcount::CANDIDATE_BIT) };

    unsafe { ll_free(cell) };

    assert_eq!(
        unsafe { crate::refcount::header_refcount(header) },
        0,
        "the retirement reads a zero count out of the parked body"
    );
    assert!(
        unsafe { crate::refcount::is_registered_candidate(header) },
        "and the bit that parked it is still standing"
    );

    unsafe { crate::refcount::clear_candidate_bit(header) };
    unsafe { ll_free(cell) };
}
