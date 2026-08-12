//! A copy that cannot be finished gives back everything it took, its
//! own slot included. The refusal here lands on the **nested
//! destination's own entity slot**, which is the one arrival where the
//! copy pointer is null and the one where the root already holds a slot
//! of its own to return.
//!
//! The instrument is a single free heap slot rather than a census: one
//! array is allocated before the pool is made to refuse, every slot its
//! block still had is taken, and that first array is given back. The
//! thread's free list then holds exactly one address, so whether the
//! refused copy returned the root's slot is the difference between the
//! next array being that same address and there being no next array.
//!
//! Nothing between raising `FORCE_OOM` and lowering it may panic: a test
//! that dies with the flag up leaves every later test on the thread
//! meeting an allocator that refuses.

use super::*;
use crate::memory::block_pool::FORCE_OOM;
use std::sync::atomic::Ordering;

/// Take every heap array slot the thread can still serve. Call with
/// `FORCE_OOM` already raised, or this runs until the machine is out of
/// memory rather than until the pool refuses.
unsafe fn exhaust_heap_arrays() -> Vec<*mut LLArray> {
    let mut fillers = Vec::new();
    loop {
        let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        if a.is_null() {
            break;
        }

        fillers.push(a);
    }

    fillers
}

unsafe fn give_one_back(a: *mut LLArray) {
    unsafe {
        crate::refcount::ll_release(a as *mut RcHeader);
        crate::object::ll_entity_die(a as *mut RcHeader);
    }
}

unsafe fn give_all_back(fillers: Vec<*mut LLArray>) {
    for a in fillers {
        unsafe { give_one_back(a) };
    }
}

/// An arena array holding one nested arena array, which is the child
/// whose copy recurses and therefore the one child that asks for a heap
/// slot of its own.
unsafe fn arena_source_with_a_nested_array() -> *mut LLArray {
    let src = unsafe { ll_array_new(MemoryCategory::RequestArena) };
    let nested = unsafe { ll_array_new(MemoryCategory::RequestArena) };
    unsafe {
        // The entry's own reference: `insert` writes the entry raw and
        // leaves the counting to the caller.
        crate::refcount::ll_retain(nested as *mut RcHeader);
        crate::array::testing::insert(
            src,
            Key::Int(0),
            Value::entity(crate::value::Tag::Array, nested as *mut RcHeader),
        );
    }

    src
}

/// The refused nested destination arrives as null, and the branch that
/// unwinds it tests that before it reads it: `ll_release` opens on
/// `&mut *entity` in one configuration and on a flags load in the other,
/// so a read there is a fault rather than a wrong answer.
#[test]
fn a_refused_nested_destination_is_tested_before_it_is_read() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    // Arena memory throughout, so building the source takes no heap slot
    // and the exhaustion below cannot refuse it.
    let src = unsafe { arena_source_with_a_nested_array() };
    let warm = arr();

    FORCE_OOM.store(true, Ordering::Relaxed);
    let fillers = unsafe { exhaust_heap_arrays() };
    // One slot free: the root destination takes it, and the nested one is
    // what the pool has left to refuse.
    unsafe { give_one_back(warm) };
    let copy = unsafe { separate(src, MemoryCategory::GcHeap, arena_ptr, CopyReason::Escape) };
    FORCE_OOM.store(false, Ordering::Relaxed);

    assert!(
        copy.is_null(),
        "the copy was meant to be refused at the nested destination"
    );

    unsafe { give_all_back(fillers) };
    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}

/// The root destination is allocated before the refusal and is named by
/// nothing when it arrives, so the unwind owes its slot back. With the
/// pool refusing, that slot is the only one the thread can serve: the
/// array asked for after the refusal is either the same address or
/// nothing at all.
#[test]
fn a_refusal_gives_the_root_destinations_slot_back() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    let src = unsafe { arena_source_with_a_nested_array() };
    let probe = arr();

    FORCE_OOM.store(true, Ordering::Relaxed);
    let fillers = unsafe { exhaust_heap_arrays() };
    unsafe { give_one_back(probe) };
    let copy = unsafe { separate(src, MemoryCategory::GcHeap, arena_ptr, CopyReason::Escape) };
    // Still under the raised flag, the free list being the only source
    // left: this succeeds exactly when the refused root gave its slot
    // back.
    let after = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    FORCE_OOM.store(false, Ordering::Relaxed);

    assert!(copy.is_null(), "the copy was meant to be refused");
    assert_eq!(
        after, probe,
        "the refused copy kept the slot it took for the root destination"
    );

    unsafe { give_one_back(after) };
    unsafe { give_all_back(fillers) };
    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}
