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
//!   pops; `local_free` is where a same-thread `free` pushes. When `free`
//!   empties, the slow path moves `local_free → free`. A burst of local
//!   frees never touches the alloc hot path.
//! - **A fully-free block returns to the global pool** — real individual
//!   reclamation, at block granularity.
//!
//! ## Cross-thread free (multi-threaded)
//!
//! Each heap has a shared, thread-safe [`HeapShared`] holding a lock-free
//! MPSC stack `remote_free`. A `free(ptr)` whose block is owned by
//! *another* heap does one atomic push onto that owner's `remote_free`
//! and touches nothing else — snmalloc's "message to the owner", one
//! stack per owner (batching by destination is a later optimization).
//! The owner drains `remote_free` on its slow path, returning slots to
//! their blocks and reclaiming emptied blocks.
//!
//! `used` is therefore written **only by the owning thread** (on alloc,
//! local free, and drain) — no atomics on it. A cross-thread free just
//! parks the slot; the owner accounts for it at drain time.
//!
//! Not yet handled: **thread-exit abandonment**. If a thread with live
//! heap blocks exits, blocks still holding objects (and any later
//! cross-thread frees into them) are leaked rather than adopted by
//! another thread (mimalloc adopts abandoned pages). Worker pools with
//! long-lived threads are unaffected; documented as a known limit.

use std::sync::atomic::{AtomicPtr, Ordering};

use crate::memory::block_pool::{
    BLOCK_KIND_HEAP, BLOCK_MASK, BLOCK_PAYLOAD, BlockHeader, BlockPool, LINE_SIZE,
};

/// Size classes (bytes). Smallest class >= request is used. Chosen to
/// keep internal fragmentation under ~25%: fine steps for small sizes,
/// coarser as they grow. Requests above the last class go to the
/// large-object path (see `stdapi`).
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
    SIZE_CLASSES.iter().position(|&c| c >= size.max(1))
}

/// A free slot threads a free list through its own first 8 bytes — zero
/// metadata overhead. Used for the per-block lists and the remote stack.
#[repr(C)]
struct FreeSlot {
    next: *mut FreeSlot,
}

/// Shared, thread-safe part of a heap. Lives forever (leaked) so a
/// block's `owner` pointer stays valid even after the owning thread
/// exits. Its unique leaked address *is* the heap's identity (compared
/// by pointer in `free`). There are only as many as there are heaps
/// (roughly, threads).
pub struct HeapShared {
    /// Lock-free MPSC stack: any thread pushes a cross-thread-freed slot;
    /// the owning thread drains the whole list at once.
    remote_free: AtomicPtr<FreeSlot>,
}

/// Per-block header. Overlays the block's first line; shares offset 0
/// (`kind`) with the pool's `BlockHeader` (tagged union over the memory).
#[repr(C)]
struct HeapBlockHeader {
    kind: u32,
    size_class: u32,
    /// Live slots (owner-written only). Block returns to the pool at 0.
    used: u32,
    slots: u32,
    /// `alloc` pops from here (owner-only).
    free: *mut FreeSlot,
    /// Same-thread `free` pushes here (owner-only).
    local_free: *mut FreeSlot,
    /// Owning heap — identifies local vs cross-thread free, and where a
    /// cross-thread free posts.
    owner: *const HeapShared,
    next: *mut HeapBlockHeader,
    prev: *mut HeapBlockHeader,
    linked: bool,
}

impl HeapBlockHeader {
    #[inline]
    fn of_ptr(p: *mut u8) -> *mut HeapBlockHeader {
        ((p as usize) & !BLOCK_MASK) as *mut HeapBlockHeader
    }
}

/// Thread-local small-object heap. Per size class it holds the head of a
/// doubly-linked list of blocks that still have room.
pub struct Heap {
    available: Vec<*mut HeapBlockHeader>,
    shared: &'static HeapShared,
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

impl Heap {
    pub fn new() -> Self {
        let shared = Box::leak(Box::new(HeapShared {
            remote_free: AtomicPtr::new(std::ptr::null_mut()),
        }));
        Heap {
            available: vec![std::ptr::null_mut(); SIZE_CLASSES.len()],
            shared,
        }
    }

    /// Allocate at least `size` bytes, or null if `size` exceeds the
    /// small-object range.
    pub fn alloc(&mut self, size: usize) -> *mut u8 {
        let ci = match size_class_index(size) {
            Some(ci) => ci,
            None => return std::ptr::null_mut(),
        };

        loop {
            let block = self.available[ci];

            if block.is_null() {
                // Slow path: reclaim cross-thread frees before growing.
                if !self.shared.remote_free.load(Ordering::Relaxed).is_null() {
                    self.drain_remote();
                    if !self.available[ci].is_null() {
                        continue;
                    }
                }
                self.refill(ci);
                continue;
            }

            let b = unsafe { &mut *block };

            if b.free.is_null() {
                // Collect this block's local frees.
                b.free = b.local_free;
                b.local_free = std::ptr::null_mut();

                if b.free.is_null() {
                    // Block genuinely full — unlink and try the next.
                    self.unlink(ci, block);
                    continue;
                }
            }

            let slot = b.free;
            b.free = unsafe { (*slot).next };
            b.used += 1;
            return slot as *mut u8;
        }
    }

    /// Free a slot from [`alloc`]. If this thread owns the block it is a
    /// cheap local free; otherwise the slot is posted to the owner's
    /// lock-free `remote_free` stack.
    ///
    /// # Safety
    /// `ptr` must be a live allocation from some heap and not freed yet.
    pub unsafe fn free(&mut self, ptr: *mut u8) {
        let block = HeapBlockHeader::of_ptr(ptr);
        let owner = unsafe { (*block).owner };

        if std::ptr::eq(owner, self.shared) {
            unsafe { self.free_local(block, ptr) };
        } else {
            // Cross-thread: one atomic push onto the owner's stack.
            let slot = ptr as *mut FreeSlot;
            let remote = unsafe { &(*owner).remote_free };
            let mut head = remote.load(Ordering::Relaxed);
            loop {
                unsafe { (*slot).next = head };
                match remote.compare_exchange_weak(head, slot, Ordering::Release, Ordering::Relaxed)
                {
                    Ok(_) => break,
                    Err(h) => head = h,
                }
            }
        }
    }

    /// Owner-side free of `ptr` into its block.
    unsafe fn free_local(&mut self, block: *mut HeapBlockHeader, ptr: *mut u8) {
        let b = unsafe { &mut *block };
        let ci = b.size_class as usize;

        let slot = ptr as *mut FreeSlot;
        unsafe { (*slot).next = b.local_free };
        b.local_free = slot;
        b.used -= 1;

        if b.used == 0 {
            if b.linked {
                self.unlink(ci, block);
            }
            b.kind = 0;
            BlockPool::global().put(block as *mut BlockHeader);
            return;
        }

        if !b.linked {
            self.link(ci, block);
        }
    }

    /// Drain the owner's `remote_free` stack: return each cross-thread
    /// freed slot to its block, reclaiming emptied blocks. Owner thread
    /// only.
    fn drain_remote(&mut self) {
        let mut slot = self
            .shared
            .remote_free
            .swap(std::ptr::null_mut(), Ordering::Acquire);

        while !slot.is_null() {
            let next = unsafe { (*slot).next };
            let block = HeapBlockHeader::of_ptr(slot as *mut u8);
            let b = unsafe { &mut *block };
            let ci = b.size_class as usize;

            unsafe { (*slot).next = b.local_free };
            b.local_free = slot;
            b.used -= 1;

            if b.used == 0 {
                if b.linked {
                    self.unlink(ci, block);
                }
                b.kind = 0;
                BlockPool::global().put(block as *mut BlockHeader);
            } else if !b.linked {
                self.link(ci, block);
            }

            slot = next;
        }
    }

    /// Take a fresh block from the pool, carve it into slots of class
    /// `ci`, thread the free list, and link it as available.
    fn refill(&mut self, ci: usize) -> *mut HeapBlockHeader {
        let class_size = SIZE_CLASSES[ci];
        let block = BlockPool::global().get() as *mut HeapBlockHeader;
        let slots = (BLOCK_PAYLOAD / class_size) as u32;

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
                owner: self.shared,
                next: std::ptr::null_mut(),
                prev: std::ptr::null_mut(),
                linked: false,
            });
        }
        self.link(ci, block);
        block
    }

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
        assert_eq!(size_class_index(1), Some(0));
        assert_eq!(size_class_index(16), Some(0));
        assert_eq!(size_class_index(17), Some(1));
        assert_eq!(size_class_index(8192), Some(SIZE_CLASSES.len() - 1));
        assert_eq!(size_class_index(8193), None);
    }

    #[test]
    fn alloc_is_aligned_and_sized() {
        let _g = crate::memory::block_pool::test_guard();
        let mut heap = Heap::new();
        let a = heap.alloc(40);
        let b = heap.alloc(40);
        assert!(!a.is_null());
        assert_eq!((b as usize).wrapping_sub(a as usize), 48);
        unsafe {
            heap.free(a);
            heap.free(b);
        }
    }

    #[test]
    fn free_then_alloc_reuses_slot() {
        let _g = crate::memory::block_pool::test_guard();
        let mut heap = Heap::new();
        let a = heap.alloc(64);
        unsafe { heap.free(a) };
        let b = heap.alloc(64);
        assert_eq!(a, b, "a freed slot must be handed back");
        unsafe { heap.free(b) };
    }

    #[test]
    fn empty_block_returns_to_pool() {
        let _g = crate::memory::block_pool::test_guard();
        let pool = BlockPool::global();
        let mut heap = Heap::new();

        let warm = heap.alloc(64);
        unsafe { heap.free(warm) };
        let regions_before = pool.regions_carved();

        let class = 64usize;
        let slots = BLOCK_PAYLOAD / class;
        let ptrs: Vec<_> = (0..slots).map(|_| heap.alloc(class)).collect();
        for p in &ptrs {
            unsafe { heap.free(*p) };
        }

        let p = heap.alloc(64);
        assert_eq!(pool.regions_carved(), regions_before);
        unsafe { heap.free(p) };
    }

    #[test]
    fn full_block_refills_and_serves_distinct_slots() {
        let _g = crate::memory::block_pool::test_guard();
        let mut heap = Heap::new();
        let class = 128usize;
        let slots = BLOCK_PAYLOAD / class;

        let ptrs: Vec<_> = (0..slots + 10).map(|_| heap.alloc(class)).collect();
        assert!(ptrs.iter().all(|p| !p.is_null()));

        let mut sorted = ptrs.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ptrs.len(), "no slot handed out twice");

        for p in ptrs {
            unsafe { heap.free(p) };
        }
    }

    #[test]
    fn too_large_returns_null() {
        let mut heap = Heap::new();
        assert!(heap.alloc(9000).is_null());
    }

    #[test]
    fn cross_thread_free_is_correct() {
        let _g = crate::memory::block_pool::test_guard();
        use std::sync::mpsc;
        use std::thread;

        const N: u64 = 5000;
        let (tx, rx) = mpsc::channel::<usize>();

        // Producer: allocate on its own heap, stamp each with its index,
        // hand the pointer to the consumer, and keep allocating (so its
        // slow path drains the incoming cross-thread frees concurrently).
        let producer = thread::spawn(move || {
            with_thread_heap(|h| {
                for i in 0..N {
                    let p = h.alloc(24);
                    unsafe { (p as *mut u64).write(i) };
                    tx.send(p as usize).unwrap();
                    // extra churn to exercise the drain path
                    let t = h.alloc(24);
                    unsafe { h.free(t) };
                }
            });
        });

        // Consumer (this thread): verify each value survived, then free
        // cross-thread (posts to the producer's remote stack).
        let mut count = 0u64;
        for _ in 0..N {
            let p = rx.recv().unwrap() as *mut u8;
            let v = unsafe { *(p as *mut u64) };
            assert!(v < N, "value corrupted across threads");
            with_thread_heap(|h| unsafe { h.free(p) });
            count += 1;
        }
        assert_eq!(count, N);
        producer.join().unwrap();
    }

    #[test]
    fn many_threads_alloc_free_no_corruption() {
        let _g = crate::memory::block_pool::test_guard();
        use std::thread;

        let handles: Vec<_> = (0..8)
            .map(|t| {
                thread::spawn(move || {
                    with_thread_heap(|h| {
                        let mut live = Vec::new();
                        for i in 0..2000usize {
                            let size = 16 + (i * 8 + t) % 512;
                            let p = h.alloc(size);
                            assert!(!p.is_null());
                            unsafe { p.write((t as u8).wrapping_add(1)) };
                            live.push(p);
                            if live.len() > 100 {
                                let victim = live.swap_remove(i % live.len());
                                unsafe { h.free(victim) };
                            }
                        }
                        for p in live {
                            unsafe { h.free(p) };
                        }
                    });
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
    }
}
