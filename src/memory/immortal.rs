//! Immortal region: bump allocation, no reset, nothing is ever freed.
//!
//! Class definitions, interned strings, vtables — entities that live
//! until process exit (`rfc/model/memory/arenas.md`, Immortal). One
//! global region under a `Mutex`: allocation here is rare (class
//! loading, interning) but can happen concurrently — JIT class-loading
//! races the request threads.
//!
//! Blocks come from the shared [`BlockPool`] and are never returned;
//! `put()` has no call path from here. `ll_free`/`ll_usable_size` on an
//! immortal pointer fall to their existing no-op default, same as
//! arena blocks — bump allocation has no per-object size tracking
//! anyway.

use std::sync::Mutex;

use crate::memory::arena::round_up_8;
use crate::memory::block_pool::{
    BLOCK_KIND_IMMORTAL, BLOCK_PAYLOAD, BLOCK_SIZE, BlockHeader, BlockPool, LINE_SIZE,
};

struct Region {
    bump: *mut u8,
    limit: *mut u8,
}

// The raw pointers are only touched under the Mutex.
unsafe impl Send for Region {}

static IMMORTAL: Mutex<Region> = Mutex::new(Region {
    bump: std::ptr::null_mut(),
    limit: std::ptr::null_mut(),
});

/// Allocate `size` bytes that will never be freed. **Null when memory
/// runs out** — class loading can happen mid-request (autoload), so a
/// refusal has to reach a frame that can raise, not kill the process.
///
/// A request above one block payload takes an OS-direct, block-aligned
/// run instead of the bump region. This used to be an `assert!` on the
/// grounds that immortal entities are small and anything larger is a
/// caller bug — but that reading only holds while no caller forwards
/// input, and under `panic = "abort"` the assert kills the worker rather
/// than raising. A class's `[Class][vtbl][itables]` train has no such
/// bound either.
pub fn immortal_alloc(size: usize) -> *mut u8 {
    let size = round_up_8(size);
    if size > BLOCK_PAYLOAD {
        return immortal_alloc_run(size);
    }

    let mut r = IMMORTAL.lock().unwrap();

    // Same overflow discipline as the arena: `size` is ABI input.
    if !r.bump.is_null() {
        if let Some(next) = (r.bump as usize).checked_add(size) {
            if next <= r.limit as usize {
                let p = r.bump;
                r.bump = next as *mut u8;
                return p;
            }
        }
    }

    // Fresh block; the remainder of the old one is abandoned (same
    // waste profile as the arena slow path).
    let block = BlockPool::global().get();
    if block.is_null() {
        // The pool reports exhaustion instead of aborting, so this path
        // reports too. The region keeps its old bump and limit: a refusal
        // leaves nothing half-rotated, and a later call can succeed.
        return std::ptr::null_mut();
    }
    // A pooled block sits inside a carved region, where the collector
    // acquire-loads every block's kind, so the store is the release one
    // even though nothing here is ever walked.
    unsafe {
        crate::memory::block_pool::store_block_kind(&raw mut (*block).kind, BLOCK_KIND_IMMORTAL)
    };
    let p = BlockHeader::payload_start(block);
    r.bump = p.wrapping_add(size);
    r.limit = BlockHeader::end(block);
    p
}

/// One immortal entity too large for a block: an OS-direct run, aligned
/// to `BLOCK_SIZE` so `BlockHeader::of_ptr` on any pointer into its first
/// block still finds the header, with the payload at `+LINE_SIZE` like
/// every other block.
///
/// The run is never freed and never returns to the pool, so it needs no
/// size field: `ll_free` on an immortal pointer is already a no-op, and
/// nothing enumerates immortal blocks. It also does not touch the bump
/// region — a huge entity must not abandon the remainder of the current
/// block behind it.
#[cold]
fn immortal_alloc_run(size: usize) -> *mut u8 {
    // `size` reaches here from ABI input in the interning path, so both
    // the header add and the block round-up are guarded: either would
    // wrap to a tiny run and hand back an under-allocation.
    let run_bytes = match size
        .checked_add(LINE_SIZE)
        .and_then(|n| n.checked_add(BLOCK_SIZE - 1))
        .map(|n| n & !(BLOCK_SIZE - 1))
    {
        Some(n) => n,
        None => return std::ptr::null_mut(),
    };
    let layout = match std::alloc::Layout::from_size_align(run_bytes, BLOCK_SIZE) {
        Ok(l) => l,
        Err(_) => return std::ptr::null_mut(),
    };
    let block = unsafe { std::alloc::alloc(layout) } as *mut BlockHeader;
    if block.is_null() {
        // Same discipline as the pooled path: report, do not abort.
        return std::ptr::null_mut();
    }
    // The one block kind in the crate still written by a plain store, and
    // the reason is the allocation rather than the discipline: this run
    // comes from the system allocator and lies inside no carved region,
    // so the collector's scan — which reads a region's blocks and nothing
    // else — never loads this word. Anything pooled goes through
    // `store_block_kind`.
    unsafe {
        (*block).kind = BLOCK_KIND_IMMORTAL;
        BlockHeader::payload_start(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::block_pool::BLOCK_KIND_FREE;

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
            unsafe { (*BlockHeader::of_ptr(a)).kind },
            BLOCK_KIND_IMMORTAL
        );
    }

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

    #[test]
    fn ll_free_on_immortal_is_a_no_op() {
        let _g = crate::memory::block_pool::test_guard();

        let p = immortal_alloc(64);
        unsafe { (p as *mut u64).write(0xC0FFEE) };
        unsafe { crate::memory::stdapi::ll_free(p) };

        // The block was not recycled: kind untouched, data intact.
        let block = BlockHeader::of_ptr(p);
        assert_eq!(unsafe { (*block).kind }, BLOCK_KIND_IMMORTAL);
        assert_ne!(unsafe { (*block).kind }, BLOCK_KIND_FREE);
        assert_eq!(unsafe { *(p as *mut u64) }, 0xC0FFEE);
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
                    unsafe { (*BlockHeader::of_ptr(p)).kind },
                    BLOCK_KIND_IMMORTAL
                );
                break;
            }
        }
    }

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
        assert_eq!(unsafe { (*block).kind }, BLOCK_KIND_IMMORTAL);

        // Writable end to end, and the tail is really ours.
        unsafe {
            std::ptr::write_bytes(p, 0xA5, size);
            assert_eq!(*p, 0xA5);
            assert_eq!(*p.add(size - 1), 0xA5);
        }

        // A free is still a no-op, as for every other immortal pointer.
        unsafe { crate::memory::stdapi::ll_free(p) };
        assert_eq!(unsafe { (*block).kind }, BLOCK_KIND_IMMORTAL);
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
