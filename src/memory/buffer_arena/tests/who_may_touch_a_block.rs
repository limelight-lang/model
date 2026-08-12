//! A chunk is an entity's body, so whichever thread drops the last
//! reference frees it: a non-owner may write only the block's
//! posting stack, which is why the header is split by access rule
//! and the stack sits on a line of its own. An arena that dies hands
//! its blocks to the abandoned list rather than dropping them, and
//! an emptied block goes back to the pool.

use super::*;

/// The split is a contract, not a comment: `kind` at offset 0 because
/// the pool's `BlockHeader` shares it, and `remote_free` on its own
/// cache line because the owner writes `live` and `free` on every
/// local free while other threads write the stack. `heap.rs` pins the
/// same two facts for the same reasons; a field added to the private
/// half would otherwise push the stack back onto the owner's line
/// with nothing to notice.
#[test]
fn the_header_is_split_by_access_rule() {
    assert_eq!(std::mem::offset_of!(BufferBlockHeader, kind), 0);
    // The private half starts on the next 8-aligned word, so `kind`
    // costs four bytes of padding — the price of it being outside a
    // borrow the owner takes on every free.
    assert_eq!(std::mem::offset_of!(BufferBlockHeader, private), 8);
    let remote = std::mem::offset_of!(BufferBlockHeader, remote);
    let shared = std::mem::offset_of!(BufferBlockHeader, shared);
    assert_eq!(remote % 64, 0, "the contended half starts a cache line");
    assert!(
        remote / 64 > shared / 64,
        "and shares no line with the owner's fields"
    );
    assert!(
        size_of::<BufferBlockHeader>() <= LINE_SIZE,
        "the whole header fits the block's header line"
    );
}

/// A chunk here is an entity's body, so whichever thread drops the
/// last reference is the one that frees it. A free from a non-owner
/// may touch only the block's posting stack: writing `live` and
/// `free` from there raced the owner, and an emptied block returned
/// from the wrong thread went to the pool while the owner was still
/// bumping into it.
///
/// Two arenas on one thread rather than two threads, because the
/// ownership test is arena identity and a second thread would only
/// add scheduling to the same code path.
#[test]
fn a_foreign_free_leaves_the_owners_block_alone() {
    let _g = crate::memory::block_pool::test_guard();
    let mut owner = BufferArena::new();
    let mut other = BufferArena::new();

    // A chunk, then a rotation past its block: the current block is
    // kept whatever happens, so the case worth testing is the other.
    let (chunk, size) = owner.alloc(32);
    let block = BufferBlockHeader::of_ptr(chunk);
    let (big, big_size) = owner.alloc(BLOCK_PAYLOAD);
    assert_ne!(owner.current, block, "rotated past it");

    unsafe { other.free(chunk, size) };
    unsafe {
        assert_eq!(
            (*block).kind.load(Ordering::Relaxed),
            BLOCK_KIND_BUFFER,
            "a foreign free sent the owner's block home"
        );
        assert_eq!(
            (*block).private.live,
            1,
            "live is the owner's count, and a posted chunk still counts"
        );
        assert!(
            !(*block)
                .remote
                .remote_free
                .load(Ordering::Relaxed)
                .is_null(),
            "the chunk belongs on the block's posting stack"
        );
    }

    // The owner accounts for it when it collects, and only then is
    // the block empty enough to go home.
    owner.collect_owned();
    unsafe {
        assert_eq!(
            (*block).kind.load(Ordering::Relaxed),
            0,
            "collected and returned to the pool"
        );
        owner.free(big, big_size);
    }
}

/// An arena that dies still holding chunks hands its blocks over
/// instead of dropping them: the memory comes back, and the frees
/// other threads are still posting into those blocks get a collector
/// again when someone adopts them.
#[test]
fn a_block_outlives_the_arena_that_owned_it() {
    let _g = crate::memory::block_pool::test_guard();

    let (chunk, size) = {
        let mut dying = BufferArena::new();
        dying.alloc(48)
    };

    let block = BufferBlockHeader::of_ptr(chunk);
    unsafe {
        assert_eq!(
            (*block).kind.load(Ordering::Relaxed),
            BLOCK_KIND_BUFFER,
            "the block was dropped on the floor with a live chunk in it"
        );
        assert!(
            (*block).shared.owner.load(Ordering::Relaxed).is_null(),
            "an abandoned block has no owner until one adopts it"
        );
    }

    // Someone else frees the chunk — no owner, so it posts — and
    // then adopts the block, which collects the post and finds it
    // empty. Adoption is one block per call and the list is global,
    // so blocks another test abandoned may come first.
    let mut next = BufferArena::new();
    unsafe { next.free(chunk, size) };
    for _ in 0..16 {
        if unsafe { (*block).kind.load(Ordering::Relaxed) } == 0 {
            break;
        }

        if ABANDONED.lock().unwrap().head.is_null() {
            break;
        }

        // Zero, because what is being tested is the collect-and-retire
        // half of adoption, not whether the tail fits a request.
        next.adopt(0);
    }

    unsafe {
        assert_eq!(
            (*block).kind.load(Ordering::Relaxed),
            0,
            "adopted, collected, and home"
        )
    };
}

/// Follows the block itself rather than the process-global
/// `blocks_out`. That counter is shared with every other test, so a
/// block returning late from elsewhere shifts it under this one's
/// feet — which made this test fail spuriously under
/// `--test-threads=16`. A block's `kind`, and who gets it next, are
/// facts about *this* block and nobody else can move them.
#[test]
fn drop_returns_the_empty_current_block() {
    let _g = crate::memory::block_pool::test_guard();

    let block;
    {
        let mut a = BufferArena::new();
        let (p, g) = a.alloc(128); // takes the current block
        block = BlockHeader::of_ptr(p);
        assert_eq!(
            unsafe { (*block).kind.load(Ordering::Relaxed) },
            BLOCK_KIND_BUFFER
        );

        unsafe { a.free(p, g) }; // live → 0, but it is still current
        assert_eq!(
            unsafe { (*block).kind.load(Ordering::Relaxed) },
            BLOCK_KIND_BUFFER,
            "free must not return the block the arena is still bumping into"
        );
    } // drop

    assert_eq!(
        unsafe { (*block).kind.load(Ordering::Relaxed) },
        crate::memory::block_pool::BLOCK_KIND_FREE,
        "Drop returned the empty current block instead of leaking it"
    );

    // And it is genuinely back in the pool, not merely restamped: the
    // next taker on this thread gets that same block.
    let mut second = BufferArena::new();
    let (p2, g2) = second.alloc(8);
    assert_eq!(
        BlockHeader::of_ptr(p2),
        block,
        "the returned block went home to the pool"
    );

    // Freed, or `second` dies holding it and the block joins the
    // global abandoned list, where every later test's rotation adopts
    // it — with a live chunk this suite can never account for.
    unsafe { second.free(p2, g2) };
}

#[test]
fn emptied_noncurrent_block_returns_to_pool() {
    let _g = crate::memory::block_pool::test_guard();
    let pool = BlockPool::global();
    let mut a = BufferArena::new();

    // Fill one block completely so the arena rotates past it.
    let payload = BLOCK_PAYLOAD / 4;
    let chunks: Vec<_> = (0..5).map(|_| a.alloc(payload)).collect();
    let first_block = BufferBlockHeader::of_ptr(chunks[0].0);
    assert_ne!(
        BufferBlockHeader::of_ptr(chunks[4].0),
        first_block,
        "fifth chunk must be in a fresh block"
    );

    let regions_before = pool.regions_carved();
    for &(p, g) in &chunks[..4] {
        unsafe { a.free(p, g) };
    }

    // The emptied first block is back in the pool: take it again.
    let reused = pool.get();
    let mut seen = vec![reused];
    let mut found = std::ptr::eq(reused as *mut BufferBlockHeader, first_block);
    for _ in 0..64 {
        if found {
            break;
        }

        let b = pool.get();
        found = std::ptr::eq(b as *mut BufferBlockHeader, first_block);
        seen.push(b);
    }

    assert!(found, "emptied buffer block was not returned to the pool");
    assert_eq!(pool.regions_carved(), regions_before);
    for b in seen {
        pool.put(b);
    }

    unsafe { a.free(chunks[4].0, chunks[4].1) };
}
