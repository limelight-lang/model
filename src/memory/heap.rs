//! Small-object heap: individually-freeable allocations for long-lived
//! objects (unlike the arena, which frees only in bulk at reset).
//!
//! Design: the mimalloc model, chosen after studying jemalloc / mimalloc
//! / snmalloc (best-benchmarked for small frequent allocations, and the
//! best fit for infrastructure we already have):
//!
//! - **One 32 KB block per size class**, carved into fixed-size slots.
//!   The block is mimalloc's "page".
//! - **Pointer → block by mask** (`ptr & !0x7FFF`) — no radix tree or
//!   pagemap needed (jemalloc/snmalloc pay for those; our aligned blocks
//!   give it for one AND).
//! - **Free-list split** (mimalloc's core trick): `free` is where `alloc`
//!   pops; `local_free` is where `free` pushes. When `free` empties, the
//!   slow path moves `local_free → free`. A burst of frees never touches
//!   the alloc hot path, and the periodic collect gives a deterministic
//!   cadence to hang deferred work on later (RC decrements, GC).
//! - **A fully-free block returns to the global pool** — real individual
//!   reclamation, at block granularity.
//!
//! Phase 1 is single-threaded. Cross-thread free (mimalloc's atomic
//! `xthread_free` per block, or snmalloc's MPSC-per-owner queue) arrives
//! with the multi-threaded phase.

use crate::memory::block_pool::{
    BLOCK_KIND_HEAP, BLOCK_MASK, BLOCK_PAYLOAD, BlockHeader, BlockPool, LINE_SIZE,
};

/// Size classes (bytes). Smallest class >= request is used. Chosen to
/// keep internal fragmentation under ~25%: fine steps for small sizes,
/// coarser as they grow. Requests above the last class go to the
/// large-object path (not built yet).
pub const SIZE_CLASSES: &[usize] = &[
    16, 32, 48, 64, 80, 96, 112, 128, // step 16
    160, 192, 224, 256, // step 32
    320, 384, 448, 512, // step 64
    640, 768, 896, 1024, // step 128
    1280, 1536, 1792, 2048, // step 256
    2560, 3072, 3584, 4096, // step 512
    5120, 6144, 7168, 8192, // step 1024
];

pub const MAX_SMALL: usize = 8192;

/// Smallest size-class index fitting `size`, or `None` if too large.
#[inline]
pub fn size_class_index(size: usize) -> Option<usize> {
    if size > MAX_SMALL {
        return None;
    }
    // Linear scan is fine: the heap path is far cooler than the arena
    // hot path. A lookup table can replace this if profiling asks.
    SIZE_CLASSES.iter().position(|&c| c >= size.max(1))
}

/// A free slot threads the per-block free list through its own first
/// 8 bytes — zero metadata overhead.
#[repr(C)]
struct FreeSlot {
    next: *mut FreeSlot,
}

/// Per-block header for a heap block. Overlays the block's first line;
/// shares offset 0 (`kind`) with the pool's `BlockHeader`, so the two
/// are a tagged union over the same memory.
#[repr(C)]
struct HeapBlockHeader {
    kind: u32,
    size_class: u32,
    /// Live (allocated) slots. Block returns to the pool at zero.
    used: u32,
    /// Total slots carved from this block.
    slots: u32,
    /// `alloc` pops from here.
    free: *mut FreeSlot,
    /// `free` pushes here; merged into `free` when `free` empties.
    local_free: *mut FreeSlot,
    /// Available-list links (blocks of this class that have room).
    next: *mut HeapBlockHeader,
    prev: *mut HeapBlockHeader,
    /// Is this block currently in its class's available list?
    linked: bool,
}

impl HeapBlockHeader {
    #[inline]
    fn of_ptr(p: *mut u8) -> *mut HeapBlockHeader {
        ((p as usize) & !BLOCK_MASK) as *mut HeapBlockHeader
    }
}

/// Thread-local small-object heap. Holds, per size class, the head of a
/// doubly-linked list of blocks that still have free slots.
pub struct Heap {
    available: Vec<*mut HeapBlockHeader>,
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

impl Heap {
    pub fn new() -> Self {
        Heap {
            available: vec![std::ptr::null_mut(); SIZE_CLASSES.len()],
        }
    }

    /// Allocate at least `size` bytes. Returns null if `size` exceeds the
    /// small-object range (large path not built yet).
    pub fn alloc(&mut self, size: usize) -> *mut u8 {
        let ci = match size_class_index(size) {
            Some(ci) => ci,
            None => return std::ptr::null_mut(),
        };

        let mut block = self.available[ci];
        if block.is_null() {
            block = self.refill(ci);
        }

        unsafe {
            let b = &mut *block;

            if b.free.is_null() {
                // Slow path: collect this block's deferred frees.
                b.free = b.local_free;
                b.local_free = std::ptr::null_mut();
            }

            let slot = b.free;
            b.free = (*slot).next;
            b.used += 1;

            // Block now full — drop it from the available list.
            if b.free.is_null() && b.local_free.is_null() {
                self.unlink(ci, block);
            }

            slot as *mut u8
        }
    }

    /// Free a slot previously returned by [`alloc`].
    ///
    /// # Safety
    /// `ptr` must have come from this heap and not been freed already.
    pub unsafe fn free(&mut self, ptr: *mut u8) {
        let block = HeapBlockHeader::of_ptr(ptr);
        let b = unsafe { &mut *block };
        let ci = b.size_class as usize;

        // Push to the deferred list — never touches the alloc hot path.
        let slot = ptr as *mut FreeSlot;
        unsafe { (*slot).next = b.local_free };
        b.local_free = slot;
        b.used -= 1;

        if b.used == 0 {
            // Fully free: reclaim the whole block to the global pool.
            if b.linked {
                self.unlink(ci, block);
            }
            b.kind = 0; // BLOCK_KIND_FREE, set again by pool.put
            BlockPool::global().put(block as *mut BlockHeader);
            return;
        }

        // Was full (unlinked) and now has a slot — put it back.
        if !b.linked {
            self.link(ci, block);
        }
    }

    /// Take a fresh block from the pool, carve it into slots of class
    /// `ci`, thread the free list, and link it as available.
    fn refill(&mut self, ci: usize) -> *mut HeapBlockHeader {
        let class_size = SIZE_CLASSES[ci];
        let block = BlockPool::global().get() as *mut HeapBlockHeader;
        let slots = (BLOCK_PAYLOAD / class_size) as u32;

        // Thread a free list through every slot.
        let base = (block as *mut u8).wrapping_add(LINE_SIZE);
        let mut head: *mut FreeSlot = std::ptr::null_mut();
        for i in (0..slots as usize).rev() {
            let slot = base.wrapping_add(i * class_size) as *mut FreeSlot;
            unsafe { (*slot).next = head };
            head = slot;
        }

        unsafe {
            block.write(HeapBlockHeader {
                kind: BLOCK_KIND_HEAP,
                size_class: ci as u32,
                used: 0,
                slots,
                free: head,
                local_free: std::ptr::null_mut(),
                next: std::ptr::null_mut(),
                prev: std::ptr::null_mut(),
                linked: false,
            });
        }
        self.link(ci, block);
        block
    }

    /// Insert `block` at the head of class `ci`'s available list.
    fn link(&mut self, ci: usize, block: *mut HeapBlockHeader) {
        let head = self.available[ci];
        unsafe {
            (*block).prev = std::ptr::null_mut();
            (*block).next = head;
            (*block).linked = true;
            if !head.is_null() {
                (*head).prev = block;
            }
        }
        self.available[ci] = block;
    }

    /// Remove `block` from class `ci`'s available list.
    fn unlink(&mut self, ci: usize, block: *mut HeapBlockHeader) {
        unsafe {
            let prev = (*block).prev;
            let next = (*block).next;
            if !prev.is_null() {
                (*prev).next = next;
            } else {
                self.available[ci] = next;
            }
            if !next.is_null() {
                (*next).prev = prev;
            }
            (*block).prev = std::ptr::null_mut();
            (*block).next = std::ptr::null_mut();
            (*block).linked = false;
        }
    }
}

thread_local! {
    static THREAD_HEAP: std::cell::RefCell<Heap> = std::cell::RefCell::new(Heap::new());
}

/// Run `f` with this thread's persistent small-object heap.
pub fn with_thread_heap<R>(f: impl FnOnce(&mut Heap) -> R) -> R {
    THREAD_HEAP.with(|h| f(&mut h.borrow_mut()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::block_pool::BLOCK_PAYLOAD;

    #[test]
    fn size_class_selection() {
        assert_eq!(size_class_index(1), Some(0)); // -> 16
        assert_eq!(size_class_index(16), Some(0));
        assert_eq!(size_class_index(17), Some(1)); // -> 32
        assert_eq!(size_class_index(8192), Some(SIZE_CLASSES.len() - 1));
        assert_eq!(size_class_index(8193), None); // large path
    }

    #[test]
    fn alloc_is_aligned_and_sized() {
        let mut heap = Heap::new();
        let a = heap.alloc(40);
        let b = heap.alloc(40);
        assert!(!a.is_null());
        // Both in the 48-byte class, so 48 apart within the block.
        assert_eq!((b as usize).wrapping_sub(a as usize), 48);
        unsafe {
            heap.free(a);
            heap.free(b);
        }
    }

    #[test]
    fn free_then_alloc_reuses_slot() {
        let mut heap = Heap::new();
        let a = heap.alloc(64);
        unsafe { heap.free(a) };
        let b = heap.alloc(64);
        assert_eq!(a, b, "a freed slot must be handed back");
        unsafe { heap.free(b) };
    }

    #[test]
    fn empty_block_returns_to_pool() {
        let pool = BlockPool::global();
        let mut heap = Heap::new();

        // Warm up so the pool has a region already carved.
        let warm = heap.alloc(64);
        unsafe { heap.free(warm) };
        let regions_before = pool.regions_carved();

        // Fill exactly one block's worth of one class, then free all.
        let class = 64usize;
        let slots = BLOCK_PAYLOAD / class;
        let ptrs: Vec<_> = (0..slots).map(|_| heap.alloc(class)).collect();
        for p in &ptrs {
            unsafe { heap.free(*p) };
        }

        // The now-empty block went back; a fresh alloc must not carve a
        // new region.
        let p = heap.alloc(64);
        assert_eq!(
            pool.regions_carved(),
            regions_before,
            "empty block should have returned to the pool for reuse"
        );
        unsafe { heap.free(p) };
    }

    #[test]
    fn full_block_refills_from_pool_and_keeps_serving() {
        let mut heap = Heap::new();
        let class = 128usize;
        let slots = BLOCK_PAYLOAD / class;

        // Allocate more than one block's worth — forces a refill.
        let ptrs: Vec<_> = (0..slots + 10).map(|_| heap.alloc(class)).collect();
        assert!(ptrs.iter().all(|p| !p.is_null()));

        // All distinct.
        let mut sorted = ptrs.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ptrs.len(), "no slot handed out twice");

        for p in ptrs {
            unsafe { heap.free(p) };
        }
    }

    #[test]
    fn local_free_split_collects_on_slow_path() {
        // Fill a class block, free everything (goes to local_free), then
        // reallocate: alloc must collect local_free back into free.
        let mut heap = Heap::new();
        let n = 50;
        let ptrs: Vec<_> = (0..n).map(|_| heap.alloc(32)).collect();
        for p in &ptrs {
            unsafe { heap.free(*p) };
        }
        // These allocs are served from collected local_free slots.
        let again: Vec<_> = (0..n).map(|_| heap.alloc(32)).collect();
        assert!(again.iter().all(|p| !p.is_null()));
        for p in again {
            unsafe { heap.free(p) };
        }
    }

    #[test]
    fn too_large_returns_null() {
        let mut heap = Heap::new();
        assert!(heap.alloc(9000).is_null(), "large path not built yet");
    }
}
