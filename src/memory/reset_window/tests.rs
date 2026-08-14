//! What the window itself owns: its nesting, and the question it answers
//! about a retained block. What it parks is observable only through a
//! reset that kills a large survivor, and those tests live beside the
//! reset (`promote::tests`).

use super::*;

/// A window opened inside another restores it rather than replacing it:
/// a destructor run by one reset can resolve a second arena and reset it.
#[test]
fn a_nested_window_restores_the_one_it_displaced() {
    let _g = crate::memory::block_pool::test_guard();
    assert!(!is_open(), "a window was left open by an earlier test");

    let outer_guard = opened();
    let outer = WINDOW.with(|cell| cell.get());
    {
        let _inner = opened();
        assert_ne!(
            WINDOW.with(|cell| cell.get()),
            outer,
            "the inner open reused the outer window"
        );
    }

    assert_eq!(
        WINDOW.with(|cell| cell.get()),
        outer,
        "the inner guard's drop did not restore the outer window"
    );

    drop(outer_guard);
    assert!(!is_open(), "the outer guard's drop left a window open");
}

/// The absorb question has three answers, and only one of them is true:
/// a reset takes back its own corpse's free, leaves an earlier reset's
/// occupant alone, and answers nothing at all outside a reset.
#[test]
fn only_an_unindexed_block_inside_a_reset_is_absorbed() {
    let _g = crate::memory::block_pool::test_guard();
    // An address, not memory: the question is asked of the registry, and
    // `register` with no occupants reads nothing through it.
    let block = 0x5eed_0000usize;

    assert!(
        !absorbs_retained_free(block),
        "a free outside a reset was absorbed"
    );

    let guard = opened();
    assert!(
        absorbs_retained_free(block),
        "the reset did not absorb the free of a block it has not indexed"
    );

    assert!(
        unsafe { crate::memory::retained::register(block, Vec::new()) },
        "an index with no occupants is empty on arrival"
    );
    assert!(
        !absorbs_retained_free(block),
        "an occupant an earlier reset counted was absorbed by this one"
    );

    assert!(
        crate::memory::retained::occupant_freed(block),
        "the index outlived the test that registered it"
    );
    drop(guard);
}
