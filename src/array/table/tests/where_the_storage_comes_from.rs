//! A long-lived table's storage is a buffer-arena chunk and a
//! request table's is an arena body; both allocators split at one
//! block payload, past which the storage is a dedicated run — the
//! split the 1025th element of a request array used to abort for. A
//! table dies wherever its last reference is dropped, so the free
//! routinely arrives from a thread that did not allocate it, and a
//! carry the reset refused leaves the category alone for promotion
//! to change.

use super::*;

/// Where a long-lived table's storage lives, pinned from both ends:
/// the block it comes out of is a buffer block, and disposing puts the
/// chunk back on that block's free list. While storage came from
/// `ll_alloc` it landed in a heap block, so the first assertion failed
/// there and the second could not be asked at all.
///
/// The return half is proved the way `string.rs` proves it for a
/// payload: in critical mode an allocation searches the free lists, so
/// the same address coming back means the chunk was really returned
/// rather than merely forgotten.
#[test]
fn heap_storage_is_a_buffer_arena_chunk_and_is_returned_to_it() {
    use crate::memory::block_pool::{BLOCK_KIND_BUFFER, BLOCK_MASK, BLOCK_PAYLOAD};
    use crate::memory::buffer::{PressureMode, set_pressure_mode};
    use crate::memory::buffer_arena::with_buffer_arena;
    let _g = crate::memory::block_pool::test_guard();

    let mut m = t();
    m.insert(Key::Int(1), Value::int(1));
    let storage = m.storage();
    let capacity = m.storage_capacity;
    assert!(!storage.is_null());
    assert!(
        capacity <= BLOCK_PAYLOAD,
        "a table of one entry is a chunk, not an OS-direct run"
    );

    let kind = unsafe { *(((storage as usize) & !BLOCK_MASK) as *const u32) };
    assert_eq!(
        kind, BLOCK_KIND_BUFFER,
        "the storage came from somewhere other than the buffer arena"
    );

    m.dispose();

    set_pressure_mode(PressureMode::Critical);
    let (reused, _) = with_buffer_arena(|a| a.alloc(capacity));
    set_pressure_mode(PressureMode::Plenty);
    assert_eq!(reused, storage, "the storage was not returned to the arena");
    with_buffer_arena(|a| unsafe { a.free(reused, capacity) });
}

/// Past a block payload the storage is an OS-direct run instead, the
/// arena's chunks being bounded by one block. The doubling that
/// crosses the line frees a chunk and allocates a run, and teardown
/// then frees the run; both are dispatched on the block kind, so a
/// storage that lands in the wrong half is released by the wrong
/// allocator. The table also has to still answer for every key it held
/// before the crossing.
#[test]
fn a_storage_over_a_block_payload_is_an_os_direct_run() {
    use crate::memory::block_pool::{BLOCK_KIND_LARGE_RUN, BLOCK_MASK, BLOCK_PAYLOAD};
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    for i in 0..1100i64 {
        m.insert(Key::Int(i), Value::int(i));
    }

    assert!(
        m.storage_capacity > BLOCK_PAYLOAD,
        "the table never grew past one block, so this proves nothing"
    );

    let kind = unsafe { *(((m.storage() as usize) & !BLOCK_MASK) as *const u32) };
    assert_eq!(
        kind, BLOCK_KIND_LARGE_RUN,
        "a storage larger than a block is a run of blocks, which is what \
         decides the free path that releases it"
    );
    for i in 0..1100i64 {
        assert_eq!(m.get(Key::Int(i)).unwrap().as_int(), i);
    }
}

/// A request-arena table has to cross the same line, and the arena
/// splits at it too: `Arena::alloc` asserts on anything larger than a
/// block payload, and a run that size belongs to `alloc_large`, which
/// records it so the reset frees it. Without the split the 1025th
/// element of a request array kills the process, the release profile
/// aborting rather than unwinding.
#[test]
fn a_request_arena_storage_over_a_block_takes_the_large_run_path() {
    use crate::memory::arena::Arena;
    use crate::memory::block_pool::BLOCK_PAYLOAD;
    use crate::memory::context::set_current_context;
    let _g = crate::memory::block_pool::test_guard();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    // An arena array, because an arena table's storage is routed by
    // the header in front of it like every other table's.
    let a = unsafe { crate::array::testing::hash_array(MemoryCategory::RequestArena) };
    let (m, head) = unsafe { crate::array::entity::as_table_mut(a) };
    for i in 0..1100i64 {
        m.insert(
            head,
            unsafe { crate::array::entity::category_of(a) },
            Key::Int(i),
            Value::int(i),
        );
    }

    assert!(
        m.storage_capacity > BLOCK_PAYLOAD,
        "the table never grew past one block, so this proves nothing"
    );
    for i in 0..1100i64 {
        assert_eq!(m.get(head, Key::Int(i)).unwrap().as_int(), i);
    }

    m.dispose(head, unsafe { crate::array::entity::category_of(a) });
    set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}

/// The reason the storage moved here at all: a table dies wherever its
/// last reference is dropped, so the thread that frees a storage is
/// routinely not the one that allocated it. What this pins is that the
/// foreign free reaches the owner's block and leaves it alive — the
/// posting stack itself is the arena's own contract, tested there.
/// Under Miri it is also the only exercise of that path in this
/// module.
#[test]
fn a_table_disposed_on_another_thread_leaves_the_owners_block_alive() {
    use crate::memory::block_pool::{BLOCK_KIND_BUFFER, BLOCK_MASK};
    let _g = crate::memory::block_pool::test_guard();

    let mut m = t();
    m.insert(Key::Int(1), Value::int(1));
    let storage = m.storage() as usize;

    // The **array** crosses, not the table: a table is embedded in
    // its entity and reads its category from the header in front of
    // it, so handing one over on its own would hand over a header
    // that is not there. An array pointer is a raw pointer and not
    // `Send` by inference; a dying entity crossing threads is the
    // case the buffer arena's ownership protocol exists for, not a
    // violation of it.
    struct HandOver(*mut crate::array::entity::LLArray);
    unsafe impl Send for HandOver {}
    let handed = std::mem::replace(&mut m, t());
    let carried = HandOver(handed.0);
    // The other thread disposes it; this one must not.
    std::mem::forget(handed);

    std::thread::spawn(move || {
        let carried = carried;
        unsafe {
            crate::array::entity::dispose_storage(
                carried.0,
                crate::array::entity::category_of(carried.0),
            );
            (*carried.0).rc.refcount = 0;
            crate::memory::stdapi::ll_free(carried.0 as *mut u8);
        }
    })
    .join()
    .unwrap();

    let kind = unsafe { *((storage & !BLOCK_MASK) as *const u32) };
    assert_eq!(
        kind, BLOCK_KIND_BUFFER,
        "the owner's block went home while the owner still held it"
    );
}

/// A refused carry decides no category of its own: it leaves the
/// storage where it is and the header saying `RequestArena`, so
/// promotion is what changes the answer a moment later
/// (`dev/DECISIONS.md`, "the `RcHeader` is the only authority on
/// which memory an entity lives in").
///
/// Where the next storage then comes from is no longer the table's to
/// decide — since S10 it is handed a category and routes by it — so
/// what it does with a promoted array is measured one layer up, in
/// `element::tests::crossing_out_of_the_arena::a_promoted_array_takes_its_next_storage_from_the_heap`.
/// The danger both halves guard is one: an owner still answering
/// `RequestArena` takes its next storage from whatever arena is
/// mounted then, and that arena's reset returns the chunk to the pool
/// with a live heap array pointing at it.
#[test]
fn a_refused_carry_leaves_the_category_where_it_was() {
    use crate::memory::arena::Arena;
    use crate::memory::block_pool::{BLOCK_PAYLOAD, FORCE_OOM};
    use crate::memory::context::set_current_context;
    use std::sync::atomic::Ordering;
    let _g = crate::memory::block_pool::test_guard();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let a = unsafe { crate::array::testing::hash_array(MemoryCategory::RequestArena) };
    let (m, head) = unsafe { crate::array::entity::as_table_mut(a) };
    for i in 0..8i64 {
        m.insert(
            head,
            unsafe { crate::array::entity::category_of(a) },
            Key::Int(i),
            Value::int(i),
        );
    }

    assert!(
        m.storage_capacity <= BLOCK_PAYLOAD,
        "an in-block storage is the only one that can be refused"
    );

    FORCE_OOM.store(true, Ordering::Relaxed);
    let carried = unsafe { crate::array::entity::carry_storage_out_of(arena_ptr, a) };
    FORCE_OOM.store(false, Ordering::Relaxed);
    assert!(!carried, "the copy was meant to be refused and was not");
    assert_eq!(
        unsafe { crate::array::entity::category_of(a) },
        MemoryCategory::RequestArena,
        "the carry decided a category of its own instead of leaving it to the header"
    );
    assert!(
        !head.storage().is_null(),
        "a refused carry left the array without the storage it had"
    );

    set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}
