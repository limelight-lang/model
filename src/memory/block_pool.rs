//! Global pool of 32 KB blocks.
//!
//! Layers (per `docs/memory-manager.md`): OS regions of 2 MB are carved
//! into 32 KB blocks aligned to their size; free blocks live in a
//! process-global lock-free stack; each thread keeps a small cache in
//! front of it (tcmalloc pattern: refill in batches, flush half on
//! overflow, flush all on thread death).
//!
//! Phase 1 simplification: regions are never returned to the OS
//! (the lazy purge policy from the doc arrives later).

use std::alloc::{Layout, alloc};
use std::cell::RefCell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

pub const BLOCK_SIZE: usize = 32 * 1024;
pub const BLOCK_MASK: usize = BLOCK_SIZE - 1;
/// One 256-byte line at the block start holds the header.
pub const LINE_SIZE: usize = 256;
pub const BLOCK_PAYLOAD: usize = BLOCK_SIZE - LINE_SIZE;

pub const REGION_SIZE: usize = 2 * 1024 * 1024;
const BLOCKS_PER_REGION: usize = REGION_SIZE / BLOCK_SIZE; // 64

const THREAD_CACHE_CAPACITY: usize = 8;
const REFILL_BATCH: usize = 4;

/// Block kinds stored in the header.
pub const BLOCK_KIND_FREE: u32 = 0;
pub const BLOCK_KIND_ARENA: u32 = 1;
pub const BLOCK_KIND_HEAP: u32 = 2;
/// A single pooled block holding one large object (8 KB..block payload).
pub const BLOCK_KIND_LARGE: u32 = 3;
/// An OS-direct, 32 KB-aligned run of blocks for a huge object.
pub const BLOCK_KIND_LARGE_RUN: u32 = 4;

/// Header in the first line of every block.
///
/// Cache-line layout rule (validated against jemalloc/mimalloc): this
/// first line holds read-mostly fields and the free-list link (written
/// only while the block is free). Future concurrent fields (remembered
/// set, line marks) get their own lines within the 256-byte header.
#[repr(C)]
pub struct BlockHeader {
    pub kind: u32,
    reserved: u32,
    /// Free-list link; meaningful only while the block sits in the pool.
    next: *mut BlockHeader,
}

impl BlockHeader {
    /// The owning block of any interior pointer: one mask.
    #[inline]
    pub fn of_ptr(p: *const u8) -> *mut BlockHeader {
        ((p as usize) & !BLOCK_MASK) as *mut BlockHeader
    }

    #[inline]
    pub fn payload_start(block: *mut BlockHeader) -> *mut u8 {
        unsafe { (block as *mut u8).add(LINE_SIZE) }
    }

    #[inline]
    pub fn end(block: *mut BlockHeader) -> *mut u8 {
        unsafe { (block as *mut u8).add(BLOCK_SIZE) }
    }
}

/// Lock-free Treiber stack of free blocks.
///
/// Blocks are 32 KB-aligned, so the low 15 bits of a block address are
/// zero — we pack an ABA tag there. Dereferencing a popped-under-us
/// block is safe in phase 1 because regions are never unmapped.
pub struct BlockPool {
    head: AtomicUsize, // block_ptr | tag(15 bits)
    regions_carved: AtomicUsize,
}

const TAG_MASK: usize = BLOCK_MASK; // low 15 bits

#[inline]
fn pack(ptr: *mut BlockHeader, tag: usize) -> usize {
    (ptr as usize) | (tag & TAG_MASK)
}

#[inline]
fn unpack(word: usize) -> (*mut BlockHeader, usize) {
    ((word & !TAG_MASK) as *mut BlockHeader, word & TAG_MASK)
}

static GLOBAL_POOL: BlockPool = BlockPool {
    head: AtomicUsize::new(0),
    regions_carved: AtomicUsize::new(0),
};

/// Serializes region carving only (rare path).
static CARVE_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    static THREAD_CACHE: RefCell<ThreadCache> =
        const { RefCell::new(ThreadCache { blocks: Vec::new() }) };
}

struct ThreadCache {
    blocks: Vec<*mut BlockHeader>,
}

impl Drop for ThreadCache {
    /// A dying thread must not take cached blocks with it.
    fn drop(&mut self) {
        for &block in &self.blocks {
            GLOBAL_POOL.push_global(block);
        }
    }
}

impl BlockPool {
    pub fn global() -> &'static BlockPool {
        &GLOBAL_POOL
    }

    /// Number of 2 MB regions requested from the OS (test/stats hook).
    pub fn regions_carved(&self) -> usize {
        self.regions_carved.load(Ordering::Relaxed)
    }

    /// Get a free block: thread cache → global stack → carve a region.
    pub fn get(&self) -> *mut BlockHeader {
        let cached = THREAD_CACHE.with(|c| c.borrow_mut().blocks.pop());
        if let Some(block) = cached {
            return block;
        }

        // Refill the cache in a batch, return one.
        let mut batch = Vec::with_capacity(REFILL_BATCH);
        while batch.len() < REFILL_BATCH {
            match self.pop_global() {
                Some(b) => batch.push(b),
                None => {
                    self.carve_region();
                }
            }
        }
        let block = batch.pop().unwrap();
        THREAD_CACHE.with(|c| c.borrow_mut().blocks.extend(batch));
        block
    }

    /// Return a block. Overflowing the thread cache flushes half of it
    /// to the global stack (jemalloc flush pattern).
    ///
    /// `block` must be a block previously handed out by [`get`]; the
    /// pool is its sole authority, so this internal API stays safe.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn put(&self, block: *mut BlockHeader) {
        unsafe { (*block).kind = BLOCK_KIND_FREE };

        THREAD_CACHE.with(|c| {
            let mut cache = c.borrow_mut();
            cache.blocks.push(block);

            if cache.blocks.len() > THREAD_CACHE_CAPACITY {
                let keep = THREAD_CACHE_CAPACITY / 2;
                for b in cache.blocks.drain(keep..) {
                    self.push_global(b);
                }
            }
        });
    }

    fn push_global(&self, block: *mut BlockHeader) {
        loop {
            let old = self.head.load(Ordering::Acquire);
            let (old_ptr, old_tag) = unpack(old);
            unsafe { (*block).next = old_ptr };
            let new = pack(block, old_tag.wrapping_add(1));

            if self
                .head
                .compare_exchange_weak(old, new, Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
        }
    }

    fn pop_global(&self) -> Option<*mut BlockHeader> {
        loop {
            let old = self.head.load(Ordering::Acquire);
            let (ptr, tag) = unpack(old);

            if ptr.is_null() {
                return None;
            }

            let next = unsafe { (*ptr).next };
            let new = pack(next, tag.wrapping_add(1));

            if self
                .head
                .compare_exchange_weak(old, new, Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                return Some(ptr);
            }
        }
    }

    /// Reserve a 2 MB region from the OS and stack its 64 blocks.
    fn carve_region(&self) {
        let _guard = CARVE_LOCK.lock().unwrap();

        // Someone may have carved while we waited for the lock.
        let (head, _) = unpack(self.head.load(Ordering::Acquire));
        if !head.is_null() {
            return;
        }

        let layout = Layout::from_size_align(REGION_SIZE, BLOCK_SIZE).unwrap();
        let region = unsafe { alloc(layout) };
        assert!(!region.is_null(), "OS refused a {REGION_SIZE}-byte region");

        for i in 0..BLOCKS_PER_REGION {
            let block = unsafe { region.add(i * BLOCK_SIZE) } as *mut BlockHeader;
            unsafe {
                (*block).kind = BLOCK_KIND_FREE;
                (*block).reserved = 0;
                (*block).next = std::ptr::null_mut();
            }
            self.push_global(block);
        }

        self.regions_carved.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_are_aligned_and_mask_finds_header() {
        let pool = BlockPool::global();
        let block = pool.get();

        assert_eq!(block as usize & BLOCK_MASK, 0, "block must be 32K-aligned");

        // Any interior pointer maps back to the header with one mask.
        let payload = BlockHeader::payload_start(block);
        let interior = unsafe { payload.add(12345) };
        assert_eq!(BlockHeader::of_ptr(interior), block);

        assert_eq!(payload as usize - block as usize, LINE_SIZE);
        assert_eq!(
            BlockHeader::end(block) as usize - block as usize,
            BLOCK_SIZE
        );

        pool.put(block);
    }

    #[test]
    fn put_then_get_reuses_without_new_region() {
        let pool = BlockPool::global();
        let block = pool.get();
        let regions_before = pool.regions_carved();

        pool.put(block);
        let again = pool.get();

        assert_eq!(again, block, "thread cache must hand the block back");
        assert_eq!(pool.regions_carved(), regions_before);
        pool.put(again);
    }

    #[test]
    fn dying_thread_returns_blocks_to_global_stack() {
        let pool = BlockPool::global();

        let block_addr = std::thread::spawn(|| {
            let pool = BlockPool::global();
            let block = pool.get();
            pool.put(block); // lands in that thread's cache
            block as usize
            // thread dies -> cache Drop flushes to the global stack
        })
        .join()
        .unwrap();

        let regions_before = pool.regions_carved();

        // Drain until we find the flushed block: it must be reachable
        // without carving a new region.
        let mut taken = Vec::new();
        let mut found = false;
        for _ in 0..1000 {
            let b = pool.get();
            if b as usize == block_addr {
                found = true;
                taken.push(b);
                break;
            }
            taken.push(b);
        }

        assert!(found, "block cached by a dead thread was lost");
        assert_eq!(pool.regions_carved(), regions_before);

        for b in taken {
            pool.put(b);
        }
    }
}
