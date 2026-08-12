//! Allocation is a bump rounded to eight bytes: a full block is
//! replaced rather than reused, `reserve` keeps a loop inside the
//! block it started in, and only the last allocation bumped can grow
//! in place.

use super::*;

#[test]
fn allocations_are_sequential_and_rounded() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();

    let a = arena.alloc(40);
    let b = arena.alloc(1);
    let c = arena.alloc(16);

    assert_eq!(b as usize - a as usize, 40, "40 stays 40");
    assert_eq!(c as usize - b as usize, 8, "1 rounds up to 8");

    // First allocation begins right after the block header.
    assert_eq!(a as usize & BLOCK_MASK, LINE_SIZE);
}

#[test]
fn slow_path_takes_new_block_exactly_at_exhaustion() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();

    // Derived from BLOCK_PAYLOAD, not hardcoded: the count is whatever
    // exactly fills one block, and spelling it as a literal silently
    // pinned this test to a 32 KB block. Changing BLOCK_SIZE then failed
    // here with "block must be exactly full", which reads like an arena
    // bug rather than a stale constant.
    let slots = BLOCK_PAYLOAD / 8;
    let first = arena.alloc(8);
    for _ in 0..slots - 1 {
        arena.alloc(8);
    }

    assert_eq!(arena.remaining(), 0, "block must be exactly full");

    let next = arena.alloc(8);
    assert_ne!(
        BlockHeader::of_ptr(next),
        BlockHeader::of_ptr(first),
        "must land in a fresh block"
    );
}

#[test]
fn reserve_prevents_mid_loop_refill() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    arena.reserve(100 * 40);

    let block = BlockHeader::of_ptr(arena.alloc(40));
    for _ in 0..99 {
        let p = arena.alloc(40);
        assert_eq!(BlockHeader::of_ptr(p), block, "reserve was violated");
    }
}

#[test]
fn extend_in_place_only_at_bump_top() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();

    let buf = arena.alloc(64);
    assert!(arena.try_extend_in_place(buf, 64, 128), "top must extend");

    let _other = arena.alloc(8); // someone allocates after us
    assert!(
        !arena.try_extend_in_place(buf, 128, 256),
        "no longer the top - must refuse"
    );
}
