//! The dispatch backwards: a row index and its block name one entity, and it
//! is the entity whose address produced that row.
//!
//! What each case pins is the **round trip**, because the two directions can
//! agree on a neighbour and terminate cleanly: a stride that is one class off
//! answers a row for every slot and an address for every row, and the pair
//! composes to the identity only where both are the block's own arithmetic.
//! The sweep that harvests a pressure collection's members walks rows and
//! hands the addresses to a teardown (`crate::cycle::members`), so a
//! neighbour's address here is a free of a live entity.

use super::*;
use crate::cycle::row::entity_at;
use crate::memory::block_pool::{
    BLOCK_KIND_ENTITY_LARGE, BLOCK_KIND_ENTITY_LARGE_RUN, BLOCK_KIND_RETAINED,
};
use crate::test_support::{POOLED_FILLERS, RUN_FILLERS, wide_class};

/// The strided population, over every slot the fixture fills: each entity's
/// own row answers its own address.
#[test]
fn every_slot_of_an_entity_block_answers_its_own_address() {
    let _g = test_guard();
    let class = ClassBuilder::new("BackSlot").prop("x", true).build();
    let object_size = unsafe { (*class).object_size } as usize;

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let entities: Vec<*mut Object> = (0..8)
        .map(|_| unsafe { new_constructed(&mut ctx, class, MemoryCategory::GcHeap) })
        .collect();

    for &entity in &entities {
        // The division rather than the reciprocal, so the index this case
        // hands back is not the one the arm under test derived.
        let row = row_by_division(entity, object_size);
        assert_eq!(
            unsafe { entity_at(row.block as *mut u8, row.population, row.index) },
            Some(entity as *mut RcHeader)
        );
    }

    for entity in entities {
        unsafe {
            assert!(ll_release(entity as *mut RcHeader));
            ll_object_die(entity);
        }
    }
}

/// The mixed-size population, whose rows are positions in a sorted list rather
/// than a stride: two occupants of one retained block answer their own
/// addresses.
///
/// The position a list does not hold has no case here, and cannot: it is
/// outside the safety condition, and a debug build ends the process on it
/// rather than answering — which is the report the `None` arm exists to make
/// visible (`crate::cycle::row::entity_at`).
#[test]
fn a_retained_block_answers_the_survivor_its_row_names() {
    let _g = test_guard();
    let narrow = ClassBuilder::new("BackNarrow").prop("x", true).build();
    let wide = ClassBuilder::new("BackWide")
        .prop("a", true)
        .prop("b", true)
        .prop("c", true)
        .prop("d", true)
        .build();
    let holder_class = ClassBuilder::new("BackHolder")
        .prop("first", true)
        .prop("second", true)
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let holder = unsafe { new_constructed(&mut ctx, holder_class, MemoryCategory::GcHeap) };
    let small = unsafe { new_constructed(&mut ctx, narrow, MemoryCategory::RequestArena) };
    let large = unsafe { new_constructed(&mut ctx, wide, MemoryCategory::RequestArena) };
    let block = small as usize & !BLOCK_MASK;

    unsafe { store_prop(&mut arena, holder, 16, small) };
    unsafe { store_prop(&mut arena, holder, 32, large) };
    unsafe { crate::promote::arena_reset_full(&mut arena) };
    assert_eq!(unsafe { block_kind(block) }, BLOCK_KIND_RETAINED);

    let occupants = unsafe { crate::memory::retained::survivor_list_copy(block) };
    for (position, &address) in occupants.iter().enumerate() {
        assert_eq!(
            unsafe { entity_at(block as *mut u8, Population::Retained, position as u32) },
            Some(address as *mut RcHeader),
            "the list read another way names the same survivor"
        );
    }
    assert!(
        occupants.contains(&(small as usize)) && occupants.contains(&(large as usize)),
        "both survivors are in the list the case just walked"
    );

    unsafe {
        assert!(ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
}

/// Both halves of the one-entity population: the row that is a word of the
/// block's own header answers the block's sole occupant.
#[test]
fn a_large_entity_answers_the_occupant_of_its_own_block() {
    let _g = test_guard();
    for (name, fillers, kind) in [
        ("BackPooled", POOLED_FILLERS, BLOCK_KIND_ENTITY_LARGE),
        ("BackRun", RUN_FILLERS, BLOCK_KIND_ENTITY_LARGE_RUN),
    ] {
        let class = wide_class(name, fillers, None);
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let entity = unsafe { new_constructed(&mut ctx, class, MemoryCategory::GcHeap) };
        let block = entity as usize & !BLOCK_MASK;
        assert_eq!(unsafe { block_kind(block) }, kind);

        assert_eq!(
            unsafe {
                entity_at(
                    block as *mut u8,
                    Population::SingleEntity,
                    SINGLE_ENTITY_INDEX,
                )
            },
            Some(entity as *mut RcHeader)
        );

        unsafe {
            assert!(ll_release(entity as *mut RcHeader));
            ll_object_die(entity);
        }
    }
}
