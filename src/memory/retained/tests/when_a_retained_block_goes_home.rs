//! The live count is what returns the block: the last occupant's
//! death hands the block to the pool, its list going with it, and an
//! occupant already dead at registration holds nothing.

use super::*;

/// The last live occupant's death empties the block, and the block
/// goes home with the list still in its own tail — the list dies with
/// the block it describes, never before it.
#[test]
fn the_last_live_occupant_empties_the_block() {
    let _g = crate::memory::block_pool::test_guard();
    let (block, cells, live) = walkable_index(2);
    unsafe {
        live[0].write(1);
        live[1].write(1);
    }

    let _empty = unsafe { register(block, &cells, list_room(block, 2)) };
    assert!(unsafe { has_survivor_list(block) });
    assert!(
        !unsafe { occupant_freed(block) },
        "one of two occupants emptied it"
    );
    assert!(unsafe { has_survivor_list(block) });
    assert!(
        unsafe { occupant_freed(block) },
        "the second death left it occupied"
    );
    unsafe {
        live[0].write(0);
        live[1].write(0);
    }

    give_back(block);
    assert_eq!(
        kind_of(block),
        BLOCK_KIND_FREE,
        "the block outlived its last occupant"
    );
}

/// An occupant already dead when the list is published is not counted,
/// or the block would wait forever for a death that has happened.
#[test]
fn an_occupant_dead_at_registration_holds_nothing() {
    let _g = crate::memory::block_pool::test_guard();
    let (block, cells, live) = walkable_index(2);
    let _empty = unsafe {
        live[0].write(1);
        register(block, &cells, list_room(block, 2))
    };

    assert!(
        unsafe { occupant_freed(block) },
        "the dead occupant was counted live"
    );
    unsafe { live[0].write(0) };
    give_back(block);
    assert_eq!(kind_of(block), BLOCK_KIND_FREE);
}
