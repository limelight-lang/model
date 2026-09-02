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
/// occupant alone, and answers nothing at all outside a reset. The
/// question is asked of the block's count word and not of its list, so
/// the block here counts one occupant and lists nothing, which is the
/// state a reset leaves a block in when it could place no list.
#[test]
fn only_an_uncounted_block_inside_a_reset_is_absorbed() {
    use crate::memory::block_pool::BlockHeader;
    let _g = crate::memory::block_pool::test_guard();
    let block = crate::memory::retained::bare_retained_block();
    let cell = BlockHeader::payload_start(block as *mut BlockHeader) as *mut u64;
    unsafe { cell.write(1) };

    assert!(
        !unsafe { absorbs_retained_free(block) },
        "a free outside a reset was absorbed"
    );

    let guard = opened();
    assert!(
        unsafe { absorbs_retained_free(block) },
        "the reset did not absorb the free of a block whose count it has not established"
    );

    assert!(
        !unsafe {
            crate::memory::retained::register(block, &[cell as usize], std::ptr::null_mut())
        },
        "a block with a live occupant is not empty on arrival"
    );
    assert!(
        !unsafe { absorbs_retained_free(block) },
        "an occupant an earlier reset counted was absorbed by this one"
    );

    assert!(
        unsafe { crate::memory::retained::occupant_freed(block) },
        "the count outlived the test that established it"
    );
    unsafe { cell.write(0) };
    unsafe { crate::memory::retained::release_emptied(block) };
    drop(guard);
}
