use super::*;
use crate::memory::buffer::set_pressure_mode;

/// A chunk is an entity's body, so whichever thread drops the last
/// reference frees it: a non-owner may write only the block's
/// posting stack, which is why the header is split by access rule
/// and the stack sits on a line of its own. An arena that dies hands
/// its blocks to the abandoned list rather than dropping them, and
/// an emptied block goes back to the pool.
mod who_may_touch_a_block {
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
}

/// An adopted block is allocated from and not only held: the
/// rotation resumes the dead thread's cursor where it stopped, later
/// rotations look at the block again rather than losing its tail to
/// the one request that triggered the adoption, and `critical` mode
/// reaches the free list it arrived with — memory nobody is going to
/// ask for by name again.
mod what_adoption_recovers {
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
}

/// One budget covers the whole owned chain rather than one per
/// block, because a per-block budget makes the search cost grow with
/// the number of blocks an arena has adopted. Under plenty a hole is
/// left alone and the bump serves instead, and every grant is at
/// least the request and at least the minimum chunk.
mod how_far_a_chunk_is_searched_for {
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
}

/// A payload that is still the last thing bumped grows by moving the
/// bump, which is the case an append loop is in on every iteration;
/// one with something allocated after it is copied and leaves a hole
/// behind. Past a block payload the chunk is an OS-direct run.
mod growing_a_payload {
    use super::*;

    /// A payload that is still the last thing bumped grows by moving the
    /// bump, and the bytes already written stay where they are.
    ///
    /// This is the case an append loop is in on every iteration, and it is
    /// worth a test of its own because the alternative — reallocate and
    /// copy — is correct, passes every other assertion in this file, and
    /// costs a copy of everything written so far on each step.
    #[test]
    fn a_payload_at_the_bump_top_grows_without_moving() {
        let _g = crate::memory::block_pool::test_guard();
        let mut b = Buffer::new();

        buffer_ensure_longlived(&mut b, 64, 0);
        unsafe { std::ptr::copy_nonoverlapping(b"payload".as_ptr(), b.data, 7) };
        b.len = 7;
        let first = b.data;

        for step in 0..4 {
            let before = b.capacity;
            buffer_ensure_longlived(&mut b, before + 1, 0);
            assert_eq!(b.data, first, "step {step}: nothing was allocated after it");
            assert!(b.capacity > before, "step {step}: no room was gained");
        }

        assert_eq!(
            unsafe { std::slice::from_raw_parts(b.data, 7) },
            b"payload",
            "extending in place does not touch what was written"
        );

        unsafe { buffer_release_longlived(&mut b) };
    }

    /// A payload with something allocated after it is not at the bump top,
    /// so growth has to move it — allocate, copy, free the old chunk — and
    /// the chunk it leaves behind is a hole that `critical` mode reuses.
    ///
    /// The spacer is what puts the payload off the top. Without it this
    /// grows in place and there is no old chunk to recycle, which is the
    /// case the test below covers instead.
    #[test]
    fn a_payload_off_the_bump_top_moves_and_leaves_a_reusable_hole() {
        let _g = crate::memory::block_pool::test_guard();
        let mut b = Buffer::new();

        buffer_ensure_longlived(&mut b, 64, 0);
        unsafe { std::ptr::copy_nonoverlapping(b"payload".as_ptr(), b.data, 7) };
        b.len = 7;
        let old = b.data;
        let old_capacity = b.capacity;

        let (spacer, spacer_size) = with_buffer_arena(|a| a.alloc(64));
        assert!(!spacer.is_null());

        set_pressure_mode(PressureMode::Critical);
        let grow_to = b.capacity + 1;
        buffer_ensure_longlived(&mut b, grow_to, 0);
        assert_ne!(b.data, old, "a payload off the bump top has to move");
        assert_eq!(unsafe { std::slice::from_raw_parts(b.data, 7) }, b"payload");

        // The old chunk is a hole now: a fitting alloc must find it.
        let (p, _) = with_buffer_arena(|a| a.alloc(old_capacity));
        assert_eq!(p, old, "old payload must be reusable in critical mode");
        set_pressure_mode(PressureMode::Plenty);

        unsafe { buffer_release_longlived(&mut b) };
        with_buffer_arena(|a| unsafe {
            a.free(p, old_capacity);
            a.free(spacer, spacer_size);
        });
    }

    #[test]
    fn over_block_payload_goes_os_direct_and_back() {
        let _g = crate::memory::block_pool::test_guard();
        let mut b = Buffer::new();

        buffer_ensure_longlived(&mut b, BLOCK_PAYLOAD * 2, 0);
        assert!(b.capacity >= BLOCK_PAYLOAD * 2);
        unsafe { std::ptr::write_bytes(b.data, 0xCD, b.capacity) };

        // Shrink-to-arena is not a thing; release routes by kind.
        unsafe { buffer_release_longlived(&mut b) };
        assert!(b.data.is_null());
        assert_eq!(b.capacity, 0);
    }
}
