//! Immortal region: bump allocation, no reset, nothing is ever freed.
//!
//! Class definitions, interned strings, vtables — entities that live
//! until process exit (`rfc/model/memory/arenas.md`, Immortal). One
//! global region under a `Mutex`: allocation here is rare (class
//! loading, interning) but can happen concurrently — JIT class-loading
//! races the request threads.
//!
//! Blocks come from the shared [`BlockPool`] and are never returned;
//! `put()` has no call path from here. `ll_free` on an immortal pointer
//! takes the arm it shares with arena memory and does nothing;
//! `ll_usable_size` answers zero from its default. Bump allocation has
//! no per-object size tracking to answer with anyway.

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
        crate::memory::block_pool::store_block_kind(&raw const (*block).kind, BLOCK_KIND_IMMORTAL)
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

    if run_bytes > isize::MAX as usize {
        return std::ptr::null_mut();
    }

    let block = crate::memory::os::map_aligned(run_bytes, BLOCK_SIZE) as *mut BlockHeader;
    if block.is_null() {
        // Same discipline as the pooled path: report, do not abort.
        return std::ptr::null_mut();
    }

    // Through the one write path like every other kind, though nothing
    // reads this one across threads: the run is its own mapping, it lies
    // inside no carved region, and it is registered nowhere the
    // enumerator reads — not the region registry, not the retained index,
    // not `large_entity`'s run list — so no scan reaches it. Uniform
    // anyway, because a discriminant punned across five header types with
    // two disciplines needs casts at the seams.
    unsafe {
        crate::memory::block_pool::store_block_kind(&raw const (*block).kind, BLOCK_KIND_IMMORTAL);
        BlockHeader::payload_start(block)
    }
}

#[cfg(test)]
mod tests;
