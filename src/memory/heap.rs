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
    /// Live slots (owner-written only). Block returns to the pool at 0
    /// (subject to the empty-reserve cap — see `Heap::retire_empty`).
    used: u32,
    slots: u32,
    /// Slots at index `< bump` have been handed out at least once;
    /// `>= bump` are virgin — never written, not on any list. See
    /// `rfc/model/memory/heap-slot-allocation.md`.
    bump: u32,
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
    /// At most one retained-but-empty block per size class, kept ready
    /// for instant reuse instead of returning to `BlockPool` and
    /// re-carving on the very next allocation. See
    /// `rfc/model/memory/heap-slot-allocation.md`.
    empty_reserve: Vec<*mut HeapBlockHeader>,
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
            empty_reserve: vec![std::ptr::null_mut(); SIZE_CLASSES.len()],
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

            if !b.free.is_null() {
                let slot = b.free;
                b.free = unsafe { (*slot).next };
                self.claim(ci, block, b);
                return slot as *mut u8;
            }

            if !b.local_free.is_null() {
                b.free = b.local_free;
                b.local_free = std::ptr::null_mut();
                continue;
            }

            if b.bump < b.slots {
                let class_size = SIZE_CLASSES[ci];
                let base = (block as *mut u8).wrapping_add(LINE_SIZE);
                let slot = base.wrapping_add(b.bump as usize * class_size);
                b.bump += 1;
                self.claim(ci, block, b);
                return slot;
            }

            // Block genuinely full — unlink and try the next.
            self.unlink(ci, block);
        }
    }

    /// Account for a slot leaving this block (from either the free lists
    /// or a fresh bump-carve): clears the empty-reserve mark if this was
    /// the class's retained spare, then bumps `used`.
    #[inline]
    fn claim(&mut self, ci: usize, block: *mut HeapBlockHeader, b: &mut HeapBlockHeader) {
        if b.used == 0 && self.empty_reserve[ci] == block {
            self.empty_reserve[ci] = std::ptr::null_mut();
        }
        b.used += 1;
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
            self.retire_empty(ci, block);
            return;
        }

        if !b.linked {
            self.link(ci, block);
        }
    }

    /// Common tail once a block's `used` count has just reached zero:
    /// keep it as the class's one bounded empty spare (instant reuse, no
    /// refill) if there isn't one already; otherwise actually return it
    /// to the global pool. See `rfc/model/memory/heap-slot-allocation.md`.
    fn retire_empty(&mut self, ci: usize, block: *mut HeapBlockHeader) {
        if self.empty_reserve[ci].is_null() {
            self.empty_reserve[ci] = block;
            if unsafe { !(*block).linked } {
                self.link(ci, block);
            }
            return;
        }
        if unsafe { (*block).linked } {
            self.unlink(ci, block);
        }
        unsafe { (*block).kind = 0 };
        BlockPool::global().put(block as *mut BlockHeader);
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
                self.retire_empty(ci, block);
            } else if !b.linked {
                self.link(ci, block);
            }

            slot = next;
        }
    }

    /// Take a fresh block from the pool and link it as available. The
    /// free list is not pre-threaded — slots are carved lazily by
    /// `bump` as `alloc` needs them. See
    /// `rfc/model/memory/heap-slot-allocation.md`.
    fn refill(&mut self, ci: usize) -> *mut HeapBlockHeader {
        let class_size = SIZE_CLASSES[ci];
        let block = BlockPool::global().get() as *mut HeapBlockHeader;
        let slots = (BLOCK_PAYLOAD / class_size) as u32;

        unsafe {
            block.write(HeapBlockHeader {
                kind: BLOCK_KIND_HEAP,
                size_class: ci as u32,
                used: 0,
                slots,
                bump: 0,
                free: std::ptr::null_mut(),
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

// --- Thread-local heap slot -----------------------------------------------
//
// Disassembly of `ll_alloc`/`ll_free` (via `dumpbin /disasm`, see
// `rfc/model/memory/heap-slot-allocation.md`) showed the compiler-emitted
// `thread_local!`/`__declspec(thread)` access on windows-msvc costs THREE
// dependent, non-pipelineable loads: `_tls_index` -> `gs:[0x58]`
// (TEB.ThreadLocalStoragePointer) -> this module's TLS block -> the field.
// That indirection exists to let a DLL's TLS block be found generically;
// we don't need it, and it cost ~2.5-3 ns of the ~4 ns gap to mimalloc.
//
// mimalloc avoids it entirely, and *also* avoids calling the real Win32
// `TlsGetValue`/`TlsSetValue` (those are real, non-inlined function calls
// through the kernel32 import table — no cheaper than the module-indirected
// path once you count the call/ret and the callee's own TEB lookup).
// Instead it reads/writes the TEB's inline "TlsSlots" array directly:
// `gs:[0x1480 + slot*8]`, one instruction, via the MSVC `__readgsqword`
// intrinsic (`mimalloc/prim.h`, `MI_TLS_SLOT`). `TlsAlloc` is called once
// (process-wide) purely to reserve a slot number the OS promises not to
// hand to anyone else; the actual reads/writes never call it again. This
// mirrors that exactly, via inline `asm!`. Falls back to the real
// TlsGetValue/TlsSetValue API only if the reserved slot lands outside the
// first 64 "fast" slots (the inline array's fixed size) — practically
// never, since this is one of the first TLS allocations in the process,
// but correctness for the rare case matters more than the fast path here.
//
// elsewhere (ELF `__thread` is already a single `%fs`-relative load, no
// module table) the portable `thread_local!` stays as-is.

#[cfg(windows)]
mod tls {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::Heap;

    unsafe extern "system" {
        fn TlsAlloc() -> u32;
        fn TlsGetValue(dw_tls_index: u32) -> *mut core::ffi::c_void;
        fn TlsSetValue(dw_tls_index: u32, lp_tls_value: *mut core::ffi::c_void) -> i32;
    }

    /// Offset of `TEB.TlsSlots[0]` on x86_64, stable across Windows
    /// versions — the same constant mimalloc, Chromium, and the CRT rely
    /// on. <https://en.wikipedia.org/wiki/Win32_Thread_Information_Block>
    const TEB_TLS_SLOTS_OFFSET: u32 = 0x1480;
    /// Fast inline slots are `TlsSlots[0..64]`; beyond that, Windows
    /// spills to a separately-allocated `TlsExpansionSlots` array not
    /// reachable by this fixed-offset trick.
    const FAST_SLOT_COUNT: u32 = 64;

    const UNINIT: u32 = u32::MAX;
    /// Either a byte offset into `gs:` (fast path, slot < 64) or, in the
    /// rare fallback case, `FALLBACK_BIT | real_tls_index`.
    static STATE: AtomicU32 = AtomicU32::new(UNINIT);
    const FALLBACK_BIT: u32 = 1 << 31;

    #[inline]
    fn state() -> u32 {
        let s = STATE.load(Ordering::Relaxed);
        if s != UNINIT { s } else { init() }
    }

    #[cold]
    fn init() -> u32 {
        let slot = unsafe { TlsAlloc() };
        let computed = if slot < FAST_SLOT_COUNT {
            TEB_TLS_SLOTS_OFFSET + slot * 8
        } else {
            FALLBACK_BIT | slot
        };
        match STATE.compare_exchange(UNINIT, computed, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => computed,
            Err(existing) => existing,
        }
    }

    /// Single-instruction read of `gs:[offset]` — mirrors MSVC's
    /// `__readgsqword`, which Rust does not expose directly.
    ///
    /// # Safety
    /// `offset` must be a valid byte offset into the current thread's TEB
    /// (i.e. `< TEB_TLS_SLOTS_OFFSET + FAST_SLOT_COUNT * 8`).
    #[inline(always)]
    unsafe fn read_gs_qword(offset: u32) -> u64 {
        let val: u64;
        unsafe {
            core::arch::asm!(
                "mov {val}, gs:[{off:e}]",
                val = out(reg) val,
                off = in(reg) offset,
                options(nostack, preserves_flags),
            );
        }
        val
    }

    /// # Safety
    /// Same contract as [`read_gs_qword`].
    #[inline(always)]
    unsafe fn write_gs_qword(offset: u32, val: u64) {
        unsafe {
            core::arch::asm!(
                "mov gs:[{off:e}], {val}",
                off = in(reg) offset,
                val = in(reg) val,
                options(nostack, preserves_flags),
            );
        }
    }

    #[inline]
    pub fn get() -> *mut Heap {
        let s = state();
        if s & FALLBACK_BIT == 0 {
            unsafe { read_gs_qword(s) as *mut Heap }
        } else {
            unsafe { TlsGetValue(s & !FALLBACK_BIT) as *mut Heap }
        }
    }

    #[inline]
    pub fn set(p: *mut Heap) {
        let s = state();
        if s & FALLBACK_BIT == 0 {
            unsafe { write_gs_qword(s, p as u64) };
        } else {
            unsafe { TlsSetValue(s & !FALLBACK_BIT, p as *mut core::ffi::c_void) };
        }
    }
}

#[cfg(not(windows))]
mod tls {
    use std::cell::Cell;

    use super::Heap;

    thread_local! {
        static THREAD_HEAP: Cell<*mut Heap> = const { Cell::new(std::ptr::null_mut()) };
    }

    #[inline]
    pub fn get() -> *mut Heap {
        THREAD_HEAP.with(|c| c.get())
    }

    #[inline]
    pub fn set(p: *mut Heap) {
        THREAD_HEAP.with(|c| c.set(p));
    }
}

/// Eagerly create this thread's heap. Must be called once per thread
/// before any allocation on it — the hot path (`with_thread_heap`)
/// trusts this and does not check. Idempotent.
///
/// This is the deliberate split: initialization is a cold, explicit,
/// one-time call (like a worker thread's startup hook), not a check
/// repeated on every `malloc`/`free`. Limelight owns its own worker
/// threads (see module doc), so this is always satisfiable — unlike a
/// libc `malloc` replacement, which cannot demand callers opt in first.
#[unsafe(no_mangle)]
pub extern "C" fn ll_thread_init() {
    if tls::get().is_null() {
        tls::set(Box::into_raw(Box::new(Heap::new())));
    }
}

/// Run `f` with this thread's persistent small-object heap.
///
/// # Safety
/// [`ll_thread_init`] must have been called on this thread first. No
/// check is made — that is the point (see `ll_thread_init`'s doc).
pub unsafe fn with_thread_heap<R>(f: impl FnOnce(&mut Heap) -> R) -> R {
    let p = tls::get();
    debug_assert!(!p.is_null(), "ll_thread_init() was not called on this thread");
    f(unsafe { &mut *p })
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

    /// Regression test for the pathology found via a real `larson.cpp`
    /// benchmark run: alloc-then-immediately-free of one object,
    /// repeated, must reuse the retained block instead of returning it
    /// to `BlockPool` and re-carving on every single cycle (which cost
    /// ~140 ns/op instead of ~8 ns/op). See
    /// `rfc/model/memory/heap-slot-allocation.md`.
    #[test]
    fn single_live_slot_churn_does_not_recarve_block() {
        let _g = crate::memory::block_pool::test_guard();
        let pool = BlockPool::global();
        let mut heap = Heap::new();

        // Warm up: this alloc carves the one block we expect to be
        // retained for the rest of the test.
        let warm = heap.alloc(64);
        unsafe { heap.free(warm) };
        let blocks_out_before = pool.blocks_out();
        let regions_before = pool.regions_carved();

        for i in 0..10_000u32 {
            let p = heap.alloc(64);
            assert!(!p.is_null());
            unsafe { p.write(i as u8) };
            unsafe { heap.free(p) };
            assert_eq!(
                pool.blocks_out(),
                blocks_out_before,
                "block was returned to the pool and re-carved on iteration {i}"
            );
        }
        assert_eq!(pool.regions_carved(), regions_before);
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
            ll_thread_init();
            unsafe {
                with_thread_heap(|h| {
                    for i in 0..N {
                        let p = h.alloc(24);
                        (p as *mut u64).write(i);
                        tx.send(p as usize).unwrap();
                        // extra churn to exercise the drain path
                        let t = h.alloc(24);
                        h.free(t);
                    }
                });
            }
        });

        // Consumer (this thread): verify each value survived, then free
        // cross-thread (posts to the producer's remote stack).
        ll_thread_init();
        let mut count = 0u64;
        for _ in 0..N {
            let p = rx.recv().unwrap() as *mut u8;
            let v = unsafe { *(p as *mut u64) };
            assert!(v < N, "value corrupted across threads");
            unsafe { with_thread_heap(|h| h.free(p)) };
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
                    ll_thread_init();
                    unsafe {
                        with_thread_heap(|h| {
                            let mut live = Vec::new();
                            for i in 0..2000usize {
                                let size = 16 + (i * 8 + t) % 512;
                                let p = h.alloc(size);
                                assert!(!p.is_null());
                                p.write((t as u8).wrapping_add(1));
                                live.push(p);
                                if live.len() > 100 {
                                    let victim = live.swap_remove(i % live.len());
                                    h.free(victim);
                                }
                            }
                            for p in live {
                                h.free(p);
                            }
                        });
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
    }
}
