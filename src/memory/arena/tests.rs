use super::*;
use crate::memory::block_pool::{BLOCK_MASK, LINE_SIZE};
use crate::refcount::{MemoryCategory, RcHeader};

/// Allocation is a bump rounded to eight bytes: a full block is
/// replaced rather than reused, `reserve` keeps a loop inside the
/// block it started in, and only the last allocation bumped can grow
/// in place.
mod the_bump_over_pooled_blocks {
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
}

/// An allocation refuses with null and an in-place extension with
/// `false`, and either leaves the arena serving. The rounding
/// saturates instead of wrapping, so a size no block can hold fails
/// the bound rather than becoming a small one.
mod what_the_arena_refuses {
    use super::*;

    /// Exhaustion is reported, not fatal. Every pooled path used to abort
    /// on it — `carve_region`, `alloc_large`, the buffer arena — while the
    /// huge-allocation path returned null, so a C caller got null for
    /// 200 KB and a dead process for 40 bytes.
    ///
    /// Revert any of those to `assert!` and this test kills the process
    /// rather than failing.
    #[test]
    fn exhaustion_reports_null_and_leaves_the_arena_usable() {
        let _g = crate::memory::block_pool::test_guard();
        use crate::memory::block_pool::FORCE_OOM;
        use std::sync::atomic::Ordering;

        let mut arena = Arena::new();

        FORCE_OOM.store(true, Ordering::Relaxed);
        let p = arena.alloc(40);
        FORCE_OOM.store(false, Ordering::Relaxed);

        assert!(p.is_null(), "exhaustion must report, not abort");

        // Still usable once memory is available again: the refusal left
        // no half-rotated state behind.
        let q = arena.alloc(40);
        assert!(!q.is_null(), "the arena survived the refusal");
        arena.reset(|_| {});
    }

    /// A size no block can hold is refused, and the refusal leaves the
    /// arena serving: the rounding saturates instead of wrapping, so the
    /// request stays huge and fails the bound rather than becoming a
    /// small one. A refusal rather than an abort, because the size
    /// arrives through `ll_arena_alloc` and a program can name it.
    #[test]
    fn absurd_size_is_refused_instead_of_wrapping() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        arena.alloc(8); // non-null bump: the fast path is reachable
        assert!(arena.alloc(usize::MAX - 64).is_null());
        assert!(arena.alloc(BLOCK_PAYLOAD + 1).is_null());
        assert!(
            !arena.alloc(8).is_null(),
            "a refusal left the arena unable to serve"
        );
    }

    #[test]
    fn extend_refuses_absurd_size_instead_of_wrapping() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let buf = arena.alloc(64);
        assert!(!arena.try_extend_in_place(buf, 64, usize::MAX - 64));
        assert!(
            arena.try_extend_in_place(buf, 64, 128),
            "sane size still extends"
        );
    }
}

/// The destructor log is segmented, so a record may not be lost at a
/// segment boundary — which is what the growth test walks past; the
/// escape log is chained the same way and no test here reaches its
/// boundary. The barrier's log grows from the thread reserve when the
/// pool refuses, because the barrier has no way to report a failure.
/// The reset drains three more logs beside these two: the
/// release-at-reset log, the weak log, and the large runs.
mod the_logs_the_reset_reads {
    use super::*;

    /// The barrier has no way to report a failure, so its log growth must
    /// not have one. When the pool refuses, the segment comes from the
    /// thread's reserve — and the escape record still lands, which is the
    /// whole point: a lost escapee dangles at reset.
    ///
    /// The reserve block must not become the arena's bump block either.
    /// If it did, ordinary allocation would spend the memory that exists
    /// so the barrier cannot fail, and the null that lets a frame raise
    /// would never be returned.
    #[test]
    fn the_barrier_log_grows_from_the_reserve_when_the_pool_refuses() {
        let _g = crate::memory::block_pool::test_guard();
        use crate::memory::block_pool::FORCE_OOM;
        use std::sync::atomic::Ordering;

        crate::memory::reserve::drain_for_test();
        assert!(crate::memory::reserve::replenish());

        let mut arena = Arena::new();
        let mut entity = RcHeader::new(MemoryCategory::RequestArena, 0);

        FORCE_OOM.store(true, Ordering::Relaxed);
        assert!(
            arena.alloc(16).is_null(),
            "ordinary allocation reports the exhaustion"
        );
        // Records an escapee: this is the path with no channel at all.
        arena.log_escapee(&mut entity);
        assert!(
            arena.alloc(16).is_null(),
            "and still reports it — the reserve is not the arena's bump block"
        );
        FORCE_OOM.store(false, Ordering::Relaxed);

        assert!(
            crate::memory::reserve::is_drawn(),
            "the draw asks the next safepoint for a refill"
        );
        assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
        assert!(
            !crate::memory::reserve::is_drawn(),
            "which the safepoint answers"
        );

        let mut seen = 0;
        arena.reset_with(|_| {}, |_| seen += 1);
        assert_eq!(seen, 1, "the escapee record survived the exhaustion");
        crate::memory::reserve::drain_for_test();
    }

    #[test]
    fn destructor_log_survives_segment_growth() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();

        // Three segments' worth of tracked objects.
        let n = LOG_SEG_RECORDS * 2 + 137;
        let objs: Vec<*mut RcHeader> = (0..n)
            .map(|_| {
                let obj = arena.alloc(16) as *mut RcHeader;
                unsafe { obj.write(RcHeader::new(MemoryCategory::RequestArena, 0)) };
                assert!(arena.track_destructor(obj));
                obj
            })
            .collect();

        let mut ran = Vec::new();
        arena.reset(|o| ran.push(o));

        assert_eq!(ran.len(), n, "every tracked destructor must be delivered");
        let expected: std::collections::HashSet<_> = objs.iter().map(|p| *p as usize).collect();
        let got: std::collections::HashSet<_> = ran.iter().map(|p| *p as usize).collect();
        assert_eq!(got, expected, "same set of objects, order unspecified");
    }

    #[test]
    fn reset_hands_destructors_and_recycles_blocks() {
        let _g = crate::memory::block_pool::test_guard();
        let pool = BlockPool::global();
        let mut arena = Arena::new();

        let obj = arena.alloc(16) as *mut RcHeader;
        unsafe { obj.write(RcHeader::new(MemoryCategory::RequestArena, 0)) };
        assert!(arena.track_destructor(obj));
        let old_block = BlockHeader::of_ptr(obj as *mut u8);

        let mut ran = Vec::new();
        arena.reset(|o| ran.push(o));
        assert_eq!(ran, vec![obj], "destructor list must reach the caller");

        let regions_before = pool.regions_carved();
        let mut second = Arena::new();
        let p = second.alloc(8);
        assert_eq!(
            BlockHeader::of_ptr(p),
            old_block,
            "next arena must reuse the recycled block"
        );
        assert_eq!(pool.regions_carved(), regions_before);
    }
}
