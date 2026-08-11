use super::*;
use crate::memory::block_pool::BLOCK_KIND_FREE;
use std::sync::atomic::Ordering;

/// Immortal memory is a bump over pooled blocks: addresses walk
/// forward and aligned, a full block is replaced rather than reused,
/// and nothing is ever given back.
mod the_bump_region {
    use super::*;

    #[test]
    fn allocations_are_sequential_and_aligned() {
        let _g = crate::memory::block_pool::test_guard();

        let a = immortal_alloc(24);
        let b = immortal_alloc(1);
        assert_eq!(a as usize % 8, 0);
        // Same block => bump distance; fresh block => still 8-aligned.
        if BlockHeader::of_ptr(a) == BlockHeader::of_ptr(b) {
            assert_eq!(b as usize - a as usize, 24);
        }

        assert_eq!(
            unsafe { (*BlockHeader::of_ptr(a)).kind.load(Ordering::Relaxed) },
            BLOCK_KIND_IMMORTAL
        );
    }

    #[test]
    fn refills_into_fresh_block_when_full() {
        let _g = crate::memory::block_pool::test_guard();

        let first = immortal_alloc(8);
        // Exhaust whatever remains of the current block.
        loop {
            let p = immortal_alloc(BLOCK_PAYLOAD / 4);
            if BlockHeader::of_ptr(p) != BlockHeader::of_ptr(first) {
                assert_eq!(
                    unsafe { (*BlockHeader::of_ptr(p)).kind.load(Ordering::Relaxed) },
                    BLOCK_KIND_IMMORTAL
                );
                break;
            }
        }
    }

    #[test]
    fn ll_free_on_immortal_is_a_no_op() {
        let _g = crate::memory::block_pool::test_guard();

        let p = immortal_alloc(64);
        unsafe { (p as *mut u64).write(0xC0FFEE) };
        unsafe { crate::memory::stdapi::ll_free(p) };

        // The block was not recycled: kind untouched, data intact.
        let block = BlockHeader::of_ptr(p);
        assert_eq!(
            unsafe { (*block).kind.load(Ordering::Relaxed) },
            BLOCK_KIND_IMMORTAL
        );
        assert_ne!(
            unsafe { (*block).kind.load(Ordering::Relaxed) },
            BLOCK_KIND_FREE
        );
        assert_eq!(unsafe { *(p as *mut u64) }, 0xC0FFEE);
    }
}

/// A request over a block payload takes an OS-direct run of its own,
/// which is a second shape beside the bump — and the bump has to be
/// left where it was when one is taken.
mod past_one_block {
    use super::*;

    /// An allocation larger than one block payload used to hit an
    /// `assert!`, which under `panic = "abort"` kills the process. That is
    /// only a defensible reading of "caller bug" while no caller forwards
    /// input, and a class's `[Class][vtbl][itables]` train has no such
    /// bound. It now takes an OS-direct run, which still answers
    /// `of_ptr` because the run is block-aligned.
    #[test]
    fn oversized_immortal_takes_an_os_direct_run() {
        let _g = crate::memory::block_pool::test_guard();

        let size = BLOCK_PAYLOAD * 3 + 7;
        let p = immortal_alloc(size);
        assert!(!p.is_null(), "an oversized immortal must not refuse here");
        assert_eq!(p as usize % 8, 0);

        let block = BlockHeader::of_ptr(p);
        assert_eq!(
            unsafe { (*block).kind.load(Ordering::Relaxed) },
            BLOCK_KIND_IMMORTAL
        );

        // Writable end to end, and the tail is really ours.
        unsafe {
            std::ptr::write_bytes(p, 0xA5, size);
            assert_eq!(*p, 0xA5);
            assert_eq!(*p.add(size - 1), 0xA5);
        }

        // A free is still a no-op, as for every other immortal pointer.
        unsafe { crate::memory::stdapi::ll_free(p) };
        assert_eq!(
            unsafe { (*block).kind.load(Ordering::Relaxed) },
            BLOCK_KIND_IMMORTAL
        );
        assert_eq!(unsafe { *p }, 0xA5);
    }

    /// The bump region must survive an oversized request: the run is its
    /// own allocation and must not disturb the current block's cursor.
    #[test]
    fn an_oversized_run_does_not_disturb_the_bump_region() {
        let _g = crate::memory::block_pool::test_guard();

        let a = immortal_alloc(16);
        let big = immortal_alloc(BLOCK_PAYLOAD + 1);
        let b = immortal_alloc(16);

        assert!(!big.is_null());
        assert_ne!(BlockHeader::of_ptr(big), BlockHeader::of_ptr(a));
        assert_eq!(BlockHeader::of_ptr(a), BlockHeader::of_ptr(b));
        assert_eq!(b as usize - a as usize, 16);
    }
}

/// Exhaustion is reported rather than fatal, and what is already
/// carved stays usable afterwards.
mod when_the_region_cannot_grow {
    use super::*;

    /// The third path that wrote the block header through the null the
    /// pool now returns. Class loading runs mid-request under autoload,
    /// so this one had to report as well.
    #[test]
    fn exhaustion_reports_null_and_leaves_the_region_usable() {
        let _g = crate::memory::block_pool::test_guard();
        use crate::memory::block_pool::FORCE_OOM;
        use std::sync::atomic::Ordering;

        // Fill whatever remains of the current block, so the next call
        // has to ask the pool.
        let _ = immortal_alloc(BLOCK_PAYLOAD);

        FORCE_OOM.store(true, Ordering::Relaxed);
        let p = immortal_alloc(64);
        FORCE_OOM.store(false, Ordering::Relaxed);
        assert!(p.is_null(), "exhaustion must report, not abort");

        let q = immortal_alloc(64);
        assert!(!q.is_null(), "the region survived the refusal");
    }
}

/// The region is process-wide, so two threads bumping it may not be
/// handed overlapping memory.
mod under_concurrency {
    use super::*;

    #[test]
    fn concurrent_allocation_hands_out_distinct_memory() {
        let _g = crate::memory::block_pool::test_guard();
        use std::thread;

        let handles: Vec<_> = (0..8)
            .map(|t| {
                thread::spawn(move || {
                    let mut mine = Vec::new();
                    for i in 0..500u64 {
                        let p = immortal_alloc(16) as *mut u64;
                        unsafe { p.write(t as u64 * 1_000_000 + i) };
                        mine.push((p, t as u64 * 1_000_000 + i));
                    }

                    // Nothing is ever freed, so every write must survive.
                    for (p, v) in mine {
                        assert_eq!(unsafe { *p }, v, "immortal memory corrupted");
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
    }
}
