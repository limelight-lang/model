//! The memory-manager door for blocks owned directly by cycle collection.
//!
//! A pool block does not become GC memory merely because its kind keeps the
//! entity walker out. Ownership begins here, where the block is stamped and
//! charged to one physical role, and ends here before it returns to the pool
//! or the critical reserve. Moving a queue segment from spare to live changes
//! no ownership and therefore crosses no function in this module.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::memory::block_pool::{
    BLOCK_KIND_ARENA, BLOCK_KIND_FREE, BLOCK_KIND_GC_METADATA, BLOCK_SIZE, BlockHeader, BlockPool,
    load_block_kind, store_block_kind,
};

const ROLE_COUNT: usize = 4;

/// The physical use for which the collector owns a block.
#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GcBlockRole {
    QueueFloor = 0,
    QueueSegment = 1,
    WorkspaceBase = 2,
    WorkspaceOverflow = 3,
}

static CURRENT: [AtomicUsize; ROLE_COUNT] = [const { AtomicUsize::new(0) }; ROLE_COUNT];
static PEAK: [AtomicUsize; ROLE_COUNT] = [const { AtomicUsize::new(0) }; ROLE_COUNT];

/// A non-transactional observation of physical ownership. Each role is read
/// independently, so concurrent handoffs may make fields describe adjacent
/// instants; bytes are still derived from their matching block counts and can
/// never disagree with them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GcMemoryStats {
    current: [usize; ROLE_COUNT],
    peak: [usize; ROLE_COUNT],
}

impl GcMemoryStats {
    #[inline]
    pub fn current_blocks(self, role: GcBlockRole) -> usize {
        self.current[role as usize]
    }

    #[inline]
    pub fn peak_blocks(self, role: GcBlockRole) -> usize {
        self.peak[role as usize]
    }

    #[inline]
    pub fn current_bytes(self, role: GcBlockRole) -> usize {
        self.current_blocks(role) * BLOCK_SIZE
    }

    #[inline]
    pub fn peak_bytes(self, role: GcBlockRole) -> usize {
        self.peak_blocks(role) * BLOCK_SIZE
    }
}

/// Observe all blocks currently owned by cycle collection, by role. The
/// observation is deliberately approximate across roles rather than protected
/// by a lock on thread init/exit.
pub fn stats() -> GcMemoryStats {
    let mut current = [0; ROLE_COUNT];
    let mut peak = [0; ROLE_COUNT];
    for role in 0..ROLE_COUNT {
        current[role] = CURRENT[role].load(Ordering::Relaxed);
        peak[role] = PEAK[role].load(Ordering::Relaxed);
    }
    GcMemoryStats { current, peak }
}

#[inline]
fn acquired(block: *mut BlockHeader, source_kind: u32, role: GcBlockRole) -> *mut BlockHeader {
    if block.is_null() {
        return block;
    }

    assert_eq!(
        unsafe { load_block_kind(&raw const (*block).kind) },
        source_kind,
        "adopting a block across the wrong ownership boundary"
    );
    let slot = role as usize;
    // Role and accounting precede the release publication of the kind. A
    // collector that sees GC_METADATA can therefore also identify the role
    // through the same header rather than trusting the eventual releaser.
    unsafe { (*block).reserved.store(slot as u32, Ordering::Relaxed) };
    let current = CURRENT[slot].fetch_add(1, Ordering::Relaxed) + 1;
    PEAK[slot].fetch_max(current, Ordering::Relaxed);
    unsafe { store_block_kind(&raw const (*block).kind, BLOCK_KIND_GC_METADATA) };
    block
}

#[inline]
fn released(block: *mut BlockHeader, role: GcBlockRole) {
    let kind = unsafe { load_block_kind(&raw const (*block).kind) };
    assert_eq!(kind, BLOCK_KIND_GC_METADATA, "returning a non-GC block");
    let recorded = unsafe { (*block).reserved.load(Ordering::Relaxed) };
    assert_eq!(
        recorded, role as u32,
        "returning a GC block as the wrong role"
    );

    CURRENT[role as usize]
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_sub(1)
        })
        .expect("a GC role counter cannot underflow");
}

/// Draw ordinary pool memory and make it GC-owned for `role`.
pub(crate) fn acquire(role: GcBlockRole) -> *mut BlockHeader {
    acquired(BlockPool::global().get(), BLOCK_KIND_FREE, role)
}

/// Adopt a block lent by the critical reserve. Null remains null.
pub(crate) fn adopt(block: *mut BlockHeader, role: GcBlockRole) -> *mut BlockHeader {
    acquired(block, BLOCK_KIND_ARENA, role)
}

/// End GC ownership and return ordinary memory to the pool.
pub(crate) fn release(block: *mut BlockHeader, role: GcBlockRole) {
    if block.is_null() {
        return;
    }
    released(block, role);
    // End the GC population before crossing the manager boundary. `put`
    // rejects a still-GC-stamped block, making a direct bypass observable.
    unsafe { store_block_kind(&raw const (*block).kind, BLOCK_KIND_FREE) };
    BlockPool::global().put(block);
}

/// End GC ownership and return a block through the critical-reserve door.
pub(crate) fn release_to_critical(block: *mut BlockHeader, role: GcBlockRole) {
    if block.is_null() {
        return;
    }
    released(block, role);
    unsafe { store_block_kind(&raw const (*block).kind, BLOCK_KIND_ARENA) };
    crate::memory::critical::give_back(block);
}

#[cfg(test)]
mod tests;
