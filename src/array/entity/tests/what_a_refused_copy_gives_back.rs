//! A copy that cannot be finished gives back everything it took, its own
//! entity slot included.
//!
//! The instrument is a counted number of free heap array slots. Arrays
//! are allocated before the pool is made to refuse, every slot the
//! thread's blocks still had is taken, and exactly as many as the copy
//! should consume are given back.
//!
//! **What a slot count can and cannot say.** It cannot say a slot was
//! taken at all: a copy that took two and returned two leaves the same
//! free list as a copy refused before it built anything. So the count
//! carries a lower bound, an upper bound and a positive control — as many
//! allocations must succeed as the copy consumed, the next one must fail,
//! and the same fixture with one more free slot must produce a copy.
//! Together those three say the refusal landed where the test aims it and
//! that each slot came back exactly once. The upper bound is the half
//! that catches a slot returned **twice**, which reads like a leak repair
//! and is a double free.
//!
//! A refusal flag is raised through [`Refusing`] rather than by a bare
//! store. Assertions do fire inside that window — the crate's own
//! `debug_assert`s among them, which is what a regression in the giveback
//! trips — and a flag left raised makes every concurrently running test
//! meet an allocator that refuses (`dev/POSTMORTEM.md`, 2026-08-12).

use super::*;
use crate::memory::block_pool::FORCE_OOM;
use crate::memory::buffer_arena::FORCE_REFUSE_LONGLIVED;
use std::sync::atomic::{AtomicBool, Ordering};

/// A raised refusal flag that lowers itself on the way out of the scope,
/// including the way out a panic takes.
struct Refusing(&'static AtomicBool);

impl Refusing {
    fn raise(flag: &'static AtomicBool) -> Self {
        flag.store(true, Ordering::Relaxed);
        Refusing(flag)
    }
}

impl Drop for Refusing {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

/// Take every heap array slot the thread can still serve. Call with the
/// pool already refusing, or this runs until the machine is out of memory
/// rather than until the pool says no.
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
/// and keeps a hole and its bump cursor, so a longlived payload asked for
/// while the pool refuses is served without a new block. Without this the
/// copy cannot get past its own storage or its work list, and every
/// refusal lands on the destination's first insert rather than where the
/// test aims it — which is what the positive control below is for.
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

    {
        let _refusing = Refusing::raise(&FORCE_OOM);
        let fillers = unsafe { exhaust_heap_arrays() };
        // One slot free: the root destination takes it, and the nested
        // one is what the pool has left to refuse.
        unsafe { give_one_back(warm) };
        let copy = unsafe { separate(src, MemoryCategory::GcHeap, arena_ptr, CopyReason::Escape) };
        assert!(
            copy.is_null(),
            "the copy was meant to be refused at the nested destination"
        );
        unsafe { give_all_back(fillers) };
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}

/// Two children and two free slots: the first child's copy is published
/// into the destination and the second is what the pool refuses, so the
/// teardown under test has a child to release and storage to dispose.
/// Both slots come back, and only both — a third array must still be
/// refused, or a slot went onto the free list twice.
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

    {
        let _refusing = Refusing::raise(&FORCE_OOM);
        let fillers = unsafe { exhaust_heap_arrays() };
        unsafe { give_one_back(first) };
        unsafe { give_one_back(second) };
        let copy = unsafe { separate(src, MemoryCategory::GcHeap, arena_ptr, CopyReason::Escape) };
        // The free list is the only source left, so these read the
        // giveback directly.
        let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let b = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let third = unsafe { ll_array_new(MemoryCategory::GcHeap) };

        assert!(copy.is_null(), "the copy was meant to be refused");
        assert!(
            !a.is_null(),
            "neither the root nor the nested copy gave its slot back"
        );
        assert!(
            !b.is_null(),
            "one of the root and the nested copy kept its slot"
        );
        assert_ne!(a, b, "one slot came back twice");
        assert!(
            third.is_null(),
            "the refusal gave back more slots than the copy took"
        );

        unsafe {
            give_one_back(a);
            give_one_back(b);
            give_all_back(fillers);
        }
    }

    unsafe { crate::memory::buffer_arena::buffer_free_longlived_payload(held.0, held.1) };
    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}

/// The positive control for the fixture above: the same source and the
/// same warmed buffer arena with **three** slots free instead of two
/// produce a copy. That is what says the two-slot run was refused at the
/// second child's entity slot rather than earlier — the counting tests
/// cannot tell a refusal at the destination's own first insert from the
/// one they aim at, and a warmed arena is allocator state they do not own.
#[test]
fn a_third_slot_is_the_difference_between_a_refusal_and_a_copy() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    let src = unsafe { arena_source_with_nested_arrays(2) };
    let held = warm_the_buffer_arena();
    let spare = [arr(), arr(), arr()];

    {
        let _refusing = Refusing::raise(&FORCE_OOM);
        let fillers = unsafe { exhaust_heap_arrays() };
        for a in spare {
            unsafe { give_one_back(a) };
        }

        let copy = unsafe { separate(src, MemoryCategory::GcHeap, arena_ptr, CopyReason::Escape) };
        assert!(
            !copy.is_null(),
            "three slots is what the copy asks for and it was still refused"
        );
        unsafe {
            crate::refcount::ll_release(copy as *mut RcHeader);
            crate::object::ll_entity_die(copy as *mut RcHeader);
            give_all_back(fillers);
        }
    }

    unsafe { crate::memory::buffer_arena::buffer_free_longlived_payload(held.0, held.1) };
    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}

/// The work list refusing is the copy's other arrival at a child it
/// cannot record, and the child's destination is already built when it
/// comes. `FORCE_REFUSE_LONGLIVED` is what drives it: the entity heap
/// serves the root and the copy while the buffer arena refuses the list.
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

    {
        let _pool = Refusing::raise(&FORCE_OOM);
        let _buffers = Refusing::raise(&FORCE_REFUSE_LONGLIVED);
        let fillers = unsafe { exhaust_heap_arrays() };
        // Two slots: the root takes one and the nested copy the other,
        // and the list is refused with that copy already built.
        unsafe { give_one_back(first) };
        unsafe { give_one_back(second) };
        let copy = unsafe { separate(src, MemoryCategory::GcHeap, arena_ptr, CopyReason::Escape) };
        let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let b = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let third = unsafe { ll_array_new(MemoryCategory::GcHeap) };

        assert!(
            copy.is_null(),
            "the copy was meant to be refused at the work list"
        );
        assert!(!a.is_null(), "the refusal gave no slot back");
        assert!(
            !b.is_null(),
            "the nested copy the list could not record kept its slot"
        );
        assert_ne!(a, b, "one slot came back twice");
        assert!(
            third.is_null(),
            "the refusal gave back more slots than the copy took"
        );

        unsafe {
            give_one_back(a);
            give_one_back(b);
            give_all_back(fillers);
        }
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}

/// The shallow door with an arena holder is where the giveback's release
/// reports no death — an arena cell is the reset's — and the teardown
/// runs anyway, which is the paragraph on `object::destroy_unpublished`
/// that nothing else exercises through a refusal.
///
/// Reaching it means leaving the arena room for the destination and none
/// for its first storage chunk: past the bump `Arena::alloc` draws a
/// block from the pool, which is what the raised flag refuses.
///
/// This is coverage rather than a regression. An arena cell is the
/// reset's either way, so no assertion here can separate a giveback that
/// runs from one that does not; what the test pins is that the arm is
/// reached at all and that nothing in it faults or writes into the
/// source.
#[test]
fn a_refused_arena_separation_leaves_the_source_whole() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    let src = unsafe { ll_array_new(MemoryCategory::RequestArena) };
    unsafe {
        crate::array::testing::insert(src, Key::Int(0), Value::int(7));
    }

    // Through the raw pointer, never through the binding: a fresh `&mut`
    // here would retag and invalidate the pointer the context holds
    // (`dev/WORKFLOW.md`, Miri).
    let entity = (size_of::<LLArray>() + 7) & !7;
    loop {
        let left = unsafe { (*arena_ptr).room_left() };
        if left <= entity {
            break;
        }

        let take = (left - entity).min(crate::memory::block_pool::BLOCK_PAYLOAD);
        assert!(
            !unsafe { (*arena_ptr).alloc(take) }.is_null(),
            "the arena refused a size its own bump holds"
        );
    }

    {
        let _refusing = Refusing::raise(&FORCE_OOM);
        let copy = unsafe {
            separate(
                src,
                MemoryCategory::RequestArena,
                arena_ptr,
                CopyReason::Duplication,
            )
        };

        assert!(copy.is_null(), "the arena copy was meant to be refused");
        assert_eq!(
            unsafe { crate::array::testing::get(src, Key::Int(0)) }
                .expect("the source lost its entry")
                .as_int(),
            7,
            "the refusal wrote into the source"
        );
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}
