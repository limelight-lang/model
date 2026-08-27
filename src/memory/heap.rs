//! Small-object heap: individually-freeable allocations for long-lived
//! objects, unlike the arena, which frees only in bulk at reset.
//!
//! The mimalloc model, chosen after studying jemalloc, mimalloc and
//! snmalloc: one 64 KB block per size class carved into fixed-size slots,
//! pointer to block by mask (`ptr & !BLOCK_MASK`) rather than through a
//! radix tree or pagemap, an intrusive free list per block beside a bump
//! cursor for virgin slots, and a fully-free block returned to the global
//! pool. `alloc` pops the list head or bump-carves when the list is empty,
//! `free` pushes the slot back, and both are O(1) and branch-only.
//!
//! The heap runs **twice per thread** over this same code: a raw heap
//! (`BLOCK_KIND_HEAP`) for C-ABI buffers and an entity heap
//! (`BLOCK_KIND_ENTITY`) for GC entities, as one [`ThreadHeaps`] pair
//! behind the TLS slot. That segregation, the bytes-8-15 free-list link,
//! the commissioning zero pass and [`for_each_entity_slot`] exist for the
//! collector: a trace strides an entity block slot by slot and reads each
//! header, so a slot must never hold a C buffer and a free slot must read
//! as refcount 0. They were built for `rc-walk` and outlive it —
//! `rc-cycle` reaches its shadow rows by the same arithmetic over the
//! same blocks (`rfc/model/gc/rc-cycle.md`, "Where the shadow count
//! lives").
//!
//! What the rejected alternatives cost, why the cross-thread stack is per
//! block rather than per heap, why `used` is written by the owner alone,
//! and what abandonment at thread exit buys are in
//! `docs/memory-manager.md` — "Heap: small objects", "Cross-thread free",
//! "Thread exit: abandonment and adoption" — the document `memory/mod.rs`
//! declares this module implements.

use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use crate::journal::kinds::journal_event;
use crate::memory::block_pool::{
    BLOCK_KIND_ENTITY, BLOCK_KIND_HEAP, BLOCK_MASK, BLOCK_PAYLOAD, BLOCK_SIZE, BLOCKS_PER_REGION,
    BlockHeader, BlockPool, LINE_SIZE,
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

/// A free slot threads a list through its own bytes 8–15 — zero metadata
/// overhead. Used both for a block's own free list and for `remote_free`,
/// the cross-thread MPSC staging stack. Every size class is ≥ 16 bytes,
/// so a slot always has room for the link, and bytes 8–15 sit on the same
/// cache line as bytes 0–7, so the measured argument for the in-slot list
/// (`rfc/model/memory/heap-slot-allocation.md`) is untouched.
///
/// Bytes 0–7 are deliberately **never written by the allocator**: in an
/// entity block they keep the dead entity's final header — refcount 0,
/// which is how a trace tells a free slot from a live entity
/// (`docs/memory-manager.md`, "Heap: small objects"). The link lived in
/// bytes 0–7 until the entity heap needed that test; one offset for both
/// populations keeps a single code path.
#[repr(C)]
struct FreeSlot {
    /// The preserved first 8 bytes: a dead entity's final header in an
    /// entity block, stale payload in a raw block. Never accessed.
    header: u64,
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
    /// Indexed by population first — `[0]` raw blocks (`BLOCK_KIND_HEAP`),
    /// `[1]` entity blocks (`BLOCK_KIND_ENTITY`) — then by size class.
    /// Separate lists because adoption must never move a block across
    /// populations: a raw heap handing out entity-block slots would put C
    /// buffers where a trace reads headers.
    heads: [[*mut HeapBlockHeader; NUM_CLASSES]; 2],
}

// SAFETY: the pointers are block headers in never-unmapped pool regions;
// the mutex is what serialises access to the chain.
unsafe impl Send for Abandoned {}

static ABANDONED: Mutex<Abandoned> = Mutex::new(Abandoned {
    heads: [[std::ptr::null_mut(); NUM_CLASSES]; 2],
});

/// The population index a block kind maps to in [`Abandoned::heads`].
#[inline]
fn population_index(block_kind: u32) -> usize {
    (block_kind == BLOCK_KIND_ENTITY) as usize
}

/// The owner-private half of the header: only the thread named by
/// [`BlockShared::owner`] may touch it. Split out so that borrowing it
/// is an *honest* exclusive claim — `&mut HeapBlockHeader` was not, since
/// it also covered the atomics every other thread reads, and the model
/// counts taking a reference as an access (audit `heap.rs:647`).
/// The fields the allocation fast path touches, kept together so they
/// share one cache line with [`BlockShared::owner`] (see that type).
#[repr(C)]
struct BlockPrivate {
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

/// The collector's three words about a block, alone on the last cache
/// line of the block's reserved header line.
///
/// **Out here rather than inside [`HeapBlockHeader`] because the
/// collector writes `shadow`**, and a write into the header's first line
/// would steal it from under the owner's bump cursor and free list on
/// every block a trace touches. The two words beside it are written once
/// at commissioning and never again, so the line the collector dirties
/// carries nothing the owner reads (`rfc/model/gc/rc-cycle.md`, "Where
/// the shadow count lives").
///
/// Written only for `BLOCK_KIND_ENTITY` blocks. The other two
/// populations a trace enters — retained and large-entity — carry their
/// rows elsewhere (`crate::cycle::row`), and a raw heap block's tail is
/// left as the pool handed it over.
#[repr(C, align(64))]
struct BlockCollector {
    /// This block's shadow row array, or null while no collection has
    /// touched the block. The one word of a block header a non-owner
    /// writes: a collection reserves the array at its first touch of the
    /// block and nulls this again at the end of the collection, on the
    /// abort path as well (`crate::cycle::arena::ShadowArena::meet`).
    shadow: AtomicPtr<u8>,
    /// `2^32 / stride + 1` for this block's size class, so a row lookup
    /// is a multiply and a shift rather than a division
    /// ([`reciprocal_for`]). Written once at commissioning and published
    /// by the kind's release store, like every other header word.
    reciprocal: AtomicU32,
    /// The collector's own copy of the size class index, duplicating
    /// [`HeapBlockHeader::size_class`] on purpose:
    /// [`collector_block_slots`] sizes a block's row array at
    /// `BLOCK_PAYLOAD / stride`, and taking the stride from here is what
    /// keeps the whole lookup on this line.
    size_class: AtomicU32,
}

/// Where a block's collector triple begins: immediately past the header,
/// which is 192 bytes today because [`BlockRemote`] aligns the header to
/// 64. Tied to the header's size rather than written as 192, so a header
/// that grows moves the triple instead of overlapping it.
const COLLECTOR_TRIPLE_OFFSET: usize = size_of::<HeapBlockHeader>();

const _: () = {
    assert!(
        COLLECTOR_TRIPLE_OFFSET % 64 == 0,
        "the triple must begin a cache line of its own"
    );
    assert!(
        COLLECTOR_TRIPLE_OFFSET + size_of::<BlockCollector>() <= LINE_SIZE,
        "the triple must fit the block's reserved header line"
    );
};

/// The collector triple of `block`.
///
/// # Safety
/// `block` must be the header of a live 64 KiB block, since the triple
/// is reached by offset from it. The words carry meaning only for a
/// `BLOCK_KIND_ENTITY` block, which is the only kind [`Heap::refill`]
/// writes them for.
#[inline]
unsafe fn block_collector(block: *mut HeapBlockHeader) -> *mut BlockCollector {
    unsafe { (block as *mut u8).add(COLLECTOR_TRIPLE_OFFSET) as *mut BlockCollector }
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
    /// The pool's tagged-union discriminant, at offset 0 of the block
    /// because a size-less free finds it by masking an address.
    ///
    /// **Outside [`BlockPrivate`], and that placement is the invariant.**
    /// The collector reads this word for every block of every region
    /// while the owner runs, so an owner that borrows it exclusively —
    /// and every method here takes `&mut (*block).private` — races that
    /// read: a retag asserts uniqueness over its whole range, and unlike
    /// a shared reference it does not stop at an `UnsafeCell`. Splitting
    /// the word out is what makes the borrow legal, the same medicine
    /// `BlockShared` and `BlockRemote` were split out with
    /// (`dev/DECISIONS.md`, "the block header is split by access rule, not
    /// by topic").
    kind: AtomicU32,
    /// The block's size class, written once at commissioning. Out here
    /// with `kind` rather than in [`BlockPrivate`] for the same reason —
    /// the owner's `&mut` over `private` must not cover a word another
    /// thread reads, and `describe_slot` reads this one from wherever it
    /// is called. A collection reads [`BlockCollector::size_class`]
    /// instead.
    size_class: AtomicU32,
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
/// Deliberately not always-on: these are stores on the hot path, and a
/// timing run built with them is measuring the counters as much as the
/// allocator.
///
/// Relaxed atomics, not `static mut`: `REMOTE_FREES` is bumped on the
/// cross-thread free path, which is cross-thread by definition, so plain
/// stores there were a data race — a real one, not a formality, and one
/// that a probe build is exactly the wrong place to debug. Relaxed adds
/// cost nothing beyond the store on any target we care about, and this
/// module only exists in a build that already accepts the counters.
#[cfg(feature = "probe-counters")]
pub mod probe {
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Entries into `Heap::alloc`. Each one examines exactly one block, so
    /// this counts block examinations, **including** re-entries made by the
    /// cold paths via `alloc_class`.
    pub static ALLOC_ENTRIES: AtomicU64 = AtomicU64::new(0);
    /// Re-entries specifically. `ENTRIES - RETRIES` is the number of real
    /// allocations, and `ENTRIES / (ENTRIES - RETRIES)` is blocks walked per
    /// allocation — 1.0 means every alloc found room in the first block it
    /// looked at.
    pub static ALLOC_RETRIES: AtomicU64 = AtomicU64::new(0);
    pub static UNLINK_CALLS: AtomicU64 = AtomicU64::new(0);
    pub static LINK_CALLS: AtomicU64 = AtomicU64::new(0);
    /// Frees that took the cross-thread path (block owned by someone else,
    /// or abandoned) rather than the local one.
    pub static REMOTE_FREES: AtomicU64 = AtomicU64::new(0);
    /// Frees total.
    pub static FREES: AtomicU64 = AtomicU64::new(0);
    /// Blocks adopted from the abandoned list.
    pub static ADOPTED: AtomicU64 = AtomicU64::new(0);

    /// Write `[entries, retries, unlinks, links, remote_frees, frees,
    /// adopted]` to `out`.
    ///
    /// # Safety
    /// `out` must point to writable space for seven `u64`s. Single-threaded
    /// probe use only.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn ll_probe_counters(out: *mut u64) {
        let all = [
            &ALLOC_ENTRIES,
            &ALLOC_RETRIES,
            &UNLINK_CALLS,
            &LINK_CALLS,
            &REMOTE_FREES,
            &FREES,
            &ADOPTED,
        ];
        for (i, c) in all.iter().enumerate() {
            unsafe { *out.add(i) = c.load(Ordering::Relaxed) };
        }
    }
}

/// Bump a `probe` counter, or compile to nothing without the feature.
macro_rules! probe_count {
    ($name:ident) => {
        #[cfg(feature = "probe-counters")]
        {
            probe::$name.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
    /// Live slots this heap took on by adopting abandoned blocks — objects
    /// belonging to threads that are already gone, which this heap will
    /// never free and must not be blamed for.
    ///
    /// Test-only, and it exists because the accounting oracle
    /// [`Heap::live_slots_after_collect`] cannot otherwise tell "a free the
    /// owner lost" from "a dead thread's live object we inherited".
    #[cfg(test)]
    adopted_live: u32,
    /// The block kind this heap stamps at refill and adopts by:
    /// `BLOCK_KIND_HEAP` for raw C-ABI allocations, `BLOCK_KIND_ENTITY`
    /// for GC entities. Two populations of the same allocator, never
    /// mixed (`docs/memory-manager.md`, "Heap: small objects").
    block_kind: u32,
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

impl Heap {
    /// A heap serving raw allocations (`BLOCK_KIND_HEAP` blocks).
    pub fn new() -> Self {
        Self::with_kind(BLOCK_KIND_HEAP)
    }

    /// A heap serving GC entities (`BLOCK_KIND_ENTITY` blocks): same
    /// allocator, segregated block population, zeroed slot headers at
    /// commissioning.
    pub fn new_entity() -> Self {
        Self::with_kind(BLOCK_KIND_ENTITY)
    }

    fn with_kind(block_kind: u32) -> Self {
        Heap {
            available: [std::ptr::null_mut(); NUM_CLASSES],
            empty_reserve: [std::ptr::null_mut(); NUM_CLASSES],
            owned: [std::ptr::null_mut(); NUM_CLASSES],
            #[cfg(test)]
            adopted_live: 0,
            block_kind,
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
    /// **A frameless leaf**, with everything rare in separate `#[cold]`
    /// `#[inline(never)]` tails: refilling from `BlockPool`, draining
    /// cross-thread frees, walking past a full block. A body holding those
    /// calls needs a stack frame and callee-saved spills, and the fast path
    /// pays for them on every call although only the rare branch needs them
    /// (`rfc/model/memory/heap-slot-allocation.md`, "Fix 5c — Fast path
    /// split out of the slow paths"). `#[cold]` is what tells LLVM the
    /// branch is rare; `refill` runs about 0.00003 times per alloc.
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

    /// Reserve up to `count` cells of `size`, best-effort contiguous
    /// (`rfc/model/memory/bulk-operations.md`). The request is a wish;
    /// this method decides: it serves from blocks already on hand —
    /// each block's virgin bump tail first (the contiguous part), then
    /// its free list — and draws at most **one** fresh block through
    /// the ordinary cold path. It never forces region growth for a
    /// speculative reservation. Returns how many cells were reserved;
    /// zero is an answer, not an error.
    ///
    /// A reserved cell is accounted exactly like an allocation
    /// (`used`, unlink-when-full, the empty-reserve fix-up), so a block
    /// carrying reserved cells can never look empty. The cell's header
    /// still reads its final `rc 0` (or virgin zero), so the walker's
    /// occupancy test skips it until construction publishes a header.
    /// Unused cells go back through the ordinary free path.
    ///
    /// Returns `(reserved, contiguous_len)`: how many cells were
    /// reserved, and how many of the *leading* ones form one adjacent
    /// run at the class's slot stride.
    pub fn reserve_cells(
        &mut self,
        size: usize,
        count: usize,
        out: &mut [*mut u8],
    ) -> (usize, usize) {
        if size > MAX_SMALL || count == 0 || out.len() < count {
            return (0, 0);
        }

        let ci = unsafe { *CLASS_LUT.get_unchecked((size + 15) >> 4) as usize };
        let class_size = unsafe { *SIZE_CLASSES.get_unchecked(ci) };
        let mut n = 0;
        let mut drew_fresh_block = false;
        while n < count {
            let block = unsafe { *self.available.get_unchecked(ci) };
            if block.is_null() {
                // The one permitted draw, through the ordinary cold
                // path (collect, adopt, or one pool block); its single
                // cell joins the reservation and the loop continues on
                // whatever block it installed.
                if drew_fresh_block {
                    break;
                }

                drew_fresh_block = true;
                let p = self.alloc(size);
                if p.is_null() {
                    break;
                }

                out[n] = p;
                n += 1;
                continue;
            }

            let b = unsafe { &mut (*block).private };
            if b.used == 0 && self.empty_reserve[ci] == block {
                self.empty_reserve[ci] = std::ptr::null_mut();
            }

            // Virgin tail first — the adjacent run.
            while n < count && b.bump < b.slots {
                let idx = b.bump as usize;
                b.bump += 1;
                let base = (block as *mut u8).wrapping_add(LINE_SIZE);
                out[n] = base.wrapping_add(idx * class_size);
                b.used += 1;
                n += 1;
            }

            // Then the block's free list.
            while n < count {
                let slot = b.free;
                if slot.is_null() {
                    break;
                }

                b.free = unsafe { (*slot).next };
                out[n] = slot as *mut u8;
                b.used += 1;
                n += 1;
            }

            if b.free.is_null() && b.bump >= b.slots {
                self.unlink(ci, block);
            } else {
                break; // room remains: the request is satisfied
            }
        }

        let mut contiguous_len = usize::from(n > 0);
        while contiguous_len < n
            && out[contiguous_len] == out[contiguous_len - 1].wrapping_add(class_size)
        {
            contiguous_len += 1;
        }

        (n, contiguous_len)
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
            let pending = unsafe {
                !(*block)
                    .remote
                    .remote_free
                    .load(Ordering::Relaxed)
                    .is_null()
            };

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
        let population = population_index(self.block_kind);
        let block = {
            let mut list = ABANDONED.lock().unwrap();
            let head = list.heads[population][ci];
            if head.is_null() {
                return false;
            }

            list.heads[population][ci] = unsafe { (*head).links.owned_next };
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
        self.collect_remote(block);

        let (used, free, bump, slots) = unsafe {
            (
                (*block).private.used,
                (*block).private.free,
                (*block).private.bump,
                (*block).private.slots,
            )
        };

        // Counted after the collect above, so slots freed while the block
        // was ownerless are not mistaken for live ones. What remains
        // belongs to a thread that has already exited, so nothing will
        // ever free it and the oracle must not read it as a lost free.
        #[cfg(test)]
        {
            self.adopted_live += used;
        }

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
        let population = population_index(self.block_kind);
        let mut list = ABANDONED.lock().unwrap();
        for ci in 0..NUM_CLASSES {
            let mut block = self.owned[ci];
            while !block.is_null() {
                let next = unsafe { (*block).links.owned_next };

                // Collect first: slots freed cross-thread may make it empty,
                // and an empty block is worth more to the pool than to the
                // abandoned list.
                self.collect_remote_locked(block);
                unsafe {
                    (*block)
                        .shared
                        .owner
                        .store(std::ptr::null_mut(), Ordering::Release)
                };

                let b = unsafe { &mut (*block).private };
                if b.used == 0 {
                    unsafe {
                        crate::memory::block_pool::store_block_kind(&raw const (*block).kind, 0)
                    };

                    BlockPool::global().put(block as *mut BlockHeader);
                } else {
                    b.linked = false;
                    b.next = std::ptr::null_mut();
                    b.prev = std::ptr::null_mut();
                    let l = unsafe { &mut (*block).links };
                    l.owned_prev = std::ptr::null_mut();
                    l.owned_next = list.heads[population][ci];
                    list.heads[population][ci] = block;
                }

                block = next;
            }

            self.owned[ci] = std::ptr::null_mut();
        }

        self.available = [std::ptr::null_mut(); NUM_CLASSES];
        self.empty_reserve = [std::ptr::null_mut(); NUM_CLASSES];
    }

    /// [`collect_remote`](Self::collect_remote) without touching `self` — for use while the
    /// abandoned list's lock is held.
    fn collect_remote_locked(&self, block: *mut HeapBlockHeader) {
        // Takes the raw block: it needs both halves, and they must be
        // reached separately — the atomic through a shared reference, the
        // private fields through an exclusive one.
        let head = unsafe {
            (*block)
                .remote
                .remote_free
                .swap(std::ptr::null_mut(), Ordering::Acquire)
        };

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

    /// Free a slot from [`alloc`](Self::alloc). If this thread owns the block it is a
    /// cheap local free; otherwise the slot is posted to the owner's
    /// lock-free `remote_free` stack.
    ///
    /// Split fast/cold for the same codegen reason as [`alloc`](Self::alloc)
    /// — see its doc. The owning-thread push is the whole fast path; the
    /// cross-thread hand-off and the block-emptied bookkeeping are cold
    /// tails.
    ///
    /// # Safety
    /// `ptr` must be a live allocation from some heap and not freed yet.
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

        let ci = unsafe { (*block).size_class.load(Ordering::Relaxed) } as usize;
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
        let head = unsafe {
            (*block)
                .remote
                .remote_free
                .swap(std::ptr::null_mut(), Ordering::Acquire)
        };

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
            crate::memory::block_pool::store_block_kind(&raw const (*block).kind, 0);
        }

        BlockPool::global().put(block as *mut BlockHeader);
    }

    /// Test oracle: live slots this heap still accounts for **and is
    /// responsible for**, after draining every block's cross-thread queue.
    ///
    /// Exists because the process-global `blocks_out` is the wrong
    /// instrument for "did the owner account for every free". It is
    /// shared, so another test's block returning late moves it in either
    /// direction, and it only reacts once a block empties completely.
    /// This counts the thing directly.
    ///
    /// Slots inherited by [`Heap::adopt`] are subtracted. An abandoned
    /// block is abandoned *because* it still holds live objects, and the
    /// thread that owned them is gone, so nobody will ever free them. They
    /// are live, they are legitimately ours, and they are not a lost free.
    /// Counting them made `many_threads_freeing_into_one_owner_lose_no_slots`
    /// flaky: it passed alone and failed under `--test-threads=32`, where
    /// an earlier test had left blocks on the abandoned list for this heap
    /// to pick up mid-run (observed: adoptions of blocks with `used` 1, 3
    /// and 147, and a failure reporting exactly the inherited count).
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

        total.saturating_sub(self.adopted_live)
    }

    /// Take a fresh block from the pool, stamp its header, and link it as
    /// available. For a raw heap that is O(1) and touches nothing but the
    /// header line: an empty free list plus `bump = 0` already means
    /// "every slot virgin", so there is no side allocation and no
    /// per-slot initialization at all — unlike both the eager free-list
    /// threading and the bitmap this replaced (see the module doc for why
    /// the bitmap lost). An **entity** heap pays one pass over the block
    /// on top of that, and the body says what buys it.
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

        // Commissioning rule for entity blocks: every slot's first 8
        // bytes must read refcount 0 until an entity is published into
        // it, or a trace striding the block meets bytes that lie about
        // occupancy. Regions come from the process allocator and blocks recycle
        // through the pool, so provenance is never a known-fresh OS commit
        // — the explicit 8-bytes-per-slot pass always runs here. Cold path
        // (once per block), ≤ 4080 stores at the smallest class.
        //
        // Raw blocks skip it: nothing ever reads their dead slots, and
        // the "no per-slot initialization" property they keep instead is
        // measured (module doc: why the bitmap lost).
        if self.block_kind == BLOCK_KIND_ENTITY {
            let base = BlockHeader::payload_start(block as *mut BlockHeader);
            for i in 0..slots as usize {
                unsafe { (base.add(i * class_size) as *mut u64).write(0) };
            }
        }

        // The kind is published LAST and through `store_block_kind`,
        // whose store is a release: a collector reading this block must
        // not see "entity" before the size class, cursor and zeroed
        // slots behind it are visible.
        //
        // Field by field rather than one struct store, for the reason
        // `large_entity::commission` writes its header the same way: a
        // struct store covers `kind` too, and the collector loads that
        // word — of **every** block in every carved region — with an
        // acquire, so a plain store to it is a data race by the model
        // however little the value changes. This one wrote 0 over the
        // pool's 0.
        unsafe {
            let private = &raw mut (*block).private;
            // Relaxed, and not through `store_block_kind`: that helper
            // is the kind's write path and says so, while this word is
            // published by the release store below like every other
            // field of the header.
            (*block).size_class.store(ci as u32, Ordering::Relaxed);
            (&raw mut (*private).used).write(0);
            (&raw mut (*private).slots).write(slots);
            (&raw mut (*private).free).write(std::ptr::null_mut());
            (&raw mut (*private).bump).write(0);
            (&raw mut (*private).linked).write(false);
            (&raw mut (*private).next).write(std::ptr::null_mut());
            (&raw mut (*private).prev).write(std::ptr::null_mut());
            (&raw mut (*block).shared).write(BlockShared {
                owner: AtomicPtr::new(self.id()),
            });

            (&raw mut (*block).remote).write(BlockRemote {
                remote_free: AtomicPtr::new(std::ptr::null_mut()),
            });

            (&raw mut (*block).links).write(BlockLinks {
                owned_next: std::ptr::null_mut(),
                owned_prev: std::ptr::null_mut(),
            });

            // Raw blocks skip it: no trace enters one.
            //
            // **What makes any of these stores sound is the publication
            // below**, not their width: until `store_block_kind` runs, no
            // collector can reach this block, so nothing races the line.
            // `kind` is the exception the helper exists for, and it is
            // read for every block of every carved region whether the
            // block is published or not.
            //
            // Field by field all the same, and the reason is `shadow`:
            // it is the one word of a block header a **non-owner** later
            // writes — a collection stamps it at the block's first touch
            // and nulls it at its sweep — so the narrow store is the
            // shape that stays right if this ever moves after the
            // publication.
            if self.block_kind == BLOCK_KIND_ENTITY {
                let triple = block_collector(block);
                (&raw mut (*triple).reciprocal).write(AtomicU32::new(reciprocal_for(class_size)));
                (&raw mut (*triple).size_class).write(AtomicU32::new(ci as u32));
                (&raw mut (*triple).shadow).write(AtomicPtr::new(std::ptr::null_mut()));
            }

            crate::memory::block_pool::store_block_kind(&raw const (*block).kind, self.block_kind);
        }

        self.own(ci, block);
        self.link(ci, block);
        block
    }

    /// Re-link a block that was full and has just had a slot freed.
    ///
    /// Deliberately **not** at the head. `alloc` serves from the head, so a
    /// just-unfulled block placed there becomes an allocation point with
    /// one free slot: the next alloc drains it and it is full again. Behind
    /// the head it accumulates more frees before `alloc` reaches it, and
    /// linking at the head instead costs 21.7% of throughput
    /// (`rfc/model/memory/heap-slot-allocation.md`, "Why `relink_unfull` is
    /// worth its cost, measured", and "What churn actually costs" for the
    /// rule behind it: the rate of block switches is what costs, not the
    /// bookkeeping around them).
    ///
    /// `#[inline(never)]` but **not** `#[cold]`. Out of line so `free`'s
    /// body stays short enough to inline into `ll_free`; not cold, because
    /// that asserts to LLVM that the branch is rare, and a workload
    /// churning blocks across the full ↔ has-room boundary takes it
    /// constantly.
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
// On windows-msvc the slot is read from the TEB's inline `TlsSlots` array
// with one instruction, `gs:[0x1480 + slot*8]`, through inline `asm!`.
// `TlsAlloc` runs once per process purely to reserve a slot number the OS
// promises to nobody else; the reads and writes never call it again. It
// falls back to `TlsGetValue`/`TlsSetValue` if the reserved slot lands
// outside the first 64 fast slots, which is practically never but is
// correct when it happens. What the compiler-emitted `thread_local!` costs
// instead, and why the Win32 API is no cheaper, are in
// `rfc/model/memory/heap-slot-allocation.md`, "Fix 3 — Fast TLS
// (Windows)".
//
// Elsewhere the portable `thread_local!` stays: ELF `__thread` is already
// a single `%fs`-relative load with no module table.

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
        // Fault injection, tests only. It sits here rather than in `init`
        // because `init` runs once per process: by the time a test asks,
        // the slot is long since reserved, and what needs simulating is
        // "there is no slot", not "reserving one failed".
        #[cfg(test)]
        if FORCE_TLS_FAILURE.load(Ordering::Relaxed) != 0 {
            return UNINIT;
        }

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

    /// Tests only: pretend the process has no TLS slots left.
    #[cfg(test)]
    pub(crate) static FORCE_TLS_FAILURE: AtomicU32 = AtomicU32::new(0);

    #[cold]
    fn init() -> u32 {
        let slot = unsafe { TlsAlloc() };
        // TlsAlloc returns TLS_OUT_OF_INDEXES (u32::MAX) on failure, which is
        // also our UNINIT sentinel: storing it would make every later `get`
        // treat the slot as uninitialised and read a bad TEB offset. So the
        // failure is reported instead — `STATE` stays UNINIT, `set` refuses,
        // `ll_thread_init` leaves the thread without a heap, and the first
        // allocation on it returns null. A process that cannot spare one TLS
        // slot is in trouble either way, but that is the caller's news to
        // hear, not ours to end the process over.
        if slot == u32::MAX {
            return UNINIT;
        }

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
    /// False when no slot could be reserved: there is nowhere to put the
    /// pointer, and the caller must not act as though there were.
    #[must_use]
    pub fn set(p: *mut Heap) -> bool {
        let s = state_or_init();
        if s == UNINIT {
            return false;
        }

        if s & FALLBACK_BIT == 0 {
            unsafe { write_gs_qword(s, p as u64) };
        } else {
            unsafe { TlsSetValue(s & !FALLBACK_BIT, p as *mut core::ffi::c_void) };
        }

        true
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
    /// Always succeeds here; the Windows path can refuse (no TLS slot).
    #[must_use]
    pub fn set(p: *mut Heap) -> bool {
        THREAD_HEAP.with(|c| c.set(p));
        true
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
    // From here this thread may not free anything whose release can be
    // parked (`thread_may_free`), and a structure built between here and
    // the end is disposed by this sequence rather than by the guard.
    EXIT_PHASE.with(|phase| phase.set(ExitPhase::Exiting));
    journal_event!(crate::journal::kinds::KIND_THREAD_EXIT, 0, 0, 0);

    // Thread exit owns the order in which this thread's runtime state
    // goes away, and owns it *explicitly*, because TLS destructor order
    // is unspecified and nothing here may rest on it.
    //
    // What the registration order is, since the answer is easy to get
    // backwards: `ll_thread_init` calls `reserve::replenish` before it
    // registers `EXIT_GUARD`, and that call touches both of this crate's
    // `thread_local!`s that have drop glue — the barrier reserve and the
    // pool's thread cache. On glibc, which destroys in reverse
    // registration order, this guard runs first of the three and both are
    // alive while it runs.
    //
    // Nothing below is built on that. Every structure this function
    // disposes is a no-drop-glue cell freed by hand (`dev/DECISIONS.md`,
    // "thread exit owns the order its per-thread state dies in"), and the
    // two that do have drop glue are reached through `try_with` on every
    // non-test path, so a destroyed slot is reported rather than panicked
    // on. A panic here cannot unwind out of a destructor, and under
    // `panic = "abort"` it ends the process.

    // 1. Static blocks let go of their roots (A6). The only step that
    //    runs user code, so it goes first, while every structure the
    //    `__destruct` bodies below it may touch is still alive — heaps,
    //    context, weak table.
    crate::static_block::run_thread_exit_teardown();

    // 2. The weak table, after every death that could still need a row.
    //    `weak.rs` pinned this position against the day static-block
    //    teardown existed; this is that day.
    crate::weak::dispose();

    // 3. The buffer arena last of the disposals, because every step above can
    //    still free a buffer into it: a static block's teardown reaches
    //    `string_die`, which returns a dynamic string's payload here, and
    //    the parked backlog's flush routes payload frees the same way.
    //    Disposing earlier is not caught — a later free would build a
    //    second arena through the lazy path and leak it. The blocks it
    //    hands back go to the process-global pool, which outlives every
    //    thread, so nothing below needs it.
    crate::memory::buffer_arena::dispose();

    // 4 is the last act of this function rather than the fourth of four,
    //    and `retire_the_journal` says why.
    let p = tls::get_raw();
    if p.is_null() {
        retire_the_journal();
        return;
    }

    // Clear the slot first: `abandon_all` must not be re-entered, and any
    // allocation after this point must build a fresh heap rather than reuse
    // one whose blocks we have just given away.
    // Clearing cannot fail: the slot exists, or `get_raw` above would
    // have reported null and returned.
    let cleared = tls::set(std::ptr::null_mut());
    debug_assert!(cleared, "the slot existed a line ago");
    // The blocks are given up by `Heap`'s `Drop` (both instances of the
    // pair), so this is one path, not two: any other way a heap dies
    // reclaims them identically.
    unsafe { drop(Box::from_raw(p as *mut ThreadHeaps)) };

    retire_the_journal();
}

/// The last act of [`ll_thread_exit`] on either path: the journal's ring
/// is handed to the registry, which keeps it readable after this thread
/// is gone (`journal.rs`).
///
/// **Last, not fourth of four.** Everything above it is worth journaling,
/// the block frees of the heap teardown included — those are a default
/// event kind — and a ring retired before them would be closed while its
/// owner still had events to raise. The position costs nothing: the ring
/// stays in this thread's cell until the call, so no second ring can open
/// under it, and an evicted ring goes back to the process-global pool
/// rather than through a heap this function has just dropped.
///
/// The phase is stamped here rather than by the caller, so that the two
/// paths out of the exit cannot disagree about it.
fn retire_the_journal() {
    // The two per-thread caches are handed back here rather than by their
    // own destructors, which run *after* this function wherever TLS is
    // destroyed in reverse registration order — `ll_thread_init` touches
    // both before it arms the guard, so on glibc the guard goes first.
    // A block returning to the pool is a default event kind
    // (`dev/design/debug-modes.md` §9.5), and after the retirement below
    // there is no ring to put one in. The reserve first: its route back
    // is `BlockPool::put`, which can push into the cache the next call
    // flushes.

    crate::memory::reserve::drain();
    crate::memory::critical::drain();
    crate::memory::block_pool::drain_thread_cache();

    crate::journal::retire_thread_ring();
    EXIT_PHASE.with(|phase| phase.set(ExitPhase::Exited));
}

/// The two heap instances a thread owns: raw C-ABI allocations and GC
/// entities — the same allocator over two segregated block populations
/// (`docs/memory-manager.md`, "Heap: small objects").
///
/// `#[repr(C)]` with `raw` first, which the layout depends on: the TLS slot
/// stores a pointer to this pair, and [`thread_heap`] hands the same
/// pointer out as the raw heap — the `ll_malloc` hot path pays no offset
/// and no second load for the split.
#[repr(C)]
pub struct ThreadHeaps {
    raw: Heap,
    entity: Heap,
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
        let layout = std::alloc::Layout::new::<ThreadHeaps>();
        let heap = unsafe { std::alloc::alloc(layout) } as *mut ThreadHeaps;
        if heap.is_null() {
            return;
        }

        unsafe {
            heap.write(ThreadHeaps {
                raw: Heap::new(),
                entity: Heap::new_entity(),
            })
        };

        // The slot stores the pair's address as `*mut Heap`: with
        // `repr(C)` and `raw` first, that IS the raw heap's address.
        if !tls::set(heap as *mut Heap) {
            // No TLS slot to hold it: hand the memory straight back and
            // leave the thread heapless, which the allocation paths
            // already report as null.
            unsafe { std::ptr::drop_in_place(heap) };
            unsafe { std::alloc::dealloc(heap as *mut u8, layout) };
            return;
        }

        // Fill the barrier's reserve while a refusal is still reportable:
        // from here the thread's first allocation reports null, and the
        // store barrier has no channel at all
        // (`crate::memory::reserve`).
        let _ = crate::memory::reserve::replenish();
        // And the collection's, on the same terms: a thread whose
        // allocation fails has no other door to the memory a trace needs
        // (`crate::memory::critical`).
        let _ = crate::memory::critical::replenish();
        // Failing to arm the guard is the right outcome: the thread is
        // already exiting, and its blocks are reclaimed by the teardown in
        // progress.
        // The branch below is entered once per *life* of a thread, so a
        // pool thread running init and exit per task enters it again and
        // journals into a ring of a new identity rather than reopening the
        // one it retired. Two conditions guard it. The exit guard, because
        // the guard is what retires the ring, and a ring opened on a
        // thread whose retirement nothing will run stays on the live list
        // for the life of the process. And no exit in progress, because a
        // heap rebuilt inside an exit is that exit repairing itself rather
        // than a new life: lowering the phase there would tell
        // `thread_may_free` that this thread may free again, inside the
        // sequence that is disposing what such a free would reach
        // (`dev/DECISIONS.md`, "the journal is complete to the exit's
        // last act and honest past it").
        if !thread_exit_running() && exit_guard_armed() {
            EXIT_PHASE.with(|phase| phase.set(ExitPhase::Live));
            crate::journal::reopen_thread();
        }

        // After the reopen, so a pool thread's second life records its
        // start in the ring of that life rather than in the one it
        // retired.
        journal_event!(crate::journal::kinds::KIND_THREAD_START, 0, 0, 0);
    }
}

/// Where a thread stands with respect to its own teardown.
///
/// Three states rather than a flag, because the three answer differently
/// and a boolean made two of them one: a heap rebuilt in the middle of an
/// exit is the teardown repairing itself, and a heap built after one is a
/// new life on the same OS thread.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitPhase {
    /// No exit has begun. The thread may free, and what retires its
    /// per-thread structures is the guard.
    Live,
    /// Inside [`ll_thread_exit`]. Its own steps are the teardown, so
    /// anything built here is disposed by the sequence in progress — and
    /// nothing may be freed, the structures a free reaches being disposed
    /// within it.
    Exiting,
    /// The exit has run. Nothing will dispose what is built now, and
    /// nothing may be freed either.
    Exited,
}

thread_local! {
    /// This thread's [`ExitPhase`]. A `Cell` with no drop glue, under the
    /// rule every per-thread structure reachable from thread exit obeys
    /// (`dev/DECISIONS.md`, "thread exit owns the order its per-thread
    /// state dies in").
    static EXIT_PHASE: std::cell::Cell<ExitPhase> = const { std::cell::Cell::new(ExitPhase::Live) };
}

/// Whether this thread may still hand memory back.
///
/// `false` from the moment [`ll_thread_exit`] begins, and afterwards: the
/// sequence disposes the structures a free reaches, and a free arriving
/// after its own step has run has nowhere to land. A caller holding
/// memory to give back at that point must leave it for another thread.
///
/// The refusal was written against a parked free, whose backlog the exit
/// disposed and nothing rebuilt. Nothing parks today, so the refusal is
/// wider than the case that produced it; it stays because S34.3 and S36.2
/// bring both parking windows back (`PLAN.md`).
pub(crate) fn thread_may_free() -> bool {
    EXIT_PHASE.with(|phase| phase.get()) == ExitPhase::Live
}

/// Whether this thread is inside its own [`ll_thread_exit`].
///
/// What is built here is disposed by the steps still to run, so a caller
/// needs neither the guard nor an initialisation of its own — and must
/// not reach for either, since `ll_thread_init` in the middle of an exit
/// rebuilds the heap the exit has torn down.
pub(crate) fn thread_exit_running() -> bool {
    EXIT_PHASE.with(|phase| phase.get()) == ExitPhase::Exiting
}

/// Whether something will run this thread's teardown.
///
/// A caller building a per-thread structure that thread exit is supposed
/// to dispose of asks this first: under a `false` nothing will ever
/// dispose it.
pub(crate) fn thread_exit_will_run() -> bool {
    thread_exit_running() || exit_guard_armed()
}

/// Arm this thread's exit guard, and report whether it is armed.
///
/// The call **is** the arming: touching the `thread_local!` is what
/// registers its destructor, so on a live thread this returns `true` and
/// leaves the guard in place. It returns `false` only when TLS teardown
/// has already destroyed the slot — a destructor allocating on the way
/// out — and that answer is permanent for the thread, since nothing
/// rebuilds a destroyed slot.
///
/// Whoever asks is deciding whether to build something this thread's exit
/// is supposed to dispose of. Under a `false` that exit will not run.
pub(crate) fn exit_guard_armed() -> bool {
    // Fault injection, tests only: TLS teardown cannot be entered on
    // demand, and an untested refusal path is a guess.
    #[cfg(test)]
    if FORCE_GUARD_UNARMED.load(Ordering::Relaxed) {
        return false;
    }

    // `try_with`, not `with`: on a slot TLS teardown has destroyed `with`
    // panics, and a panic inside a TLS destructor cannot unwind — under the
    // release profile's `panic = "abort"` it ends the process.
    EXIT_GUARD.try_with(|_| {}).is_ok()
}

/// Makes [`exit_guard_armed`] answer `false`, tests only. It names that
/// guard and nothing else: the heap, the pool and the arena are
/// unaffected, so a test using it proves which structure was refused.
#[cfg(test)]
pub(crate) static FORCE_GUARD_UNARMED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// This thread's raw heap, or null if it has never allocated.
///
/// The null case is what lets `ll_malloc`/`ll_c_free` self-initialise on a
/// cold branch instead of making every caller wrap them in an init check.
#[inline]
pub fn thread_heap() -> *mut Heap {
    tls::get_raw()
}

/// This thread's entity heap, or null if it has never allocated. One
/// field offset past the TLS read — the pair is `repr(C)`.
#[inline]
pub fn thread_entity_heap() -> *mut Heap {
    let p = tls::get_raw() as *mut ThreadHeaps;
    if p.is_null() {
        return std::ptr::null_mut();
    }

    unsafe { &raw mut (*p).entity }
}

/// Allocate a GC entity of `size` bytes from this thread's entity heap.
/// Self-initialising like `ll_alloc`. A size past the largest size class
/// takes a block-aligned allocation of its own instead
/// (`memory::large_entity`), because a packed slot that large would take
/// a whole block and leave the entity-block population the walk
/// enumerates.
///
/// # Safety
/// Standard allocator contract; the caller publishes an `RcHeader` into
/// the slot's first 8 bytes (header last — see `ll_object_new`).
#[inline]
pub unsafe fn entity_alloc(size: usize) -> *mut u8 {
    if size <= MAX_SMALL {
        let h = thread_entity_heap();
        if h.is_null() {
            unsafe { entity_alloc_init(size) }
        } else {
            unsafe { (*h).alloc(size) }
        }
    } else {
        // Not `ll_alloc`: that stamps a raw-buffer kind, and an entity
        // under one is invisible to both enumerators and freed without
        // the entity assertions. Past the largest size class an entity
        // takes a block-aligned allocation of its own
        // (`rfc/model/memory/large-entities.md`).
        crate::memory::large_entity::alloc(size)
    }
}

/// Reserve up to `count` GcHeap entity cells of `size` from this
/// thread's entity heap (`rfc/model/memory/bulk-operations.md`). Writes
/// the cells into `out_cells`, the leading adjacent-run length into
/// `contiguous_len`, and returns how many cells were reserved — any
/// number from 0 to `count`; the caller's fallback is the ordinary
/// factory. Cells are consumed by `ll_object_new_in` and unused ones
/// are owed back via [`ll_entity_cells_return`].
///
/// # Safety
/// `out_cells` must have room for `count` pointers; `contiguous_len`
/// must be valid to write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_entity_reserve(
    size: usize,
    count: usize,
    out_cells: *mut *mut u8,
    contiguous_len: *mut usize,
) -> usize {
    let h = thread_entity_heap();
    let h = if h.is_null() {
        ll_thread_init();
        let h = thread_entity_heap();
        if h.is_null() {
            unsafe { *contiguous_len = 0 };
            return 0;
        }

        h
    } else {
        h
    };

    let out = unsafe { std::slice::from_raw_parts_mut(out_cells, count) };
    let (n, contiguous) = unsafe { (*h).reserve_cells(size, count, out) };
    unsafe { *contiguous_len = contiguous };
    n
}

/// Return unused reserved cells (`rfc/model/memory/bulk-operations.md`):
/// each goes back through the ordinary size-less free path, which routes
/// it to its block's free list, exactly like any other free. Nothing
/// parks today; S34.3 and S36.2 give the free path its two windows back
/// (`PLAN.md`).
///
/// # Safety
/// Every element must be an unconsumed cell from [`ll_entity_reserve`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_entity_cells_return(cells: *const *mut u8, count: usize) {
    for i in 0..count {
        unsafe { crate::memory::stdapi::ll_free(*cells.add(i)) };
    }
}

/// Cold tail: first entity allocation on this thread.
///
/// # Safety
/// As [`entity_alloc`].
#[cold]
#[inline(never)]
unsafe fn entity_alloc_init(size: usize) -> *mut u8 {
    ll_thread_init();
    let h = thread_entity_heap();
    if h.is_null() {
        return std::ptr::null_mut();
    }

    unsafe { (*h).alloc(size) }
}

/// Null the shadow-row pointer of a block that can carry rows, which
/// every collection owes for every block it stamped — at its end and on
/// its abort alike.
///
/// A stale pointer left behind names an arena that has since been
/// recommissioned, so the next collection would decrement rows that now
/// hold live payload (`rfc/model/gc/rc-cycle.md`, "Where the shadow
/// count lives"). The store is a release, and the acquire half is
/// [`block_shadow`].
///
/// **It is also how a block is commissioned into a row-carrying kind.**
/// The word is written once at an entity block's `refill` and never
/// again, but a former-arena block is stamped `BLOCK_KIND_RETAINED`
/// over whatever its previous life left in the collector line, so
/// `promote` nulls it there before the kind's release store publishes
/// the block to a trace.
///
/// # Safety
/// `block` must be the header of a live 64 KiB block whose collector
/// line is the collector's: `BLOCK_KIND_ENTITY`, `BLOCK_KIND_RETAINED`,
/// or a block about to be stamped one of them.
pub(crate) unsafe fn clear_block_shadow(block: *mut u8) {
    let triple = unsafe { block_collector(block as *mut HeapBlockHeader) };
    unsafe {
        (*triple)
            .shadow
            .store(std::ptr::null_mut(), Ordering::Release)
    };
}

/// The shadow-row pointer of a block, null when no collection holds rows
/// for it — which is what a collection's first touch of the block reads
/// to know it owes the block an array (`crate::cycle::arena`).
///
/// # Safety
/// As [`clear_block_shadow`].
pub(crate) unsafe fn block_shadow(block: *mut u8) -> *mut u8 {
    let triple = unsafe { block_collector(block as *mut HeapBlockHeader) };
    unsafe { (*triple).shadow.load(Ordering::Acquire) }
}

/// Stamp a block's shadow-row pointer. **The caller owes the enrolment
/// first**, and why that order rather than the other is
/// `crate::cycle::arena`, "Enrolment cannot fail after the rows exist".
///
/// # Safety
/// As [`clear_block_shadow`].
pub(crate) unsafe fn set_block_shadow(block: *mut u8, rows: *mut u8) {
    let triple = unsafe { block_collector(block as *mut HeapBlockHeader) };
    unsafe { (*triple).shadow.store(rows, Ordering::Release) };
}

/// The slot index of `entity` inside its own entity block: which row of
/// that block's shadow array carries the collector's working count for
/// it (`rfc/model/gc/rc-cycle.md`, "Where the shadow count lives").
///
/// The index is derived by a reciprocal multiply rather than by a
/// division: the block's collector triple carries `2^32 / stride + 1`,
/// written there once at commissioning, and the high word of the
/// multiply is the index. An index that is off by one names another live
/// entity's row instead of faulting, so the arithmetic is proven
/// exhaustively against the division rather than against an address
/// recomputed from the index
/// (`the_reciprocal_multiply_is_the_division_over_a_whole_block`).
///
/// The caller resolves the block's kind first, and only a block that
/// reads `BLOCK_KIND_ENTITY` reaches here: the other populations of the
/// GC heap have no stride to divide by (`crate::cycle::row::edge_to`).
///
/// # Safety
/// `entity` must be a slot of a commissioned `BLOCK_KIND_ENTITY` block.
pub(crate) unsafe fn entity_slot_index(entity: *mut u8) -> u32 {
    let triple = unsafe { block_collector(HeapBlockHeader::of_ptr(entity)) };
    // Relaxed, because the caller's acquire load of the kind published
    // this word: both are written once at commissioning, the kind last
    // and with release (`block_pool::collector_load_block_kind`).
    let reciprocal = unsafe { (*triple).reciprocal.load(Ordering::Relaxed) };
    let offset = (entity as usize & BLOCK_MASK) - LINE_SIZE;
    slot_index_by_reciprocal(offset, reciprocal)
}

/// How many slots an entity block holds, which is how many rows its
/// shadow array needs (`crate::cycle::arena`).
///
/// The size class comes from the collector's own copy in the block's
/// triple rather than from [`HeapBlockHeader::size_class`]: the whole
/// row lookup is meant to touch one cache line, and the owner's half of
/// the header is borrowed as `&mut` by every allocation, which a
/// collector's read of it would sit under.
///
/// # Safety
/// `block` must be the header of a commissioned `BLOCK_KIND_ENTITY`
/// block.
pub(crate) unsafe fn collector_block_slots(block: *mut u8) -> u32 {
    let triple = unsafe { block_collector(block as *mut HeapBlockHeader) };
    // Relaxed for the reason `entity_slot_index` loads its reciprocal
    // relaxed: the caller's acquire load of the kind published it.
    let class = unsafe { (*triple).size_class.load(Ordering::Relaxed) } as usize;
    (BLOCK_PAYLOAD / SIZE_CLASSES[class]) as u32
}

/// The reciprocal a block of this `stride` carries in its collector
/// triple: `2^32 / stride + 1`.
///
/// Exact for every offset a 64 KiB block can hold, at every size class:
/// the multiply's error stays below `2^-16` while a quotient's fraction
/// is at most `(stride - 1) / stride`, so the two never cross while
/// `stride` is under 65536.
#[inline]
const fn reciprocal_for(stride: usize) -> u32 {
    ((1u64 << 32) / stride as u64 + 1) as u32
}

/// The slot index of `offset` under a reciprocal already in hand — the
/// form a row lookup takes, the block's triple having paid for the
/// division once at commissioning.
///
/// `offset` is measured from the payload start, `LINE_SIZE` already off
/// the address, and stays below `BLOCK_PAYLOAD`; `reciprocal` is
/// [`reciprocal_for`] of that block's stride. Outside either, the result
/// is arithmetic with no meaning rather than a fault.
#[inline]
fn slot_index_by_reciprocal(offset: usize, reciprocal: u32) -> u32 {
    ((offset as u64 * reciprocal as u64) >> 32) as u32
}

/// The same index from a `stride` rather than from a reciprocal: the
/// composition the exhaustive proof is driven over, so that the two
/// production halves are the ones proven and not a copy of them.
///
/// `#[cfg(test)]` because no production path composes them — a row
/// lookup takes the reciprocal from the block, and the block took it
/// from [`reciprocal_for`] at commissioning.
#[cfg(test)]
#[inline]
fn slot_index_of_offset(offset: usize, stride: usize) -> u32 {
    slot_index_by_reciprocal(offset, reciprocal_for(stride))
}

/// Visit every occupied slot of every entity block, process-wide — the
/// census primitive a whole-heap pass is built on. Nothing in the
/// production build calls it: its callers are `cells::heap_census` and
/// the leak tests, all `#[cfg(test)]`.
///
/// Occupancy is exact by construction: commissioning zeroes slot
/// headers, the factory publishes a header last, and a freed slot keeps
/// its final refcount-0 header (the free-list link lives in bytes 8–15).
/// The scan is bounded by each block's bump cursor, so virgin slots are
/// never read. Blocks of every other kind — arena, large, buffer, raw
/// heap — are skipped: un-walked memory is a root source, never an
/// error.
///
/// **Three populations, not one.** Size-class entity blocks are strided;
/// retained former-arena blocks carry no stride and are enumerated from
/// the index the reset left; and an entity too large for a size class
/// holds a block-aligned allocation alone, contributing exactly one
/// slot — found by the region scan while it is a pooled block, and from
/// `memory::large_entity`'s registry once it is an OS-direct run outside
/// every region.
///
/// # Safety
/// Requires a quiescent mutator: block kinds, cursors and slot headers
/// are read unsynchronised, which is sound only while no thread
/// allocates or frees concurrently (the crate's single-mutator phase;
/// the concurrent collector adds its snapshot discipline in build
/// step 3).
pub unsafe fn for_each_entity_slot(mut visit: impl FnMut(*mut crate::refcount::RcHeader)) {
    for region in BlockPool::global().regions() {
        for i in 0..BLOCKS_PER_REGION {
            let block = unsafe { region.add(i * BLOCK_SIZE) } as *mut HeapBlockHeader;
            // The kind gates every further header read: only `kind` (and
            // the pool link) is initialized in a block that has never been
            // commissioned — reading `size_class`/`bump` there is reading
            // uninitialized memory (Miri-caught). An entity block's header
            // was fully written by `refill`.
            let kind =
                unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) };
            // A pooled block holding one large entity: no stride and no
            // cursor, one slot at the payload start, and the same
            // occupancy word decides. A run of the same population is
            // outside every region and comes from the registry below.
            if kind == crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE {
                let (entity, _) =
                    unsafe { crate::memory::large_entity::occupant(block as *mut u8) };
                let slot = entity as *mut crate::refcount::RcHeader;
                if unsafe { crate::refcount::header_refcount(slot) } != 0 {
                    visit(slot);
                }

                continue;
            }

            if kind != BLOCK_KIND_ENTITY {
                continue;
            }

            let (size_class, bump) = unsafe {
                (
                    (*block).size_class.load(Ordering::Relaxed),
                    (*block).private.bump,
                )
            };

            let class_size = SIZE_CLASSES[size_class as usize];
            let base = unsafe { (block as *mut u8).add(LINE_SIZE) };
            for s in 0..bump as usize {
                let slot = unsafe { base.add(s * class_size) } as *mut crate::refcount::RcHeader;
                if unsafe { crate::refcount::header_refcount(slot) } != 0 {
                    visit(slot);
                }
            }
        }
    }

    // Retained former-arena blocks carry no stride, so they are
    // enumerated from the object index the reset left behind rather
    // than by striding. The occupancy test is the same word: a survivor
    // that has since died reads refcount 0 and is skipped exactly as a
    // free slot is (`memory/retained.rs`).
    for (_block, index) in crate::memory::retained::snapshot() {
        for &addr in index.iter() {
            let slot = addr as *mut crate::refcount::RcHeader;
            if unsafe { crate::refcount::header_refcount(slot) } != 0 {
                visit(slot);
            }
        }
    }

    // An entity too large even for a pooled block lives in an OS-direct
    // run, which no region contains, so the registry is the only thing
    // that names it (`memory/large_entity.rs`). One occupant each, at the
    // line after the header.
    for block in crate::memory::large_entity::snapshot() {
        let (entity, _) = unsafe { crate::memory::large_entity::occupant(block as *mut u8) };
        let slot = entity as *mut crate::refcount::RcHeader;
        if unsafe { crate::refcount::header_refcount(slot) } != 0 {
            visit(slot);
        }
    }
}

/// What the enumerator sees at `addr`, as text, for a test that found an
/// entity missing from a census and needs to say why.
///
/// Every field `for_each_entity_slot` gates on, in the order it reads
/// them: whether the address is inside a registered region at all, then
/// the block's kind, then its stride and bump, then the refcount the
/// occupancy test reads. A slot index at or past `bump` is a slot the
/// walk does not reach.
///
/// The header is reported as its two mutator halves rather than as one
/// word, because a mutator-side read of a published header may not span
/// byte 6. The collector's bits 16-31 are therefore absent from the text,
/// having no mutator-side reader at all yet.
///
/// A large-entity block answers a different set, because it has no
/// stride and no cursor: its occupant's size, and — the membership that
/// decides whether a run is enumerated at all — whether the registry
/// names it.
#[cfg(test)]
pub(crate) fn describe_slot(addr: usize) -> String {
    let block = (addr & !BLOCK_MASK) as *mut HeapBlockHeader;
    let in_region = BlockPool::global().regions().iter().any(|&r| {
        let base = r as usize;
        addr >= base && addr < base + crate::memory::block_pool::REGION_SIZE
    });

    // A large-entity block first, because the header below it is a
    // different struct: reading a size class and a bump cursor out of it
    // yields numbers that look like an unreachable slot at exactly the
    // moment the walk reaches this one unconditionally, and for a run the
    // cursor's offset is past anything commissioning wrote.
    let kind_word = unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) };
    if crate::memory::large_entity::is_large_entity(kind_word) {
        let (entity, size) = unsafe { crate::memory::large_entity::occupant(block as *mut u8) };
        let registered = crate::memory::large_entity::snapshot().contains(&(block as usize));
        let (refcount, flags) =
            unsafe { crate::refcount::header_pair(addr as *const crate::refcount::RcHeader) };
        return format!(
            "addr {addr:#x} block {:#x} in_region {in_region} kind {kind_word} \
             large_entity size {size} occupant {:#x} registered_run {registered} \
             refcount {refcount} flags {flags:#06x}",
            block as usize, entity as usize
        );
    }

    let (kind, size_class, used, slots, bump) = unsafe {
        (
            crate::memory::block_pool::load_block_kind(&raw const (*block).kind),
            (*block).size_class.load(Ordering::Relaxed),
            (*block).private.used,
            (*block).private.slots,
            (*block).private.bump,
        )
    };

    let (refcount, flags) =
        unsafe { crate::refcount::header_pair(addr as *const crate::refcount::RcHeader) };
    let stride = SIZE_CLASSES
        .get(size_class as usize)
        .copied()
        .unwrap_or(usize::MAX);
    let index = (addr - block as usize - LINE_SIZE) / stride.max(1);
    let retained = crate::memory::retained::snapshot()
        .iter()
        .any(|(_, ix)| ix.contains(&addr));
    format!(
        "addr {addr:#x} block {:#x} in_region {in_region} kind {kind} class {size_class} \
         stride {stride} used {used} slots {slots} bump {bump} slot_index {index} \
         retained_index {retained} refcount {refcount} flags {flags:#06x}",
        block as usize
    )
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
mod tests;
