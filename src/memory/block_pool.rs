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

use std::cell::RefCell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering};

use crate::journal::kinds::journal_event;

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

/// Blocks one overflow flush can move, and the width of the stack array
/// [`BlockPool::put`] stages them in. A `put` overflows the cache by one
/// block and the flush keeps half, so the excess is
/// `THREAD_CACHE_CAPACITY / 2 + 1` and never more.
const FLUSH_MAX: usize = THREAD_CACHE_CAPACITY / 2 + 1;

/// Store a block header's `kind` discriminant, and **the only path that
/// writes one**. The store is a **release**, and its reader is a
/// collector enumerating the blocks of every carved region: the release
/// ordering publishes a commissioned block's other header fields before
/// its kind says "entity". That reader loads through
/// [`collector_load_block_kind`], the acquire half of the pair.
///
/// The field is an [`AtomicU32`] in every header that overlays offset 0,
/// and it sits outside the half `Heap::alloc` borrows — position, not
/// type, is what keeps that borrow legal (`docs/memory-manager.md`,
/// "`HeapBlockHeader`, and why it is four structs"; `dev/POSTMORTEM.md`,
/// "an atomic field does not survive a `&mut` over the struct"). The one
/// access the type does not defend against is a whole-struct store, which
/// writes these four bytes plainly, so a header is commissioned field by
/// field and its kind last, through here.
#[inline]
pub(crate) unsafe fn store_block_kind(kind_field: *const AtomicU32, kind: u32) {
    unsafe { (*kind_field).store(kind, Ordering::Release) };
}

/// Read a block header's `kind` on the thread that owns the block, where
/// no ordering is needed: the owner is the only writer, and a reader on
/// another thread is the collector, which loads with acquire of its own.
#[inline]
pub(crate) unsafe fn load_block_kind(kind_field: *const AtomicU32) -> u32 {
    unsafe { (*kind_field).load(Ordering::Relaxed) }
}

/// Read a block header's `kind` from a thread that does not own the
/// block, which is the collector resolving the child of a traced edge to
/// the population it belongs to (`rfc/model/gc/rc-cycle.md`, "Where the
/// shadow count lives").
///
/// The load is an **acquire** and pairs with [`store_block_kind`]'s
/// release. What that buys is the commissioning that accompanies the
/// value read: a kind read as `BLOCK_KIND_ENTITY` guarantees the size
/// class beside it, the cursor and the zeroed slot headers of *that*
/// commissioning are visible without a second ordered load.
///
/// **It bounds nothing about staleness**, so it is not what keeps a
/// reader off a block that has since been recycled under a different
/// size class: acquire orders what accompanies a value and places no age
/// limit on the value itself. What excludes that is the parking rule —
/// a slot returns only when the collection that could be reading its row
/// is over, so a block cannot empty and reach the pool mid-trace
/// (`rfc/model/gc/rc-cycle.md`, "Death while enrolled"). Nothing parks
/// today; S36.2 of `PLAN.md` is the step that builds the window, and
/// until it lands this pair is sound only because one thread runs.
///
/// # Safety
/// `kind_field` must be the `kind` word of a block header mapped for the
/// duration of the load: a block of a carved region, or a run the pool
/// has not returned to the OS. Any `u32` may come back, including a kind
/// this crate does not define.
#[inline]
pub(crate) unsafe fn collector_load_block_kind(kind_field: *const AtomicU32) -> u32 {
    unsafe { (*kind_field).load(Ordering::Acquire) }
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
/// One pooled block holding **one entity** that no size class serves
/// (`rfc/model/memory/large-entities.md`). Separate from
/// `BLOCK_KIND_LARGE`, which is the same physical shape holding a raw C
/// buffer: a walker reading such a buffer's first 8 bytes as an
/// `RcHeader` is the mistake this segregation exists to prevent.
pub const BLOCK_KIND_ENTITY_LARGE: u32 = 9;
/// An OS-direct, block-aligned run holding one entity, above what a
/// pooled block can hold. Outside every carved region, so it is
/// enumerated from `memory::large_entity`'s registry rather than by the
/// region scan.
pub const BLOCK_KIND_ENTITY_LARGE_RUN: u32 = 10;
/// Entity block: same size-class layout as `BLOCK_KIND_HEAP`, but its
/// slots hold GC entities (header at offset 0) and only these blocks are
/// traced by a cycle collector (`docs/memory-manager.md`, "Heap: small
/// objects"). A raw C-ABI buffer must never land in one: a trace reads
/// every occupied slot's first 8 bytes as an `RcHeader`.
pub const BLOCK_KIND_ENTITY: u32 = 8;

/// Header in the first line of every block.
///
/// Cache-line layout rule (validated against jemalloc/mimalloc): this
/// first line holds read-mostly fields and the free-list link (written
/// only while the block is free). Future concurrent fields (remembered
/// set, line marks) get their own lines within the 256-byte header.
#[repr(C)]
pub struct BlockHeader {
    /// The tagged union's discriminant, and the one word of a block
    /// header that two threads touch: the owner writes it through
    /// [`store_block_kind`], the collector reads it for every block of
    /// every region. Atomic by type so that a `&mut` to a header — or to
    /// a private half that contains it — leaves these four bytes alone.
    pub kind: AtomicU32,
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
/// The Treiber stack it replaced was unsound here for a reason
/// particular to this crate rather than for ABA: `pop_global` read
/// `(*ptr).next` non-atomically, and those same bytes are the heap's
/// `used` counter in the tagged union, written non-atomically by the
/// block's new owner on every allocation. A correct lock-free version is
/// still possible, and would need the pool's **own** link field at an
/// offset no owner view aliases. The lock costs nothing measured on either
/// benchmark; revisit if this path stops being cold
/// (`dev/DECISIONS.md`, "cold concurrent structures take a lock, not a
/// CAS loop").
struct FreeList {
    head: *mut BlockHeader,
}

// SAFETY: the pointers are block headers in never-unmapped pool regions;
// the mutex is what serialises access to the chain.
unsafe impl Send for FreeList {}

/// Region bases the registry holds in one chunk. The chunk is one block,
/// so a chunk records 8 190 regions — 16 GB of them — and a second is
/// mapped only past that.
const REGISTRY_CHUNK_ENTRIES: usize = (BLOCK_SIZE - 2 * size_of::<usize>()) / size_of::<usize>();

/// One link of the region registry, mapped from the operating system
/// rather than allocated: a `Vec` that cannot grow aborts the process,
/// and the registry is written on the one path whose whole contract is
/// that a refusal is reported (`memory::os`).
///
/// **Read without a lock**, which is what `len` and `next` are atomics
/// for. A reader acquire-loads `len` and sees every base the matching
/// release store published; a chunk is never rewritten and never
/// unmapped, so nothing it has read can go away underneath it.
struct RegistryChunk {
    next: AtomicPtr<RegistryChunk>,
    len: AtomicUsize,
    bases: [usize; REGISTRY_CHUNK_ENTRIES],
}

/// The writer's end of the chain, serialised by the pool's registry lock.
/// Null until the first region is carved.
struct RegistryTail(*mut RegistryChunk);

// SAFETY: the chunks are OS mappings owned by the pool alone and never
// unmapped; the mutex around this pointer is what serialises the appends
// that move it.
unsafe impl Send for RegistryTail {}

pub struct BlockPool {
    free: Mutex<FreeList>,
    regions_carved: AtomicUsize,
    /// Blocks handed out minus blocks returned — block-granular
    /// occupancy. Bumped only on the (rare) block operations, never on
    /// object allocation: the telemetry design taxes no hot path.
    blocks_out: AtomicUsize,
    /// Region registry: the base address of every 2 MB region ever
    /// carved. The pool used to count regions without recording their
    /// bases, so nothing could enumerate blocks — which a whole-heap
    /// trace must. Append-only and regions are never unmapped (phase 1),
    /// so an index is a stable handle for as long as the process lives.
    /// OS-direct `BLOCK_KIND_LARGE_RUN` allocations are not regions and
    /// are not here — huge objects stay outside such a pass,
    /// conservatively.
    ///
    /// Two fields rather than one, and the split is what keeps the
    /// enumeration lock-free: readers walk from `registry_head` taking
    /// nothing, and the mutex serialises appends alone. A reader that
    /// took a lock would run its visitor under a lock the allocator
    /// takes, which `memory/large_entity.rs` states as the rule this
    /// crate keeps — and once this manager is Rust's
    /// `#[global_allocator]`, a visitor that allocated would re-enter
    /// `carve_region` and deadlock on the lock its own walk held.
    registry_head: AtomicPtr<RegistryChunk>,
    registry_tail: Mutex<RegistryTail>,
}

static GLOBAL_POOL: BlockPool = BlockPool {
    free: Mutex::new(FreeList {
        head: std::ptr::null_mut(),
    }),
    regions_carved: AtomicUsize::new(0),
    blocks_out: AtomicUsize::new(0),
    registry_head: AtomicPtr::new(std::ptr::null_mut()),
    registry_tail: Mutex::new(RegistryTail(std::ptr::null_mut())),
};

/// Serializes region carving only (rare path).
static CARVE_LOCK: Mutex<()> = Mutex::new(());

/// Makes [`BlockPool::get`] report exhaustion. Test-only, and the only
/// way to reach the out-of-memory paths deliberately.
#[cfg(test)]
pub(crate) static FORCE_OOM: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

// Blocks this thread has asked the pool for, whichever tier served it.
// Counted rather than inferred from the global allocator, because a
// request the thread cache serves allocates nothing and still takes the
// pool's word — and a path that may not lock is judged by requests, not
// by allocations (`crate::test_support::allocation_probe`).
#[cfg(test)]
thread_local! {
    static POOL_REQUESTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Pool requests on this thread, and zero the count.
#[cfg(test)]
pub(crate) fn take_pool_requests() -> usize {
    POOL_REQUESTS.with(|c| c.replace(0))
}

thread_local! {
    static THREAD_CACHE: RefCell<ThreadCache> = const {
        RefCell::new(ThreadCache {
            blocks: CachedBlocks {
                blocks: [std::ptr::null_mut(); THREAD_CACHE_CAPACITY + 1],
                len: 0,
            },
        })
    };
}

/// The thread cache's storage: a fixed array, never a `Vec`.
///
/// A `Vec` that cannot grow calls `handle_alloc_error`, which aborts, and
/// this cache sits on the path whose whole contract is that exhaustion is
/// reported (`BlockPool::get`). The array is one wider than
/// [`THREAD_CACHE_CAPACITY`] because [`BlockPool::put`] pushes first and
/// flushes after, so the cache is over its capacity by exactly one block
/// for the length of that borrow.
struct CachedBlocks {
    blocks: [*mut BlockHeader; THREAD_CACHE_CAPACITY + 1],
    /// Valid entries, occupying `blocks[..len]`.
    len: usize,
}

impl CachedBlocks {
    fn len(&self) -> usize {
        self.len
    }

    /// Caller must have checked there is room: the array's width is a
    /// contract of the flush policy, not a runtime condition.
    fn push(&mut self, block: *mut BlockHeader) {
        debug_assert!(self.len < self.blocks.len(), "the thread cache overflowed");
        self.blocks[self.len] = block;
        self.len += 1;
    }

    fn pop(&mut self) -> Option<*mut BlockHeader> {
        if self.len == 0 {
            return None;
        }

        self.len -= 1;
        Some(self.blocks[self.len])
    }

    /// Append as many of `blocks` as the array still has room for, and
    /// answer how many were left behind — the caller owns those and hands
    /// them back to the global list.
    fn extend(&mut self, blocks: &[*mut BlockHeader]) -> usize {
        // Capacity, not the array's width: the extra slot belongs to
        // [`BlockPool::put`], which pushes before it flushes. A refill
        // that filled it would leave the next `put` indexing past the
        // array, and a slice-bounds panic under `panic = "abort"` ends
        // the process on the one path whose contract is that nothing
        // does.
        let room = THREAD_CACHE_CAPACITY - self.len;
        let taken = room.min(blocks.len());
        self.blocks[self.len..self.len + taken].copy_from_slice(&blocks[..taken]);
        self.len += taken;

        blocks.len() - taken
    }

    /// Copy `count` entries starting at `from` into `out` and close the
    /// gap, which is the flush half of the overflow policy.
    fn drain_into(&mut self, from: usize, count: usize, out: &mut [*mut BlockHeader]) {
        out[..count].copy_from_slice(&self.blocks[from..from + count]);
        self.blocks.copy_within(from + count..self.len, from);
        self.len -= count;
    }

    /// Every entry, oldest first, leaving the cache empty.
    fn take(&mut self) -> ([*mut BlockHeader; THREAD_CACHE_CAPACITY + 1], usize) {
        let len = self.len;
        self.len = 0;

        (self.blocks, len)
    }
}

struct ThreadCache {
    blocks: CachedBlocks,
}

impl Drop for ThreadCache {
    /// The fallback for a thread that never ran `ll_thread_exit` — this
    /// pool serves threads the runtime never initialised. On the contract
    /// path [`drain_thread_cache`] has already emptied this.
    ///
    /// A dying thread must not take cached blocks with it either way.
    fn drop(&mut self) {
        let (blocks, len) = self.blocks.take();
        for &block in &blocks[..len] {
            GLOBAL_POOL.push_global(block);
        }
    }
}

/// How many blocks this thread's cache holds. Tests only.
#[cfg(test)]
pub(crate) fn thread_cache_len() -> usize {
    THREAD_CACHE
        .try_with(|cache| cache.borrow().blocks.len())
        .unwrap_or(0)
}

/// Flush this thread's cached blocks to the global list, by hand, while
/// the thread still exists.
///
/// Called from `heap::ll_thread_exit` before the journal's ring retires,
/// so that the handovers land inside the ring rather than after it: a
/// block going back to the pool is a default event kind
/// (`dev/design/debug-modes.md` §9.5), and this cell's destructor runs
/// after the exit wherever TLS is destroyed in reverse registration
/// order.
///
/// `try_with`, because it can be reached from a destructor after this
/// cell's own has run. Idempotent: a flushed cache flushes nothing.
pub(crate) fn drain_thread_cache() {
    let (blocks, len) = THREAD_CACHE
        .try_with(|cache| cache.borrow_mut().blocks.take())
        .unwrap_or(([std::ptr::null_mut(); THREAD_CACHE_CAPACITY + 1], 0));
    for &block in &blocks[..len] {
        GLOBAL_POOL.push_global(block);
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

    /// Every 2 MB region base, in carve order, handed to `visit` one at a
    /// time. Carve order is what makes a region's position a stable handle
    /// — the registry is append-only and a region is never unmapped.
    ///
    /// **No lock is held while `visit` runs**, so a visitor may allocate,
    /// take locks of its own and re-enter this pool. A region carved
    /// during the walk may or may not be seen; one carved before it began
    /// always is.
    pub fn for_each_region(&self, mut visit: impl FnMut(*mut u8)) {
        let mut chunk = self.registry_head.load(Ordering::Acquire);
        while !chunk.is_null() {
            // Acquire against the release store that published it: every
            // base below `len` was written before that store.
            let len = unsafe { (*chunk).len.load(Ordering::Acquire) };
            for i in 0..len {
                let base = unsafe { (*chunk).bases[i] };
                visit(base as *mut u8);
            }

            chunk = unsafe { (*chunk).next.load(Ordering::Acquire) };
        }
    }

    /// Record a carved region, answering false when the operating system
    /// refuses the chunk a new link needs. A false answer leaves the
    /// registry exactly as it was, so the caller's own undo has a
    /// consistent state to return to.
    fn register_region(&self, base: usize) -> bool {
        let mut tail = self.registry_tail.lock().unwrap();
        let full = tail.0.is_null()
            || unsafe { (*tail.0).len.load(Ordering::Relaxed) } == REGISTRY_CHUNK_ENTRIES;
        if full && !self.grow_registry(&mut tail) {
            return false;
        }

        // The base first, then the length that publishes it: a reader
        // acquire-loading this length sees the write above it.
        unsafe {
            let len = (*tail.0).len.load(Ordering::Relaxed);
            let bases = std::ptr::addr_of_mut!((*tail.0).bases) as *mut usize;
            bases.add(len).write(base);
            (*tail.0).len.store(len + 1, Ordering::Release);
        }

        true
    }

    /// Map one more chunk and link it at the tail.
    #[cold]
    fn grow_registry(&self, tail: &mut RegistryTail) -> bool {
        let chunk = crate::memory::os::map_aligned(BLOCK_SIZE, BLOCK_SIZE) as *mut RegistryChunk;
        if chunk.is_null() {
            return false;
        }

        // The mapping arrives zeroed, so `next` and `len` already read
        // what a fresh chunk needs; only the link into the chain is owed,
        // and it is a release store because it publishes them.
        if tail.0.is_null() {
            self.registry_head.store(chunk, Ordering::Release);
        } else {
            unsafe { (*tail.0).next.store(chunk, Ordering::Release) };
        }

        tail.0 = chunk;
        true
    }

    /// The same walk collected into a vector.
    ///
    /// Tests only, and it knowingly breaks the visitor's own rule by
    /// allocating under the lock: a test has no global allocator installed
    /// over this pool, so neither the abort nor the re-entry that rule
    /// guards against can happen there.
    #[cfg(test)]
    pub fn regions(&self) -> Vec<*mut u8> {
        let mut out = Vec::new();
        self.for_each_region(|base| out.push(base));

        out
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
        let block = self.take_block();
        // The journal's site sits out here rather than in the body,
        // because the body borrows the cache's `RefCell` and a record
        // raised under that borrow re-enters this pool through
        // `ll_malloc` if it is some thread's first — a borrow panic,
        // which this crate aborts on (`debug-modes.md` §9.7).
        if !block.is_null() {
            journal_event!(
                crate::journal::kinds::KIND_BLOCK_COMMISSIONED,
                block as u64,
                0,
                0
            );
        }

        block
    }

    /// A free block from the thread cache, the global stack or a freshly
    /// carved region; null when the OS refuses memory.
    ///
    /// Every borrow of the thread cache is inside this function, which is
    /// what lets [`get`](Self::get) raise its record with none held.
    fn take_block(&self) -> *mut BlockHeader {
        // Fault injection, tests only: the OS refusing memory cannot be
        // provoked on demand, and an untested failure path is a guess.
        #[cfg(test)]
        if FORCE_OOM.load(Ordering::Relaxed) {
            return std::ptr::null_mut();
        }

        #[cfg(test)]
        POOL_REQUESTS.with(|c| c.set(c.get() + 1));

        self.blocks_out.fetch_add(1, Ordering::Relaxed);
        let cached = THREAD_CACHE
            .try_with(|c| c.borrow_mut().blocks.pop())
            .unwrap_or(None);
        if let Some(block) = cached {
            return block;
        }

        // Refill the cache in a batch, return one. A refused region stops
        // the loop rather than spinning on it — see `carve_region`. The
        // batch is a stack array: a `Vec` here would abort the process on
        // the one path built to report exhaustion instead.
        let mut batch = [std::ptr::null_mut::<BlockHeader>(); REFILL_BATCH];
        let mut filled = 0;
        while filled < REFILL_BATCH {
            match self.pop_global() {
                Some(b) => {
                    batch[filled] = b;
                    filled += 1;
                }
                None => {
                    if !self.carve_region() {
                        break;
                    }
                }
            }
        }

        if filled == 0 {
            // Nothing anywhere: undo the optimistic count and report.
            self.blocks_out.fetch_sub(1, Ordering::Relaxed);
            return std::ptr::null_mut();
        }

        filled -= 1;
        let block = batch[filled];

        let spare = &batch[..filled];
        let left_over = THREAD_CACHE
            .try_with(|c| c.borrow_mut().blocks.extend(spare))
            .unwrap_or(filled);

        // No cache to put them in (see `get`'s note), or no room in it —
        // hand the remainder back.
        for &b in &spare[filled - left_over..] {
            self.push_global(b);
        }

        block
    }

    /// Return a block. Overflowing the thread cache flushes half of it
    /// to the global stack (jemalloc flush pattern).
    ///
    /// `block` must be a block previously handed out by [`get`](Self::get); the
    /// pool is its sole authority, so this internal API stays safe.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn put(&self, block: *mut BlockHeader) {
        // A retained block carries promoted survivors, and it comes back
        // only through `retained::give_block_back`, which restamps it once
        // the registry reports nothing left holding it — the last live
        // occupant gone (`ll_free`'s retained arm) or the last pinned
        // payload freed. Arriving here still stamped retained means a
        // caller took a shortcut past that arm, with survivors possibly
        // alive — not a step of a defect, the defect itself, and catching
        // it here names the thread and the moment. Planted while hunting
        // the loaded-box flake in `census_counts_objects_and_their_edges`,
        // where a walk sees a block that should not be reachable.
        debug_assert_ne!(
            unsafe { load_block_kind(&raw const (*block).kind) },
            BLOCK_KIND_RETAINED,
            "a retained block went back to the pool with its survivors alive"
        );
        self.blocks_out.fetch_sub(1, Ordering::Relaxed);
        // Read before the store below overwrites it, and behind the same
        // feature as the site that reads it: without `debug-journal` there
        // is no site and no load (`debug-modes.md` §9.6).
        #[cfg(feature = "debug-journal")]
        let arrived_as = unsafe { load_block_kind(&raw const (*block).kind) };
        unsafe { store_block_kind(&raw const (*block).kind, BLOCK_KIND_FREE) };

        // The borrow decides and stages, and everything else happens
        // after it ends. A record raised under it — the journal's site
        // below, if it is some thread's first — comes back into this pool
        // through `ll_malloc` and finds the cache already borrowed. The
        // failure is a borrow panic rather than the deadlock §9.7's
        // phrasing suggests, and under `panic = "abort"` it ends the
        // process. So the flush copies out and the borrow closes
        // (`dev/design/debug-modes.md` §9.7).
        //
        // `try_with` for the same reason as `get`: this runs from a TLS
        // destructor on the thread-exit path, where the cache may be gone.
        let mut flushed = [std::ptr::null_mut::<BlockHeader>(); FLUSH_MAX];
        let staged = THREAD_CACHE.try_with(|c| {
            let mut cache = c.borrow_mut();
            cache.blocks.push(block);
            if cache.blocks.len() <= THREAD_CACHE_CAPACITY {
                return 0;
            }

            let keep = THREAD_CACHE_CAPACITY / 2;
            // The `min` bounds a future refill path that overfills the
            // cache: `taken` slices `flushed` below, and a `taken` past
            // that array's width panics on a path that may not panic. One
            // `put` overflows by a single block, so the bound does not
            // bind today, and the assert is what says so.
            let excess = cache.blocks.len() - keep;
            debug_assert!(
                excess <= FLUSH_MAX,
                "the cache overflowed by more than one put"
            );
            let taken = excess.min(FLUSH_MAX);
            cache.blocks.drain_into(keep, taken, &mut flushed);

            taken
        });

        // Between the two: the borrow has ended, so the record may take
        // the allocator's own path if it is this thread's first, and the
        // block has not been handed on, so no other thread can have
        // commissioned it yet. After the pushes the two records of one
        // address could arrive in the wrong order — a ring showing a
        // block taken into service before it was ever returned — and the
        // rings' own lack of order across threads (§9.1) does not excuse
        // it: what §9.1 gives up is order, not causality.
        journal_event!(
            crate::journal::kinds::KIND_BLOCK_DECOMMISSIONED,
            block as u64,
            arrived_as as u64,
            0
        );
        match staged {
            Ok(taken) => {
                for &b in &flushed[..taken] {
                    self.push_global(b);
                }
            }

            // No cache to put it in (see `get`'s note) — hand it back.
            Err(_) => self.push_global(block),
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

        let region = crate::memory::os::map_aligned(REGION_SIZE, BLOCK_SIZE);
        if region.is_null() {
            return false;
        }

        // Register before any block is handed out: the walker may
        // enumerate the registry at any time, and a block it cannot map
        // back to a region would be invisible to the census. A registry
        // that cannot record it therefore refuses the whole region —
        // handing out blocks the census cannot reach is worse than
        // reporting exhaustion one region early.
        if !self.register_region(region as usize) {
            crate::memory::os::unmap(region, REGION_SIZE);
            return false;
        }

        for i in 0..BLOCKS_PER_REGION {
            let block = unsafe { region.add(i * BLOCK_SIZE) } as *mut BlockHeader;
            unsafe {
                store_block_kind(&raw const (*block).kind, BLOCK_KIND_FREE);
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
    assert!(
        crate::memory::heap::ll_thread_init(),
        "the runtime started this thread"
    );
    // The ring too, and for the same reason as the init above: it is a
    // block, so the record that allocates it draws one out of this
    // thread's cache, and a test that names a block cannot have that
    // happen halfway through it (`dev/POSTMORTEM.md`, "a ring is a block,
    // and a thread's first record decides when it is taken").
    #[cfg(feature = "debug-journal")]
    crate::journal::take_ring_for_test();
    guard
}

/// Holds the test lock and, on drop, releases this thread's heap *while
/// still holding it*. Without this the heap goes back via the TLS
/// destructor after the test body released the lock, so its
/// `BlockPool::put`s mutate `blocks_out` under a later test's feet, which
/// is what made the stats test flake.
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
        crate::memory::critical::drain_for_test();
        crate::memory::heap::ll_thread_exit();
    }
}

#[cfg(test)]
mod tests;
