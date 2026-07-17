//! Small-object heap: individually-freeable allocations for long-lived
//! objects (unlike the arena, which frees only in bulk at reset).
//!
//! Design: the mimalloc model, chosen after studying jemalloc / mimalloc
//! / snmalloc (best-benchmarked for small frequent allocations, and the
//! best fit for infrastructure we already have):
//!
//! - **One 64 KB block per size class**, carved into fixed-size slots.
//!   The block is mimalloc's "page".
//! - **Pointer → block by mask** (`ptr & !BLOCK_MASK`) — no radix tree or
//!   pagemap needed (jemalloc/snmalloc pay for those; our aligned blocks
//!   give it for one AND).
//! - **Intrusive free list per block, plus a bump cursor for virgin
//!   slots** — mimalloc's `page->free`/extend scheme. `alloc` pops the
//!   list head (or bump-carves if the list is empty), `free` pushes the
//!   slot back. Both are O(1) and branch-only.
//!
//!   A bitmap was tried instead (one bit per slot, `tzcnt` to find a free
//!   one) and **lost**: it needs a side allocation per block, so every
//!   alloc pays a second dependent load into a line nothing else touches,
//!   answering "is this block full?" costs a scan of every word, and
//!   `free` needs `(ptr - base) / class_size` — a real integer division,
//!   since `class_size` isn't a power of two for most classes. The free
//!   list has none of those: its `next` lives *inside* the slot, which is
//!   the very memory the caller is about to write (on alloc) or has just
//!   stopped using (on free), so it rides a line that is already hot and
//!   needs no index arithmetic at all. Measured +18-20% on a real
//!   `larson.cpp` through the C ABI. See
//!   `rfc/model/memory/heap-slot-allocation.md` ("Fix 5") — including why
//!   the benchmark that originally chose the bitmap was not measuring what
//!   it claimed.
//! - **A fully-free block returns to the global pool** — real individual
//!   reclamation, at block granularity (subject to the bounded
//!   empty-block retention below).
//!
//! ## Cross-thread free (multi-threaded)
//!
//! Each heap has a shared, thread-safe [`HeapShared`] holding a lock-free
//! MPSC stack `remote_free`. A `free(ptr)` whose block is owned by
//! *another* heap does one atomic push onto that owner's `remote_free`
//! and touches nothing else — snmalloc's "message to the owner", one
//! stack per owner. The owner drains `remote_free` on its slow path,
//! returning slots to their blocks and reclaiming emptied blocks.
//!
//! One stack per owner is **not** sharded per page the way mimalloc's
//! `xthread_free` is, and that used to be recorded here and in
//! `benches/RESULTS.md` as a known weakness worth ~15% on the cross-thread
//! pattern, with per-destination batching as the planned fix. That was
//! wrong about the cause: this path is now ~40% *ahead* of mimalloc on the
//! same benchmark with the single stack untouched — what moved was the work
//! the *drain* does per slot, not the contention on the stack. Do not build
//! the sharding until a measurement asks for it. See
//! `rfc/model/memory/heap-slot-allocation.md` ("Fix 6").
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

/// Number of size classes. Named so the per-class tables in [`Heap`] can be
/// fixed-size arrays stored inline rather than `Vec`s: a `Vec` puts the
/// table behind a pointer (load the pointer, load `len` to bounds-check,
/// then load the element) on a path that runs on every allocation, where an
/// inline array is one load against a compile-time-constant bound.
pub const NUM_CLASSES: usize = SIZE_CLASSES.len();

/// Direct lookup table at 16-byte granularity: one array read, zero
/// branches. Profiling on a real varying-size workload (8..1000 bytes)
/// showed the previous linear scan — fully unrolled by the compiler into
/// up to 26 sequential compare+branch pairs, since the compiler can't
/// know `size` at compile time across the `ll_malloc` C ABI boundary —
/// was a real, measurable cost, not a `size=64`-only artifact. See
/// `rfc/model/memory/heap-slot-allocation.md`. Built once at compile
/// time (`const fn`), embedded directly in `.rodata` — zero runtime
/// construction cost.
const CLASS_LUT_LEN: usize = MAX_SMALL / 16 + 2;

const fn build_class_lut() -> [u8; CLASS_LUT_LEN] {
    let mut table = [0u8; CLASS_LUT_LEN];
    let mut g = 0;
    while g < CLASS_LUT_LEN {
        let size = g * 16;
        let mut ci = 0;
        while ci < SIZE_CLASSES.len() {
            if SIZE_CLASSES[ci] >= size {
                break;
            }
            ci += 1;
        }
        table[g] = ci as u8;
        g += 1;
    }
    table
}

static CLASS_LUT: [u8; CLASS_LUT_LEN] = build_class_lut();

/// Smallest size-class index fitting `size`, or `None` if too large.
#[inline]
pub fn size_class_index(size: usize) -> Option<usize> {
    if size > MAX_SMALL {
        return None;
    }
    let ci = CLASS_LUT[(size + 15) >> 4] as usize;
    Some(ci)
}

/// A free slot threads a list through its own first 8 bytes — zero
/// metadata overhead. Used both for a block's own free list and for
/// `remote_free`, the cross-thread MPSC staging stack. Every size class is
/// ≥ 16 bytes, so a slot always has room for the link.
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
    /// Head of this block's free-slot list. `next` lives inside the freed
    /// slot itself, so the list costs no side allocation and no metadata
    /// line of its own — see the module doc.
    free: *mut FreeSlot,
    /// Slots handed out at least once, counting from the block start.
    /// Slots at index `>= bump` are virgin: never touched, nothing to
    /// read or maintain, carved by address arithmetic on demand.
    bump: u32,
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

/// Push `ptr` back onto its block's free list.
///
/// No index arithmetic: the link lives in the slot itself, so returning a
/// slot needs neither its index nor the division-by-`class_size` that
/// computing one would cost (`class_size` is not a power of two for most
/// classes, so it cannot fold to a shift). `Heap::free` open-codes this on
/// its fast path; this helper exists for [`Heap::drain_remote`].
///
/// # Safety
/// `ptr` must be a slot belonging to `b`.
#[inline]
unsafe fn push_free(b: &mut HeapBlockHeader, ptr: *mut u8) {
    let slot = ptr as *mut FreeSlot;
    unsafe {
        (*slot).next = b.free;
    }
    b.free = slot;
}

/// Opt-in counters behind the `probe-counters` feature, read out over the C
/// ABI by `bench-external/larson/{walk_probe,churn_probe}.cpp`. They answer
/// "how many blocks does one alloc walk?" and "how often do blocks cross the
/// full/not-full line?" — the two questions that located the block-list
/// churn in `rfc/model/memory/heap-slot-allocation.md` ("Fix 5b").
///
/// Deliberately not always-on: these are plain stores on the hot path, and a
/// timing run built with them is measuring the counters as much as the
/// allocator. Single-threaded probe use only — no atomics.
#[cfg(feature = "probe-counters")]
pub mod probe {
    /// Entries into `Heap::alloc`. Each one examines exactly one block, so
    /// this counts block examinations, **including** re-entries made by the
    /// cold paths via `alloc_class`.
    pub static mut ALLOC_ENTRIES: u64 = 0;
    /// Re-entries specifically. `ENTRIES - RETRIES` is the number of real
    /// allocations, and `ENTRIES / (ENTRIES - RETRIES)` is blocks walked per
    /// allocation — 1.0 means every alloc found room in the first block it
    /// looked at.
    pub static mut ALLOC_RETRIES: u64 = 0;
    pub static mut UNLINK_CALLS: u64 = 0;
    pub static mut LINK_CALLS: u64 = 0;

    /// Write `[entries, retries, unlinks, links]` to `out`.
    ///
    /// # Safety
    /// `out` must point to writable space for four `u64`s. Single-threaded
    /// probe use only.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn ll_probe_counters(out: *mut u64) {
        unsafe {
            *out.add(0) = ALLOC_ENTRIES;
            *out.add(1) = ALLOC_RETRIES;
            *out.add(2) = UNLINK_CALLS;
            *out.add(3) = LINK_CALLS;
        }
    }
}

/// Bump a [`probe`] counter, or compile to nothing without the feature.
macro_rules! probe_count {
    ($name:ident) => {
        #[cfg(feature = "probe-counters")]
        unsafe {
            probe::$name += 1;
        }
    };
}

/// Thread-local small-object heap. Per size class it holds the head of a
/// doubly-linked list of blocks that still have room.
pub struct Heap {
    available: [*mut HeapBlockHeader; NUM_CLASSES],
    /// At most one retained-but-empty block per size class, kept ready
    /// for instant reuse instead of returning to `BlockPool` and
    /// re-carving on the very next allocation. See
    /// `rfc/model/memory/heap-slot-allocation.md`.
    empty_reserve: [*mut HeapBlockHeader; NUM_CLASSES],
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
            available: [std::ptr::null_mut(); NUM_CLASSES],
            empty_reserve: [std::ptr::null_mut(); NUM_CLASSES],
            shared,
        }
    }

    /// Allocate at least `size` bytes, or null if `size` exceeds the
    /// small-object range.
    ///
    /// ## Why this is split into a fast path and `#[cold]` tails
    ///
    /// Everything rare — refilling from `BlockPool`, draining cross-thread
    /// frees, walking past a full block — lives in separate `#[cold]`
    /// `#[inline(never)]` functions rather than in this body. That is not
    /// tidiness, it is the codegen: a function containing calls and needing
    /// many live values gets a stack frame and callee-saved register
    /// spills, and **the fast path pays for them on every call even though
    /// only the rare branch needs them**. Disassembly of the pre-split
    /// version showed `Heap::alloc` opening with five `push`es, a
    /// `sub rsp, 48` and a `movaps` saving `xmm6` (LLVM had picked a
    /// callee-saved SSE register to zero 16 bytes in `unlink`), plus the
    /// mirror image on exit — roughly 20 instructions of frame management
    /// around a ~4-instruction free-list pop, and no inlining into
    /// `ll_alloc` because the function was too big.
    ///
    /// Kept as a leaf, this compiles to a handful of instructions with no
    /// frame, and the cold tails are reached by a tail call — exactly the
    /// shape mimalloc's `mi_malloc` has (`jmp _mi_malloc_generic`).
    /// `refill` runs ~0.00003 times per alloc (measured); without `#[cold]`
    /// LLVM has no way to know that and optimises the body as if the
    /// branches were balanced.
    #[inline]
    pub fn alloc(&mut self, size: usize) -> *mut u8 {
        probe_count!(ALLOC_ENTRIES);
        if size > MAX_SMALL {
            return std::ptr::null_mut();
        }
        // SAFETY: `CLASS_LUT` is const-built with one entry per 16-byte
        // step up to `MAX_SMALL`, and every entry it stores is an index
        // `< NUM_CLASSES` for any `size <= MAX_SMALL` (the one larger entry
        // is only reachable past that bound, excluded above). The bounds
        // check LLVM would otherwise emit here cannot fire.
        let ci = unsafe { *CLASS_LUT.get_unchecked((size + 15) >> 4) as usize };
        debug_assert!(ci < NUM_CLASSES);

        let block = unsafe { *self.available.get_unchecked(ci) };
        if block.is_null() {
            return self.alloc_no_block(ci);
        }

        let b = unsafe { &mut *block };

        // O(1) pop from the in-slot free list, else carve a virgin slot,
        // else the block is full. Branch-only: no scan, and no dependent
        // load into a side structure.
        let slot = b.free;
        let p = if !slot.is_null() {
            b.free = unsafe { (*slot).next };
            slot as *mut u8
        } else if b.bump < b.slots {
            let idx = b.bump as usize;
            b.bump += 1;
            let class_size = unsafe { *SIZE_CLASSES.get_unchecked(ci) };
            let base = (block as *mut u8).wrapping_add(LINE_SIZE);
            base.wrapping_add(idx * class_size)
        } else {
            return self.alloc_block_full(ci, block);
        };

        // Inline, not a `#[cold]` call: `p` is live across this point, so a
        // call here would strand it in a callee-saved register and drag the
        // whole stack frame back into the fast path. The `used == 0` test
        // short-circuits, so the array read costs nothing in the common case.
        if b.used == 0 && self.empty_reserve[ci] == block {
            self.empty_reserve[ci] = std::ptr::null_mut();
        }
        b.used += 1;

        // If that was the block's last slot, retire it from `available`
        // right now, while its header is still hot in registers. Leaving it
        // at the head instead means the *next* alloc reloads this header
        // cold only to discover it is full and unlink it then — same
        // unlink, one extra cache miss. Not `#[cold]`: measured at ~0.32
        // per alloc, far too common to bury behind a call.
        if b.free.is_null() && b.bump >= b.slots {
            self.unlink(ci, block);
        }

        p
    }

    /// Cold tail: this class has no block with room. Reclaim cross-thread
    /// frees, else take a fresh block from the pool, then retry.
    #[cold]
    #[inline(never)]
    fn alloc_no_block(&mut self, ci: usize) -> *mut u8 {
        if !self.shared.remote_free.load(Ordering::Relaxed).is_null() {
            self.drain_remote();
            if !self.available[ci].is_null() {
                return self.alloc_class(ci);
            }
        }
        self.refill(ci);
        self.alloc_class(ci)
    }

    /// Cold tail: the head block turned out to be full. Unlink it and retry
    /// with whatever is behind it.
    #[cold]
    #[inline(never)]
    fn alloc_block_full(&mut self, ci: usize, block: *mut HeapBlockHeader) -> *mut u8 {
        self.unlink(ci, block);
        self.alloc_class(ci)
    }

    /// Re-enter the fast path once a cold tail has made a slot available.
    /// Separate from `alloc` only because the cold tails already know `ci`
    /// and must not redo the size lookup.
    fn alloc_class(&mut self, ci: usize) -> *mut u8 {
        probe_count!(ALLOC_RETRIES);
        let size = SIZE_CLASSES[ci];
        self.alloc(size)
    }

    /// Free a slot from [`alloc`]. If this thread owns the block it is a
    /// cheap local free; otherwise the slot is posted to the owner's
    /// lock-free `remote_free` stack.
    ///
    /// # Safety
    /// `ptr` must be a live allocation from some heap and not freed yet.
    ///
    /// Split fast/cold for the same codegen reason as [`alloc`] — see its
    /// doc. The owning-thread push is the whole fast path; the cross-thread
    /// hand-off and the block-emptied bookkeeping are cold tails.
    #[inline]
    pub unsafe fn free(&mut self, ptr: *mut u8) {
        let block = HeapBlockHeader::of_ptr(ptr);
        let b = unsafe { &mut *block };

        if !std::ptr::eq(b.owner, self.shared) {
            return unsafe { Self::free_remote(b.owner, ptr) };
        }

        // Push onto the block's free list: the `next` write lands in the
        // slot the program has just stopped using, already hot in cache.
        let slot = ptr as *mut FreeSlot;
        unsafe { (*slot).next = b.free };
        b.free = slot;
        b.used -= 1;

        let ci = b.size_class as usize;
        if b.used == 0 {
            return self.retire_empty(ci, block);
        }
        if !b.linked {
            self.relink_unfull(ci, block);
        }
    }

    /// Cold tail: the block belongs to another thread's heap. One atomic
    /// push onto that owner's stack, touching nothing else.
    ///
    /// # Safety
    /// `ptr` must be a live slot owned by `owner`.
    #[cold]
    #[inline(never)]
    unsafe fn free_remote(owner: *const HeapShared, ptr: *mut u8) {
        let slot = ptr as *mut FreeSlot;
        let remote = unsafe { &(*owner).remote_free };
        let mut head = remote.load(Ordering::Relaxed);
        loop {
            unsafe { (*slot).next = head };
            match remote.compare_exchange_weak(head, slot, Ordering::Release, Ordering::Relaxed) {
                Ok(_) => break,
                Err(h) => head = h,
            }
        }
    }

    /// Common tail once a block's `used` count has just reached zero:
    /// keep it as the class's one bounded empty spare (instant reuse, no
    /// refill) if there isn't one already; otherwise actually return it
    /// to the global pool (reclaiming the bitmap's own allocation first).
    /// See `rfc/model/memory/heap-slot-allocation.md`.
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
        unsafe {
            (*block).kind = 0;
        }
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

            unsafe { push_free(b, slot as *mut u8) };
            b.used -= 1;

            if b.used == 0 {
                self.retire_empty(ci, block);
            } else if !b.linked {
                self.link(ci, block);
            }

            slot = next;
        }
    }

    /// Take a fresh block from the pool, allocate and initialize its
    /// bitmap (all slots free), and link it as available. Initializing
    /// the bitmap is O(slots/64) words, not O(slots) — it never touches
    /// the slots' own memory, unlike the old eager free-list threading.
    fn refill(&mut self, ci: usize) -> *mut HeapBlockHeader {
        let class_size = SIZE_CLASSES[ci];
        let block = BlockPool::global().get() as *mut HeapBlockHeader;
        let slots = (BLOCK_PAYLOAD / class_size) as u32;

        // No side allocation at all: an empty free list plus `bump = 0`
        // means "every slot virgin" — O(1), touching nothing but the
        // header line.
        unsafe {
            block.write(HeapBlockHeader {
                kind: BLOCK_KIND_HEAP,
                size_class: ci as u32,
                used: 0,
                slots,
                free: std::ptr::null_mut(),
                bump: 0,
                owner: self.shared,
                next: std::ptr::null_mut(),
                prev: std::ptr::null_mut(),
                linked: false,
            });
        }
        self.link(ci, block);
        block
    }

    /// Re-link a block that was full and has just had a slot freed.
    ///
    /// Deliberately **not** at the head, even though inserting behind it
    /// costs three header writes against the head-insert's one. `alloc`
    /// serves from the head, so a just-unfulled block placed there becomes
    /// an allocation point with exactly one free slot: the next alloc drains
    /// it and it is full again immediately. Measured — link at head instead
    /// and the block-switch rate doubles (0.32 → 0.77 per alloc) for
    /// **−21.7%** throughput. Behind the head, the block accumulates more
    /// frees before `alloc` reaches it.
    ///
    /// This is the general rule for everything on this path: **the rate of
    /// block switches is what costs, not the bookkeeping around them.** A
    /// switch means loading a block header and free-list head this thread
    /// has not touched recently; the pointer updates are noise beside it.
    /// Replacing this whole list with a per-class bitmap — zero foreign
    /// header traffic — was measured at +0.3%, i.e. nothing. See
    /// `rfc/model/memory/heap-slot-allocation.md`, "What churn actually
    /// costs".
    fn relink_unfull(&mut self, ci: usize, block: *mut HeapBlockHeader) {
        probe_count!(LINK_CALLS);
        let head = self.available[ci];
        if head.is_null() {
            self.link(ci, block);
            return;
        }
        unsafe {
            let second = (*head).next;
            (*block).prev = head;
            (*block).next = second;
            (*block).linked = true;
            (*head).next = block;
            if !second.is_null() {
                (*second).prev = block;
            }
        }
    }

    fn link(&mut self, ci: usize, block: *mut HeapBlockHeader) {
        probe_count!(LINK_CALLS);
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
        probe_count!(UNLINK_CALLS);
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

    /// Resolve the slot, initializing it if this is the first ever call.
    /// Only [`set`] and [`ensure_slot`] need this: both run from
    /// `ll_thread_init`, the one place allowed to be the first caller.
    #[inline]
    fn state_or_init() -> u32 {
        let s = STATE.load(Ordering::Relaxed);
        if s != UNINIT { s } else { init() }
    }

    /// Reserve the process-wide TLS slot if nobody has yet. Must run before
    /// the first [`get`] on any thread — `ll_thread_init` calls it, which is
    /// exactly what lets `get` skip the initialized check.
    #[inline]
    pub fn ensure_slot() {
        let _ = state_or_init();
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

    /// Read this thread's heap pointer.
    ///
    /// Does **not** check whether `STATE` is initialized: `ll_thread_init`
    /// must have run on this thread first, and it is what initializes
    /// `STATE`, so by the time any allocation reaches here the value is
    /// resolved. That contract already exists (see `ll_thread_init`'s doc —
    /// the hot path trusts it and does not check); this just stops paying
    /// for a second, redundant check of it. Disassembly showed the check
    /// cost a global load plus two branches ahead of the one instruction
    /// that actually reads the TEB.
    #[inline]
    pub fn get() -> *mut Heap {
        let s = STATE.load(Ordering::Relaxed);
        debug_assert_ne!(s, UNINIT, "ll_thread_init() was not called on this thread");
        if s & FALLBACK_BIT == 0 {
            unsafe { read_gs_qword(s) as *mut Heap }
        } else {
            get_fallback(s)
        }
    }

    /// The reserved slot landed outside the TEB's 64 inline slots, so the
    /// fixed-offset read is unavailable and the real Win32 call is needed.
    /// Practically unreachable — this is one of the first `TlsAlloc`s in the
    /// process — but keeping the call out of line matters: any call in the
    /// hot function forces it to build a stack frame that the fast path then
    /// pays for on every allocation.
    #[cold]
    #[inline(never)]
    fn get_fallback(s: u32) -> *mut Heap {
        unsafe { TlsGetValue(s & !FALLBACK_BIT) as *mut Heap }
    }

    #[inline]
    pub fn set(p: *mut Heap) {
        let s = state_or_init();
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

    /// No-op here: `thread_local!` needs no process-wide slot reserved.
    /// Exists so `ll_thread_init` is written once for both targets.
    #[inline]
    pub fn ensure_slot() {}

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
    // Must precede the first `tls::get()` anywhere: `get` deliberately does
    // not check whether the slot has been reserved (see its doc), so this
    // is the call that establishes that invariant.
    tls::ensure_slot();
    if tls::get().is_null() {
        tls::set(Box::into_raw(Box::new(Heap::new())));
    }
}

/// Run `f` with this thread's persistent small-object heap.
///
/// `#[inline(always)]`, not merely `#[inline]`: left to its own judgement
/// LLVM kept this as a real call, which forced the caller to materialise
/// `f` in a stack slot and pass it by pointer — `ll_malloc` opened with
/// `sub rsp, 40; mov [rsp+32], rcx; lea rcx, [rsp+32]; call` purely to hand
/// over a closure that captures one integer. Inlined, the closure vanishes
/// entirely and the TEB read lands directly in the caller.
///
/// # Safety
/// [`ll_thread_init`] must have been called on this thread first. No
/// check is made — that is the point (see `ll_thread_init`'s doc).
#[inline(always)]
pub unsafe fn with_thread_heap<R>(f: impl FnOnce(&mut Heap) -> R) -> R {
    let p = tls::get();
    debug_assert!(
        !p.is_null(),
        "ll_thread_init() was not called on this thread"
    );
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
