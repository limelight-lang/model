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
//! Every block carries its own lock-free MPSC stack, `remote_free`. A
//! `free(ptr)` whose block is owned by *another* heap does one atomic push
//! onto **that block's** stack and touches nothing else.
//!
//! Per block, not per heap, and that is load-bearing: it is what makes
//! adoption (below) race-free. A thread freeing a slot reads `owner`, sees
//! it is not itself, and pushes. If an adoption is racing that read it does
//! not matter which owner was seen — the message lands in the block, and the
//! block's *current* owner drains it. Parked in a per-heap stack instead, a
//! message posted to a dying owner after adoption is stranded forever.
//!
//! The owner collects a block's parked frees in two places, both cold:
//! [`Heap::alloc_block_full`], when it has just run that block out of slots
//! (exactly when they are worth having), and [`Heap::collect_owned`], which
//! sweeps this class's blocks before asking the pool for more. The sweep is
//! not optional: a block unlinked as full is never revisited otherwise, so
//! its parked frees would sit forever and the thread would refill instead —
//! measured at 34.2M -> 2.3M ops/s on the bleeding pattern when it was
//! missing. mimalloc's full queue exists for the same reason.
//!
//! `used` is written **only by the owning thread** (on alloc, local free,
//! and collect) — no atomics on it. A cross-thread free just parks the slot;
//! the owner accounts for it at collect time. That is also why `used == 0` is
//! safe to act on: a parked slot still counts as live, so a block with one
//! can never look empty.
//!
//! ## Thread-exit abandonment
//!
//! [`ll_thread_exit`] hands a dying thread's blocks over: empty ones to the
//! pool, ones still holding live objects onto a global per-class abandoned
//! list, from which the next thread needing that class adopts them
//! ([`Heap::adopt`], on the refill path).
//!
//! This is not an optimisation. Without it every block a thread still owned
//! when it died was stranded permanently, along with every later
//! cross-thread free into it. Real `larson.cpp` — whose entire point is that
//! a server's workers come and go, so it respawns its worker every ~20 ms —
//! held **1.7 GiB against a 2.5 MiB live set**. With it: 10 MiB.
//!
//! Known limit: an abandoned block is only reclaimed when someone adopts it,
//! so a class that goes permanently idle keeps its abandoned blocks. Bounded
//! by what was live at thread exit, and no periodic trim exists yet.

use std::sync::Mutex;
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

/// Blocks whose owning thread died while they still held live objects,
/// per size class, chained through `owned_next`.
///
/// A `Mutex`, not a lock-free stack, on purpose: both users are cold —
/// thread exit, and `alloc`'s refill path, which runs ~0.00003 times per
/// allocation (measured). A lock-free stack here would buy nothing real and
/// would need ABA tagging to be correct.
struct Abandoned {
    heads: [*mut HeapBlockHeader; NUM_CLASSES],
}

// SAFETY: the pointers are block headers in never-unmapped pool regions;
// the mutex is what serialises access to the chain.
unsafe impl Send for Abandoned {}

static ABANDONED: Mutex<Abandoned> = Mutex::new(Abandoned {
    heads: [std::ptr::null_mut(); NUM_CLASSES],
});

/// The owner-private half of the header: only the thread named by
/// [`BlockShared::owner`] may touch it. Split out so that borrowing it
/// is an *honest* exclusive claim — `&mut HeapBlockHeader` was not, since
/// it also covered the atomics every other thread reads, and the model
/// counts taking a reference as an access (audit `heap.rs:647`).
/// The fields the allocation fast path touches, kept together so they
/// share one cache line with [`BlockShared::owner`] (see that type).
#[repr(C)]
struct BlockPrivate {
    /// Offset 0 of the block, shared with the pool `BlockHeader`'s
    /// tagged-union discriminant: must stay the first field.
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
    linked: bool,
    /// `available` list for this size class (blocks with room). Hot, and
    /// deliberately still in line 0: `link`/`unlink`/`relink_unfull` run
    /// on every full ↔ has-room transition, and `rptest` churns blocks
    /// across that boundary constantly. Evicting these to their own line
    /// measured slower (direction only, same caveat as [`BlockShared`]).
    next: *mut HeapBlockHeader,
    prev: *mut HeapBlockHeader,
}

/// Owner-private and genuinely cold: only thread exit and adoption walk
/// these, so they sit past the remote line.
#[repr(C)]
struct BlockLinks {
    /// Every block this heap owns, full or not, so a dying thread can
    /// enumerate them. `available` cannot serve: a full block is unlinked
    /// from it and would otherwise be unreachable — which is exactly how
    /// they used to leak.
    owned_next: *mut HeapBlockHeader,
    owned_prev: *mut HeapBlockHeader,
}

/// Shared, but **read-mostly**: written once at refill and again at
/// adoption or abandonment, and read by every `free` to decide whether
/// the slot is local. So it deliberately stays in the hot line, right
/// behind [`BlockPrivate`], rather than moving out with `remote_free`.
///
/// Splitting it out of the private half at all is what makes the owner's
/// `&mut` honest: a non-owner reads this concurrently by design, so no
/// exclusive borrow may ever cover it (audit `heap.rs:647`).
///
/// Rejected alternative: parking `owner` on the isolated line together
/// with `remote_free`. Every local free then touches a second line just
/// to check ownership, and it measured clearly slower. (Direction only —
/// that run was against a stale baseline on a noisy box, so no figure is
/// quoted; the trustworthy A/B is the one in this change's commit.)
#[repr(C)]
struct BlockShared {
    /// Identity of the owning heap, or null once abandoned. **Compared,
    /// never dereferenced** by a non-owner — which is what lets it be the
    /// address of a thread-local `Heap`. Adoption claims a block with a
    /// plain `Release` store, not a CAS: the block is off the abandoned
    /// list under its mutex by then, so no other thread is contending for
    /// it, and a racing `free` is correct either way — it either sees the
    /// old owner and parks the slot in `remote_free`, which the new owner
    /// drains, or sees the new one.
    owner: AtomicPtr<Heap>,
}

/// The contended half, alone on its own cache line.
///
/// This is the field cross-thread frees hammer with CAS, and it used to
/// sit beside `used`/`free`/`bump`, so every push stole the line from
/// under the owner's hot path (audit `heap.rs:212`). The header owns the
/// block's whole reserved 256-byte line, so the padding costs no payload
/// and no slots.
#[repr(C, align(64))]
struct BlockRemote {
    /// Cross-thread frees destined for **this block**.
    ///
    /// The single most important field for adoption to be race-free, and the
    /// reason it lives here rather than in the heap (where it used to). A
    /// thread freeing a slot reads `owner`, sees it is not itself, and pushes
    /// here. If an adoption is racing that read, it does not matter which
    /// value of `owner` was seen: the message lands in the block, and the
    /// block's *current* owner is the one who drains it. Parked in a heap
    /// instead, a message posted to the dying owner after adoption would be
    /// stranded forever — nobody drains a dead heap.
    remote_free: AtomicPtr<FreeSlot>,
}

/// Per-block header. Overlays the block's first line; shares offset 0
/// (`private.kind`) with the pool's `BlockHeader` (tagged union over the
/// memory).
///
/// Field order is the layout contract, pinned by
/// `block_header_halves_are_laid_out_as_the_design_requires`: hot private
/// fields and `owner` in line 0, the contended `remote_free` alone in
/// line 1, cold links after it.
#[repr(C)]
struct HeapBlockHeader {
    private: BlockPrivate,
    shared: BlockShared,
    remote: BlockRemote,
    links: BlockLinks,
}

impl HeapBlockHeader {
    #[inline]
    fn of_ptr(p: *mut u8) -> *mut HeapBlockHeader {
        ((p as usize) & !BLOCK_MASK) as *mut HeapBlockHeader
    }
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
    /// Frees that took the cross-thread path (block owned by someone else,
    /// or abandoned) rather than the local one.
    pub static mut REMOTE_FREES: u64 = 0;
    /// Frees total.
    pub static mut FREES: u64 = 0;
    /// Blocks adopted from the abandoned list.
    pub static mut ADOPTED: u64 = 0;

    /// Write `[entries, retries, unlinks, links, remote_frees, frees,
    /// adopted]` to `out`.
    ///
    /// # Safety
    /// `out` must point to writable space for seven `u64`s. Single-threaded
    /// probe use only.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn ll_probe_counters(out: *mut u64) {
        unsafe {
            *out.add(0) = ALLOC_ENTRIES;
            *out.add(1) = ALLOC_RETRIES;
            *out.add(2) = UNLINK_CALLS;
            *out.add(3) = LINK_CALLS;
            *out.add(4) = REMOTE_FREES;
            *out.add(5) = FREES;
            *out.add(6) = ADOPTED;
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
    /// Every block this heap owns, full or not, chained through
    /// `owned_next` — **per size class**, not one global chain.
    ///
    /// Per class because `collect_owned` walks this to find blocks holding
    /// parked cross-thread frees, and it only ever wants one class: a single
    /// chain makes that walk O(all our blocks) where it should be O(blocks of
    /// this class) — ~67 vs ~3 at a 5000-object live set, and it showed
    /// (bleeding larson: 22M vs 34M ops/s).
    ///
    /// Also what lets [`Heap::abandon_all`] enumerate at thread exit;
    /// `available` cannot, since a full block is unlinked from it.
    owned: [*mut HeapBlockHeader; NUM_CLASSES],
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

impl Heap {
    pub fn new() -> Self {
        Heap {
            available: [std::ptr::null_mut(); NUM_CLASSES],
            empty_reserve: [std::ptr::null_mut(); NUM_CLASSES],
            owned: [std::ptr::null_mut(); NUM_CLASSES],
        }
    }

    /// This heap's identity, as stored in `HeapBlockHeader::owner`.
    #[inline]
    fn id(&self) -> *mut Heap {
        self as *const Heap as *mut Heap
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

        let b = unsafe { &mut (*block).private };

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
        // Our own blocks first: a block we unlinked as full may since have
        // been filled with cross-thread frees, which nobody else will ever
        // collect. `alloc_block_full` only ever collects the block it is
        // serving from, so without this sweep a workload where another
        // thread does the freeing strands every full block and refills
        // forever. (Measured, the hard way: 34.2M -> 2.3M ops/s on
        // `mt_bench`'s bleeding pattern.) This is what mimalloc's full queue
        // is for.
        if self.collect_owned(ci) {
            return self.alloc_class(ci);
        }
        // Then adopt: an abandoned block of this class is memory we already
        // hold, already carved for this exact size. Skipping this and carving
        // fresh is how a thread-churning workload grows without bound
        // (larson: 1.7 GiB resident against a 2.5 MiB live set).
        if self.adopt(ci) {
            return self.alloc_class(ci);
        }
        if self.refill(ci).is_null() {
            return std::ptr::null_mut();
        }
        self.alloc_class(ci)
    }

    /// Sweep this heap's blocks of class `ci` for parked cross-thread frees.
    /// Returns true if any block gained slots.
    ///
    /// O(blocks this heap owns), but only on the path that would otherwise
    /// take a whole new block from the pool — always the better trade.
    fn collect_owned(&mut self, ci: usize) -> bool {
        let mut block = self.owned[ci];
        let mut found = false;
        while !block.is_null() {
            let next = unsafe { (*block).links.owned_next };
            let pending =
                unsafe { !(*block).remote.remote_free.load(Ordering::Relaxed).is_null() };
            if pending && self.collect_remote(block) {
                let b = unsafe { &mut (*block).private };
                if b.used == 0 {
                    self.retire_empty(ci, block);
                    found = true;
                } else if !b.linked {
                    self.link(ci, block);
                    found = true;
                }
            }
            block = next;
        }
        found
    }

    /// Cold tail: the head block turned out to be full. Unlink it and retry
    /// with whatever is behind it.
    #[cold]
    #[inline(never)]
    fn alloc_block_full(&mut self, ci: usize, block: *mut HeapBlockHeader) -> *mut u8 {
        // Before writing the block off as full, take whatever other threads
        // freed into it. This is one of the two places cross-thread frees
        // are reclaimed — `collect_owned` sweeps the rest — and it is the
        // natural one: we are here precisely because
        // this block has no slots left, which is exactly when its parked
        // frees are worth the walk.
        if self.collect_remote(block) {
            return self.alloc_class(ci);
        }
        self.unlink(ci, block);
        self.alloc_class(ci)
    }

    /// Take an abandoned block of this class, if one exists, and make it
    /// ours. Returns false if there was nothing to adopt.
    fn adopt(&mut self, ci: usize) -> bool {
        let block = {
            let mut list = ABANDONED.lock().unwrap();
            let head = list.heads[ci];
            if head.is_null() {
                return false;
            }
            list.heads[ci] = unsafe { (*head).links.owned_next };
            head
        };

        probe_count!(ADOPTED);
        // Through the raw pointer, like `own`/`link`/`retire_empty`. Holding
        // a `&mut` to the header across `self.own` would alias it: `own`
        // writes these very fields through `block`, which invalidates any
        // outstanding reference, and the old code then kept using it.
        unsafe {
            (*block).links.owned_next = std::ptr::null_mut();
            (*block).links.owned_prev = std::ptr::null_mut();
            (*block).private.next = std::ptr::null_mut();
            (*block).private.prev = std::ptr::null_mut();
            (*block).private.linked = false;

            // Claim it. Any free racing this either saw the old owner or
            // sees us; both push into `remote_free`, which we now own and
            // will collect.
            (*block).shared.owner.store(self.id(), Ordering::Release);
        }
        self.own(ci, block);

        // Slots freed while it was ownerless are parked; take them now. The
        // borrow lasts exactly this call.
        self.collect_remote(unsafe { &mut *block });

        let (used, free, bump, slots) =
            unsafe { ((*block).private.used, (*block).private.free, (*block).private.bump, (*block).private.slots) };
        if used == 0 {
            self.retire_empty(ci, block);
        } else if free.is_null() && bump >= slots {
            // Adopted full: keep it (we own it) but it serves nothing yet.
            return false;
        } else {
            self.link(ci, block);
        }
        !self.available[ci].is_null()
    }

    /// Add `block` to this heap's owned chain for its class.
    fn own(&mut self, ci: usize, block: *mut HeapBlockHeader) {
        unsafe {
            (*block).links.owned_prev = std::ptr::null_mut();
            (*block).links.owned_next = self.owned[ci];
            if !self.owned[ci].is_null() {
                (*self.owned[ci]).links.owned_prev = block;
            }
        }
        self.owned[ci] = block;
    }

    /// Remove `block` from this heap's owned chain.
    fn disown(&mut self, ci: usize, block: *mut HeapBlockHeader) {
        unsafe {
            let prev = (*block).links.owned_prev;
            let next = (*block).links.owned_next;
            if !prev.is_null() {
                (*prev).links.owned_next = next;
            } else {
                self.owned[ci] = next;
            }
            if !next.is_null() {
                (*next).links.owned_prev = prev;
            }
            (*block).links.owned_prev = std::ptr::null_mut();
            (*block).links.owned_next = std::ptr::null_mut();
        }
    }

    /// Thread exit: give up every block this heap owns.
    ///
    /// Empty ones go back to the pool. Ones still holding live objects go on
    /// the global abandoned list for the next thread that needs that class —
    /// they cannot go to the pool (their objects are alive) and they must not
    /// simply be dropped (that is the leak this exists to fix).
    ///
    /// Idempotent: safe to call from both `ll_thread_exit` and the TLS guard.
    pub fn abandon_all(&mut self) {
        let mut list = ABANDONED.lock().unwrap();
        for ci in 0..NUM_CLASSES {
            let mut block = self.owned[ci];
            while !block.is_null() {
                let next = unsafe { (*block).links.owned_next };

                // Collect first: slots freed cross-thread may make it empty,
                // and an empty block is worth more to the pool than to the
                // abandoned list.
                self.collect_remote_locked(block);
                unsafe { (*block).shared.owner.store(std::ptr::null_mut(), Ordering::Release) };

                let b = unsafe { &mut (*block).private };
                if b.used == 0 {
                    b.kind = 0;
                    BlockPool::global().put(block as *mut BlockHeader);
                } else {
                    b.linked = false;
                    b.next = std::ptr::null_mut();
                    b.prev = std::ptr::null_mut();
                    let l = unsafe { &mut (*block).links };
                    l.owned_prev = std::ptr::null_mut();
                    l.owned_next = list.heads[ci];
                    list.heads[ci] = block;
                }
                block = next;
            }
            self.owned[ci] = std::ptr::null_mut();
        }
        self.available = [std::ptr::null_mut(); NUM_CLASSES];
        self.empty_reserve = [std::ptr::null_mut(); NUM_CLASSES];
    }

    /// [`collect_remote`] without touching `self` — for use while the
    /// abandoned list's lock is held.
    fn collect_remote_locked(&self, block: *mut HeapBlockHeader) {
        // Takes the raw block: it needs both halves, and they must be
        // reached separately — the atomic through a shared reference, the
        // private fields through an exclusive one.
        let head = unsafe { (*block).remote.remote_free.swap(std::ptr::null_mut(), Ordering::Acquire) };
        if head.is_null() {
            return;
        }
        let b = unsafe { &mut (*block).private };
        let mut n = 0u32;
        let mut last = head;
        unsafe {
            loop {
                n += 1;
                let nxt = (*last).next;
                if nxt.is_null() {
                    break;
                }
                last = nxt;
            }
            (*last).next = b.free;
        }
        b.free = head;
        b.used -= n;
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

        probe_count!(FREES);
        // Reference the atomic alone, never the header, until ownership is
        // established. `&mut *block` here would retag the whole header, and
        // a retag counts as an access — on a block owned by another thread
        // that races the owner's own borrow (audit `heap.rs:647`, which Miri
        // reports as a data race between the two retags).
        if unsafe { (*block).shared.owner.load(Ordering::Relaxed) } != self.id() {
            probe_count!(REMOTE_FREES);
            return Self::free_remote(block, ptr);
        }

        // Ours: the rest of the header is ours to borrow exclusively.
        let b = unsafe { &mut (*block).private };

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

    /// Cold tail: this block is owned by another thread, or is abandoned.
    /// One atomic push onto **the block's own** stack, touching nothing else.
    ///
    /// Note `used` is deliberately not touched: it is owner-only state, and
    /// the owner accounts for this slot when it collects (see
    /// [`Heap::collect_remote`]). That is also what makes `used == 0` safe to
    /// act on — a slot parked here still counts as live, so a block with a
    /// parked free can never look empty.
    #[cold]
    #[inline(never)]
    fn free_remote(block: *mut HeapBlockHeader, ptr: *mut u8) {
        let slot = ptr as *mut FreeSlot;
        // The one field this thread may touch. Everything else in the header
        // belongs to the owner, which is mutating it as we run, so no
        // reference spanning the header may exist here.
        let remote_free = unsafe { &(*block).remote.remote_free };
        let mut head = remote_free.load(Ordering::Relaxed);
        loop {
            unsafe { (*slot).next = head };
            match remote_free.compare_exchange_weak(
                head,
                slot,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(h) => head = h,
            }
        }
    }

    /// Pull this block's cross-thread frees onto its local free list.
    /// Returns true if anything arrived.
    ///
    /// O(n) in what arrived since the last collect, walked once to get the
    /// count `used` must drop by — the same amortised deal mimalloc makes in
    /// `_mi_page_thread_free_collect`.
    fn collect_remote(&mut self, block: *mut HeapBlockHeader) -> bool {
        // See [`collect_remote_locked`] on why this takes the raw block.
        let head = unsafe { (*block).remote.remote_free.swap(std::ptr::null_mut(), Ordering::Acquire) };
        if head.is_null() {
            return false;
        }
        let b = unsafe { &mut (*block).private };
        let mut n = 0u32;
        let mut last = head;
        unsafe {
            loop {
                n += 1;
                let nxt = (*last).next;
                if nxt.is_null() {
                    break;
                }
                last = nxt;
            }
            (*last).next = b.free;
        }
        b.free = head;
        b.used -= n;
        true
    }

    /// Common tail once a block's `used` count has just reached zero:
    /// keep it as the class's one bounded empty spare (instant reuse, no
    /// refill) if there isn't one already; otherwise actually return it
    /// to the global pool. See `rfc/model/memory/heap-slot-allocation.md`.
    #[cold]
    #[inline(never)]
    fn retire_empty(&mut self, ci: usize, block: *mut HeapBlockHeader) {
        if self.empty_reserve[ci].is_null() {
            self.empty_reserve[ci] = block;
            if unsafe { !(*block).private.linked } {
                self.link(ci, block);
            }
            return;
        }
        if unsafe { (*block).private.linked } {
            self.unlink(ci, block);
        }
        self.disown(ci, block);
        unsafe {
            (*block)
                .shared
                .owner
                .store(std::ptr::null_mut(), Ordering::Release);
            (*block).private.kind = 0;
        }
        BlockPool::global().put(block as *mut BlockHeader);
    }

    /// Test oracle: total live slots this heap still accounts for, after
    /// draining every block's cross-thread queue.
    ///
    /// Exists because the process-global `blocks_out` is the wrong
    /// instrument for "did the owner account for every free". It is
    /// shared, so another test's block returning late moves it in either
    /// direction, and it only reacts once a block empties completely.
    /// This counts the thing directly.
    #[cfg(test)]
    fn live_slots_after_collect(&mut self) -> u32 {
        for ci in 0..NUM_CLASSES {
            self.collect_owned(ci);
        }
        let mut total = 0;
        for ci in 0..NUM_CLASSES {
            let mut block = self.owned[ci];
            while !block.is_null() {
                total += unsafe { (*block).private.used };
                block = unsafe { (*block).links.owned_next };
            }
        }
        total
    }

    /// Take a fresh block from the pool, stamp its header, and link it as
    /// available. O(1) and touching nothing but the header line: an empty
    /// free list plus `bump = 0` already means "every slot virgin", so
    /// there is no side allocation and no per-slot initialization at all
    /// — unlike both the eager free-list threading and the bitmap this
    /// replaced (see the module doc for why the bitmap lost).
    /// Null when the pool is empty and the OS refused more; the heap is
    /// left untouched, so the caller can report the failure and the heap
    /// stays usable for smaller classes.
    fn refill(&mut self, ci: usize) -> *mut HeapBlockHeader {
        let class_size = SIZE_CLASSES[ci];
        let block = BlockPool::global().get() as *mut HeapBlockHeader;
        if block.is_null() {
            return block;
        }
        let slots = (BLOCK_PAYLOAD / class_size) as u32;

        // No side allocation at all: an empty free list plus `bump = 0`
        // means "every slot virgin" — O(1), touching nothing but the
        // header line.
        unsafe {
            block.write(HeapBlockHeader {
                private: BlockPrivate {
                    kind: BLOCK_KIND_HEAP,
                    size_class: ci as u32,
                    used: 0,
                    slots,
                    free: std::ptr::null_mut(),
                    bump: 0,
                    linked: false,
                    next: std::ptr::null_mut(),
                    prev: std::ptr::null_mut(),
                },
                shared: BlockShared {
                    owner: AtomicPtr::new(self.id()),
                },
                remote: BlockRemote {
                    remote_free: AtomicPtr::new(std::ptr::null_mut()),
                },
                links: BlockLinks {
                    owned_next: std::ptr::null_mut(),
                    owned_prev: std::ptr::null_mut(),
                },
            });
        }
        self.own(ci, block);
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
    /// `#[inline(never)]` but **not** `#[cold]`, and the difference is the
    /// point. Out of line so `free`'s body stays short enough to inline
    /// into `ll_free`; not `#[cold]`, because that is an assertion to LLVM
    /// that the branch is rare, and here it is not obviously true — a
    /// workload churning blocks across the full ↔ has-room boundary takes
    /// it constantly. Claiming coldness that a workload disproves
    /// deoptimizes a path that is actually hot.
    #[inline(never)]
    fn relink_unfull(&mut self, ci: usize, block: *mut HeapBlockHeader) {
        probe_count!(LINK_CALLS);
        let head = self.available[ci];
        if head.is_null() {
            self.link(ci, block);
            return;
        }
        unsafe {
            let second = (*head).private.next;
            (*block).private.prev = head;
            (*block).private.next = second;
            (*block).private.linked = true;
            (*head).private.next = block;
            if !second.is_null() {
                (*second).private.prev = block;
            }
        }
    }

    fn link(&mut self, ci: usize, block: *mut HeapBlockHeader) {
        probe_count!(LINK_CALLS);
        let head = self.available[ci];
        unsafe {
            (*block).private.prev = std::ptr::null_mut();
            (*block).private.next = head;
            (*block).private.linked = true;
            if !head.is_null() {
                (*head).private.prev = block;
            }
        }
        self.available[ci] = block;
    }

    fn unlink(&mut self, ci: usize, block: *mut HeapBlockHeader) {
        probe_count!(UNLINK_CALLS);
        unsafe {
            let prev = (*block).private.prev;
            let next = (*block).private.next;
            if !prev.is_null() {
                (*prev).private.next = next;
            } else {
                self.available[ci] = next;
            }
            if !next.is_null() {
                (*next).private.prev = prev;
            }
            (*block).private.prev = std::ptr::null_mut();
            (*block).private.next = std::ptr::null_mut();
            (*block).private.linked = false;
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
        // TlsAlloc returns TLS_OUT_OF_INDEXES (u32::MAX) on failure — which
        // also equals our UNINIT sentinel, so storing it would make every
        // later `get` treat the slot as uninitialised and read a bad TEB
        // offset. Fail loudly instead of corrupting silently.
        assert_ne!(slot, u32::MAX, "TlsAlloc failed: no free TLS slots");
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

    /// [`get`] for callers that may legitimately run before any slot was
    /// reserved — `ll_thread_init` on the very first thread, and
    /// `ll_thread_exit` on a thread that never allocated. Returns null rather
    /// than asserting.
    #[inline]
    pub fn get_raw() -> *mut Heap {
        let s = STATE.load(Ordering::Relaxed);
        if s == UNINIT {
            return std::ptr::null_mut();
        }
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

    /// Mirrors the Windows path's signature; nothing to tolerate here.
    #[inline]
    pub fn get_raw() -> *mut Heap {
        get()
    }

    #[inline]
    pub fn set(p: *mut Heap) {
        THREAD_HEAP.with(|c| c.set(p));
    }
}

/// Give up this thread's heap blocks: empty ones to the pool, ones with
/// live objects to the global abandoned list, where the next thread needing
/// that size class will adopt them.
///
/// **This must happen before a thread that allocated exits.** Skipping it
/// is not a leak of one heap — it strands every block that thread still
/// owned, permanently, along with any later cross-thread free into them.
/// Measured cost of skipping it, on `larson.cpp` (which respawns its
/// worker every ~20 ms, by design — that is what the benchmark is *for*):
/// 1.7 GiB resident against a 2.5 MiB live set.
///
/// It happens **automatically** on every target: the TLS guard installed
/// by [`ll_thread_init`] calls this when a thread unwinds normally. This
/// export exists for callers who manage their own thread lifetimes, and
/// for FFI callers whose threads Rust knows nothing about.
///
/// Idempotent, and safe to call on a thread that never allocated.
#[unsafe(no_mangle)]
pub extern "C" fn ll_thread_exit() {
    let p = tls::get_raw();
    if p.is_null() {
        return;
    }
    // Clear the slot first: `abandon_all` must not be re-entered, and any
    // allocation after this point must build a fresh heap rather than reuse
    // one whose blocks we have just given away.
    tls::set(std::ptr::null_mut());
    // The blocks are given up by `Heap`'s `Drop`, so this is one path, not
    // two: any other way a heap dies reclaims them identically.
    unsafe { drop(Box::from_raw(p)) };
}

impl Drop for Heap {
    /// Give the blocks up. Without this a `Heap` dropped by any route
    /// other than [`ll_thread_exit`] stranded every block it owned —
    /// permanently, since nothing else knows about them — and left the
    /// blocks' `owner` pointing at freed memory. If a later `Heap` were
    /// then placed at the same address, `free`'s owner comparison would
    /// mistake those foreign blocks for its own.
    ///
    /// The crate's own tests are exactly such a route: they build a
    /// `Heap`, allocate, and let it fall out of scope.
    ///
    /// `abandon_all` is idempotent, so the explicit thread-exit path
    /// and this one compose without double-releasing anything.
    fn drop(&mut self) {
        self.abandon_all();
    }
}

thread_local! {
    /// Calls [`ll_thread_exit`] when a thread that allocated unwinds.
    ///
    /// A separate `thread_local!` purely for its destructor, on **every**
    /// target: the heap pointer itself lives in a slot that has none. On
    /// Windows that is a raw TEB slot, chosen precisely to avoid the
    /// module-indirected access a `thread_local!` would cost on every
    /// allocation; elsewhere it is a `Cell<*mut Heap>`, which has no
    /// `Drop` either. Costs one registration per thread, on the cold init
    /// path.
    ///
    /// This used to be `#[cfg(windows)]`, which meant ELF targets had no
    /// automatic reclamation at all: a thread that allocated and then
    /// exited stranded every block it owned, permanently, along with any
    /// later cross-thread free into them. That is the same leak measured
    /// on Windows before the guard existed — 1.7 GiB resident against a
    /// 2.5 MiB live set on `larson.cpp`, which respawns its worker every
    /// ~20 ms by design.
    static EXIT_GUARD: ExitGuard = const { ExitGuard };
}

struct ExitGuard;

impl Drop for ExitGuard {
    fn drop(&mut self) {
        ll_thread_exit();
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
///
/// Also installs the TLS guard that returns this thread's blocks when it
/// exits, so [`ll_thread_exit`] need not be called by hand.
#[unsafe(no_mangle)]
pub extern "C" fn ll_thread_init() {
    // Must precede the first `tls::get()` anywhere: `get` deliberately does
    // not check whether the slot has been reserved (see its doc), so this
    // is the call that establishes that invariant.
    tls::ensure_slot();
    if tls::get_raw().is_null() {
        // Not `Box::new`: its failure mode is `handle_alloc_error`, which
        // aborts — an abort nobody chose and no caller can see coming.
        // A refusal leaves the slot null, which is a state the whole
        // module already models (`thread_heap` documents it), so every
        // allocation path reports null instead of the process dying.
        let layout = std::alloc::Layout::new::<Heap>();
        let heap = unsafe { std::alloc::alloc(layout) } as *mut Heap;
        if heap.is_null() {
            return;
        }
        unsafe { heap.write(Heap::new()) };
        tls::set(heap);
        // Fill the barrier's reserve while a refusal is still reportable:
        // from here the thread's first allocation reports null, and the
        // store barrier has no channel at all
        // (`crate::memory::reserve`).
        let _ = crate::memory::reserve::replenish();
        // `try_with`, not `with`: this can run *during* TLS teardown, when
        // a destructor allocates and self-initializes a heap on a thread
        // whose `EXIT_GUARD` slot is already destroyed. `with` panics
        // there, and the release profile is `panic = "abort"`, so a
        // perfectly ordinary thread exit would take the process down.
        // Failing to register the guard is the right outcome instead: the
        // thread is already exiting, and its blocks are reclaimed by the
        // teardown in progress.
        let _ = EXIT_GUARD.try_with(|_| {});
    }
}

/// This thread's heap, or null if it has never allocated.
///
/// The null case is what lets `ll_malloc`/`ll_c_free` self-initialise on a
/// cold branch instead of making every caller wrap them in an init check.
#[inline]
pub fn thread_heap() -> *mut Heap {
    tls::get_raw()
}

/// Post `ptr` to its block's cross-thread stack, without needing a heap of
/// our own — for a thread that frees something it never could have allocated.
///
/// # Safety
/// `ptr` must be a live slot from some heap's block.
pub unsafe fn free_foreign(ptr: *mut u8) {
    let block = HeapBlockHeader::of_ptr(ptr);
    Heap::free_remote(block, ptr);
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
    fn block_header_halves_are_laid_out_as_the_design_requires() {
        use std::mem::offset_of;

        // `kind` is the pool's tagged-union discriminant: the whole
        // overlay depends on it staying at offset 0 of the block.
        assert_eq!(offset_of!(HeapBlockHeader, private), 0);
        assert_eq!(offset_of!(BlockPrivate, kind), 0);

        // `owner` is read by every `free` to decide whether the slot is
        // local, so it must stay in the same line as the hot private
        // fields. Giving it a line of its own cost +10.8% on
        // `rptest_10k_blocks_40_iters` (measured), because each local
        // free then touched a second line just for the check.
        // Everything the fast paths touch — the counters, the free list,
        // the `available` links, and `owner` — must fit line 0 together.
        // Each field evicted from it costs a miss on a path that runs per
        // allocation or per full ↔ has-room transition; both evictions
        // that were tried measured slower (see `BlockShared`).
        let shared = offset_of!(HeapBlockHeader, shared);
        assert_eq!(
            (shared + size_of::<BlockShared>()).div_ceil(64),
            1,
            "the hot set (private + owner) must fit one cache line, got {} bytes",
            shared + size_of::<BlockShared>()
        );

        // The contended field, by contrast, must be alone on its line, or
        // a cross-thread push steals the line holding `used`/`free`/`bump`
        // (audit `heap.rs:212`).
        let remote = offset_of!(HeapBlockHeader, remote);
        assert_eq!(remote % 64, 0, "remote_free must begin a cache line");
        assert!(remote >= 64, "remote_free must leave the hot line");
        assert_eq!(
            offset_of!(HeapBlockHeader, links) / 64 >= 1,
            true,
            "cold links must not crowd the hot line"
        );

        // The header lives in the block's reserved first line; growing it
        // past that would eat payload and change slots-per-block.
        assert!(
            size_of::<HeapBlockHeader>() <= LINE_SIZE,
            "header must fit the reserved line: {} > {LINE_SIZE}",
            size_of::<HeapBlockHeader>()
        );
    }

    /// A thread that allocates and then exits **without** calling
    /// `ll_thread_exit` must still give its blocks back: the TLS guard is
    /// what makes that automatic, and it is the whole reason the guard
    /// exists.
    ///
    /// Regression for audit H9. The guard used to be `#[cfg(windows)]`, so
    /// on ELF targets nothing reclaimed anything — every worker thread
    /// stranded its blocks forever. This test passes natively on Windows
    /// either way; the one that matters is the Miri run, which executes
    /// the non-Windows path (see `dev/WORKFLOW.md`):
    ///
    /// ```text
    /// MIRIFLAGS="-Zmiri-ignore-leaks" cargo +nightly miri test \
    ///     --target x86_64-unknown-linux-gnu --lib h9_
    /// ```
    #[test]
    fn h9_exiting_thread_returns_its_blocks_without_an_explicit_call() {
        let _g = crate::memory::block_pool::test_guard();
        let pool = BlockPool::global();
        let before = pool.blocks_out();

        for _ in 0..3 {
            std::thread::spawn(|| {
                ll_thread_init();
                let p = unsafe { crate::memory::stdapi::ll_alloc(40, 16) };
                assert!(!p.is_null());
                unsafe { crate::memory::stdapi::ll_free(p) };
                // Deliberately no `ll_thread_exit()`: the guard must do it.
            })
            .join()
            .unwrap();
        }

        assert_eq!(
            pool.blocks_out(),
            before,
            "an exiting thread must not strand its blocks"
        );
    }

    /// A `Heap` that dies by falling out of scope must give its blocks
    /// back, exactly as `ll_thread_exit` does. Before `Drop` existed they
    /// were stranded: nothing else knew about them, so the pool never saw
    /// them again. Revert `impl Drop for Heap` and this test fails on the
    /// final assert.
    #[test]
    fn a_dropped_heap_returns_its_blocks_to_the_pool() {
        let _g = crate::memory::block_pool::test_guard();
        let pool = BlockPool::global();
        let before = pool.blocks_out();

        {
            let mut heap = Heap::new();
            let p = heap.alloc(40);
            assert!(!p.is_null());
            assert!(
                pool.blocks_out() > before,
                "the heap took a block from the pool"
            );
            // Free it so the block is empty: an empty block goes home to
            // the pool, which is what makes the count observable here.
            unsafe { heap.free(p) };
        }

        assert_eq!(
            pool.blocks_out(),
            before,
            "a heap dropped out of scope must not strand its blocks"
        );
    }

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

    /// Several threads freeing into the **same** owner's blocks at once.
    ///
    /// The existing coverage missed this: `many_threads_alloc_free_no_corruption`
    /// has every thread allocate and free on its own heap, so no slot ever
    /// reaches `remote_free`, and `cross_thread_free_is_correct` has exactly
    /// one producer. The multi-producer push had no test at all.
    ///
    /// What would break if it were wrong: `free_remote` is a CAS loop, so a
    /// lost race would drop a slot from the chain, and the owner would
    /// account for fewer slots than were actually freed. That is measured
    /// directly — after every freer has finished, the owner drains its
    /// queues and must report **zero** live slots. Corruption of the slot
    /// contents before the free is caught by the stamp check in each freer.
    ///
    /// It deliberately does *not* assert on the process-global
    /// `blocks_out`. That counter is shared with every other test, so a
    /// block returning late from elsewhere moves it in either direction —
    /// which made this test flaky at ~2 runs in 10 under
    /// `--test-threads=16`, failing on someone else's straggler rather
    /// than on anything it was testing.
    #[test]
    fn many_threads_freeing_into_one_owner_lose_no_slots() {
        let _g = crate::memory::block_pool::test_guard();
        use std::sync::mpsc;
        use std::thread;

        const FREERS: usize = 4;
        const PER: usize = 500;
        const STAMP: u8 = 0xAB;


        let mut txs = Vec::with_capacity(FREERS);
        let mut freers = Vec::with_capacity(FREERS);
        for _ in 0..FREERS {
            let (tx, rx) = mpsc::channel::<usize>();
            txs.push(tx);
            freers.push(thread::spawn(move || {
                ll_thread_init();
                let mut n = 0usize;
                for p in rx {
                    let p = p as *mut u8;
                    assert_eq!(
                        unsafe { *p },
                        STAMP,
                        "slot corrupted before its cross-thread free"
                    );
                    unsafe { with_thread_heap(|h| h.free(p)) };
                    n += 1;
                }
                ll_thread_exit();
                n
            }));
        }

        // This thread owns the blocks. Hand slots out round-robin so all
        // four freers contend on the same block, and keep churning so the
        // drain path runs while their pushes are arriving.
        ll_thread_init();
        unsafe {
            with_thread_heap(|h| {
                for i in 0..(FREERS * PER) {
                    let p = h.alloc(24);
                    p.write(STAMP);
                    txs[i % FREERS].send(p as usize).unwrap();
                    let churn = h.alloc(24);
                    h.free(churn);
                }
            });
        }
        drop(txs);

        let freed: usize = freers.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(freed, FREERS * PER, "every slot was freed exactly once");

        // Every freer is done, so every push has landed. Drain the queues
        // and account: a slot lost in the CAS loop shows up here as a live
        // slot nobody holds.
        let live = unsafe { with_thread_heap(|h| h.live_slots_after_collect()) };
        assert_eq!(
            live, 0,
            "the owner lost track of a slot freed from another thread"
        );
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
