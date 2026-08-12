//! One module answers where a memory category allocates from, and the
//! answer changes at the size limit rather than at the category: past
//! it every category takes a block-aligned allocation of its own, and
//! the limits differ only because the allocators do.

use super::*;

/// The arena bump-packs up to a block payload and takes a run of its
/// own past it, through `Arena::alloc_entity`. `Arena::alloc` keeps
/// refusing that size and is pinned doing so by
/// `arena::tests::absurd_size_is_refused_instead_of_wrapping`: it
/// serves `ll_arena_alloc` from the C ABI, where an entity and a byte
/// buffer are the same request, so the split has to be made by a door
/// that knows which one it is holding.
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
