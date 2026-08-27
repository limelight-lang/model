//! The arena's whole contract is what it leaves behind, and the path
//! that matters is the one nobody plans for: the collection that ran out
//! of memory halfway. Every test here ends by asking whether the arena
//! kept anything, and the ones with no heap of their own ask the pool
//! the same question. A test that builds heaps cannot: `retire_empty`
//! keeps one emptied block per size class as the class's spare, so a
//! fixture heap holds a block after its last slot is freed and the pool
//! count is not the instrument there.

use super::*;

/// The ordinary life: bump, then reset, and the pool is where it was.
#[test]
fn an_arena_returns_every_block_it_took() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let before = BlockPool::global().blocks_out();

    let mut arena = ShadowArena::new();
    // Past one block, so the block list is a list rather than a block.
    for _ in 0..3 {
        assert!(!arena.alloc(BLOCK_PAYLOAD / 2).is_null());
    }
    assert!(arena.blocks_held() >= 2, "the bump crossed a block");

    arena.reset();
    assert_eq!(arena.blocks_held(), 0);
    assert_eq!(BlockPool::global().blocks_out(), before);
    crate::memory::critical::drain_for_test();
}

/// The refusal path. `FORCE_OOM` closes the ordinary door and the
/// reserve is emptied by hand, so the null comes from both doors having
/// refused rather than from one of them — and the arena still gives back
/// what it was already holding.
#[test]
fn a_refusal_at_both_doors_leaves_nothing_behind() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let before = BlockPool::global().blocks_out();

    let mut arena = ShadowArena::new();
    assert!(!arena.alloc(64).is_null(), "the ordinary door served");
    assert_eq!(arena.blocks_held(), 1);

    FORCE_OOM.store(true, Ordering::Relaxed);
    assert!(
        BlockPool::global().get().is_null(),
        "the ordinary door is refusing"
    );
    assert_eq!(
        crate::memory::critical::blocks_held(),
        0,
        "and the critical door has nothing to serve"
    );
    assert!(
        arena.alloc(BLOCK_PAYLOAD).is_null(),
        "so a growth is refused rather than a process killed"
    );
    FORCE_OOM.store(false, Ordering::Relaxed);

    arena.reset();
    assert_eq!(BlockPool::global().blocks_out(), before, "no block leaked");
    crate::memory::critical::drain_for_test();
}

/// The ordinary door is asked first, and the reserve is not touched
/// while it serves. A collection that drew on the reserve whenever it
/// wanted memory would turn the reserve into ordinary memory with extra
/// steps, which is what `critical-reserve.md` forbids in terms.
#[test]
fn the_reserve_is_untouched_while_the_pool_serves() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    assert!(crate::memory::critical::replenish());
    let held = crate::memory::critical::blocks_held();

    let mut arena = ShadowArena::new();
    for _ in 0..4 {
        assert!(!arena.alloc(BLOCK_PAYLOAD).is_null());
    }
    assert_eq!(arena.blocks_held(), 4);
    assert_eq!(
        crate::memory::critical::blocks_held(),
        held,
        "four blocks came from the pool and none from the reserve"
    );

    arena.reset();
    crate::memory::critical::drain_for_test();
}

/// The reserve serves once the pool has refused, and the arena's reset
/// puts back what it drew before the pool sees anything — the retry
/// after an abort wants a door that is open.
#[test]
fn the_reserve_serves_after_a_refusal_and_is_refilled_at_reset() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    assert!(crate::memory::critical::replenish());
    let held = crate::memory::critical::blocks_held();
    let before = BlockPool::global().blocks_out();

    let mut arena = ShadowArena::new();
    FORCE_OOM.store(true, Ordering::Relaxed);
    assert!(
        BlockPool::global().get().is_null(),
        "the ordinary door is the one refusing"
    );
    assert!(!arena.alloc(64).is_null(), "the critical door served");
    assert_eq!(
        crate::memory::critical::blocks_held(),
        held - 1,
        "and the block came out of the reserve"
    );
    assert!(crate::memory::critical::is_drawn());
    FORCE_OOM.store(false, Ordering::Relaxed);

    arena.reset();
    assert_eq!(
        crate::memory::critical::blocks_held(),
        held,
        "the reserve is whole again without a safepoint"
    );
    assert!(!crate::memory::critical::is_drawn());
    assert_eq!(
        BlockPool::global().blocks_out(),
        before,
        "and nothing changed hands with the pool"
    );
    crate::memory::critical::drain_for_test();
}

/// The stale-pointer clause. A block whose shadow pointer survives its
/// arena names memory the pool has handed to someone else, and the next
/// collection would decrement rows that now hold live payload — so the
/// abort nulls every one it stamped, not only the ones a finished
/// collection would have.
#[test]
fn an_abort_nulls_the_shadow_of_every_block_it_stamped() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let (mut heap, slot, block) = an_entity_block();

    let mut arena = ShadowArena::new();
    let rows = arena.alloc(64);
    assert!(!rows.is_null());
    unsafe { crate::memory::heap::set_block_shadow(block, rows) };
    assert!(unsafe { arena.note_touched(block) });
    assert_eq!(unsafe { crate::memory::heap::block_shadow(block) }, rows);

    // The abort is the reset, reached before anything was judged.
    arena.reset();
    assert!(
        unsafe { crate::memory::heap::block_shadow(block) }.is_null(),
        "a stamped block is nulled on the way out"
    );
    assert_eq!(arena.blocks_held(), 0);

    unsafe { heap.free(slot) };
    crate::memory::critical::drain_for_test();
}

/// The touched list outgrows one segment, and the sweep still reaches
/// the far end of the chain. A list that only ever nulled its newest
/// run would pass every test above and leave the first blocks stamped.
///
/// **Three blocks at three positions, because fewer prove less.** The
/// chain is newest-first, so the block enrolled first ends furthest from
/// the head. One block enrolled everywhere would let a walk of the head
/// segment alone read as a walk of the chain; two at the ends would let
/// a walk that visited only index 0 of each segment read the same way.
/// The third sits at the far end of the full segment, where only a walk
/// of every entry reaches it.
#[test]
fn the_sweep_reaches_every_entry_of_every_segment() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let (mut first_heap, first_slot, first) = an_entity_block();
    let (mut middle_heap, middle_slot, middle) = an_entity_block();
    let (mut last_heap, last_slot, last) = an_entity_block();
    assert_ne!(first, middle);
    assert_ne!(middle, last);
    assert_ne!(first, last);

    let mut arena = ShadowArena::new();
    let rows = arena.alloc(64);
    assert!(!rows.is_null());
    for block in [first, middle, last] {
        unsafe { crate::memory::heap::set_block_shadow(block, rows) };
    }

    // `first` at index 0 of the far segment, `middle` at its last index,
    // `last` alone in the head segment.
    assert!(unsafe { arena.note_touched(first) });
    for _ in 0..TOUCHED_PER_SEGMENT - 2 {
        assert!(unsafe { arena.note_touched(last) });
    }

    assert!(unsafe { arena.note_touched(middle) });
    assert!(unsafe { arena.note_touched(last) });

    arena.reset();
    assert!(
        unsafe { crate::memory::heap::block_shadow(last) }.is_null(),
        "the head of the chain was walked"
    );
    assert!(
        unsafe { crate::memory::heap::block_shadow(first) }.is_null(),
        "and the first entry of the run behind it"
    );
    assert!(
        unsafe { crate::memory::heap::block_shadow(middle) }.is_null(),
        "and its last entry, which only a full walk reaches"
    );

    assert_eq!(arena.blocks_held(), 0);
    unsafe { first_heap.free(first_slot) };
    unsafe { middle_heap.free(middle_slot) };
    unsafe { last_heap.free(last_slot) };
    crate::memory::critical::drain_for_test();
}

/// The sweep runs at the end of scan, not at the arena's reset, because
/// the slot returns that follow the token's release can hand a block to
/// the pool and another collection can recommission it. What that means
/// here: after the sweep the list is empty, so the reset that follows
/// cannot write into a header word that has changed owner.
#[test]
fn a_swept_list_is_not_swept_again_at_reset() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let (mut heap, slot, block) = an_entity_block();

    let mut arena = ShadowArena::new();
    let rows = arena.alloc(64);
    assert!(!rows.is_null());
    assert!(unsafe { arena.note_touched(block) });
    unsafe { crate::memory::heap::set_block_shadow(block, rows) };

    arena.sweep_touched();
    assert!(unsafe { crate::memory::heap::block_shadow(block) }.is_null());

    // What a recommissioning would leave, and what the reset must not
    // touch: the block belongs to the next collection now.
    unsafe { crate::memory::heap::set_block_shadow(block, rows) };
    arena.reset();
    assert_eq!(
        unsafe { crate::memory::heap::block_shadow(block) },
        rows,
        "the reset walked an emptied list"
    );

    unsafe { crate::memory::heap::clear_block_shadow(block) };
    unsafe { heap.free(slot) };
    crate::memory::critical::drain_for_test();
}

/// A block enrolled and never stamped is swept all the same, which is
/// what makes enrolling before stamping the safe order: the call that
/// can fail runs first, and the sweep of an unstamped block stores null
/// over null.
#[test]
fn an_enrolled_block_that_was_never_stamped_is_swept_harmlessly() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let (mut heap, slot, block) = an_entity_block();

    let mut arena = ShadowArena::new();
    assert!(unsafe { arena.note_touched(block) });
    assert!(
        unsafe { crate::memory::heap::block_shadow(block) }.is_null(),
        "nothing stamped it"
    );

    arena.reset();
    assert!(unsafe { crate::memory::heap::block_shadow(block) }.is_null());

    unsafe { heap.free(slot) };
    crate::memory::critical::drain_for_test();
}

/// The reserve is spent to its last block and every one of them goes
/// back to it. A return path that gave back a fixed number rather than
/// the number drawn would pass every other test here and leave a
/// pressure collection's reserve short by whatever it drew beyond one.
#[test]
fn every_block_the_reserve_lent_comes_back_to_it() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    assert!(crate::memory::critical::replenish());
    let before = BlockPool::global().blocks_out();

    let mut arena = ShadowArena::new();
    FORCE_OOM.store(true, Ordering::Relaxed);
    assert!(
        BlockPool::global().get().is_null(),
        "the ordinary door is refusing"
    );

    // One block per allocation, the payload being what a block holds.
    for _ in 0..crate::memory::critical::CRITICAL_BLOCKS {
        assert!(!arena.alloc(BLOCK_PAYLOAD).is_null());
    }
    assert_eq!(
        crate::memory::critical::blocks_held(),
        0,
        "the reserve is spent to its last block"
    );
    assert!(
        arena.alloc(BLOCK_PAYLOAD).is_null(),
        "and the next growth has nowhere left to ask"
    );
    FORCE_OOM.store(false, Ordering::Relaxed);

    arena.reset();
    assert_eq!(
        crate::memory::critical::blocks_held(),
        crate::memory::critical::CRITICAL_BLOCKS,
        "every block it lent came back to it"
    );
    assert_eq!(BlockPool::global().blocks_out(), before);
    crate::memory::critical::drain_for_test();
}
