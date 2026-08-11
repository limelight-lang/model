use super::*;
use crate::memory::block_pool::BLOCK_PAYLOAD;
use std::sync::atomic::Ordering;

/// A request picks a size class and comes back at that class's
/// stride, so it is at least as large as asked, and it returns to the
/// free list of the block it came from. A size past the largest class
/// is refused with null, and bulk-reserved cells are accounted live
/// until they are handed back.
mod the_allocation_itself {
    use super::*;

    #[test]
    fn size_class_selection() {
        assert_eq!(size_class_index(1), Some(0));
        assert_eq!(size_class_index(16), Some(0));
        assert_eq!(size_class_index(17), Some(1));
        assert_eq!(size_class_index(8192), Some(SIZE_CLASSES.len() - 1));
        assert_eq!(size_class_index(8193), None);
    }

    /// Sixteen is the alignment every caller is entitled to: a `Value`
    /// is sixteen bytes and an entity header sits at offset 0 of a slot,
    /// so anything less would misalign the atomics the collector reads.
    /// Anything stricter leaves the heap for the pooled path, which is
    /// `stdapi`'s test.
    #[test]
    fn alloc_is_aligned_and_sized() {
        let _g = crate::memory::block_pool::test_guard();
        let mut heap = Heap::new();
        let a = heap.alloc(40);
        let b = heap.alloc(40);
        assert!(!a.is_null());
        assert_eq!((b as usize).wrapping_sub(a as usize), 48);

        // Every class, and two slots of each: the first slot's alignment
        // comes from the block header's size and every later one from
        // the class's stride, so a class that is not a multiple of
        // sixteen misaligns the second slot and nothing else.
        for &size in SIZE_CLASSES.iter() {
            let first = heap.alloc(size);
            let second = heap.alloc(size);
            assert!(!first.is_null() && !second.is_null());
            for p in [first, second] {
                assert_eq!(
                    p as usize % 16,
                    0,
                    "a slot of class {size} came back misaligned"
                );
            }

            unsafe {
                heap.free(first);
                heap.free(second);
            }
        }

        unsafe {
            heap.free(a);
            heap.free(b);
        }
    }

    #[test]
    fn free_then_alloc_reuses_slot() {
        let _g = crate::memory::block_pool::test_guard();
        let mut heap = Heap::new();
        let a = heap.alloc(64);
        unsafe { heap.free(a) };
        let b = heap.alloc(64);
        assert_eq!(a, b, "a freed slot must be handed back");
        unsafe { heap.free(b) };
    }

    #[test]
    fn too_large_returns_null() {
        let mut heap = Heap::new();
        assert!(heap.alloc(9000).is_null());
    }

    /// Cell reservation (`rfc/model/memory/bulk-operations.md`): the
    /// manager answers with 0..=count cells, reports the leading
    /// adjacent run honestly, accounts reserved cells as live, and
    /// takes returned cells back into ordinary circulation.
    #[test]
    fn reserved_cells_are_accounted_returned_cells_recirculate() {
        let _g = crate::memory::block_pool::test_guard();
        let mut cells = [std::ptr::null_mut::<u8>(); 8];
        let mut contiguous = 0usize;
        let n = unsafe { ll_entity_reserve(48, 8, cells.as_mut_ptr(), &mut contiguous) };
        assert!(n >= 1 && n <= 8, "an answer between 0 and count; got {n}");
        assert!(contiguous <= n);
        // The reported run is honest: adjacent at a constant class
        // stride. Two things this used to get wrong, both invisible until
        // pool pressure made them real. It read `cells[1]` after asserting
        // only `n >= 1`, so a reserve that answered 1 subtracted from
        // null. And it took the stride unsigned, while the free list is
        // LIFO and hands cells back in descending address order, so the
        // honest stride is negative about as often as not. The run's
        // length is `contiguous`, not `n`.
        if contiguous >= 2 {
            let stride = cells[1] as isize - cells[0] as isize;
            for i in 1..contiguous {
                assert_eq!(
                    cells[i] as isize - cells[i - 1] as isize,
                    stride,
                    "cell {i} breaks the reported run"
                );
            }
        }

        // Ordinary allocation must not hand out a reserved cell.
        let p = unsafe { entity_alloc(48) };
        assert!(
            !cells[..n].contains(&p),
            "a reserved cell was double-issued"
        );
        unsafe { crate::memory::stdapi::ll_free(p) };
        // Returned cells recirculate: the free-list is LIFO, so the
        // next allocation is the last cell returned.
        unsafe { ll_entity_cells_return(cells.as_ptr(), n) };
        let reused = unsafe { entity_alloc(48) };
        assert!(
            cells[..n].contains(&reused),
            "a returned cell did not recirculate"
        );
        unsafe { crate::memory::stdapi::ll_free(reused) };
    }
}

/// The header's halves are split by access rule, `kind` at offset 0
/// because the pool's own header shares that word. A block is kept
/// rather than returned and re-carved — with one live slot left in
/// it, which the larson run measured at ~140 ns/op against ~8, and
/// emptied of everything too, where it becomes the class's one
/// bounded empty spare. A full block is replaced by a refill.
mod the_block_under_the_slots {
    use super::*;

    #[test]
    fn block_header_halves_are_laid_out_as_the_design_requires() {
        use std::mem::offset_of;

        // `kind` is the pool's tagged-union discriminant: the whole
        // overlay depends on it staying at offset 0 of the block.
        assert_eq!(offset_of!(HeapBlockHeader, kind), 0);
        assert_eq!(offset_of!(HeapBlockHeader, size_class), 4);
        assert_eq!(offset_of!(HeapBlockHeader, private), 8);

        // `owner` is read by every `free` to decide whether the slot is
        // local, so it must stay in the same line as the hot private
        // fields. Giving it a line of its own cost +10.8% on
        // `rptest_10k_blocks_40_iters` (measured), because each local
        // free then touched a second line just for the check.
        // Everything the fast paths touch — the counters, the free list,
        // the `available` links, and `owner` — must fit line 0 together.
        // Each field evicted from it costs a miss on a path that runs per
        // allocation or per full ↔ has-room transition; both evictions
        // that were tried measured slower (see `BlockShared`).
        let shared = offset_of!(HeapBlockHeader, shared);
        assert_eq!(
            (shared + size_of::<BlockShared>()).div_ceil(64),
            1,
            "the hot set (private + owner) must fit one cache line, got {} bytes",
            shared + size_of::<BlockShared>()
        );

        // The contended field, by contrast, must be alone on its line, or
        // a cross-thread push steals the line holding `used`/`free`/`bump`
        // (audit `heap.rs:212`).
        let remote = offset_of!(HeapBlockHeader, remote);
        assert_eq!(remote % 64, 0, "remote_free must begin a cache line");
        assert!(remote >= 64, "remote_free must leave the hot line");
        assert_eq!(
            offset_of!(HeapBlockHeader, links) / 64 >= 1,
            true,
            "cold links must not crowd the hot line"
        );

        // The header lives in the block's reserved first line; growing it
        // past that would eat payload and change slots-per-block.
        assert!(
            size_of::<HeapBlockHeader>() <= LINE_SIZE,
            "header must fit the reserved line: {} > {LINE_SIZE}",
            size_of::<HeapBlockHeader>()
        );
    }

    /// Regression test for the pathology found via a real `larson.cpp`
    /// benchmark run: alloc-then-immediately-free of one object,
    /// repeated, must reuse the retained block instead of returning it
    /// to `BlockPool` and re-carving on every single cycle (which cost
    /// ~140 ns/op instead of ~8 ns/op). See
    /// `rfc/model/memory/heap-slot-allocation.md`.
    #[test]
    fn single_live_slot_churn_does_not_recarve_block() {
        let _g = crate::memory::block_pool::test_guard();
        let pool = BlockPool::global();
        let mut heap = Heap::new();

        // Warm up: this alloc carves the one block we expect to be
        // retained for the rest of the test.
        let warm = heap.alloc(64);
        unsafe { heap.free(warm) };
        let blocks_out_before = pool.blocks_out();
        let regions_before = pool.regions_carved();

        for i in 0..10_000u32 {
            let p = heap.alloc(64);
            assert!(!p.is_null());
            unsafe { p.write(i as u8) };
            unsafe { heap.free(p) };
            assert_eq!(
                pool.blocks_out(),
                blocks_out_before,
                "block was returned to the pool and re-carved on iteration {i}"
            );
        }

        assert_eq!(pool.regions_carved(), regions_before);
    }

    /// **Two blocks, because one proves nothing.** `Heap::retire_empty`
    /// keeps the first emptied block of a class as that class's one
    /// bounded empty spare and returns only the next one, so a test that
    /// fills and empties a single block watches a block that never
    /// leaves the thread. The second block is followed by address, the
    /// way the buffer arena's own test follows one: the process-global
    /// `blocks_out` moves under this test for reasons it is not about.
    #[test]
    fn empty_block_returns_to_pool() {
        let _g = crate::memory::block_pool::test_guard();
        let pool = BlockPool::global();
        let mut heap = Heap::new();

        let class = 64usize;
        let slots = BLOCK_PAYLOAD / class;
        let ptrs: Vec<_> = (0..2 * slots).map(|_| heap.alloc(class)).collect();
        let spare = ptrs[0] as usize & !BLOCK_MASK;
        let returned = ptrs[2 * slots - 1] as usize & !BLOCK_MASK;
        assert_ne!(spare, returned, "the fill has to span two blocks");

        let regions_before = pool.regions_carved();
        for p in &ptrs {
            unsafe { heap.free(*p) };
        }

        let mut drawn = Vec::new();
        let mut found = false;
        for _ in 0..16 {
            let b = pool.get();
            assert!(!b.is_null());
            drawn.push(b);
            if b as usize == returned {
                found = true;
                break;
            }
        }

        for b in drawn {
            pool.put(b);
        }

        assert!(found, "the second emptied block never reached the pool");

        // And the first is still here, so the next allocation is served
        // without carving.
        let p = heap.alloc(class);
        assert_eq!(p as usize & !BLOCK_MASK, spare, "the spare serves it");
        assert_eq!(pool.regions_carved(), regions_before);
        unsafe { heap.free(p) };
    }

    #[test]
    fn full_block_refills_and_serves_distinct_slots() {
        let _g = crate::memory::block_pool::test_guard();
        let mut heap = Heap::new();
        let class = 128usize;
        let slots = BLOCK_PAYLOAD / class;

        let ptrs: Vec<_> = (0..slots + 10).map(|_| heap.alloc(class)).collect();
        assert!(ptrs.iter().all(|p| !p.is_null()));

        let mut sorted = ptrs.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ptrs.len(), "no slot handed out twice");

        for p in ptrs {
            unsafe { heap.free(p) };
        }
    }
}

/// An entity block's slots are read as headers by a walker that owns
/// none of them, so the free-list link is written at bytes 8-15 and
/// commissioning zeroes every slot's first word, whatever the block
/// held before. The kind goes through one published store the
/// collector's region scan may read concurrently, and a raw heap
/// never adopts an entity block, or C buffers would land where the
/// walker reads headers.
mod what_a_walker_reads_between_the_slots {
    use super::*;

    /// The free-list link lives in slot bytes 8–15, so a free leaves the
    /// first 8 bytes exactly as the dying occupant left them — in an
    /// entity block that is the final refcount-0 header, the walker's
    /// occupancy test (`rfc/model/gc/rc-walk.md`). Fails with the link at
    /// bytes 0–7, where the push clobbered them.
    #[test]
    fn free_leaves_the_slots_first_8_bytes_untouched() {
        let _g = crate::memory::block_pool::test_guard();
        let mut heap = Heap::new();
        let p = heap.alloc(64);
        unsafe { std::ptr::write_bytes(p, 0xAA, 64) };
        unsafe { heap.free(p) };
        for i in 0..8 {
            assert_eq!(
                unsafe { *p.add(i) },
                0xAA,
                "byte {i} of a freed slot was clobbered by the free-list link"
            );
        }

        let again = heap.alloc(64);
        assert_eq!(again, p, "the slot still threads the free list");
        unsafe { heap.free(again) };
    }

    /// Commissioning an entity block zeroes every slot's first 8 bytes,
    /// whatever the block held before — the rc-walk rule that closes the
    /// carve-to-first-store window. The raw population deliberately skips
    /// the pass, which is also the control proving the scribble survives
    /// the pool round-trip (i.e. that this test can fail).
    #[test]
    fn entity_commissioning_zeroes_slot_headers_of_a_recycled_block() {
        let _g = crate::memory::block_pool::test_guard();
        let pool = BlockPool::global();
        let class = 3072usize; // uncommon class: no adoptable leftovers
        let slots = BLOCK_PAYLOAD / class;

        let scribble = |block: *mut BlockHeader| unsafe {
            std::ptr::write_bytes(BlockHeader::payload_start(block), 0xEE, BLOCK_PAYLOAD);
        };

        let slot_header = |p: *mut u8, s: usize| unsafe {
            ((p as usize & !BLOCK_MASK) as *mut u8)
                .add(LINE_SIZE + s * class)
                .cast::<u64>()
                .read()
        };

        // Control: the raw population keeps the garbage (thread-cache
        // LIFO hands the scribbled block straight back to refill).
        let block = pool.get();
        scribble(block);
        pool.put(block);
        let mut raw = Heap::new();
        let p = raw.alloc(class);
        assert_eq!(p as usize & !BLOCK_MASK, block as usize, "LIFO reuse");
        assert_eq!(
            slot_header(p, 1),
            0xEEEE_EEEE_EEEE_EEEE,
            "control: without the pass the scribble survives commissioning"
        );
        unsafe { raw.free(p) };
        drop(raw);

        // The entity population must not: every slot header reads 0.
        let block = pool.get();
        scribble(block);
        pool.put(block);
        let mut entity = Heap::new_entity();
        let p = entity.alloc(class);
        assert_eq!(p as usize & !BLOCK_MASK, block as usize, "LIFO reuse");
        for s in 1..slots {
            assert_eq!(
                slot_header(p, s),
                0,
                "slot {s} header must be zeroed at entity commissioning"
            );
        }

        unsafe { entity.free(p) };
    }

    /// Commissioning an entity block writes its header while the
    /// collector reads the kind of every block in every carved region, so
    /// that word is published through `store_block_kind` and is touched
    /// by nothing else — including the whole-header struct store that
    /// used to write it on the way past, with the value that was already
    /// there.
    ///
    /// **Miri's data-race model is the instrument this test is for.**
    /// Under `cargo test` the two shapes are indistinguishable, which is
    /// what let the plain store stand until S11.6 read it. Neither thread
    /// waits on the other: the accesses are unordered whatever the
    /// interleaving, which is the whole of the report.
    #[cfg(feature = "rc-walk")]
    #[test]
    fn commissioning_an_entity_block_does_not_race_the_snapshot() {
        let _g = crate::memory::block_pool::test_guard();

        // The collector's first act, and the only part of an epoch that
        // touches a block being commissioned.
        let reader = std::thread::spawn(|| {
            for _ in 0..64 {
                let _ = snapshot_entity_blocks();
            }
        });

        // The largest size class puts seven slots in a block, so a short
        // run of allocations commissions several.
        let mut slots = Vec::new();
        for _ in 0..24 {
            let slot = unsafe { entity_alloc(MAX_SMALL) };
            assert!(!slot.is_null(), "the pool refused mid-test");
            slots.push(slot);
        }

        reader.join().unwrap();

        // Given back: a leaked slot holds its block off the pool for the
        // rest of the binary. The headers still read the zero
        // commissioning left, which is what the free door asks of a slot.
        for slot in slots {
            unsafe { crate::memory::stdapi::ll_free(slot) };
        }
    }

    /// A block never crosses populations through abandonment: a raw heap
    /// must not adopt an entity block, or C buffers land where the walker
    /// reads headers.
    #[test]
    fn adoption_never_crosses_block_populations() {
        let _g = crate::memory::block_pool::test_guard();
        let class = 3072usize; // uncommon class: adoption is deterministic

        let mut donor = Heap::new_entity();
        let held = donor.alloc(class);
        assert!(!held.is_null());
        let entity_block = held as usize & !BLOCK_MASK;
        donor.abandon_all(); // still holds `held`: goes to the abandoned list

        // A raw heap of the same class must refill fresh, not adopt it.
        let mut raw = Heap::new();
        let p = raw.alloc(class);
        assert_ne!(
            p as usize & !BLOCK_MASK,
            entity_block,
            "a raw heap must never serve from an entity block"
        );
        unsafe {
            assert_eq!(
                (*HeapBlockHeader::of_ptr(p)).kind.load(Ordering::Relaxed),
                BLOCK_KIND_HEAP
            );
        }

        unsafe { raw.free(p) };

        // An entity heap is the legitimate adopter; freeing both slots
        // sends the block home instead of leaving it stranded abandoned.
        let mut adopter = Heap::new_entity();
        let q = adopter.alloc(class);
        assert_eq!(
            q as usize & !BLOCK_MASK,
            entity_block,
            "the entity heap adopts the abandoned entity block"
        );
        unsafe {
            adopter.free(q);
            adopter.free(held);
        }
    }

    /// End to end over the C ABI: the factory's population is disjoint
    /// from `ll_malloc`'s, and `ll_free` routes each kind to its heap.
    #[test]
    fn entity_and_raw_allocations_are_segregated_end_to_end() {
        let _g = crate::memory::block_pool::test_guard();
        let e = unsafe { entity_alloc(40) };
        let r = unsafe { crate::memory::stdapi::ll_malloc(40) };
        assert!(!e.is_null() && !r.is_null());
        unsafe {
            assert_eq!(
                (*HeapBlockHeader::of_ptr(e)).kind.load(Ordering::Relaxed),
                BLOCK_KIND_ENTITY
            );
            assert_eq!(
                (*HeapBlockHeader::of_ptr(r)).kind.load(Ordering::Relaxed),
                BLOCK_KIND_HEAP
            );
        }

        assert_ne!(
            e as usize & !BLOCK_MASK,
            r as usize & !BLOCK_MASK,
            "the two populations never share a block"
        );
        // Both go home through the one size-less free.
        unsafe {
            crate::memory::stdapi::ll_free(e);
            crate::memory::stdapi::ll_free(r);
        }
    }
}

/// A thread that ends without calling `ll_thread_exit` still gives
/// its blocks back through the TLS guard, and a heap that falls out
/// of scope gives them back through `Drop`; before either existed
/// the blocks were stranded, nothing else knowing about them. A
/// process with no TLS slot left reports that as a null allocation
/// rather than ending.
mod blocks_going_home_with_nobody_asking {
    use super::*;

    /// A thread that allocates and then exits **without** calling
    /// `ll_thread_exit` must still give its blocks back: the TLS guard is
    /// what makes that automatic, and it is the whole reason the guard
    /// exists.
    ///
    /// Regression for audit H9. The guard used to be `#[cfg(windows)]`, so
    /// on ELF targets nothing reclaimed anything — every worker thread
    /// stranded its blocks forever. This test passes natively on Windows
    /// either way; the one that matters is the Miri run, which executes
    /// the non-Windows path (see `dev/WORKFLOW.md`):
    ///
    /// ```text
    /// MIRIFLAGS="-Zmiri-ignore-leaks" cargo +nightly miri test \
    ///     --target x86_64-unknown-linux-gnu --lib h9_
    /// ```
    #[test]
    fn h9_exiting_thread_returns_its_blocks_without_an_explicit_call() {
        // The ring a journaling thread takes is a block the registry keeps
        // after that thread is gone, and this test counts the blocks the
        // pool has out. Before the pool's guard, as `set_sites_for_test`
        // requires.
        let _quiet = crate::journal::kinds::disable_sites_for_test();
        let _g = crate::memory::block_pool::test_guard();
        let pool = BlockPool::global();
        let before = pool.blocks_out();

        for _ in 0..3 {
            std::thread::spawn(|| {
                ll_thread_init();
                let p = unsafe { crate::memory::stdapi::ll_alloc(40, 16) };
                assert!(!p.is_null());
                unsafe { crate::memory::stdapi::ll_free(p) };
                // Deliberately no `ll_thread_exit()`: the guard must do it.
            })
            .join()
            .unwrap();
        }

        assert_eq!(
            pool.blocks_out(),
            before,
            "an exiting thread must not strand its blocks"
        );
    }

    /// A `Heap` that dies by falling out of scope must give its blocks
    /// back, exactly as `ll_thread_exit` does. Before `Drop` existed they
    /// were stranded: nothing else knew about them, so the pool never saw
    /// them again. Revert `impl Drop for Heap` and this test fails on the
    /// final assert.
    #[test]
    fn a_dropped_heap_returns_its_blocks_to_the_pool() {
        let _g = crate::memory::block_pool::test_guard();
        let pool = BlockPool::global();
        let before = pool.blocks_out();

        {
            let mut heap = Heap::new();
            let p = heap.alloc(40);
            assert!(!p.is_null());
            assert!(
                pool.blocks_out() > before,
                "the heap took a block from the pool"
            );
            // Free it so the block is empty: an empty block goes home to
            // the pool, which is what makes the count observable here.
            unsafe { heap.free(p) };
        }

        assert_eq!(
            pool.blocks_out(),
            before,
            "a heap dropped out of scope must not strand its blocks"
        );
    }

    /// A process with no TLS slot left cannot give this thread a heap.
    /// That is reported the same way as any other exhaustion — the
    /// allocation returns null — and not by ending the process, which is
    /// what storing `TlsAlloc`'s failure value used to lead to: it equals
    /// our "uninitialised" sentinel, so the slot would have looked
    /// unreserved and every read would have gone to a bad TEB offset.
    #[cfg(windows)]
    #[test]
    fn a_thread_without_a_tls_slot_reports_instead_of_dying() {
        let _g = crate::memory::block_pool::test_guard();
        use std::sync::atomic::Ordering;

        // A fresh thread: this one already has its heap installed.
        let (told, heapless, refused) = std::thread::spawn(|| {
            tls::FORCE_TLS_FAILURE.store(1, Ordering::Relaxed);
            // The refusal has to be *said*, not swallowed: a silent miss
            // leaves the caller believing the pointer was stored.
            let told = !tls::set(std::ptr::null_mut());
            ll_thread_init();
            let heapless = thread_heap().is_null();
            let p = unsafe { crate::memory::stdapi::ll_alloc(40, 16) };
            tls::FORCE_TLS_FAILURE.store(0, Ordering::Relaxed);
            (told, heapless, p.is_null())
        })
        .join()
        .unwrap();

        assert!(
            told,
            "installing into a slot that does not exist must report"
        );
        assert!(heapless, "so the thread stays without a heap");
        assert!(refused, "and the allocation reports null");
    }
}

/// A slot freed by a non-owner is posted to the owning block's
/// stack, and that push is a CAS loop, so several producers at once
/// may lose none of it: after the freers finish, the owner drains
/// and must report zero live slots.
mod frees_arriving_from_another_thread {
    use super::*;

    #[test]
    fn cross_thread_free_is_correct() {
        let _g = crate::memory::block_pool::test_guard();
        use std::sync::mpsc;
        use std::thread;

        const N: u64 = 5000;
        let (tx, rx) = mpsc::channel::<usize>();

        // Producer: allocate on its own heap, stamp each with its index,
        // hand the pointer to the consumer, and keep allocating (so its
        // slow path drains the incoming cross-thread frees concurrently).
        let producer = thread::spawn(move || {
            ll_thread_init();
            unsafe {
                with_thread_heap(|h| {
                    for i in 0..N {
                        let p = h.alloc(24);
                        (p as *mut u64).write(i);
                        tx.send(p as usize).unwrap();
                        // extra churn to exercise the drain path
                        let t = h.alloc(24);
                        h.free(t);
                    }
                });
            }
        });

        // Consumer (this thread): verify each value survived, then free
        // cross-thread (posts to the producer's remote stack).
        ll_thread_init();
        let mut count = 0u64;
        for _ in 0..N {
            let p = rx.recv().unwrap() as *mut u8;
            let v = unsafe { *(p as *mut u64) };
            assert!(v < N, "value corrupted across threads");
            unsafe { with_thread_heap(|h| h.free(p)) };
            count += 1;
        }

        assert_eq!(count, N);
        producer.join().unwrap();
    }

    /// Several threads freeing into the **same** owner's blocks at once.
    ///
    /// The existing coverage missed this: `many_threads_alloc_free_no_corruption`
    /// has every thread allocate and free on its own heap, so no slot ever
    /// reaches `remote_free`, and `cross_thread_free_is_correct` has exactly
    /// one producer. The multi-producer push had no test at all.
    ///
    /// What would break if it were wrong: `free_remote` is a CAS loop, so a
    /// lost race would drop a slot from the chain, and the owner would
    /// account for fewer slots than were actually freed. That is measured
    /// directly — after every freer has finished, the owner drains its
    /// queues and must report **zero** live slots. Corruption of the slot
    /// contents before the free is caught by the stamp check in each freer.
    ///
    /// It deliberately does *not* assert on the process-global
    /// `blocks_out`. That counter is shared with every other test, so a
    /// block returning late from elsewhere moves it in either direction —
    /// which made this test flaky at ~2 runs in 10 under
    /// `--test-threads=16`, failing on someone else's straggler rather
    /// than on anything it was testing.
    #[test]
    fn many_threads_freeing_into_one_owner_lose_no_slots() {
        let _g = crate::memory::block_pool::test_guard();
        use std::sync::mpsc;
        use std::thread;

        const FREERS: usize = 4;
        const PER: usize = 500;
        const STAMP: u8 = 0xAB;

        let mut txs = Vec::with_capacity(FREERS);
        let mut freers = Vec::with_capacity(FREERS);
        for _ in 0..FREERS {
            let (tx, rx) = mpsc::channel::<usize>();
            txs.push(tx);
            freers.push(thread::spawn(move || {
                ll_thread_init();
                let mut n = 0usize;
                for p in rx {
                    let p = p as *mut u8;
                    assert_eq!(
                        unsafe { *p },
                        STAMP,
                        "slot corrupted before its cross-thread free"
                    );
                    unsafe { with_thread_heap(|h| h.free(p)) };
                    n += 1;
                }

                ll_thread_exit();
                n
            }));
        }

        // This thread owns the blocks. Hand slots out round-robin so all
        // four freers contend on the same block, and keep churning so the
        // drain path runs while their pushes are arriving.
        ll_thread_init();
        unsafe {
            with_thread_heap(|h| {
                for i in 0..(FREERS * PER) {
                    let p = h.alloc(24);
                    p.write(STAMP);
                    txs[i % FREERS].send(p as usize).unwrap();
                    let churn = h.alloc(24);
                    h.free(churn);
                }
            });
        }

        drop(txs);

        let freed: usize = freers.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(freed, FREERS * PER, "every slot was freed exactly once");

        // Every freer is done, so every push has landed. Drain the queues
        // and account: a slot lost in the CAS loop shows up here as a live
        // slot nobody holds.
        let live = unsafe { with_thread_heap(|h| h.live_slots_after_collect()) };
        assert_eq!(
            live, 0,
            "the owner lost track of a slot freed from another thread"
        );
    }

    #[test]
    fn many_threads_alloc_free_no_corruption() {
        let _g = crate::memory::block_pool::test_guard();
        use std::thread;

        let handles: Vec<_> = (0..8)
            .map(|t| {
                thread::spawn(move || {
                    ll_thread_init();
                    unsafe {
                        with_thread_heap(|h| {
                            let mut live = Vec::new();
                            for i in 0..2000usize {
                                let size = 16 + (i * 8 + t) % 512;
                                let p = h.alloc(size);
                                assert!(!p.is_null());
                                p.write((t as u8).wrapping_add(1));
                                live.push(p);
                                if live.len() > 100 {
                                    let victim = live.swap_remove(i % live.len());
                                    h.free(victim);
                                }
                            }

                            for p in live {
                                h.free(p);
                            }
                        });
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
    }
}
