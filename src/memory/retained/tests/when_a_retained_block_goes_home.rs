//! The live count is what returns the block: the last occupant's
//! death drops the index and hands the block to the pool, and an
//! occupant already dead at registration holds nothing.

use super::*;

/// The last live occupant's death empties the block, and the index
/// is gone before the caller is told to hand the block over — the
/// order the enumerators' readable-address contract requires.
#[test]
fn the_last_live_occupant_empties_the_block() {
    let _g = crate::memory::block_pool::test_guard();
    let (block, cells, live) = walkable_index(2);
    unsafe {
        live[0].write(1);
        live[1].write(1);
    }

    let _empty = unsafe { register(block, cells.clone()) };
    assert!(snapshot().iter().any(|&(b, _)| b == block));
    assert!(!occupant_freed(block), "one of two occupants emptied it");
    assert!(snapshot().iter().any(|&(b, _)| b == block));
    assert!(occupant_freed(block), "the second death left it occupied");
    assert!(!snapshot().iter().any(|&(b, _)| b == block));
    unsafe {
        live[0].write(0);
        live[1].write(0);
    }
}

/// An occupant already dead when the index is built is not counted,
/// or the block would wait forever for a death that has happened.
#[test]
fn an_occupant_dead_at_registration_holds_nothing() {
    let _g = crate::memory::block_pool::test_guard();
    let (block, cells, live) = walkable_index(2);
    let _empty = unsafe {
        live[0].write(1);
        register(block, cells.clone())
    };

    assert!(occupant_freed(block), "the dead occupant was counted live");
    assert!(!snapshot().iter().any(|&(b, _)| b == block));
    unsafe { live[0].write(0) };
}
