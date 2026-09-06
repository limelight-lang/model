//! An out-of-line string draws its entity slot before its payload, so a
//! refused payload leaves a slot no caller will ever name. Both factories
//! that take a slot in that order give it back.
//!
//! **The instrument is the block's occupant count**, which rises at the
//! allocation and falls at the free whatever order the block hands slots out
//! in (`crate::memory::heap::block_occupancy`). Equality across the refused
//! build is the whole assertion: one higher is the slot kept, one lower is
//! the slot returned twice.
//!
//! **What says the slot was taken at all** is the refusal counter. A build
//! that failed before its own allocation leaves the count equal too, and the
//! payload is asked for after the slot is drawn, so a refusal recorded there
//! places the failure past the line under test
//! (`crate::memory::buffer_arena::REFUSALS`).
//!
//! **The slot the build draws is a recycled one, and that is what pins which
//! free is called.** A virgin slot carries no mark, so a plain `ll_free` of it
//! works and the choice between the two is unexercised; a slot a free has
//! taken is refused a second free, and losing it is what
//! `crate::memory::stdapi::free_unpublished` exists to prevent. The class's
//! virgin space is drained through a reservation and given straight back,
//! which leaves the free list holding it and the bump with nothing, and the
//! probe below asserts the state of the slot it is served.
//!
//! **The refusal is the buffer arena's rather than the pool's.** The entity
//! slot comes from the entity heap and the payload from the long-lived buffer
//! layer, and only the second may fail: refusing the pool would take the slot
//! down with the payload and the build would never reach the line under test.

use super::*;
use crate::memory::block_pool::BLOCK_MASK;
use crate::memory::buffer_arena::{FORCE_REFUSE_LONGLIVED, REFUSALS};
use crate::memory::heap::{
    block_occupancy, entity_alloc, ll_entity_cells_return, ll_entity_reserve,
};
use crate::refcount::{SlotState, slot_state};
use std::sync::atomic::Ordering;

/// A raised refusal flag that lowers itself on the way out of the scope,
/// including the way out a panic takes: a flag left raised makes every
/// concurrently running test meet a layer that refuses
/// (`dev/POSTMORTEM.md`, 2026-08-12).
struct RefusingLongLived;

impl RefusingLongLived {
    fn raise() -> Self {
        FORCE_REFUSE_LONGLIVED.store(true, Ordering::Relaxed);
        RefusingLongLived
    }
}

impl Drop for RefusingLongLived {
    fn drop(&mut self) {
        FORCE_REFUSE_LONGLIVED.store(false, Ordering::Relaxed);
    }
}

/// Content wide enough that the string is out of line in the GC heap, which
/// is what puts a payload behind the entity slot.
fn content() -> Vec<u8> {
    vec![b'p'; crate::memory::heap::MAX_SMALL + 16]
}

/// Leave the class of the string entity serving recycled slots, and answer the
/// block holding the one the next allocation takes.
///
/// Three steps, and each is load-bearing. The class's virgin space is drained
/// into a reservation and returned, so the free list is the only source left.
/// A probe slot is then drawn and freed through `free_unpublished`, which
/// puts it at the head of that list and leaves it carrying the mark `ll_free`
/// takes — asserted rather than assumed, because a virgin slot reads `Free`
/// here and would make the clear under test a no-op. The block is the
/// probe's, which is therefore the block the refused build draws from.
///
/// # Safety
/// Runs on a thread with an initialised entity heap.
unsafe fn a_class_serving_recycled_slots() -> *mut u8 {
    let size = size_of::<LLStringDynamic>();
    let mut drained = vec![std::ptr::null_mut::<u8>(); 4096];
    let mut contiguous = 0usize;
    let n = unsafe {
        ll_entity_reserve(
            size,
            drained.len(),
            drained.as_mut_ptr(),
            &raw mut contiguous,
        )
    };
    assert!(n > 1, "the class served nothing to drain; got {n}");
    unsafe { ll_entity_cells_return(drained.as_ptr(), n) };

    let probe = unsafe { entity_alloc(size) };
    assert!(
        !probe.is_null(),
        "the drained class serves from its free list"
    );
    let block = (probe as usize & !BLOCK_MASK) as *mut u8;
    assert_eq!(
        unsafe { slot_state(probe as *const RcHeader) },
        SlotState::DeadInPlace,
        "the served slot is one a free has taken, which is what the build's \
         own free has to hand back"
    );
    unsafe { crate::memory::stdapi::free_unpublished(probe) };

    block
}

/// Run `build` under a refused long-lived payload and answer what the block's
/// occupancy did across it, asserting on the way that the build reached the
/// payload at all.
///
/// # Safety
/// `block` is the entity block of this thread's class under test.
unsafe fn occupancy_across_a_refused_build(block: *mut u8, build: impl FnOnce()) -> (u32, u32) {
    let refusals = REFUSALS.load(Ordering::Relaxed);
    let before = unsafe { block_occupancy(block) };

    {
        let _refusing = RefusingLongLived::raise();
        build();
    }

    let after = unsafe { block_occupancy(block) };
    assert_eq!(
        REFUSALS.load(Ordering::Relaxed),
        refusals + 1,
        "the build reached the payload and was refused there, rather than \
         failing before it drew a slot"
    );

    (before, after)
}

/// `ll_string_new_dynamic`'s refusal, which is `new_out_of_line`'s.
#[test]
fn a_refused_payload_gives_back_the_dynamic_factorys_slot() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    let block = unsafe { a_class_serving_recycled_slots() };
    // Bound outside the call so that the unsafety of the build under test
    // stands at the build rather than under the helper's own `unsafe`.
    let build = || {
        let refused =
            unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, &content(), 0) };
        assert!(
            refused.is_null(),
            "the payload was refused, so the build was"
        );
    };
    let (before, after) = unsafe { occupancy_across_a_refused_build(block, build) };

    assert_eq!(
        after, before,
        "the refused build did not give back the slot it took"
    );
}

/// `new_uninit`'s out-of-line arm, which the template's flattening and the
/// copy-on-write separation reach.
#[test]
fn a_refused_payload_gives_back_the_reserving_factorys_slot() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    let block = unsafe { a_class_serving_recycled_slots() };
    let len = content().len();
    let build = || {
        let reserved = unsafe { crate::string::new_uninit(&mut ctx, MemoryCategory::GcHeap, len) };
        assert!(
            reserved.bytes.is_null(),
            "the payload was refused, so the reservation was"
        );
    };
    let (before, after) = unsafe { occupancy_across_a_refused_build(block, build) };

    assert_eq!(
        after, before,
        "the refused reservation did not give back the slot it took"
    );
}
