//! Long-lived buffer arena: `BLOCK_KIND_BUFFER`.
//!
//! Realloc-heavy buffer churn isolated from the object heap so its
//! fragmentation never pollutes it (`rfc/model/memory/buffers.md`). No
//! size classes — buffers vary continuously — so this is bump
//! allocation plus a **per-block intrusive LIFO free list**:
//!
//! - the list head lives in the block's header, the chain never leaves
//!   the block (L2-resident by construction);
//! - a freed chunk threads `{ next, size }` through its own payload —
//!   zero metadata on live buffers (minimum chunk is 16 bytes for it);
//! - `plenty`/`tight` allocation just bumps; `critical` first-fit
//!   searches at most [`CRITICAL_SEARCH_BOUND`] entries of the current
//!   block's list;
//! - chunks never coalesce (accepted: damage bounded by one block);
//! - a per-block live count returns fully-emptied blocks to the pool.
//!
//! Payloads larger than a block payload are OS-direct (`ll_alloc` /
//! `ll_free`), invisible to this machinery.
//!
//! Phase 1 limit, same as the early heap: the arena is thread-local
//! and frees must come from the owning thread. The cross-thread story
//! (heap.rs remote-free) can be replicated when a real consumer needs
//! it.

use crate::memory::arena::round_up_8;
use crate::memory::block_pool::{
    BLOCK_KIND_BUFFER, BLOCK_MASK, BLOCK_PAYLOAD, BlockHeader, BlockPool, LINE_SIZE,
};
use crate::memory::buffer::{Buffer, PressureMode, pressure_mode};

/// Bound on the critical-mode free-list walk. Tunable; calibration is
/// blocked on real workloads (PLAN.md).
pub const CRITICAL_SEARCH_BOUND: usize = 16;

/// A free chunk threads this through its own first 16 bytes.
#[repr(C)]
struct FreeChunk {
    next: *mut FreeChunk,
    size: usize,
}

const MIN_CHUNK: usize = size_of::<FreeChunk>();

/// Per-block header, overlaying the block's first line. Shares offset 0
/// (`kind`) with the pool's `BlockHeader`.
#[repr(C)]
struct BufferBlockHeader {
    kind: u32,
    /// Live chunks in this block; back to the pool at 0.
    live: u32,
    /// Head of the block-local free list.
    free: *mut FreeChunk,
}

impl BufferBlockHeader {
    #[inline]
    fn of_ptr(p: *mut u8) -> *mut BufferBlockHeader {
        ((p as usize) & !BLOCK_MASK) as *mut BufferBlockHeader
    }
}

/// Thread-local long-lived buffer arena.
pub struct BufferArena {
    bump: *mut u8,
    limit: *mut u8,
    current: *mut BufferBlockHeader,
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
        }
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

        if self.bump.is_null() || (self.remaining()) < size {
            self.rotate_block();
        }

        let p = self.bump;
        self.bump = p.wrapping_add(size);
        unsafe { (*self.current).live += 1 };
        (p, size)
    }

    /// Free a chunk previously handed out by [`alloc`] on this thread.
    /// `size` must be the granted capacity (the owner tracks it as the
    /// buffer's `capacity` anyway) — the zero-metadata contract.
    ///
    /// # Safety
    /// `ptr`/`size` must be exactly one live allocation of this arena.
    pub unsafe fn free(&mut self, ptr: *mut u8, size: usize) {
        let size = round_up_8(size).max(MIN_CHUNK);
        let block = BufferBlockHeader::of_ptr(ptr);
        debug_assert_eq!(unsafe { (*block).kind }, BLOCK_KIND_BUFFER);

        let b = unsafe { &mut *block };
        b.live -= 1;

        // A fully-empty non-current block goes home; the current block
        // stays (its bump is still advancing).
        if b.live == 0 && block != self.current {
            b.kind = 0;
            BlockPool::global().put(block as *mut BlockHeader);
            return;
        }

        let chunk = ptr as *mut FreeChunk;
        unsafe {
            (*chunk).next = b.free;
            (*chunk).size = size;
        }
        b.free = chunk;
    }

    /// First-fit over the current block's list, bounded by
    /// [`CRITICAL_SEARCH_BOUND`]. Takes the whole chunk: no splitting,
    /// the caller keeps the granted capacity.
    fn pop_fit(&mut self, size: usize) -> Option<(*mut u8, usize)> {
        if self.current.is_null() {
            return None;
        }
        let b = unsafe { &mut *self.current };

        let mut prev: *mut *mut FreeChunk = &mut b.free;
        let mut walked = 0;
        unsafe {
            while !(*prev).is_null() && walked < CRITICAL_SEARCH_BOUND {
                let chunk = *prev;
                if (*chunk).size >= size {
                    *prev = (*chunk).next;
                    b.live += 1;
                    return Some((chunk as *mut u8, (*chunk).size));
                }
                prev = &mut (*chunk).next;
                walked += 1;
            }
        }
        None
    }

    #[inline]
    fn remaining(&self) -> usize {
        self.limit as usize - self.bump as usize
    }

    fn rotate_block(&mut self) {
        // An old current that emptied while current can only go home
        // now, at rotation — `free` keeps it alive until this moment.
        if !self.current.is_null() && unsafe { (*self.current).live } == 0 {
            unsafe { (*self.current).kind = 0 };
            BlockPool::global().put(self.current as *mut BlockHeader);
        }

        let block = BlockPool::global().get() as *mut BufferBlockHeader;
        unsafe {
            block.write(BufferBlockHeader {
                kind: BLOCK_KIND_BUFFER,
                live: 0,
                free: std::ptr::null_mut(),
            });
        }
        self.current = block;
        self.bump = (block as *mut u8).wrapping_add(LINE_SIZE);
        self.limit = (block as *mut u8).wrapping_add(crate::memory::block_pool::BLOCK_SIZE);
    }
}

impl Drop for BufferArena {
    /// A dying thread must not take its buffer block with it. `free` never
    /// returns the *current* block (only rotation does), so an arena whose
    /// last chunk was freed would still leak that block at thread exit.
    /// Return it if it holds no live chunks. A block with live chunks means
    /// the owner never freed those buffers — its bug — and the data cannot
    /// be reclaimed here; nothing else this arena owns is reachable (rotated
    /// blocks return themselves when their last chunk frees).
    fn drop(&mut self) {
        if !self.current.is_null() && unsafe { (*self.current).live } == 0 {
            unsafe { (*self.current).kind = 0 };
            BlockPool::global().put(self.current as *mut BlockHeader);
            self.current = std::ptr::null_mut();
        }
    }
}

thread_local! {
    static THREAD_BUFFER_ARENA: std::cell::RefCell<BufferArena> =
        const { std::cell::RefCell::new(BufferArena::new()) };
}

/// Run `f` with this thread's persistent long-lived buffer arena.
pub fn with_buffer_arena<R>(f: impl FnOnce(&mut BufferArena) -> R) -> R {
    THREAD_BUFFER_ARENA.with(|a| f(&mut a.borrow_mut()))
}

// --- Long-lived growth over the arena -------------------------------------

/// Long-lived counterpart of `buffer_ensure`: no bump-top trick (no
/// arena reset will save us), growth is alloc-new + copy + free-old.
/// Size routing per `rfc/model/memory/buffers.md`: payloads over a
/// block payload are OS-direct.
pub fn buffer_ensure_longlived(buf: &mut Buffer, min_capacity: usize, hint: usize) -> *mut u8 {
    if buf.capacity >= min_capacity {
        return buf.data;
    }

    let target = round_up_8(crate::memory::buffer::desired_capacity(
        buf.capacity,
        min_capacity,
        hint,
    ));

    let (new_data, granted) = if target <= BLOCK_PAYLOAD {
        with_buffer_arena(|a| a.alloc(target))
    } else {
        let p = unsafe { crate::memory::stdapi::ll_alloc(target, 16) };
        assert!(!p.is_null(), "OS refused a {target}-byte buffer");
        (p, target)
    };

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
    let kind = unsafe { *(((ptr as usize) & !BLOCK_MASK) as *const u32) };
    if kind == BLOCK_KIND_BUFFER {
        with_buffer_arena(|a| unsafe { a.free(ptr, capacity) });
    } else {
        // OS-direct run: the standard path frees it by mask.
        unsafe { crate::memory::stdapi::ll_free(ptr) };
    }
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
mod tests {
    use super::*;
    use crate::memory::buffer::set_pressure_mode;

    #[test]
    fn drop_returns_the_empty_current_block() {
        let _g = crate::memory::block_pool::test_guard();
        let pool = BlockPool::global();
        let before = pool.blocks_out();
        {
            let mut a = BufferArena::new();
            let (p, g) = a.alloc(128); // takes the current block
            assert_eq!(pool.blocks_out(), before + 1);
            unsafe { a.free(p, g) }; // live → 0, but current: `free` keeps it
            assert_eq!(
                pool.blocks_out(),
                before + 1,
                "the current block is not returned by free"
            );
        } // drop
        assert_eq!(
            pool.blocks_out(),
            before,
            "Drop returned the empty current block instead of leaking it"
        );
    }

    #[test]
    fn alloc_grants_at_least_requested_and_min_chunk() {
        let _g = crate::memory::block_pool::test_guard();
        let mut a = BufferArena::new();
        let (p, granted) = a.alloc(1);
        assert!(!p.is_null());
        assert_eq!(
            granted, MIN_CHUNK,
            "tiny chunks round up to the free-slot size"
        );
        unsafe { a.free(p, granted) };
    }

    #[test]
    fn critical_mode_reuses_freed_chunk_plenty_does_not() {
        let _g = crate::memory::block_pool::test_guard();
        let mut a = BufferArena::new();

        let (p, g) = a.alloc(128);
        let (_live, _) = a.alloc(64); // keeps the block non-empty
        unsafe { a.free(p, g) };

        set_pressure_mode(PressureMode::Plenty);
        let (q, _) = a.alloc(128);
        assert_ne!(q, p, "plenty must bump, not consult holes");

        unsafe { a.free(q, 128) };
        set_pressure_mode(PressureMode::Critical);
        let (r, granted) = a.alloc(100);
        assert_eq!(r, q, "critical must pop the fitting hole");
        assert_eq!(granted, 128, "the whole chunk is granted, no split");
        set_pressure_mode(PressureMode::Plenty);
    }

    #[test]
    fn emptied_noncurrent_block_returns_to_pool() {
        let _g = crate::memory::block_pool::test_guard();
        let pool = BlockPool::global();
        let mut a = BufferArena::new();

        // Fill one block completely so the arena rotates past it.
        let payload = BLOCK_PAYLOAD / 4;
        let chunks: Vec<_> = (0..5).map(|_| a.alloc(payload)).collect();
        let first_block = BufferBlockHeader::of_ptr(chunks[0].0);
        assert_ne!(
            BufferBlockHeader::of_ptr(chunks[4].0),
            first_block,
            "fifth chunk must be in a fresh block"
        );

        let regions_before = pool.regions_carved();
        for &(p, g) in &chunks[..4] {
            unsafe { a.free(p, g) };
        }
        // The emptied first block is back in the pool: take it again.
        let reused = pool.get();
        let mut seen = vec![reused];
        let mut found = std::ptr::eq(reused as *mut BufferBlockHeader, first_block);
        for _ in 0..64 {
            if found {
                break;
            }
            let b = pool.get();
            found = std::ptr::eq(b as *mut BufferBlockHeader, first_block);
            seen.push(b);
        }
        assert!(found, "emptied buffer block was not returned to the pool");
        assert_eq!(pool.regions_carved(), regions_before);
        for b in seen {
            pool.put(b);
        }
        unsafe { a.free(chunks[4].0, chunks[4].1) };
    }

    #[test]
    fn longlived_growth_copies_and_recycles_old_payload() {
        let _g = crate::memory::block_pool::test_guard();
        let mut b = Buffer::new();

        buffer_ensure_longlived(&mut b, 64, 0);
        unsafe { std::ptr::copy_nonoverlapping(b"payload".as_ptr(), b.data, 7) };
        b.len = 7;
        let old = b.data;

        set_pressure_mode(PressureMode::Critical);
        let grow_to = b.capacity + 1;
        buffer_ensure_longlived(&mut b, grow_to, 0);
        assert_ne!(b.data, old, "long-lived growth always moves");
        assert_eq!(unsafe { std::slice::from_raw_parts(b.data, 7) }, b"payload");

        // The old chunk is a hole now: a fitting alloc must find it.
        let (p, _) = with_buffer_arena(|a| a.alloc(64));
        assert_eq!(p, old, "old payload must be reusable in critical mode");
        set_pressure_mode(PressureMode::Plenty);

        unsafe { buffer_release_longlived(&mut b) };
        with_buffer_arena(|a| unsafe { a.free(p, 64) });
    }

    #[test]
    fn over_block_payload_goes_os_direct_and_back() {
        let _g = crate::memory::block_pool::test_guard();
        let mut b = Buffer::new();

        buffer_ensure_longlived(&mut b, BLOCK_PAYLOAD * 2, 0);
        assert!(b.capacity >= BLOCK_PAYLOAD * 2);
        unsafe { std::ptr::write_bytes(b.data, 0xCD, b.capacity) };

        // Shrink-to-arena is not a thing; release routes by kind.
        unsafe { buffer_release_longlived(&mut b) };
        assert!(b.data.is_null());
        assert_eq!(b.capacity, 0);
    }

    #[test]
    fn search_is_bounded() {
        let _g = crate::memory::block_pool::test_guard();
        let mut a = BufferArena::new();

        // Build a list of > BOUND small holes, then one big hole beyond
        // the bound; a big request must NOT find it.
        let (anchor, ag) = a.alloc(64); // keeps the block alive
        let big = a.alloc(1024);
        let smalls: Vec<_> = (0..CRITICAL_SEARCH_BOUND + 4)
            .map(|_| a.alloc(16))
            .collect();
        unsafe { a.free(big.0, big.1) }; // deepest in LIFO
        for (p, g) in smalls {
            unsafe { a.free(p, g) };
        }

        set_pressure_mode(PressureMode::Critical);
        let (p, _) = a.alloc(1024);
        assert_ne!(p, big.0, "hit beyond the K-bound must fall back to bump");
        set_pressure_mode(PressureMode::Plenty);

        unsafe {
            a.free(p, 1024);
            a.free(anchor, ag);
        }
    }
}
