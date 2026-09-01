//! One module answers where a memory category allocates from, and the
//! answer changes at the size limit rather than at the category: past
//! it every category takes a block-aligned allocation of its own, and
//! the limits differ only because the allocators do.

use super::*;

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

/// The arena bump-packs up to a block payload and takes a run of its own past
/// it, through `Arena::alloc_entity`. `Arena::alloc` keeps refusing that size
/// and is pinned doing so by
/// `arena::tests::what_the_arena_refuses::absurd_size_is_refused_instead_of_wrapping`:
/// it serves `ll_arena_alloc` from the C ABI, where an entity and a byte buffer
/// are the same request, so the split has to be made by an entry point that
/// knows which one it is holding.
#[test]
fn a_request_arena_entity_past_one_block_payload_takes_a_run_of_its_own() {
    let _g = test_guard();
    served_at_the_limit_and_past_it(
        MemoryCategory::RequestArena,
        BLOCK_PAYLOAD,
        crate::memory::block_pool::BLOCK_KIND_ARENA,
        crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE_RUN,
    );
}

#[test]
fn a_heap_entity_past_the_largest_size_class_takes_a_block_of_its_own() {
    let _g = test_guard();
    served_at_the_limit_and_past_it(
        MemoryCategory::GcHeap,
        MAX_SMALL,
        crate::memory::block_pool::BLOCK_KIND_ENTITY,
        crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE,
    );
}

#[test]
fn a_long_lived_entity_takes_the_same_shape_as_a_heap_one() {
    let _g = test_guard();
    served_at_the_limit_and_past_it(
        MemoryCategory::LongLived,
        MAX_SMALL,
        crate::memory::block_pool::BLOCK_KIND_ENTITY,
        crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE,
    );
}

/// The immortal region needs none of `large_entity`'s machinery — an
/// immortal entity is never freed and never walked — and it already
/// serves a larger request from a run of its own, which is why its
/// gate was a policy rather than a limit.
#[test]
fn an_immortal_entity_past_one_block_payload_comes_from_a_run() {
    let _g = test_guard();
    served_at_the_limit_and_past_it(
        MemoryCategory::Immortal,
        BLOCK_PAYLOAD,
        crate::memory::block_pool::BLOCK_KIND_IMMORTAL,
        crate::memory::block_pool::BLOCK_KIND_IMMORTAL,
    );
}
