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

    let mut arena = TraceScratchArena::new();
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

    let mut arena = TraceScratchArena::new();
    assert!(!arena.alloc(64).is_null(), "the ordinary door served");
    assert_eq!(arena.blocks_held(), 1);

    let oom = force_oom();
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
    drop(oom);

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

    let mut arena = TraceScratchArena::new();
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

    let mut arena = TraceScratchArena::new();
    let oom = force_oom();
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
    drop(oom);

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

    let mut arena = TraceScratchArena::new();
    met(unsafe { arena.ensure_row(slot_row(block, 0), 1) });
    assert!(!unsafe { crate::memory::heap::block_shadow(block) }.is_null());

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

/// The sweep walks the whole chain rather than its head. Three blocks,
/// because two prove less: the list is newest-first, so a sweep that
/// stopped after one entry would still null the last block touched, and
/// one that stopped after two would null the last two. The block touched
/// first is the one only a full walk reaches.
#[test]
fn the_sweep_reaches_every_block_of_the_chain() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let (mut first_heap, first_slot, first) = an_entity_block();
    let (mut middle_heap, middle_slot, middle) = an_entity_block();
    let (mut last_heap, last_slot, last) = an_entity_block();
    assert_ne!(first, middle);
    assert_ne!(middle, last);
    assert_ne!(first, last);

    let mut arena = TraceScratchArena::new();
    for block in [first, middle, last] {
        met(unsafe { arena.ensure_row(slot_row(block, 0), 1) });
    }

    assert_eq!(arena.touched_blocks(), 3, "one entry per touched block");

    arena.reset();
    for (block, position) in [
        (last, "the head of the chain"),
        (middle, "its middle"),
        (first, "its far end"),
    ] {
        assert!(
            unsafe { crate::memory::heap::block_shadow(block) }.is_null(),
            "{position} was walked"
        );
    }

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

    let mut arena = TraceScratchArena::new();
    let rows = met(unsafe { arena.ensure_row(slot_row(block, 0), 1) }) as *mut u8;

    arena.clear_touched_rows();
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

/// The refusal lands before the block is stamped, which is what makes
/// one allocation for the rows and the enrolment worth its shape: there
/// is no instant at which a block points at rows the abort is about to
/// give back. Both doors refuse, so the first touch answers
/// [`RowLookup::AllocationFailed`] and the block's shadow word is untouched.
#[test]
fn a_refused_first_touch_stamps_nothing() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let (mut heap, slot, block) = an_entity_block();

    let mut arena = TraceScratchArena::new();
    let oom = force_oom();
    assert!(
        BlockPool::global().get().is_null(),
        "the ordinary door is refusing"
    );
    assert_eq!(
        crate::memory::critical::blocks_held(),
        0,
        "and the critical door has nothing to serve"
    );

    assert_eq!(
        unsafe { arena.ensure_row(slot_row(block, 0), 1) },
        RowLookup::AllocationFailed,
        "a first touch with no memory aborts the collection"
    );
    drop(oom);

    assert!(
        unsafe { crate::memory::heap::block_shadow(block) }.is_null(),
        "and leaves the block unstamped"
    );
    assert_eq!(arena.touched_blocks(), 0, "and unenrolled");

    arena.reset();
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

    let mut arena = TraceScratchArena::new();
    let oom = force_oom();
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
    drop(oom);

    arena.reset();
    assert_eq!(
        crate::memory::critical::blocks_held(),
        crate::memory::critical::CRITICAL_BLOCKS,
        "every block it lent came back to it"
    );
    assert_eq!(BlockPool::global().blocks_out(), before);
    crate::memory::critical::drain_for_test();
}
