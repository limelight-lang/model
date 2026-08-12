//! One budget covers the whole owned chain rather than one per
//! block, because a per-block budget makes the search cost grow with
//! the number of blocks an arena has adopted. Under plenty a hole is
//! left alone and the bump serves instead, and every grant is at
//! least the request and at least the minimum chunk.

use super::*;

/// The bound on the `critical` walk is one budget for the whole chain,
/// not one per block: a fitting hole behind a current block whose list
/// has already spent the budget is not reached, and the allocation
/// bumps instead.
///
/// Pinning a miss looks backwards until the alternative is written
/// out: a per-block budget makes the search cost grow with the number
/// of blocks the arena owns, and an arena that keeps adopting owns
/// more of them the longer it lives.
#[test]
fn the_critical_search_budget_covers_the_whole_chain() {
    let _g = crate::memory::block_pool::test_guard();
    let mut a = BufferArena::new();

    // Fill one block exactly, then rotate off it and free a chunk
    // there: a fitting hole in a block that is no longer current.
    let quarter = BLOCK_PAYLOAD / 4;
    let filling: Vec<_> = (0..4).map(|_| a.alloc(quarter)).collect();
    let (keeper, keeper_size) = a.alloc(64);
    let first = BufferBlockHeader::of_ptr(filling[0].0);
    assert_ne!(BufferBlockHeader::of_ptr(keeper), first, "rotated off it");
    unsafe { a.free(filling[0].0, filling[0].1) };

    // Exactly the budget's worth of misses on the current block.
    let smalls: Vec<_> = (0..CRITICAL_SEARCH_BOUND).map(|_| a.alloc(16)).collect();
    for &(p, g) in &smalls {
        unsafe { a.free(p, g) };
    }

    set_pressure_mode(PressureMode::Critical);
    let (served, granted) = a.alloc(quarter);
    set_pressure_mode(PressureMode::Plenty);
    assert_ne!(
        BufferBlockHeader::of_ptr(served),
        first,
        "the budget was spent on the current block and refilled for the next"
    );

    unsafe {
        a.free(served, granted);
        a.free(keeper, keeper_size);
        for &(p, g) in &filling[1..] {
            a.free(p, g);
        }
    }
}

#[test]
fn search_is_bounded() {
    let _g = crate::memory::block_pool::test_guard();
    let mut a = BufferArena::new();

    // Build a list of > BOUND small holes, then one big hole beyond
    // the bound; a big request must NOT find it.
    let (anchor, ag) = a.alloc(64); // keeps the block alive
    let big = a.alloc(1024);
    let smalls: Vec<_> = (0..CRITICAL_SEARCH_BOUND + 4)
        .map(|_| a.alloc(16))
        .collect();
    unsafe { a.free(big.0, big.1) }; // deepest in LIFO
    for (p, g) in smalls {
        unsafe { a.free(p, g) };
    }

    set_pressure_mode(PressureMode::Critical);
    let (p, _) = a.alloc(1024);
    assert_ne!(p, big.0, "hit beyond the K-bound must fall back to bump");
    set_pressure_mode(PressureMode::Plenty);

    unsafe {
        a.free(p, 1024);
        a.free(anchor, ag);
    }
}

#[test]
fn critical_mode_reuses_freed_chunk_plenty_does_not() {
    let _g = crate::memory::block_pool::test_guard();
    let mut a = BufferArena::new();

    let (p, g) = a.alloc(128);
    let (live, live_size) = a.alloc(64); // keeps the block non-empty
    unsafe { a.free(p, g) };

    set_pressure_mode(PressureMode::Plenty);
    let (q, _) = a.alloc(128);
    assert_ne!(q, p, "plenty must bump, not consult holes");

    unsafe { a.free(q, 128) };
    set_pressure_mode(PressureMode::Critical);
    let (r, granted) = a.alloc(100);
    assert_eq!(r, q, "critical must pop the fitting hole");
    assert_eq!(granted, 128, "the whole chunk is granted, no split");
    set_pressure_mode(PressureMode::Plenty);

    // Freed, or this arena dies holding chunks and its block goes to
    // the abandoned list, where the next test's rotation adopts it —
    // block identity is what several tests here assert.
    unsafe {
        a.free(r, granted);
        a.free(live, live_size);
    }
}

#[test]
fn alloc_grants_at_least_requested_and_min_chunk() {
    let _g = crate::memory::block_pool::test_guard();
    let mut a = BufferArena::new();
    let (p, granted) = a.alloc(1);
    assert!(!p.is_null());
    assert_eq!(
        granted, MIN_CHUNK,
        "tiny chunks round up to the free-slot size"
    );
    unsafe { a.free(p, granted) };
}
