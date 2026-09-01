//! Four populations reach the trace through one child pointer. The
//! block's kind separates three of them; an entity past one block
//! payload shares its kind with the arena, and the memory category is
//! what separates that one. What each test here pins is the row's
//! **identity**: a descent that terminates proves nothing, because
//! arithmetic that returns a neighbour's row terminates just as cleanly
//! and decrements a live entity's count.

use super::*;
use crate::memory::block_pool::{
    BLOCK_KIND_ARENA, BLOCK_KIND_ENTITY_LARGE, BLOCK_KIND_ENTITY_LARGE_RUN, BLOCK_KIND_IMMORTAL,
    BLOCK_KIND_RETAINED,
};
use crate::test_support::{POOLED_FILLERS, RUN_FILLERS, wide_class};

/// The strided population. Every slot of one entity block answers with
/// its own index, checked against the division rather than against an
/// address rebuilt from the index, which would agree with any
/// reciprocal at all.
#[test]
fn every_slot_of_an_entity_block_resolves_to_its_own_row() {
    let _g = test_guard();
    let class = ClassBuilder::new("RowSlot").prop("x", true).build();
    let object_size = unsafe { (*class).object_size } as usize;

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let entities: Vec<*mut Object> = (0..8)
        .map(|_| unsafe { new_constructed(&mut ctx, class, MemoryCategory::GcHeap) })
        .collect();

    let mut rows = Vec::new();
    for &entity in &entities {
        assert_eq!(
            unsafe { block_kind(entity as usize) },
            crate::memory::block_pool::BLOCK_KIND_ENTITY
        );
        let resolved = unsafe { resolve_edge_target(entity as *mut RcHeader) };
        assert_eq!(
            resolved,
            EdgeTarget::Tracked(row_by_division(entity, object_size)),
            "the reciprocal multiply and the division name the same slot"
        );
        rows.push(resolved);
    }

    for (i, row) in rows.iter().enumerate() {
        assert!(
            !rows[..i].contains(row),
            "two live slots of one block share a row"
        );
    }

    for &entity in &entities {
        unsafe {
            assert!(ll_release(entity as *mut RcHeader));
            ll_object_die(entity);
        }
    }
}

/// The population the arithmetic cannot serve: a former arena block, bump
/// filled at mixed sizes. Both survivors land in one block, and the two
/// rows have to differ — an implementation that divided by a stride here
/// would give the wider occupant the narrower one's row.
#[test]
fn two_sizes_in_one_retained_block_resolve_to_distinct_rows() {
    let _g = test_guard();
    let narrow = ClassBuilder::new("RowNarrow").prop("x", true).build();
    let wide = ClassBuilder::new("RowWide")
        .prop("a", true)
        .prop("b", true)
        .prop("c", true)
        .prop("d", true)
        .build();
    let holder_class = ClassBuilder::new("RowHolder")
        .prop("first", true)
        .prop("second", true)
        .build();
    assert_ne!(
        unsafe { (*narrow).object_size },
        unsafe { (*wide).object_size },
        "the fixture's point is that the two occupants are different sizes"
    );

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let holder = unsafe { new_constructed(&mut ctx, holder_class, MemoryCategory::GcHeap) };
    let small = unsafe { new_constructed(&mut ctx, narrow, MemoryCategory::RequestArena) };
    let large = unsafe { new_constructed(&mut ctx, wide, MemoryCategory::RequestArena) };
    let block = small as usize & !BLOCK_MASK;
    assert_eq!(
        large as usize & !BLOCK_MASK,
        block,
        "one arena bump filled both, so they share a block"
    );

    // Both escape into a heap holder, which is what makes them survive
    // the reset and turns their block retained.
    unsafe { store_prop(&mut arena, holder, 16, small) };
    unsafe { store_prop(&mut arena, holder, 32, large) };
    unsafe { crate::promote::arena_reset_full(&mut arena) };

    assert_eq!(unsafe { block_kind(block) }, BLOCK_KIND_RETAINED);
    let occupants = crate::memory::retained::snapshot()
        .into_iter()
        .find(|&(registered, _)| registered == block)
        .expect("the reset registered the retained block's occupant index")
        .1;

    let mut rows = Vec::new();
    for &survivor in &[small, large] {
        // A linear scan of the same index the dispatch binary-searches:
        // the same data read another way, so an agreement is about the
        // dispatch rather than about the search.
        let position = occupants
            .iter()
            .position(|&address| address == survivor as usize)
            .expect("a survivor is named by its block's index");
        let resolved = unsafe { resolve_edge_target(survivor as *mut RcHeader) };
        assert_eq!(
            resolved,
            EdgeTarget::Tracked(RowKey {
                block,
                index: position as u32,
                population: Population::Retained,
            })
        );
        rows.push(resolved);
    }

    // The distinctness the name promises follows from the two
    // assertions above: each row is pinned to its own position in an
    // array of distinct sorted addresses, so an `assert_ne!` here could
    // not fail. A stride implementation is refuted by the same pair,
    // since a stride answers with byte-offset quotients.

    unsafe {
        assert!(ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
}

/// Both halves of the large-entity population: the pooled block and the
/// OS-direct run. Neither has slots to index, so the row is the one its
/// own block header carries.
///
/// **What this pins is the arm's existence, not its index.** Merging it
/// into the entity arm would answer row 0 here too, by an accident:
/// `LargeEntityHeader::_pad` sits where a `HeapBlockHeader` keeps its
/// size class and `commission` writes it zero, so the reciprocal for
/// `SIZE_CLASSES[0]` over offset zero gives zero. The arm is separate
/// for safety rather than for its value — reading a size class out of a
/// header that has none indexes `SIZE_CLASSES` by whatever that field is
/// repurposed to hold. What the arm's category test buys is exercised by
/// `a_child_outside_the_gc_heap_stops_the_descent`.
#[test]
fn a_large_entity_resolves_to_the_one_row_in_its_own_block() {
    let _g = test_guard();
    for (name, fillers, kind) in [
        ("RowPooled", POOLED_FILLERS, BLOCK_KIND_ENTITY_LARGE),
        ("RowRun", RUN_FILLERS, BLOCK_KIND_ENTITY_LARGE_RUN),
    ] {
        let class = wide_class(name, fillers, None);
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let entity = unsafe { new_constructed(&mut ctx, class, MemoryCategory::GcHeap) };
        let block = entity as usize & !BLOCK_MASK;

        assert_eq!(unsafe { block_kind(block) }, kind);
        assert_eq!(
            entity as usize,
            block + LINE_SIZE,
            "the sole occupant starts where the header line ends"
        );
        assert_eq!(
            unsafe { resolve_edge_target(entity as *mut RcHeader) },
            EdgeTarget::Tracked(RowKey {
                block,
                index: SINGLE_ENTITY_INDEX,
                population: Population::SingleEntity,
            })
        );

        unsafe {
            assert!(ll_release(entity as *mut RcHeader));
            ll_object_die(entity);
        }
    }
}

/// Every child outside the collected heap, and the third case is the one the
/// block kind cannot answer: an arena entity past one block payload takes the
/// kind a heap one takes, so only the category separates them
/// (`resolve_edge_target`, the large arm). What makes it worth a case of its
/// own is where a wrong answer lands — the teardown commits half a double free
/// and the arena's reset completes it, nowhere near the dispatch that caused
/// it.
#[test]
fn a_child_outside_the_gc_heap_stops_the_descent() {
    let _g = test_guard();
    let class = ClassBuilder::new("RowOutside").prop("x", true).build();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    let in_arena = unsafe { new_constructed(&mut ctx, class, MemoryCategory::RequestArena) };
    assert_eq!(unsafe { block_kind(in_arena as usize) }, BLOCK_KIND_ARENA);
    assert_eq!(
        unsafe { resolve_edge_target(in_arena as *mut RcHeader) },
        EdgeTarget::Untracked
    );

    let immortal = unsafe { new_constructed(&mut ctx, class, MemoryCategory::Immortal) };
    assert_eq!(
        unsafe { block_kind(immortal as usize) },
        BLOCK_KIND_IMMORTAL
    );
    assert_eq!(
        unsafe { resolve_edge_target(immortal as *mut RcHeader) },
        EdgeTarget::Untracked
    );

    let wide = wide_class("RowArenaRun", RUN_FILLERS, None);
    let in_a_run = unsafe { new_constructed(&mut ctx, wide, MemoryCategory::RequestArena) };
    assert_eq!(
        unsafe { block_kind(in_a_run as usize) },
        BLOCK_KIND_ENTITY_LARGE_RUN,
        "an arena entity past one block payload takes the kind a heap one takes"
    );
    assert_eq!(
        unsafe { resolve_edge_target(in_a_run as *mut RcHeader) },
        EdgeTarget::Untracked
    );

    // No escapee, so the reset hands back the arena's blocks and frees
    // the run from its own log; the immortal entity is never freed,
    // which is what its category means.
    unsafe { crate::promote::arena_reset_full(&mut arena) };
}
