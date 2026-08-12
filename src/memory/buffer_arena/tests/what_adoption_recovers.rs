//! An adopted block is allocated from and not only held: the
//! rotation resumes the dead thread's cursor where it stopped, later
//! rotations look at the block again rather than losing its tail to
//! the one request that triggered the adoption, and `critical` mode
//! reaches the free list it arrived with — memory nobody is going to
//! ask for by name again.

use super::*;

/// What adoption is worth is the tail the dead thread left, not only
/// the collector it gives the block back: the allocation that
/// triggered the rotation is served from the adopted block, and the
/// pool is not asked for a fresh one.
///
/// The lower bound on the served address is the load-bearing half —
/// it is what says the resumed cursor stopped where the dead thread
/// stopped, and did not hand out the live chunk it was abandoned for.
///
/// Reads the head of the global abandoned list directly, so a test
/// that leaves a block on it breaks this one. That is the intent:
/// nothing else in the suite notices a leaked buffer block.
#[test]
fn adoption_resumes_the_tail_when_it_fits_the_request() {
    let _g = crate::memory::block_pool::test_guard();

    let (chunk, size) = {
        let mut dying = BufferArena::new();
        dying.alloc(48)
    };

    let abandoned = BufferBlockHeader::of_ptr(chunk);

    let mut heir = BufferArena::new();
    assert!(heir.adopt(1024), "63 KiB of tail can serve 1 KiB");
    assert_eq!(
        heir.current, abandoned,
        "a tail that fits makes its block current"
    );

    let (served, _) = heir.alloc(1024);
    assert_eq!(BufferBlockHeader::of_ptr(served), abandoned);
    assert!(
        served >= chunk.wrapping_add(size),
        "the resumed cursor handed out the live chunk it was abandoned for"
    );

    unsafe {
        heir.free(served, 1024);
        heir.free(chunk, size);
    }
}

/// An adopted block is looked at again on later rotations, which is
/// what keeps its tail from being lost to the one request that
/// happened to trigger the adoption. Here that request is a whole
/// block payload, which no inherited tail can serve.
#[test]
fn an_adopted_tail_serves_the_request_after_the_one_that_adopted_it() {
    let _g = crate::memory::block_pool::test_guard();

    let (chunk, size) = {
        let mut dying = BufferArena::new();
        dying.alloc(48)
    };

    let abandoned = BufferBlockHeader::of_ptr(chunk);

    let mut heir = BufferArena::new();
    // Adopts the block, cannot use it, and exhausts a fresh one.
    let (filler, filler_size) = heir.alloc(BLOCK_PAYLOAD);
    assert_ne!(BufferBlockHeader::of_ptr(filler), abandoned);

    let (served, _) = heir.alloc(1024);
    assert_eq!(
        BufferBlockHeader::of_ptr(served),
        abandoned,
        "the second rotation took a fresh block and left the adopted tail unused"
    );
    assert!(
        served >= chunk.wrapping_add(size),
        "the resumed cursor handed out the live chunk it was abandoned for"
    );

    unsafe {
        heir.free(served, 1024);
        heir.free(chunk, size);
        heir.free(filler, filler_size);
    }
}

/// The free list an adopted block arrives with is memory nobody is
/// going to ask for again, so `critical` mode has to reach it — and
/// the block holding it is not the current one, which is the case the
/// bounded search used to miss by construction.
#[test]
fn critical_mode_reuses_a_hole_in_an_adopted_block() {
    let _g = crate::memory::block_pool::test_guard();

    let (keeper, keeper_size, hole, hole_size) = {
        let mut dying = BufferArena::new();
        let (keeper, keeper_size) = dying.alloc(64);
        let (hole, hole_size) = dying.alloc(256);
        unsafe { dying.free(hole, hole_size) };
        (keeper, keeper_size, hole, hole_size)
    };

    let mut heir = BufferArena::new();
    // A request no tail can serve, so the block is adopted without
    // becoming current: its inherited list is what is under test.
    assert!(!heir.adopt(BLOCK_PAYLOAD));
    assert!(
        heir.current.is_null(),
        "adopted, and not as the current block"
    );

    set_pressure_mode(PressureMode::Critical);
    let (served, granted) = heir.alloc(256);
    set_pressure_mode(PressureMode::Plenty);
    assert_eq!(
        served, hole,
        "an inherited hole must serve a fitting request"
    );
    assert_eq!(granted, hole_size, "the whole chunk is granted, no split");

    unsafe {
        heir.free(served, granted);
        heir.free(keeper, keeper_size);
    }
}
