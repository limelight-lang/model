//! What an entity allocation does when the allocator below it has refused:
//! it collects this thread's cycles once and asks once more.
//!
//! Every case runs on a thread of its own, because what it needs is an entity
//! heap whose block for one size class it filled itself — the harness reuses
//! its threads, and a class an earlier case allocated in holds a block with
//! room that would serve the request these mean to refuse. Each case fills
//! every available slot before observing the pool cap.
//!
//! The injection is `block_pool::budget_blocks` rather than `FORCE_OOM`: a
//! budget refuses this thread only, and refuses after counting the request, so
//! a case can read both that the pool was asked and what a teardown gave back.

use super::*;

use crate::class::{Class, ClassBuilder};
use crate::cycle::collect::take_pressure_collections;
use crate::memory::arena::Arena;
use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockHeader, budget_blocks, take_pool_requests};
use crate::refcount::{RcHeader, SlotState, slot_state};

/// A class whose instance fills one size class exactly, with `fillers`
/// properties behind the first deciding which class that is.
///
/// **Each case of this module passes a different count**, because a block is
/// adopted by size class and not by class: `Heap::alloc_no_block` takes an
/// abandoned block of the requested class before it asks the pool. Distinct
/// widths keep the two fixtures' occupancy readings independent.
///
/// Two properties are asserted. The size class is an exact fit, so how many
/// slots a block holds is arithmetic rather than a guess. And it is not the
/// widest class, which `the_allocation_itself` drains and reads the identity
/// of the slot that comes back from.
fn a_class_of_its_own(name: &str, fillers: usize) -> *const Class {
    let mut builder = ClassBuilder::new(name).prop("child", true);
    let names: Vec<String> = (0..fillers).map(|i| format!("f{i}")).collect();
    for filler in &names {
        builder = builder.prop(filler, true);
    }

    let class = builder.build();
    let size = unsafe { (*class).object_size } as usize;
    let ci = size_class_index(size).expect("a size class serves this instance");
    assert_eq!(
        SIZE_CLASSES[ci], size,
        "the instance fits its size class exactly, so a block's slot count is arithmetic"
    );
    assert!(
        ci < SIZE_CLASSES.len() - 1,
        "and it is not the widest class, which another case drains and reads addresses from"
    );
    class
}

/// How many slots of `class`'s size one block holds.
fn slots_per_block(class: *const Class) -> usize {
    let size = unsafe { (*class).object_size } as usize;
    let ci = size_class_index(size).expect("a size class serves this instance");
    BLOCK_PAYLOAD / SIZE_CLASSES[ci]
}

/// The block `slot` stands in, as an address.
fn block_of(slot: *const u8) -> usize {
    BlockHeader::of_ptr(slot) as usize
}

/// Live occupants of the block `entity` stands in, which is what falls when a
/// slot is returned and stands when one is withheld
/// (`dev/DECISIONS.md`, "A block's `used` falls at the slot's return").
///
/// # Safety
/// `entity` addresses a slot of a commissioned entity block.
unsafe fn occupants_beside(entity: *mut RcHeader) -> u32 {
    let block = BlockHeader::of_ptr(entity as *const u8) as *mut HeapBlockHeader;
    unsafe { (*block).private.used }
}

/// Take slots of `size` until one is refused, and answer the slots taken.
///
/// **Allocating rather than counting is what makes a case independent of the
/// heap it starts from.** The blocks a class can be served from are its current
/// one and any the process abandoned at that size — an earlier case's thread
/// leaves such a block behind, and `Heap::adopt` takes it before the pool is
/// asked at all. Taking slots until the refusal empties whatever is there, so
/// the refusal a case reads is the pool's.
///
/// `bound` is how many slots the caller expects at most; past it the budget is
/// refusing nothing and the case has no refusal to read. The slots taken are
/// given back before that is reported, because a panic holding them would
/// abandon their blocks with live occupants and every later case of this class
/// would adopt them one at a time.
///
/// The slots carry no header: nothing is published into them, and they go back
/// through `free_unpublished`, which is the return an unpublished cell owes.
///
/// # Safety
/// Runs on a thread the runtime has started, under a budgeted pool.
unsafe fn take_slots_until_refused(size: usize, bound: usize) -> Vec<*mut u8> {
    let mut taken = Vec::new();
    loop {
        let slot = unsafe { entity_alloc(size) };
        if slot.is_null() {
            return taken;
        }

        taken.push(slot);
        if taken.len() > bound {
            unsafe { give_back(&taken) };
            panic!("the budgeted pool refused nothing, so this case reads no refusal");
        }
    }
}

/// Give back what [`take_slots_until_refused`] took.
///
/// # Safety
/// Every slot came from that call and none was published.
unsafe fn give_back(slots: &[*mut u8]) {
    for &slot in slots {
        unsafe { crate::memory::stdapi::free_unpublished(slot) };
    }
}

/// The wiring, read by what it asks of the pool: one collection for the
/// request, and two requests — the attempt that found no room, and the retry
/// the collection paid for.
#[test]
fn a_refusal_starts_one_collection_and_asks_once_more() {
    let _g = crate::memory::block_pool::test_guard();

    let (collections, requests, armed) = std::thread::spawn(|| {
        assert!(ll_thread_init(), "the pool served this thread");
        // The collection workspace is a block, drawn at a thread's first
        // collection: taken here rather than inside the budgeted window, where
        // it would be the refusal the case reads
        // (`cycle::queue::warm_workspace_base`).
        crate::cycle::queue::warm_workspace_base();

        let class = a_class_of_its_own("RefusalWiring", 254);
        let size = unsafe { (*class).object_size } as usize;
        let bound = 4 * slots_per_block(class);
        let mut arena = Arena::new();
        let ring = unsafe { crate::cycle::testing::ring(&mut arena, [class, class, class]) };
        crate::gc::disarm();

        let _budgeted = budget_blocks(0);
        let _ = take_pool_requests();
        let _ = take_pressure_collections();
        let taken = unsafe { take_slots_until_refused(size, bound) };
        let answer = (
            take_pressure_collections(),
            take_pool_requests(),
            crate::gc::is_armed(),
        );

        drop(_budgeted);
        unsafe { give_back(&taken) };
        let _ = ring;
        answer
    })
    .join()
    .unwrap();

    assert_eq!(
        collections, 2,
        "the served retry and the final exhausted request each collect once"
    );
    assert_eq!(
        requests, 3,
        "one refused pool request before the served retry, then two at final exhaustion"
    );
    assert!(
        !armed,
        "the round traced every root and freed, so it hands nothing to the poll"
    );
}

/// The control arm: with nothing to collect, the shape of the path is the same
/// — one collection, one retry — and the caller is answered the refusal it
/// raises memory-exhausted on (`rfc/runtime/exceptions.md`, "Allocation failure
/// is an ordinary exception").
///
/// It differs from the case above by the ring and by nothing else, so what the
/// pair reads is that neither the collection nor the retry is conditional on
/// there being garbage.
#[test]
fn a_refusal_with_nothing_to_collect_asks_once() {
    let _g = crate::memory::block_pool::test_guard();

    let (collections, requests) = std::thread::spawn(|| {
        assert!(ll_thread_init(), "the pool served this thread");
        crate::cycle::queue::warm_workspace_base();

        let class = a_class_of_its_own("RefusalNoGarbage", 126);
        let size = unsafe { (*class).object_size } as usize;
        let bound = 4 * slots_per_block(class);
        crate::gc::disarm();

        let _budgeted = budget_blocks(0);
        let _ = take_pool_requests();
        let _ = take_pressure_collections();
        let taken = unsafe { take_slots_until_refused(size, bound) };
        let answer = (take_pressure_collections(), take_pool_requests());

        drop(_budgeted);
        unsafe { give_back(&taken) };
        answer
    })
    .join()
    .unwrap();

    assert_eq!(collections, 1, "the refusal collects once here too");
    assert_eq!(
        requests, 2,
        "and asks twice: the retry is unconditional, because what a collection freed is not \
         what the allocator can use"
    );
}

/// The retry can reuse all three member slots under a pool cap: retirement
/// lowers occupancy and a later allocation returns each original address.
#[test]
fn a_freed_members_slot_serves_the_retry_under_the_pool_cap() {
    let _g = crate::memory::block_pool::test_guard();

    let (states, expected_occupants, occupants, reused) = std::thread::spawn(|| {
        assert!(ll_thread_init(), "the pool served this thread");
        crate::cycle::queue::warm_workspace_base();

        let class = a_class_of_its_own("RefusalWithholding", 62);
        let size = unsafe { (*class).object_size } as usize;
        let bound = 4 * slots_per_block(class);
        let mut arena = Arena::new();
        let ring = unsafe { crate::cycle::testing::ring(&mut arena, [class, class, class]) };
        let ring_block = block_of(ring[0] as *const u8);
        // Read before the loop, because the block need not have started empty:
        // an adopted one carries whatever its last thread left in it.
        let occupants_before = unsafe { occupants_beside(ring[0] as *mut RcHeader) } as usize;

        let _budgeted = budget_blocks(0);
        let taken = unsafe { take_slots_until_refused(size, bound) };

        let states: Vec<SlotState> = ring
            .iter()
            .map(|&member| unsafe { slot_state(member as *mut RcHeader) })
            .collect();
        // The block's own count, which is the statement the state cannot make:
        // `DeadInPlace` is also what a slot on its block's free list reads,
        // while `used` falls at a return and stands at a withholding. The loop
        // took whatever that block had free, so the count rises by exactly the
        // slots taken out of it when the three members are still occupants,
        // and by three fewer when they came back.
        let occupants = unsafe { occupants_beside(ring[0] as *mut RcHeader) } as usize;
        let taken_here = taken
            .iter()
            .filter(|&&slot| block_of(slot as *const u8) == ring_block)
            .count();
        // And the addresses, for the same return read as a slot rather than as
        // a count: a retry served off a member's slot hands out one of these
        // three.
        let members: Vec<usize> = ring.iter().map(|&member| member as usize).collect();
        let reused = taken
            .iter()
            .filter(|&&slot| members.contains(&(slot as usize)))
            .count();

        drop(_budgeted);
        unsafe { give_back(&taken) };
        (states, occupants_before + taken_here - 3, occupants, reused)
    })
    .join()
    .unwrap();

    assert_eq!(
        states,
        vec![SlotState::DeadInPlace; 3],
        "every member is torn down and its slot taken"
    );
    assert_eq!(
        occupants, expected_occupants,
        "retirement returned all three member slots before their reuse"
    );
    assert_eq!(
        reused, 3,
        "the cap forces all three member slots to serve later requests"
    );
}
