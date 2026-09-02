//! Long-lived buffer arena: `BLOCK_KIND_BUFFER`.
//!
//! Realloc-heavy buffer churn isolated from the object heap so its
//! fragmentation never pollutes it (`rfc/model/memory/buffers.md`). No
//! size classes, buffers varying continuously, so this is bump allocation
//! plus a **per-block intrusive LIFO free list**:
//!
//! - the list head lives in the block's header, and the chain never leaves
//!   the block, so it is L2-resident by construction;
//! - a freed chunk threads `{ next, size }` through its own payload, so a
//!   live buffer carries no metadata (minimum chunk 16 bytes for it);
//! - `plenty` and `tight` allocation just bump; `critical` first-fit
//!   searches at most [`CRITICAL_SEARCH_BOUND`] entries across the lists
//!   of the blocks this arena owns, current first;
//! - chunks never coalesce, the damage being bounded by one block;
//! - a per-block live count returns fully-emptied blocks to the pool.
//!
//! Payloads larger than a block payload are OS-direct (`ll_alloc` and
//! `ll_free`), invisible to this machinery.
//!
//! A buffer here holds the body of an entity — a string's bytes and an
//! array's table storage — and an entity dies wherever its last reference
//! is dropped, so this heap obeys the object heap's ownership rules in the
//! same shape: per-block `owner`, per-block lock-free stack for frees from
//! other threads, owner-written `live`, hand-over at thread exit, adoption
//! on the refill path (`docs/memory-manager.md`, "Cross-thread free").
//!
//! An adopted block is **reused rather than merely reclaimed**: its bump
//! cursor lives in its own header, so the adopter resumes the tail its
//! previous owner left, and the inherited free list is searched with
//! everything else the arena owns. A block abandoned with one live chunk
//! would otherwise hold the other 63 KiB out of circulation until that
//! chunk died (`dev/DECISIONS.md`, "a buffer block carries its own cursor,
//! so an adopted block is reused and not just held").

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use crate::memory::arena::round_up_8;
use crate::memory::block_pool::{
    BLOCK_KIND_BUFFER, BLOCK_MASK, BLOCK_PAYLOAD, BlockHeader, BlockPool, LINE_SIZE,
    load_block_kind,
};
use crate::memory::buffer::{Buffer, PressureMode, pressure_mode};

/// Bound on the critical-mode free-list walk. Tunable, and uncalibrated
/// until there is a workload to calibrate against.
pub const CRITICAL_SEARCH_BOUND: usize = 16;

/// A free chunk threads this through its own first 16 bytes.
#[repr(C)]
struct FreeChunk {
    next: *mut FreeChunk,
    size: usize,
}

const MIN_CHUNK: usize = size_of::<FreeChunk>();

/// The owner-private half: only the thread named by
/// [`BufferBlockShared::owner`] may touch these.
#[repr(C)]
struct BufferBlockPrivate {
    /// Live chunks in this block, **owner-written only**: a cross-thread
    /// free posts to [`BufferBlockRemote::remote_free`] and the owner
    /// accounts for it when it collects. That is what makes zero safe to
    /// act on — a posted chunk still counts as live, so the block cannot
    /// look empty while another thread holds an address inside it.
    live: u32,
    /// Head of the block-local free list.
    free: *mut FreeChunk,
    /// Next block in its owner's chain.
    owned_next: *mut BufferBlockHeader,
    /// Where bump allocation in this block stopped: the first byte of
    /// its unused tail, or the block's end when there is none.
    ///
    /// Owned by the block rather than by the arena, which is what lets
    /// the tail outlive the thread that opened it — an adopter reads it
    /// and keeps bumping. While the block is its arena's current one the
    /// arena's own `bump` runs ahead of this field, and
    /// [`BufferArena::settle_cursor`] settles it at every point the block
    /// can change hands. `heap.rs` keeps the cursor here too, and keeps no
    /// second copy — it reads and writes the block's own on every
    /// allocation, so it has no window where the header is behind; the
    /// cache here is what buys the bump path one line and costs the
    /// settling protocol. mimalloc likewise carries a page's `capacity`
    /// in the page, which is what lets a reclaimed page keep extending.
    bump: *mut u8,
}

/// The half a non-owner reads: one word, and it is compared for identity
/// rather than dereferenced, which is what lets a freeing thread read it
/// while the owner is dying.
///
/// Separate from the private half **as a type**, not by discipline. The
/// same shape in `heap.rs` was `&mut` over the whole header twice, and
/// twice that was a Stacked Borrows violation the audit had to find
/// (`dev/DECISIONS.md`, 2026-07-20: "Making it a type rule was the only
/// option that cannot be violated again"). An owner taking `&mut` over a
/// header that contains an atomic another thread is reading pops that
/// thread's tag.
#[repr(C)]
struct BufferBlockShared {
    owner: AtomicPtr<BufferArena>,
}

/// The contended half, alone on its own cache line by type rather than
/// by a hand-counted pad: chunks freed by threads that do not own this
/// block, as a lock-free MPSC stack threaded through the freed chunks
/// themselves. Per block rather than per arena, which is what makes
/// adoption race-free — a message posted to a dying owner still lands in
/// the block, and whoever owns it next drains it.
#[repr(C, align(64))]
struct BufferBlockRemote {
    remote_free: AtomicPtr<FreeChunk>,
}

/// Per-block header, overlaying the block's first line (256 bytes, of
/// which this uses 128). Split by **access rule**, not by topic, the way
/// `heap.rs` splits its own: the owner's fields and `owner` itself on the
/// first cache line, the stack every other thread writes on the second.
#[repr(C)]
struct BufferBlockHeader {
    /// The pool's discriminant at offset 0, and outside the private half
    /// for the reason `heap::HeapBlockHeader::kind` is outside its own:
    /// the collector reads it while the owner holds a `&mut` over
    /// everything below, and a `&mut` retag covers an atomic as readily
    /// as a plain word.
    kind: AtomicU32,
    private: BufferBlockPrivate,
    shared: BufferBlockShared,
    remote: BufferBlockRemote,
}

impl BufferBlockHeader {
    #[inline]
    fn of_ptr(p: *mut u8) -> *mut BufferBlockHeader {
        ((p as usize) & !BLOCK_MASK) as *mut BufferBlockHeader
    }
}

/// Blocks whose owning thread exited while they still held live chunks.
/// A lock rather than a CAS loop, per the 2026-07-20 decision: the list
/// is touched at thread exit and on the refill path, both cold.
struct Abandoned {
    head: *mut BufferBlockHeader,
}

/// Safe for the same reason the heap's list is: the pointers are block
/// headers, and a block on this list has no owner, so nothing but the
/// adopting thread will touch its private half.
unsafe impl Send for Abandoned {}

static ABANDONED: Mutex<Abandoned> = Mutex::new(Abandoned {
    head: std::ptr::null_mut(),
});

/// Thread-local long-lived buffer arena.
pub struct BufferArena {
    /// The current block's cursor, held here while that block is current
    /// so the allocation path touches one cache line instead of two. The
    /// block's own [`BufferBlockPrivate::bump`] is the copy that survives
    /// this arena.
    bump: *mut u8,
    limit: *mut u8,
    current: *mut BufferBlockHeader,
    /// Every block this arena owns, `current` included. The owner needs
    /// the chain to collect posted frees: a block it has bumped past is
    /// reachable no other way, and its posted chunks would sit forever.
    owned: *mut BufferBlockHeader,
}

impl Default for BufferArena {
    fn default() -> Self {
        Self::new()
    }
}

impl BufferArena {
    pub const fn new() -> Self {
        BufferArena {
            bump: std::ptr::null_mut(),
            limit: std::ptr::null_mut(),
            current: std::ptr::null_mut(),
            owned: std::ptr::null_mut(),
        }
    }

    /// This arena's identity, as stored in `BufferBlockHeader::owner`.
    /// The pointer is compared, never dereferenced, by anyone but the
    /// owner itself.
    #[inline]
    fn id(&mut self) -> *mut BufferArena {
        self as *mut BufferArena
    }

    /// Allocate at least `size` bytes; returns `(ptr, granted)` where
    /// `granted >= size` is the real capacity handed out (a reused
    /// chunk may be bigger; the caller keeps it all).
    pub fn alloc(&mut self, size: usize) -> (*mut u8, usize) {
        let size = round_up_8(size).max(MIN_CHUNK);
        assert!(
            size <= BLOCK_PAYLOAD,
            "over-block buffers are OS-direct, not buffer-arena"
        );

        // Critical mode consults the current block's free list first.
        if pressure_mode() == PressureMode::Critical {
            if let Some(hit) = self.pop_fit(size) {
                return hit;
            }
        }

        if (self.bump.is_null() || (self.remaining()) < size) && !self.rotate_block(size) {
            // Out of memory: null with a zero grant, so a caller that
            // ignores the pointer cannot mistake the capacity for real.
            return (std::ptr::null_mut(), 0);
        }

        let p = self.bump;
        self.bump = p.wrapping_add(size);
        unsafe { (*self.current).private.live += 1 };
        (p, size)
    }

    /// Grow the chunk at `ptr` from `old_size` to `new_size` without
    /// moving it, when it is the last one bumped and the block has room.
    /// False when it is not, and nothing is changed then.
    ///
    /// The condition is `ptr + old_size == bump`, which also establishes
    /// that the chunk is in the current block and owned by this thread:
    /// `bump` points into `current`, and a chunk in any other block, or
    /// one this arena does not own, cannot be adjacent to it.
    ///
    /// `live` is untouched — one chunk before, one chunk after. So is the
    /// free list: nothing is released here, which is the second thing this
    /// path is worth. The growth it replaces frees the old payload, and a
    /// payload freed while a worker trace is in flight has to be withheld
    /// (S38.3);
    /// a payload that never moves has nothing to withhold.
    ///
    /// # Safety
    /// `(ptr, old_size)` must be exactly one live allocation of this arena.
    pub unsafe fn try_grow_in_place(
        &mut self,
        ptr: *mut u8,
        old_size: usize,
        new_size: usize,
    ) -> bool {
        // The same rounding `alloc` applied on the way in, so the
        // adjacency test compares what was really handed out.
        let old_size = round_up_8(old_size).max(MIN_CHUNK);
        let new_size = round_up_8(new_size).max(MIN_CHUNK);

        if new_size > BLOCK_PAYLOAD || ptr.wrapping_add(old_size) != self.bump {
            return false;
        }

        // checked_add: the same overflow discipline as `alloc`.
        match (ptr as usize).checked_add(new_size) {
            Some(new_end) if new_end <= self.limit as usize => {
                self.bump = new_end as *mut u8;
                true
            }
            _ => false,
        }
    }

    /// Free a chunk previously handed out by [`alloc`](Self::alloc) on this
    /// thread. `size` must be the granted capacity (the holding entity tracks
    /// it as the buffer's `capacity` anyway) — the zero-metadata contract.
    ///
    /// # Safety
    /// `ptr`/`size` must be exactly one live allocation of this arena.
    pub unsafe fn free(&mut self, ptr: *mut u8, size: usize) {
        let size = round_up_8(size).max(MIN_CHUNK);
        let block = BufferBlockHeader::of_ptr(ptr);
        debug_assert_eq!(
            unsafe { load_block_kind(&raw const (*block).kind) },
            BLOCK_KIND_BUFFER
        );

        // Not ours: post it and touch nothing else. Neither `live` nor
        // `free` may be written from here — they are the owner's, and an
        // emptied block returned from the wrong thread would go back to
        // the pool while the owner still bumps into it.
        if unsafe { (*block).shared.owner.load(Ordering::Relaxed) } != self.id() {
            return unsafe { post_remote(block, ptr, size) };
        }

        let b = unsafe { &mut (*block).private };
        b.live -= 1;

        // A fully-empty non-current block goes home; the current block
        // stays (its bump is still advancing).
        if b.live == 0 && block != self.current {
            self.retire(block);
            return;
        }

        let chunk = ptr as *mut FreeChunk;
        unsafe {
            (*chunk).next = b.free;
            (*chunk).size = size;
        }

        b.free = chunk;
    }

    /// First-fit over the free lists of the blocks this arena owns,
    /// current first, spending at most [`CRITICAL_SEARCH_BOUND`] misses
    /// across the whole chain. Takes the whole chunk: no splitting, the
    /// caller keeps the granted capacity.
    ///
    /// The chain and not just the current block, because an adopted
    /// block arrives with the free list of a thread that has exited, and
    /// that list is the one memory this arena holds and nobody is going
    /// to ask for again. The budget is shared rather than per block so a
    /// long chain cannot turn the bounded walk `buffers.md` promises into
    /// a linear one.
    fn pop_fit(&mut self, size: usize) -> Option<(*mut u8, usize)> {
        let mut budget = CRITICAL_SEARCH_BOUND;
        if let Some(hit) = pop_fit_in(self.current, size, &mut budget) {
            return Some(hit);
        }

        let mut block = self.owned;
        while !block.is_null() && budget > 0 {
            let next = unsafe { (*block).private.owned_next };
            if block != self.current
                && let Some(hit) = pop_fit_in(block, size, &mut budget)
            {
                return Some(hit);
            }

            block = next;
        }

        None
    }

    #[inline]
    /// What is left in the current block, for a test that has to know
    /// whether a growth could have been served in place.
    ///
    /// A test-only shorthand rather than a widening of [`remaining`]:
    /// production code decides adjacency and room inside
    /// [`try_grow_in_place`](Self::try_grow_in_place), where both are one
    /// question, and a caller able to ask them separately would be able
    /// to act on an answer that is stale by the time it does.
    #[cfg(test)]
    pub(crate) fn room_in_the_current_block(&self) -> usize {
        self.remaining()
    }

    fn remaining(&self) -> usize {
        self.limit as usize - self.bump as usize
    }

    /// Link a block this arena now owns into its chain.
    fn own(&mut self, block: *mut BufferBlockHeader) {
        unsafe { (*block).private.owned_next = self.owned };
        self.owned = block;
    }

    /// Unlink an owned block and give it back to the pool. The unlink is
    /// a walk rather than a doubly-linked splice: a buffer arena holds a
    /// handful of blocks, and the chain is touched only when one empties
    /// or the thread ends.
    fn retire(&mut self, block: *mut BufferBlockHeader) {
        debug_assert!(unsafe { (*block).private.live } == 0);
        let mut link = &raw mut self.owned;
        unsafe {
            while !(*link).is_null() {
                if *link == block {
                    *link = (*block).private.owned_next;
                    break;
                }

                link = &raw mut (**link).private.owned_next;
            }

            (*block)
                .shared
                .owner
                .store(std::ptr::null_mut(), Ordering::Release);
        }

        // The kind is not cleared here: `put` stamps `BLOCK_KIND_FREE`
        // itself, through the release store the collector's acquire load
        // of every block's kind requires, and it reads the old value on
        // the way in — the journal's block-release record names the kind
        // the block arrived with (`debug-modes.md` §9.5).
        BlockPool::global().put(block as *mut BlockHeader);
    }

    /// Take the chunks other threads posted into `block` onto its own
    /// free list and account for them. Returns true when the block is
    /// empty afterwards.
    ///
    /// The count is what makes `live` truthful again: a posted chunk was
    /// never subtracted, exactly so that the block could not look empty
    /// while the posting thread still held its address.
    fn collect_remote(&self, block: *mut BufferBlockHeader) -> bool {
        let head = unsafe {
            (*block)
                .remote
                .remote_free
                .swap(std::ptr::null_mut(), Ordering::Acquire)
        };

        if head.is_null() {
            return unsafe { (*block).private.live } == 0;
        }

        let b = unsafe { &mut (*block).private };
        let mut n = 0u32;
        let mut last = head;
        unsafe {
            loop {
                n += 1;
                let next = (*last).next;
                if next.is_null() {
                    break;
                }

                last = next;
            }

            (*last).next = b.free;
        }

        b.free = head;
        b.live -= n;
        b.live == 0
    }

    /// Collect every owned block and return the ones that emptied. The
    /// sweep is not optional: a block the bump has moved past is
    /// reachable only through this chain, so without it the chunks other
    /// threads posted into it would sit there for the life of the
    /// thread and the block would never go home.
    fn collect_owned(&mut self) {
        let mut block = self.owned;
        while !block.is_null() {
            let next = unsafe { (*block).private.owned_next };
            if self.collect_remote(block) && block != self.current {
                self.retire(block);
            }

            block = next;
        }
    }

    /// Take over one block from a thread that exited holding chunks, so
    /// its memory comes back into circulation and the frees still being
    /// posted into it have a collector again.
    ///
    /// True when the block became this arena's current one, which happens
    /// when the tail its previous owner left can serve `size`: the
    /// allocation that triggered the rotation is then served from memory
    /// this process already holds and no pool block is taken. False when
    /// there was nothing to adopt, when the block turned out empty and
    /// went home, or when its tail is too short — the block is owned and
    /// swept from then on either way, its tail is reachable through
    /// [`resume_owned`](Self::resume_owned) and its free list through
    /// [`pop_fit`](Self::pop_fit).
    ///
    /// One block per call: a rotation that adopts a block too full to
    /// serve it moves on to the rest of the refill path rather than
    /// walking the list for a better fit.
    fn adopt(&mut self, size: usize) -> bool {
        let block = {
            let mut list = ABANDONED.lock().unwrap();
            let head = list.head;
            if head.is_null() {
                return false;
            }

            list.head = unsafe { (*head).private.owned_next };
            head
        };

        unsafe {
            (*block).private.owned_next = std::ptr::null_mut();
            // Claim it. A free racing this read either saw null or sees
            // us; both post into `remote_free`, which is now ours to
            // collect.
            (*block).shared.owner.store(self.id(), Ordering::Release);
        }

        self.own(block);
        if self.collect_remote(block) {
            // Everything it held was freed while it was ownerless.
            self.retire(block);
            return false;
        }

        if tail_of(block) < size {
            return false;
        }

        self.make_current(block);
        true
    }

    /// Settle the current block's cursor in its own header, so whoever
    /// holds the block next resumes where this arena stopped. Called at
    /// every point the block stops being current: rotation, and hand-over
    /// at thread exit.
    fn settle_cursor(&mut self) {
        if !self.current.is_null() {
            unsafe { (*self.current).private.bump = self.bump };
        }
    }

    /// Make `block` this arena's current one and take up its cursor,
    /// settling the outgoing block's first.
    ///
    /// The single writer of the `current`/`bump`/`limit` triple, so the
    /// settle can never be forgotten at one of the three sites that hand
    /// the role over. The assertion is what a stale or foreign cursor
    /// looks like from here, and the subtraction in [`tail_of`] would
    /// otherwise wrap into a tail big enough to satisfy any request.
    fn make_current(&mut self, block: *mut BufferBlockHeader) {
        let bump = unsafe { (*block).private.bump };
        let base = block as *mut u8;
        debug_assert!(
            bump >= base.wrapping_add(LINE_SIZE)
                && bump <= base.wrapping_add(crate::memory::block_pool::BLOCK_SIZE),
            "a block's cursor points outside its own block"
        );

        self.settle_cursor();
        self.current = block;
        self.bump = bump;
        self.limit = base.wrapping_add(crate::memory::block_pool::BLOCK_SIZE);
    }

    /// Go back to an owned block whose unused tail can serve `size` and
    /// make it current. False when none has the room.
    ///
    /// Two blocks reach this state. One this arena bumped past because a
    /// larger request did not fit, and one it adopted whose tail was too
    /// short for the request that adopted it — an adopted block is
    /// otherwise looked at once, on the rotation that claimed it, and a
    /// smaller request later would never see the 63 KiB it came with.
    ///
    /// Requires every owned cursor to be settled
    /// ([`settle_cursor`](Self::settle_cursor)), which is what makes the walk
    /// read all blocks the same way.
    ///
    /// O(blocks this arena owns), on the path that would otherwise take a
    /// block from the pool — the same trade `collect_owned` makes, on the
    /// same chain, in the same call.
    fn resume_owned(&mut self, size: usize) -> bool {
        let mut block = self.owned;
        while !block.is_null() {
            if tail_of(block) >= size {
                self.make_current(block);
                return true;
            }

            block = unsafe { (*block).private.owned_next };
        }

        false
    }

    /// Hand this arena's blocks over at thread exit: the empty ones to
    /// the pool, the ones still holding chunks to the abandoned list.
    ///
    /// Dropping them instead is what the old `Drop` did, and it was only
    /// safe while every chunk was freed by the thread that allocated it:
    /// a block with live chunks was called the owner's bug. With entity
    /// bodies living here the holder can be any thread, so the block has
    /// to stay findable.
    fn hand_over(&mut self) {
        // The tail of the current block is the one thing here that only
        // this arena knows; settled now, it is what the adopter resumes.
        self.settle_cursor();
        self.current = std::ptr::null_mut();
        self.bump = std::ptr::null_mut();
        self.limit = std::ptr::null_mut();

        let mut list = ABANDONED.lock().unwrap();
        let mut block = self.owned;
        while !block.is_null() {
            let next = unsafe { (*block).private.owned_next };
            // Collect first: a block that the posted frees have emptied
            // is worth more to the pool than to the abandoned list.
            let empty = self.collect_remote(block);
            unsafe {
                (*block)
                    .shared
                    .owner
                    .store(std::ptr::null_mut(), Ordering::Release)
            };

            if empty {
                // The kind stays for `put` to overwrite, as in `retire`.
                unsafe { (*block).private.owned_next = std::ptr::null_mut() };
                BlockPool::global().put(block as *mut BlockHeader);
            } else {
                unsafe { (*block).private.owned_next = list.head };
                list.head = block;
            }

            block = next;
        }

        self.owned = std::ptr::null_mut();
    }

    /// False when the OS refuses. The arena is left empty rather than
    /// half-rotated: the previous current block has already gone home by
    /// then, so it must not stay referenced.
    #[must_use]
    fn rotate_block(&mut self, size: usize) -> bool {
        // Posted frees first: they may have emptied blocks this arena
        // bumped past, and an emptied block is worth more to the pool
        // than a fresh one is to this thread.
        self.collect_owned();

        // An old current that emptied while current can only go home
        // now, at rotation — `free` keeps it alive until this moment.
        if !self.current.is_null() && unsafe { (*self.current).private.live } == 0 {
            let old = self.current;
            self.current = std::ptr::null_mut();
            self.retire(old);
        }

        // The old current stops being current here whatever comes next,
        // so its cursor is settled before anything reads the chain: from
        // this line on, every owned block's tail is in its own header.
        self.settle_cursor();

        // Adoption before this arena's own tails, which is the opposite
        // of `heap.rs`, where a block with room is found in `available`
        // and the abandoned list is reached only when there is none. The
        // reason to differ: a block with no owner has nobody to collect
        // the frees still being posted into it, and every rotation is one
        // pickup, so the number of ownerless blocks cannot grow with a
        // thread that keeps finding room in its own. The price is that a
        // busy arena accumulates foreign blocks it can never empty, and
        // pays their length on each of the three chain walks a rotation
        // makes (`dev/DECISIONS.md`, 2026-08-05).
        if self.adopt(size) {
            return true;
        }

        // Then the tails already in hand, including the one just adopted
        // for a request it could not serve.
        if self.resume_owned(size) {
            return true;
        }

        let block = BlockPool::global().get() as *mut BufferBlockHeader;
        if block.is_null() {
            // The old current has already gone home above, so leave the
            // arena empty rather than pointing at a freed block.
            self.current = std::ptr::null_mut();
            self.bump = std::ptr::null_mut();
            self.limit = std::ptr::null_mut();
            return false;
        }

        let id = self.id();
        // Field by field, and the kind last through `store_block_kind`.
        // A whole-header store is the one access the atomic type does not
        // defend against: it writes those four bytes plainly, and the
        // collector reads the kind of every block of every region
        // (`block_pool::store_block_kind`, `Heap::refill`).
        unsafe {
            let private = &raw mut (*block).private;
            (&raw mut (*private).live).write(0);
            (&raw mut (*private).free).write(std::ptr::null_mut());
            (&raw mut (*private).owned_next).write(std::ptr::null_mut());
            (&raw mut (*private).bump).write((block as *mut u8).wrapping_add(LINE_SIZE));
            (&raw mut (*block).shared).write(BufferBlockShared {
                owner: AtomicPtr::new(id),
            });

            (&raw mut (*block).remote).write(BufferBlockRemote {
                remote_free: AtomicPtr::new(std::ptr::null_mut()),
            });

            crate::memory::block_pool::store_block_kind(
                &raw const (*block).kind,
                BLOCK_KIND_BUFFER,
            );
        }

        self.own(block);
        self.current = block;
        self.bump = (block as *mut u8).wrapping_add(LINE_SIZE);
        self.limit = (block as *mut u8).wrapping_add(crate::memory::block_pool::BLOCK_SIZE);
        true
    }
}

impl Drop for BufferArena {
    /// A dying thread must not take its blocks with it, and since
    /// 2026-08-04 it must not drop the ones still holding chunks either:
    /// a chunk here is an entity's body and its holder may be any
    /// thread. [`BufferArena::hand_over`] gives the empty blocks to the
    /// pool and the rest to the abandoned list.
    fn drop(&mut self) {
        self.hand_over();
    }
}

/// Bytes between a block's settled cursor and its end. Meaningful for
/// any block whose cursor is settled, which is every owned block except
/// the current one ([`BufferArena::settle_cursor`]).
fn tail_of(block: *mut BufferBlockHeader) -> usize {
    let end = (block as *mut u8).wrapping_add(crate::memory::block_pool::BLOCK_SIZE) as usize;
    end - unsafe { (*block).private.bump } as usize
}

/// First-fit within one block's free list, spending at most `budget`
/// misses and decrementing it by what was spent. Null block, or an empty
/// list, is a miss that costs nothing.
///
/// A free function because it needs no arena state: the block it works on
/// is whichever the caller is walking, current or not.
fn pop_fit_in(
    block: *mut BufferBlockHeader,
    size: usize,
    budget: &mut usize,
) -> Option<(*mut u8, usize)> {
    if block.is_null() {
        return None;
    }

    let b = unsafe { &mut (*block).private };

    let mut prev: *mut *mut FreeChunk = &mut b.free;
    unsafe {
        while !(*prev).is_null() && *budget > 0 {
            let chunk = *prev;
            if (*chunk).size >= size {
                *prev = (*chunk).next;
                b.live += 1;
                return Some((chunk as *mut u8, (*chunk).size));
            }

            prev = &mut (*chunk).next;
            *budget -= 1;
        }
    }

    None
}

/// Post a chunk to the block's cross-thread stack: one CAS loop, and
/// nothing else in the block is touched.
///
/// The link is written into the freed chunk itself, which is sound for
/// the same reason the owner's free list is — the chunk is dead, and its
/// first 16 bytes are the arena's by contract. A chunk a worker trace may
/// still be reading must not reach here — S38.3 owns that cross-thread
/// withholding (`PLAN.md`).
///
/// # Safety
/// `(ptr, size)` is a live chunk of `block`, freed by this call.
unsafe fn post_remote(block: *mut BufferBlockHeader, ptr: *mut u8, size: usize) {
    let chunk = ptr as *mut FreeChunk;
    unsafe { (*chunk).size = size };
    let head = unsafe { &(*block).remote.remote_free };
    let mut top = head.load(Ordering::Relaxed);
    loop {
        unsafe { (*chunk).next = top };
        match head.compare_exchange_weak(top, chunk, Ordering::Release, Ordering::Relaxed) {
            Ok(_) => return,
            Err(now) => top = now,
        }
    }
}

thread_local! {
    /// This thread's persistent buffer arena, behind a raw pointer in a
    /// `Cell` rather than a `RefCell<BufferArena>` — the shape every
    /// thread-local reachable from thread exit was converted to on
    /// 2026-08-03. What keeps drop glue is the four structures that
    /// need a destructor of their own: the two memory reserves, the
    /// pool's thread cache and the exit guard (`dev/DECISIONS.md`,
    /// "what the first touch of a thread-local with drop glue may
    /// cost").
    ///
    /// The thread-exit path reaches this arena: static-block teardown
    /// runs `__destruct` bodies, those release entities, and a dying
    /// dynamic string frees its payload here
    /// (`string::string_die`). `ll_thread_exit` is itself called from a
    /// TLS destructor, glibc runs those in reverse registration order,
    /// and that puts the exit guard **last** exactly because
    /// `ll_thread_init` registers it first. A `RefCell<BufferArena>` has
    /// drop glue, so its key would be registered and reliably already
    /// destroyed by then; `with` would panic with `AccessError`, and a
    /// panic inside a TLS destructor cannot unwind — the process aborts.
    ///
    /// A `Cell<*mut _>` has no drop glue, is never registered, and stays
    /// readable for the whole life of the thread. [`dispose`] frees it
    /// explicitly, at the position `ll_thread_exit` chooses.
    static THREAD_BUFFER_ARENA: Cell<*mut BufferArena> = const { Cell::new(std::ptr::null_mut()) };
}

/// Run `f` with this thread's persistent long-lived buffer arena,
/// creating it on first use.
///
/// The `RefCell`'s borrow guard is gone with the conversion, and nothing
/// needs it: no path inside `BufferArena` calls back out, so there is no
/// reentrancy to catch.
pub fn with_buffer_arena<R>(f: impl FnOnce(&mut BufferArena) -> R) -> R {
    let arena = THREAD_BUFFER_ARENA.with(|cell| {
        let mut p = cell.get();
        if p.is_null() {
            p = Box::into_raw(Box::new(BufferArena::new()));
            cell.set(p);
        }

        p
    });

    f(unsafe { &mut *arena })
}

/// Give this thread's buffer arena back, running its [`Drop`] by hand.
///
/// Called from `heap::ll_thread_exit` rather than from a TLS destructor,
/// which is the whole point (see [`THREAD_BUFFER_ARENA`]). Its position
/// there is **after** every step that can still free a buffer — the
/// static blocks whose teardown runs user code, and the withheld backlog
/// whose flush routes payload frees back here — and the blocks it
/// returns go to the process-global pool, which outlives every thread.
///
/// Null-tolerant and idempotent: a thread that never allocated a buffer,
/// and a second call, both find nothing. Disposing too early is not
/// caught: a later free would silently build a second arena through the
/// lazy path above and leak it, which is why the position is stated
/// rather than assumed.
pub fn dispose() {
    let p = THREAD_BUFFER_ARENA.with(|cell| cell.replace(std::ptr::null_mut()));
    if !p.is_null() {
        drop(unsafe { Box::from_raw(p) });
    }
}

// --- Long-lived growth over the arena -------------------------------------

/// Allocate a long-lived payload of `size` bytes, routed the way
/// `rfc/model/memory/buffers.md` routes them: an arena chunk while it fits
/// in one block, an OS-direct run above that. Null with a zero grant on
/// refusal, so a caller that ignores the pointer cannot mistake the
/// capacity for real.
///
/// The second number is the bytes **really granted**, which a reused chunk
/// can make larger than the request. A caller keeps that number and hands
/// it back to [`buffer_free_longlived_payload`], the arena's free being
/// size-carrying; freeing by the request instead loses the difference from
/// the block's free list.
///
/// This is where the size split lives, so that the entity holding a payload — a
/// string's bytes, an array's table storage — carries no knowledge of how
/// big a block is. `Arena::alloc_body` is the request-arena counterpart.
pub fn buffer_alloc_longlived_payload(size: usize) -> (*mut u8, usize) {
    // The other half of the fault injection at `buffer_ensure_longlived`,
    // and the half a carried array storage takes.
    #[cfg(test)]
    if longlived_refusal_takes_this_one() {
        return (std::ptr::null_mut(), 0);
    }

    if size <= BLOCK_PAYLOAD {
        return with_buffer_arena(|a| a.alloc(size));
    }

    let p = unsafe { crate::memory::stdapi::ll_alloc(size, 16) };
    if p.is_null() { (p, 0) } else { (p, size) }
}

/// Long-lived counterpart of `buffer_ensure`: extend off the bump top
/// when the payload is the last chunk bumped, otherwise alloc-new + copy
/// + free-old. Size routing per `rfc/model/memory/buffers.md`: payloads
/// over a block payload are OS-direct.
pub fn buffer_ensure_longlived(buf: &mut Buffer, min_capacity: usize, hint: usize) -> *mut u8 {
    // Fault injection, tests only, and narrower than
    // `block_pool::FORCE_OOM` on purpose: that flag refuses the *pool*,
    // and this arena can serve a request from a block it already owns or
    // adopts, so a test that needs this allocation refused cannot get one
    // deterministically through the pool (a promote test was flaky 5 runs
    // in 40 before this existed). Named for the one allocation it
    // refuses, so a test using it says which.
    #[cfg(test)]
    if longlived_refusal_takes_this_one() {
        return std::ptr::null_mut();
    }

    if buf.capacity >= min_capacity {
        return buf.data;
    }

    let target = round_up_8(crate::memory::buffer::desired_capacity(
        buf.capacity,
        min_capacity,
        hint,
    ));

    // An append loop on the newest buffer hits this every time: nothing
    // has been bumped since, so the payload is still at the top and the
    // growth costs a pointer store instead of an allocation and a copy of
    // everything written so far. Refused for an OS-direct payload, whose
    // block header is not a buffer block's, and for one that is not
    // adjacent to the bump — where the reallocating path below is right.
    if !buf.data.is_null()
        && target <= BLOCK_PAYLOAD
        && buf.capacity <= BLOCK_PAYLOAD
        && with_buffer_arena(|a| unsafe { a.try_grow_in_place(buf.data, buf.capacity, target) })
    {
        buf.capacity = round_up_8(target).max(MIN_CHUNK);
        return buf.data;
    }

    let (new_data, granted) = buffer_alloc_longlived_payload(target);

    if new_data.is_null() {
        // Out of memory. Leave the buffer exactly as it was — its old
        // payload is still live and still its capacity — and report.
        // Freeing the old one here would turn a failed growth into a
        // dangling buffer.
        return std::ptr::null_mut();
    }

    if buf.len > 0 {
        unsafe { std::ptr::copy_nonoverlapping(buf.data, new_data, buf.len) };
    }

    if !buf.data.is_null() {
        unsafe { buffer_free_longlived_payload(buf.data, buf.capacity) };
    }

    buf.data = new_data;
    buf.capacity = granted;
    buf.data
}

/// Release a long-lived payload, routing by the owning block's kind.
///
/// # Safety
/// `(ptr, capacity)` must be a live payload from
/// [`buffer_ensure_longlived`] on this thread, not freed yet.
pub unsafe fn buffer_free_longlived_payload(ptr: *mut u8, capacity: usize) {
    let kind = unsafe { load_block_kind(((ptr as usize) & !BLOCK_MASK) as *const AtomicU32) };
    if kind == crate::memory::block_pool::BLOCK_KIND_RETAINED {
        // A payload promotion could not carry: the reset kept its block
        // out of circulation instead (`string::carry_payload_out_of`).
        // The bytes stay where they are — former arena memory has no free
        // list to take them back — and this call is the payload's death
        // event, which is what the block was waiting for. With its last
        // occupant and its last payload gone the block goes home.
        //
        // A worker trace in flight has to stop this: the trace holds
        // addresses inside the block, and a block handed to the pool is
        // re-stamped as another kind under them. `rc-walk` held back the
        // whole call for the length of its epoch; S38.3 owns `rc-cycle`'s
        // narrower per-owner replacement and is not built.
        let block = (ptr as usize) & !BLOCK_MASK;
        if unsafe { crate::memory::retained::payload_freed(block) } {
            unsafe { crate::memory::retained::release_emptied(block) };
        }
    } else if kind == BLOCK_KIND_BUFFER {
        // The same hazard as the retained arm above, and the same gap:
        // `free` decrements the block's live count, and an emptied block
        // goes back to the global pool to be re-stamped as another kind
        // while a trace still holds addresses inside it. S38.3 owns the
        // replacement withholding for this buffer-chunk path.
        unsafe { free_chunk(ptr, capacity) };
    } else {
        // OS-direct run: the standard path frees it by mask.
        unsafe { crate::memory::stdapi::ll_free(ptr) };
    }
}

/// Makes both long-lived allocations — [`buffer_ensure_longlived`] and
/// [`buffer_alloc_longlived_payload`] — report exhaustion, which is every
/// body allocation a `GcHeap` or `LongLived` owner can make
/// (`memory/routing.rs`), from the first request on unless
/// [`SERVE_BEFORE_REFUSING`] holds some back. Test-only, and the only
/// deterministic way to reach a pinned payload: see the note at the first
/// of the two.
#[cfg(test)]
pub(crate) static FORCE_REFUSE_LONGLIVED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// How many allocations the flag above has refused, for a test whose
/// subject is what happens **after** a refusal. Such a test asserts on
/// state the refused entity no longer owns — a block's kind, an index —
/// and the same state can be reached with no refusal at all, so the
/// count is what tells the two runs apart
/// (`dev/POSTMORTEM.md`, "a forced-refusal test that never proved the
/// refusal").
#[cfg(test)]
pub(crate) static REFUSALS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The refusals so far, which a test reads on both sides of the call it
/// is forcing.
#[cfg(test)]
pub(crate) fn refusals() -> usize {
    REFUSALS.load(std::sync::atomic::Ordering::Relaxed)
}

/// How many long-lived requests the flag serves before it begins
/// refusing, spent one per request. Zero — the default — refuses the
/// first, which is what a test aiming at the first allocation of a path
/// wants.
///
/// A test whose subject is a later allocation sets it, so that the
/// refusal it studies is the one it names. The array copy is why it
/// exists: it asks for its destination's storage before it asks for its
/// work list, and a test about the work list stops at the storage
/// otherwise — passing on assertions that hold either way, which is the
/// shape `dev/POSTMORTEM.md` records as "a forced-refusal test that
/// never proved the refusal".
///
/// The unit is the injection point rather than the growth: a
/// [`buffer_ensure_longlived`] that reallocates consults this on its own
/// way in and again through [`buffer_alloc_longlived_payload`], so it
/// spends two. A test aiming past an `ensure` counts accordingly.
///
/// The test that sets it lowers it again, the count being global to the
/// process like the flag it qualifies.
#[cfg(test)]
pub(crate) static SERVE_BEFORE_REFUSING: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Whether the injected refusal takes this request, spending one of the
/// grace count when it does not. Single-threaded by construction: a test
/// raising the flag owns the process for its scope.
#[cfg(test)]
fn longlived_refusal_takes_this_one() -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    if !FORCE_REFUSE_LONGLIVED.load(Relaxed) {
        return false;
    }

    let grace = SERVE_BEFORE_REFUSING.load(Relaxed);
    if grace > 0 {
        SERVE_BEFORE_REFUSING.store(grace - 1, Relaxed);
        return false;
    }

    REFUSALS.fetch_add(1, Relaxed);
    true
}

/// Free a chunk without building this thread's arena to do it.
///
/// A thread that never allocated a buffer can still be the one that drops
/// the last reference to a string another thread built — that is what the
/// ownership protocol is for. Going through [`with_buffer_arena`] would
/// allocate a `BufferArena` on the system allocator just to compute an
/// identity that cannot match, and `Box::new` aborts the process when it
/// refuses: an abort on a free path. `stdapi::ll_free` answers the same
/// question the same way for an entity slot with no thread heap.
///
/// # Safety
/// `(ptr, capacity)` must be exactly one live chunk of some arena.
unsafe fn free_chunk(ptr: *mut u8, capacity: usize) {
    let existing = THREAD_BUFFER_ARENA.with(|cell| cell.get());
    if existing.is_null() {
        let block = BufferBlockHeader::of_ptr(ptr);
        let size = round_up_8(capacity).max(MIN_CHUNK);
        return unsafe { post_remote(block, ptr, size) };
    }

    unsafe { (*existing).free(ptr, capacity) };
}

/// Release a long-lived buffer: frees the payload, zeroes the struct.
///
/// # Safety
/// `buf` must be live and owned by this thread; not used after except
/// to grow again from empty.
pub unsafe fn buffer_release_longlived(buf: &mut Buffer) {
    if !buf.data.is_null() {
        unsafe { buffer_free_longlived_payload(buf.data, buf.capacity) };
    }

    *buf = Buffer::new();
}

// --- C ABI ---------------------------------------------------------------

/// Long-lived `ll_buffer_ensure`: same contract, thread-persistent
/// buffer arena instead of the request arena. `ctx` ignored (ABI
/// uniformity).
///
/// # Safety
/// `buf` must point to a live long-lived `Buffer` owned by this thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_buffer_ensure_longlived(
    _ctx: *mut crate::memory::context::LLContext,
    buf: *mut Buffer,
    min_capacity: usize,
    hint: usize,
) -> *mut u8 {
    buffer_ensure_longlived(unsafe { &mut *buf }, min_capacity, hint)
}

/// Free a long-lived buffer's payload and zero the struct.
///
/// # Safety
/// Same ownership contract as [`ll_buffer_ensure_longlived`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_buffer_release_longlived(
    _ctx: *mut crate::memory::context::LLContext,
    buf: *mut Buffer,
) {
    unsafe { buffer_release_longlived(&mut *buf) }
}

#[cfg(test)]
mod tests;
