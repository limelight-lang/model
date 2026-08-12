//! A copy that cannot be finished gives back everything it took, its own
//! entity slot included.
//!
//! The instrument is a counted number of free heap array slots. Arrays
//! are allocated before the pool is made to refuse, every slot the
//! thread's blocks still had is taken, and exactly as many as the copy
//! should consume are given back. What the copy owes is then readable
//! from the other end: after the refusal, that many `ll_array_new` calls
//! must succeed again, and one slot short means one entity kept.
//!
//! **Two slots rather than one**, because a single slot cannot tell a
//! root that took its slot and returned it from a root that was never
//! allocated: both leave one slot free. With two, a destination that was
//! never built returns one and the assertion fails.
//!
//! Nothing between raising a refusal flag and lowering it may panic: a
//! test that dies with the flag up leaves every later test on the thread
//! meeting an allocator that refuses everything, and the crash it then
//! reports is not its own (`dev/POSTMORTEM.md`, 2026-08-12).

use super::*;
use crate::memory::block_pool::FORCE_OOM;
use crate::memory::buffer_arena::FORCE_REFUSE_LONGLIVED;
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

/// An arena array holding `n` nested arena arrays, which are the children
/// whose copies recurse and therefore the ones that ask for a heap slot
/// each.
unsafe fn arena_source_with_nested_arrays(n: i64) -> *mut LLArray {
    let src = unsafe { ll_array_new(MemoryCategory::RequestArena) };
    for i in 0..n {
        let nested = unsafe { ll_array_new(MemoryCategory::RequestArena) };
        unsafe {
            // The entry's own reference: `insert` writes the entry raw and
            // leaves the counting to the caller.
            crate::refcount::ll_retain(nested as *mut RcHeader);
            crate::array::testing::insert(
                src,
                Key::Int(i),
                Value::entity(crate::value::Tag::Array, nested as *mut RcHeader),
            );
        }
    }

    src
}

/// Two buffer-arena chunks, one of them given back: the block stays live
/// and holds a hole and its bump cursor, so a longlived payload asked for
/// while the pool refuses is served without a new block. Without this the
/// copy cannot get past its own storage or its work list, and every
/// refusal lands on the first element rather than where the test aims it.
///
/// Returns the chunk still held, for the caller to give back afterwards.
fn warm_the_buffer_arena() -> (*mut u8, usize) {
    let held = crate::memory::buffer_arena::buffer_alloc_longlived_payload(8192);
    let spare = crate::memory::buffer_arena::buffer_alloc_longlived_payload(8192);
    assert!(
        !held.0.is_null() && !spare.0.is_null(),
        "the buffer arena served nothing"
    );
    unsafe { crate::memory::buffer_arena::buffer_free_longlived_payload(spare.0, spare.1) };
    held
}

/// The refused nested destination arrives as null, and the branch that
/// unwinds it tests that before it reads it: `ll_release` opens on
/// `&mut *entity` in one configuration and on a flags load in the other,
/// so a read there faults rather than answering wrongly.
///
/// This is a crash canary and nothing more. It cannot tell the guard it
/// was written for from any other refusal that does not dereference:
/// what it pins is that the process is still alive to report.
#[test]
fn a_refused_nested_destination_is_not_read() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    // Arena memory throughout, so building the source takes no heap slot
    // and the exhaustion below cannot refuse it.
    let src = unsafe { arena_source_with_nested_arrays(1) };
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

/// The root destination and the nested copy it published are both named
/// by nothing when the refusal arrives, so the unwind owes both slots
/// back. Two children and two free slots: the first child's copy is
/// published into the destination and the second is what the pool
/// refuses, so the teardown this exercises has children to release and
/// storage to dispose rather than being empty.
#[test]
fn a_refusal_gives_back_the_root_and_the_copy_it_published() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    let src = unsafe { arena_source_with_nested_arrays(2) };
    let held = warm_the_buffer_arena();
    let first = arr();
    let second = arr();

    FORCE_OOM.store(true, Ordering::Relaxed);
    let fillers = unsafe { exhaust_heap_arrays() };
    unsafe { give_one_back(first) };
    unsafe { give_one_back(second) };
    let copy = unsafe { separate(src, MemoryCategory::GcHeap, arena_ptr, CopyReason::Escape) };
    // Still under the raised flag, the free list being the only source
    // left: both succeed exactly when both slots came back.
    let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let b = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    FORCE_OOM.store(false, Ordering::Relaxed);

    assert!(copy.is_null(), "the copy was meant to be refused");
    assert!(
        !a.is_null(),
        "neither the root nor the nested copy gave its slot back"
    );
    assert!(
        !b.is_null(),
        "one of the root and the nested copy kept its slot"
    );

    unsafe {
        give_one_back(a);
        give_one_back(b);
        give_all_back(fillers);
        crate::memory::buffer_arena::buffer_free_longlived_payload(held.0, held.1);
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}

/// The work list refusing is the copy's other arrival at a nested child
/// it cannot record, and the child's destination is already built when it
/// comes. `FORCE_REFUSE_LONGLIVED` is what drives it: the entity heap
/// serves the copy and the buffer arena refuses the list.
#[test]
fn a_refused_work_list_gives_the_nested_copy_back() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    let src = unsafe { arena_source_with_nested_arrays(1) };
    let first = arr();
    let second = arr();

    FORCE_OOM.store(true, Ordering::Relaxed);
    FORCE_REFUSE_LONGLIVED.store(true, Ordering::Relaxed);
    let fillers = unsafe { exhaust_heap_arrays() };
    // Two slots: the root takes one and the nested copy the other, and
    // the list is refused with that copy already built.
    unsafe { give_one_back(first) };
    unsafe { give_one_back(second) };
    let copy = unsafe { separate(src, MemoryCategory::GcHeap, arena_ptr, CopyReason::Escape) };
    let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let b = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    FORCE_REFUSE_LONGLIVED.store(false, Ordering::Relaxed);
    FORCE_OOM.store(false, Ordering::Relaxed);

    assert!(
        copy.is_null(),
        "the copy was meant to be refused at the work list"
    );
    assert!(!a.is_null(), "the refusal gave no slot back");
    assert!(
        !b.is_null(),
        "the nested copy the list could not record kept its slot"
    );

    unsafe {
        give_one_back(a);
        give_one_back(b);
        give_all_back(fillers);
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}
