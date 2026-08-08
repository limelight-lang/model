//! Global pool of 64 KB blocks.
//!
//! Layers (per `docs/memory-manager.md`): OS regions of 2 MB are carved
//! into 64 KB blocks aligned to their size; free blocks live in a
//! process-global chain behind a `Mutex`; each thread keeps a small cache in
//! front of it (tcmalloc pattern: refill in batches, flush half on
//! overflow, flush all on thread death).
//!
//! Phase 1 simplification: regions are never returned to the OS
//! (the lazy purge policy from the doc arrives later).

use std::alloc::{Layout, alloc};
use std::cell::RefCell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Block size, and with it the number of slots a size class gets per block.
///
/// 64 KB, matching mimalloc's small page (`MI_SMALL_PAGE_SHIFT`). The number
/// is chosen against block-list churn, not against footprint: a class's head
/// block is unlinked when it fills and re-linked when a slot frees, and each
/// crossing rewrites two neighbouring blocks' cold headers. Halving the
/// slots per block doubles how often that line is crossed. At a 5000-object
/// live set, 32 KB gave 0.63 link/unlink per alloc against 64 KB's 0.32, and
/// the throughput followed. See `rfc/model/memory/heap-slot-allocation.md`.
///
/// Nothing else in the tree may assume this value: derive from it, or from
/// `BLOCK_PAYLOAD` / `BLOCK_MASK`.
pub const BLOCK_SIZE: usize = 64 * 1024;
pub const BLOCK_MASK: usize = BLOCK_SIZE - 1;
/// One 256-byte line at the block start holds the header.
pub const LINE_SIZE: usize = 256;
pub const BLOCK_PAYLOAD: usize = BLOCK_SIZE - LINE_SIZE;

pub const REGION_SIZE: usize = 2 * 1024 * 1024;
pub(crate) const BLOCKS_PER_REGION: usize = REGION_SIZE / BLOCK_SIZE;

const THREAD_CACHE_CAPACITY: usize = 8;
const REFILL_BATCH: usize = 4;

/// Store a block header's `kind` discriminant. Under `rc-walk` the
/// store is a release atomic: the collector's snapshot reads kinds of
/// every block in every region concurrently, a racing plain store is
/// undefined behaviour, and the release ordering is what publishes a
/// commissioned block's other header fields before its kind says
/// "entity" (`heap::snapshot_entity_blocks` loads with acquire).
#[inline]
pub(crate) unsafe fn store_block_kind(kind_field: *mut u32, kind: u32) {
    #[cfg(not(feature = "rc-walk"))]
    unsafe {
        kind_field.write(kind)
    };
    #[cfg(feature = "rc-walk")]
    unsafe {
        (*(kind_field as *const std::sync::atomic::AtomicU32))
            .store(kind, std::sync::atomic::Ordering::Release)
    };
}

/// Block kinds stored in the header.
pub const BLOCK_KIND_FREE: u32 = 0;
pub const BLOCK_KIND_ARENA: u32 = 1;
pub const BLOCK_KIND_HEAP: u32 = 2;
/// A single pooled block holding one large object (8 KB..block payload).
pub const BLOCK_KIND_LARGE: u32 = 3;
/// An OS-direct, block-aligned run of blocks for a huge object.
pub const BLOCK_KIND_LARGE_RUN: u32 = 4;
/// Immortal-region block: bump-allocated, never returned to the pool.
pub const BLOCK_KIND_IMMORTAL: u32 = 5;
/// Long-lived buffer-arena block: bump + per-block free list.
pub const BLOCK_KIND_BUFFER: u32 = 6;
/// A former arena block retained at reset because it carries survivors
/// (`rfc/model/memory/arena-reset.md`, dense-block retention). A freed
/// survivor is a no-op: Immix line recycling is dropped from the plan
/// (2026-07-25), so the block's memory stays out of circulation while
/// its survivors live; reclaiming a fully-emptied retained block is a
/// small future mechanism.
pub const BLOCK_KIND_RETAINED: u32 = 7;
/// Entity block: same size-class layout as `BLOCK_KIND_HEAP`, but its
/// slots hold GC entities (header at offset 0) and only these blocks are
/// walked by the cycle collector (`rfc/model/gc/rc-walk.md`,
/// "Prerequisite: entity blocks are segregated"). A raw C-ABI buffer must
/// never land in one: the walker reads every occupied slot's first 8
/// bytes as an `RcHeader`.
pub const BLOCK_KIND_ENTITY: u32 = 8;

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
    /// Free-list link while the block sits in the pool. While a block
    /// is owned, the owner may reuse it as its own chain link (the
    /// arena threads its block list through here — no side `Vec`).
    pub(crate) next: *mut BlockHeader,
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

/// The chain of free blocks, threaded through `BlockHeader::next`.
///
/// Behind a `Mutex`, not a lock-free stack — the same call the crate
/// already makes for [`Abandoned`](crate::memory::heap), and for the same
/// reason: every user of this chain is cold. A per-thread cache sits in
/// front of it and refills in batches (`REFILL_BATCH`), so the global
/// chain is touched only on a cache miss or an overflow flush.
///
/// It was a Treiber stack with an ABA tag packed into the block-aligned
/// low bits. That was **unsound**, though not because of ABA, and not
/// because of reclamation either — regions are never unmapped, so
/// dereferencing a node popped from under you cannot fault. The defect
/// was narrower and particular to this codebase: `pop_global` read
/// `(*ptr).next` non-atomically, and the block header is a tagged union
/// in which those same bytes are the heap's `used` counter, which the
/// block's new owner writes non-atomically on every allocation. Two
/// conflicting accesses, at least one non-atomic, is a data race by the
/// memory model however harmless it looks — and Miri reports it.
///
/// Note what that does *not* say: a correct lock-free version is
/// perfectly possible. Making `next` atomic alone would not do it, since
/// the racing write is the owner's and lives on the allocation hot path
/// where it must stay non-atomic. But giving the pool its **own** link
/// field, at an offset no owner view aliases, would — atomic on both
/// sides, with the existing tag for ABA.
///
/// The lock is a trade, not a necessity. Every user of this chain is
/// cold: a per-thread cache fronts it and refills in batches, so it is
/// touched only on a miss or an overflow flush. Measured live, the lock
/// costs nothing on either benchmark, and it removes the race and the
/// ABA tag together rather than adding a layout invariant to a union
/// five modules already share. Revisit if this path ever stops being
/// cold.
struct FreeList {
    head: *mut BlockHeader,
}

// SAFETY: the pointers are block headers in never-unmapped pool regions;
// the mutex is what serialises access to the chain.
unsafe impl Send for FreeList {}

pub struct BlockPool {
    free: Mutex<FreeList>,
    regions_carved: AtomicUsize,
    /// Blocks handed out minus blocks returned — block-granular
    /// occupancy. Bumped only on the (rare) block operations, never on
    /// object allocation: the telemetry design taxes no hot path.
    blocks_out: AtomicUsize,
    /// Region registry: the base address of every 2 MB region ever
    /// carved. The pool used to count regions without recording their
    /// bases, so nothing could enumerate blocks — which the entity
    /// walker must (`rfc/model/gc/rc-walk.md`, "a region registry").
    /// Append-only and regions are never unmapped (phase 1), so an index
    /// is a stable handle for as long as the process lives. OS-direct
    /// `BLOCK_KIND_LARGE_RUN` allocations are not regions and are not
    /// here — huge objects stay outside the walk, conservatively.
    regions: Mutex<Vec<usize>>,
}

static GLOBAL_POOL: BlockPool = BlockPool {
    free: Mutex::new(FreeList {
        head: std::ptr::null_mut(),
    }),
    regions_carved: AtomicUsize::new(0),
    blocks_out: AtomicUsize::new(0),
    regions: Mutex::new(Vec::new()),
};

/// Serializes region carving only (rare path).
static CARVE_LOCK: Mutex<()> = Mutex::new(());

/// Makes [`BlockPool::get`] report exhaustion. Test-only, and the only
/// way to reach the out-of-memory paths deliberately.
#[cfg(test)]
pub(crate) static FORCE_OOM: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

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

    /// Blocks currently out of the pool (arena, heap, buffer,
    /// immortal, large — every consumer). Block granularity: the whole
    /// point is that no per-object path pays for stats.
    pub fn blocks_out(&self) -> usize {
        self.blocks_out.load(Ordering::Relaxed)
    }

    /// Snapshot of the region registry: every 2 MB region base, in carve
    /// order. Indices are stable handles (append-only, never unmapped).
    /// Cold: a clone of a handful of words under the registry lock.
    pub fn regions(&self) -> Vec<*mut u8> {
        self.regions
            .lock()
            .unwrap()
            .iter()
            .map(|&base| base as *mut u8)
            .collect()
    }

    /// Get a free block: thread cache → global stack → carve a region.
    ///
    /// **Returns null when the OS refuses memory.** Every caller must
    /// handle it; nothing on this path aborts. That is the one contract
    /// for exhaustion across the whole manager — the huge-allocation path
    /// in `stdapi` has always returned null, and this used to abort, so a
    /// C caller got null for 200 KB and a dead process for 40 bytes.
    ///
    /// `try_with`, not `with`: this can run from a TLS destructor (a thread
    /// giving its heap blocks back on the way out), by which point this
    /// module's own `thread_local!` may already have been destroyed. TLS
    /// destructor order is not defined, so the cache has to be optional
    /// rather than assumed — `with` panics there, and panicking while a
    /// thread unwinds is not a real option.
    pub fn get(&self) -> *mut BlockHeader {
        // Fault injection, tests only: the OS refusing memory cannot be
        // provoked on demand, and an untested failure path is a guess.
        #[cfg(test)]
        if FORCE_OOM.load(Ordering::Relaxed) {
            return std::ptr::null_mut();
        }

        self.blocks_out.fetch_add(1, Ordering::Relaxed);
        let cached = THREAD_CACHE
            .try_with(|c| c.borrow_mut().blocks.pop())
            .unwrap_or(None);
        if let Some(block) = cached {
            return block;
        }

        // Refill the cache in a batch, return one. A refused region stops
        // the loop rather than spinning on it — see `carve_region`.
        let mut batch = Vec::with_capacity(REFILL_BATCH);
        while batch.len() < REFILL_BATCH {
            match self.pop_global() {
                Some(b) => batch.push(b),
                None => {
                    if !self.carve_region() {
                        break;
                    }
                }
            }
        }
        let Some(block) = batch.pop() else {
            // Nothing anywhere: undo the optimistic count and report.
            self.blocks_out.fetch_sub(1, Ordering::Relaxed);
            return std::ptr::null_mut();
        };
        if THREAD_CACHE
            .try_with(|c| c.borrow_mut().blocks.append(&mut batch))
            .is_err()
        {
            // No cache to put them in (see `get`'s note) — hand them back.
            for b in batch {
                self.push_global(b);
            }
        }
        block
    }

    /// Return a block. Overflowing the thread cache flushes half of it
    /// to the global stack (jemalloc flush pattern).
    ///
    /// `block` must be a block previously handed out by [`get`]; the
    /// pool is its sole authority, so this internal API stays safe.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn put(&self, block: *mut BlockHeader) {
        // A retained block carries promoted survivors, and it comes back
        // only through `stdapi::ll_free`'s retained arm, which restamps it
        // after `retained::occupant_freed` reports the last live occupant
        // gone. Arriving here still stamped retained therefore means a
        // caller took a shortcut past that arm, with survivors possibly
        // alive — not a step of a defect, the defect itself, and catching
        // it here names the thread and the moment. Planted while hunting
        // the loaded-box flake in `census_counts_objects_and_their_edges`,
        // where a walk sees a block that should not be reachable.
        debug_assert_ne!(
            unsafe { (*block).kind },
            BLOCK_KIND_RETAINED,
            "a retained block went back to the pool with its survivors alive"
        );
        self.blocks_out.fetch_sub(1, Ordering::Relaxed);
        unsafe { store_block_kind(&raw mut (*block).kind, BLOCK_KIND_FREE) };

        // `try_with` for the same reason as `get`: this runs from a TLS
        // destructor on the thread-exit path, where the cache may be gone.
        let cached = THREAD_CACHE.try_with(|c| {
            let mut cache = c.borrow_mut();
            cache.blocks.push(block);

            if cache.blocks.len() > THREAD_CACHE_CAPACITY {
                let keep = THREAD_CACHE_CAPACITY / 2;
                for b in cache.blocks.drain(keep..) {
                    self.push_global(b);
                }
            }
        });
        if cached.is_err() {
            self.push_global(block);
        }
    }

    fn push_global(&self, block: *mut BlockHeader) {
        let mut free = self.free.lock().unwrap();
        unsafe { (*block).next = free.head };
        free.head = block;
    }

    fn pop_global(&self) -> Option<*mut BlockHeader> {
        let mut free = self.free.lock().unwrap();
        let head = free.head;
        if head.is_null() {
            return None;
        }
        // Reading `next` under the lock is the whole point: no other
        // thread can be holding this block, so nobody is writing these
        // bytes through another view of the header.
        free.head = unsafe { (*head).next };
        Some(head)
    }

    /// Reserve a 2 MB region from the OS and stack its `BLOCKS_PER_REGION` blocks.
    /// Returns false if the OS refused the region — the caller must not
    /// spin. Out of memory is a condition to **report**, not to abort on:
    /// a request that cannot get memory is the request's problem, and the
    /// worker goes on to serve the next one. The abort that used to be
    /// here also disagreed with the huge-allocation path, which has always
    /// returned null.
    fn carve_region(&self) -> bool {
        let _guard = CARVE_LOCK.lock().unwrap();

        // Someone may have carved while we waited for the lock. Take the
        // free-list lock only to peek, and drop it before pushing below —
        // `push_global` takes it itself.
        if !self.free.lock().unwrap().head.is_null() {
            return true;
        }

        let layout = Layout::from_size_align(REGION_SIZE, BLOCK_SIZE).unwrap();
        let region = unsafe { alloc(layout) };
        if region.is_null() {
            return false;
        }
        // Register before any block is handed out: the walker may
        // enumerate the registry at any time, and a block it cannot map
        // back to a region would be invisible to the census.
        self.regions.lock().unwrap().push(region as usize);

        for i in 0..BLOCKS_PER_REGION {
            let block = unsafe { region.add(i * BLOCK_SIZE) } as *mut BlockHeader;
            unsafe {
                store_block_kind(&raw mut (*block).kind, BLOCK_KIND_FREE);
                (*block).reserved = 0;
                (*block).next = std::ptr::null_mut();
            }
            self.push_global(block);
        }

        self.regions_carved.fetch_add(1, Ordering::Relaxed);
        true
    }
}

/// Serializes memory tests that touch the process-global block pool.
/// Tests share one global pool, so any test asserting on it (region
/// counts, specific block reuse, the global free stack) must not run
/// while another test carves or drains concurrently. Every memory unit
/// test holds this for its duration. `into_inner` recovers a poisoned
/// lock so one panicking test doesn't cascade.
#[cfg(test)]
pub(crate) fn test_guard() -> TestGuard {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // Tests are just another embedder of the C ABI's explicit-init
    // contract (see `heap::ll_thread_init`); idempotent per thread, so
    // folding it into the shared test fixture beats patching every test
    // that happens to allocate a `GcHeap`/`LongLived` object.
    //
    // Lock first, initialize second. Init takes blocks — the heap's own,
    // and the barrier reserve's — so doing it before the lock lets a
    // thread queued on the lock move the global block count under the
    // test that is currently running and counting.
    let guard = TestGuard(LOCK.lock().unwrap_or_else(|e| e.into_inner()));
    crate::memory::heap::ll_thread_init();
    guard
}

/// Holds the test lock and, on drop, releases this thread's heap *while
/// still holding it*. Without this the heap goes back via the TLS
/// destructor after the test body released the lock, so its
/// `BlockPool::put`s mutate `blocks_out` under a later test's feet —
/// the stats-test flake recorded in PLAN.md.
#[cfg(test)]
pub(crate) struct TestGuard(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

#[cfg(test)]
impl Drop for TestGuard {
    fn drop(&mut self) {
        // Runs before the mutex guard field is released: the thread's
        // blocks return to the pool inside the serialized section. The
        // barrier reserve is spare memory held by the same thread, and
        // block-accounting tests count what is out — so it goes back
        // here too, rather than sitting on two blocks per test thread.
        crate::memory::reserve::drain_for_test();
        crate::memory::heap::ll_thread_exit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_are_aligned_and_mask_finds_header() {
        let _g = test_guard();
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
    fn region_registry_covers_every_pooled_block() {
        let _g = test_guard();
        let pool = BlockPool::global();
        let block = pool.get();

        let regions = pool.regions();
        assert!(!regions.is_empty(), "carving must register the region");
        let covered = regions.iter().any(|&r| {
            let base = r as usize;
            (block as usize) >= base && (block as usize) < base + REGION_SIZE
        });
        assert!(
            covered,
            "a pooled block must fall inside a registered region"
        );

        pool.put(block);
    }

    #[test]
    fn put_then_get_reuses_without_new_region() {
        let _g = test_guard();
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
        let _g = test_guard();
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
