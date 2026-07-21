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
use crate::memory::block_pool::{BLOCK_KIND_IMMORTAL, BLOCK_PAYLOAD, BlockHeader, BlockPool};

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
/// Panics on sizes above a block payload: immortal entities (class
/// metadata, interned strings) are small; anything bigger is a caller
/// bug, which is a different thing from a machine out of memory.
pub fn immortal_alloc(size: usize) -> *mut u8 {
    let size = round_up_8(size);
    assert!(
        size <= BLOCK_PAYLOAD,
        "immortal entities must fit one block"
    );

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
    unsafe { (*block).kind = BLOCK_KIND_IMMORTAL };
    let p = BlockHeader::payload_start(block);
    r.bump = p.wrapping_add(size);
    r.limit = BlockHeader::end(block);
    p
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

    #[test]
    #[should_panic(expected = "immortal entities must fit one block")]
    fn oversized_immortal_is_a_caller_bug() {
        immortal_alloc(BLOCK_PAYLOAD + 1);
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
