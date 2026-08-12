use super::*;
use crate::memory::arena::Arena;
use crate::memory::block_pool::{BLOCK_PAYLOAD, test_guard};
use crate::memory::context::LLContext;
use crate::memory::heap::MAX_SMALL;

/// An entity of exactly what a category's allocator packs into a
/// shared block is served from that allocator, and the first byte
/// past it is served too — from a block-aligned allocation of its
/// own, which is what `kind` reports. The limits differ because the
/// allocators do: both arenas bump within one block, and the entity
/// heap has size classes up to `MAX_SMALL`, past which a packed slot
/// would take a whole block and leave the walk.
fn served_at_the_limit_and_past_it(
    category: MemoryCategory,
    limit: usize,
    shared_kind: u32,
    own_kind: u32,
) {
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;

    let at = unsafe { entity_alloc_in(context_ptr, category, limit) };
    assert!(
        !at.is_null(),
        "{category:?} refused an entity of exactly what it can pack"
    );
    assert_eq!(
        unsafe { block_kind_of(at) },
        shared_kind,
        "{category:?} gave a whole allocation to an entity that shares"
    );

    let past = unsafe { entity_alloc_in(context_ptr, category, limit + 1) };
    assert!(
        !past.is_null(),
        "{category:?} refused an entity one byte past what it packs"
    );
    assert_eq!(
        unsafe { block_kind_of(past) },
        own_kind,
        "{category:?} packed an entity larger than its packing unit"
    );
    assert_eq!(
        past as usize % crate::memory::block_pool::LINE_SIZE,
        0,
        "the entity starts on the line after the header"
    );
    assert_ne!(
        (at as usize) & !crate::memory::block_pool::BLOCK_MASK,
        (past as usize) & !crate::memory::block_pool::BLOCK_MASK,
        "{category:?} put both in one block, so nothing stopped sharing"
    );

    // What this test allocates it gives back, both halves: a leaked
    // entity slot keeps its block off the pool for the life of the
    // binary, and a leaked large entity keeps a whole block or run.
    // `ll_free` serves every category here — arena and immortal reach
    // no-op arms, and a virgin slot reads the refcount 0 its
    // commissioning left.
    unsafe {
        crate::memory::stdapi::ll_free(at);
        // Except the arena's run, which the arena owns: it is in the
        // large-run log, so the reset below is its free and a second
        // one here would hand the same memory back twice.
        if category != MemoryCategory::RequestArena {
            crate::memory::stdapi::ll_free(past);
        }
    }

    arena.reset(|_| {});
}

/// The kind of the block an entity address belongs to — one mask and
/// one load, the same route every free takes.
unsafe fn block_kind_of(entity: *mut u8) -> u32 {
    let block = (entity as usize & !crate::memory::block_pool::BLOCK_MASK) as *const u32;
    unsafe { *block }
}

mod where_a_category_gets_its_bytes;
