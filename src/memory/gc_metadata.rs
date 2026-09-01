//! The memory-manager door for blocks owned directly by cycle collection.
//!
//! A pool block does not become GC memory merely because its kind keeps the
//! entity walker out. Ownership begins here, where the block is stamped and
//! counted, and ends here before it returns to the pool or the critical
//! reserve. Moving a queue segment from spare to live changes no ownership and
//! therefore crosses no function in this module.
//!
//! What the count answers is "how much memory does collection hold", and the
//! block kind is what separates that memory from a request arena's or an
//! entity heap's. A split by use — queue against workspace — is not kept: no
//! reader needs it, and a measurement that wants it can be taken on the day
//! (`dev/DECISIONS.md`, 2026-09-01).

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::memory::block_pool::{
    BLOCK_KIND_ARENA, BLOCK_KIND_FREE, BLOCK_KIND_GC_METADATA, BLOCK_SIZE, BlockHeader, BlockPool,
    load_block_kind, store_block_kind,
};

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// A non-transactional observation of physical ownership. The two figures are
/// read independently, so a concurrent handoff may make them describe adjacent
/// instants; bytes are derived from their own block count and can never
/// disagree with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GcMemoryStats {
    current: usize,
    peak: usize,
}

impl GcMemoryStats {
    #[inline]
    pub fn current_blocks(self) -> usize {
        self.current
    }

    #[inline]
    pub fn peak_blocks(self) -> usize {
        self.peak
    }

    #[inline]
    pub fn current_bytes(self) -> usize {
        self.current * BLOCK_SIZE
    }

    #[inline]
    pub fn peak_bytes(self) -> usize {
        self.peak * BLOCK_SIZE
    }
}

/// Observe the blocks cycle collection owns now and the most it has held.
pub fn stats() -> GcMemoryStats {
    GcMemoryStats {
        current: CURRENT.load(Ordering::Relaxed),
        peak: PEAK.load(Ordering::Relaxed),
    }
}

#[inline]
fn acquired(block: *mut BlockHeader, source_kind: u32) -> *mut BlockHeader {
    if block.is_null() {
        return block;
    }

    assert_eq!(
        unsafe { load_block_kind(&raw const (*block).kind) },
        source_kind,
        "adopting a block across the wrong ownership boundary"
    );
    // The count precedes the release publication of the kind, so a reader that
    // sees GC_METADATA sees a block already charged rather than one the
    // eventual releaser will have to account for.
    let current = CURRENT.fetch_add(1, Ordering::Relaxed) + 1;
    PEAK.fetch_max(current, Ordering::Relaxed);
    unsafe { store_block_kind(&raw const (*block).kind, BLOCK_KIND_GC_METADATA) };
    block
}

#[inline]
fn released(block: *mut BlockHeader) {
    let kind = unsafe { load_block_kind(&raw const (*block).kind) };
    assert_eq!(kind, BLOCK_KIND_GC_METADATA, "returning a non-GC block");

    CURRENT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_sub(1)
        })
        .expect("the GC block counter cannot underflow");
}

/// Draw ordinary pool memory and make it GC-owned.
pub(crate) fn acquire() -> *mut BlockHeader {
    acquired(BlockPool::global().get(), BLOCK_KIND_FREE)
}

/// Adopt a block lent by the critical reserve. Null remains null.
pub(crate) fn adopt(block: *mut BlockHeader) -> *mut BlockHeader {
    acquired(block, BLOCK_KIND_ARENA)
}

/// End GC ownership and return ordinary memory to the pool.
pub(crate) fn release(block: *mut BlockHeader) {
    if block.is_null() {
        return;
    }
    released(block);
    // End the GC population before crossing the manager boundary. `put`
    // rejects a still-GC-stamped block, making a direct bypass observable.
    unsafe { store_block_kind(&raw const (*block).kind, BLOCK_KIND_FREE) };
    BlockPool::global().put(block);
}

/// End GC ownership and return a block through the critical-reserve door.
pub(crate) fn release_to_critical(block: *mut BlockHeader) {
    if block.is_null() {
        return;
    }
    released(block);
    unsafe { store_block_kind(&raw const (*block).kind, BLOCK_KIND_ARENA) };
    crate::memory::critical::give_back(block);
}

#[cfg(test)]
mod tests;
