//! Request arena: bump allocation over pooled 32 KB blocks.
//!
//! The hot path is `alloc` — four instructions after inlining. The
//! slow path takes a block from the global pool. Nothing is freed
//! per-object: `reset` hands the destructor list to the caller and
//! returns every block to the pool.

use crate::memory::block_pool::{BLOCK_KIND_ARENA, BLOCK_PAYLOAD, BlockHeader, BlockPool};
use crate::refcount::RcHeader;

#[inline]
pub(crate) fn round_up_8(size: usize) -> usize {
    // Saturating: a near-usize::MAX size must stay huge (and fail the
    // bump check) rather than wrap to a small number.
    size.saturating_add(7) & !7
}

pub struct Arena {
    bump: *mut u8,
    limit: *mut u8,
    blocks: Vec<*mut BlockHeader>,
    destructors: Vec<*mut RcHeader>,
    /// OS-direct payloads owned by this arena (buffers larger than a
    /// block); freed at reset like everything else the arena owns.
    larges: Vec<*mut u8>,
}

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}

impl Arena {
    pub const fn new() -> Self {
        Arena {
            bump: std::ptr::null_mut(),
            limit: std::ptr::null_mut(),
            blocks: Vec::new(),
            destructors: Vec::new(),
            larges: Vec::new(),
        }
    }

    /// The hot path. Sizes are rounded to 8; on a constant size the
    /// rounding folds away at compile time.
    #[inline]
    pub fn alloc(&mut self, size: usize) -> *mut u8 {
        let size = round_up_8(size);
        let p = self.bump;

        // checked_add: `size` is caller-controlled ABI input; an
        // overflowed `next` must reach the slow path's size assert,
        // not wrap past the limit check.
        if !p.is_null() {
            if let Some(next) = (p as usize).checked_add(size) {
                if next <= self.limit as usize {
                    self.bump = next as *mut u8;
                    return p;
                }
            }
        }

        self.alloc_slow(size)
    }

    #[cold]
    fn alloc_slow(&mut self, size: usize) -> *mut u8 {
        assert!(
            size <= BLOCK_PAYLOAD,
            "large objects take the dedicated-run path, not the arena"
        );

        self.fresh_block();

        let p = self.bump;
        self.bump = p.wrapping_add(size);
        p
    }

    /// Compiler batch hook: guarantee `bytes` of headroom so a loop of
    /// allocations runs without limit checks.
    pub fn reserve(&mut self, bytes: usize) {
        assert!(bytes <= BLOCK_PAYLOAD, "reserve larger than a block");
        if self.remaining() < bytes {
            self.fresh_block();
        }
    }

    #[inline]
    pub fn remaining(&self) -> usize {
        if self.bump.is_null() {
            0
        } else {
            self.limit as usize - self.bump as usize
        }
    }

    /// Grow an allocation in place — only possible when it is the last
    /// one (its end is the bump top) and the block has room.
    pub fn try_extend_in_place(&mut self, p: *mut u8, old_size: usize, new_size: usize) -> bool {
        let old_size = round_up_8(old_size);
        let new_size = round_up_8(new_size);

        if p.wrapping_add(old_size) == self.bump {
            // checked_add: same overflow discipline as `alloc`.
            if let Some(new_end) = (p as usize).checked_add(new_size) {
                if new_end <= self.limit as usize {
                    self.bump = new_end as *mut u8;
                    return true;
                }
            }
        }

        false
    }

    /// Allocation too large for a block: OS-direct via the standard
    /// path, owned by the arena — freed at `reset`, not individually.
    pub fn alloc_large(&mut self, size: usize) -> *mut u8 {
        assert!(size > BLOCK_PAYLOAD, "block-sized allocations use alloc");
        let p = unsafe { crate::memory::stdapi::ll_alloc(size, 16) };
        assert!(!p.is_null(), "OS refused a {size}-byte allocation");
        self.larges.push(p);
        p
    }

    /// Objects with side-effect destructors register here; `reset`
    /// hands them back to the caller (the object-lifecycle layer owns
    /// the actual `__destruct` protocol).
    pub fn track_destructor(&mut self, obj: *mut RcHeader) {
        self.destructors.push(obj);
    }

    /// End of request: run pre-destructors via the callback, then
    /// return every block to the pool. O(blocks), not O(objects).
    pub fn reset(&mut self, mut run_destructor: impl FnMut(*mut RcHeader)) {
        for obj in self.destructors.drain(..) {
            run_destructor(obj);
        }

        let pool = BlockPool::global();
        for block in self.blocks.drain(..) {
            pool.put(block);
        }
        for p in self.larges.drain(..) {
            unsafe { crate::memory::stdapi::ll_free(p) };
        }

        self.bump = std::ptr::null_mut();
        self.limit = std::ptr::null_mut();
    }

    fn fresh_block(&mut self) {
        let block = BlockPool::global().get();
        unsafe { (*block).kind = BLOCK_KIND_ARENA };
        self.blocks.push(block);
        self.bump = BlockHeader::payload_start(block);
        self.limit = BlockHeader::end(block);
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        // Destructors are the lifecycle layer's duty; by Drop time the
        // host must have reset. Blocks still go back to the pool.
        self.reset(|_| {});
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::block_pool::{BLOCK_MASK, LINE_SIZE};
    use crate::refcount::{MemoryCategory, RcHeader};

    #[test]
    fn allocations_are_sequential_and_rounded() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();

        let a = arena.alloc(40);
        let b = arena.alloc(1);
        let c = arena.alloc(16);

        assert_eq!(b as usize - a as usize, 40, "40 stays 40");
        assert_eq!(c as usize - b as usize, 8, "1 rounds up to 8");

        // First allocation begins right after the block header.
        assert_eq!(a as usize & BLOCK_MASK, LINE_SIZE);
    }

    #[test]
    fn slow_path_takes_new_block_exactly_at_exhaustion() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();

        let first = arena.alloc(8);
        // 32512 payload / 8 = 4064 slots; one taken, fill the rest.
        for _ in 0..4063 {
            arena.alloc(8);
        }
        assert_eq!(arena.remaining(), 0, "block must be exactly full");

        let next = arena.alloc(8);
        assert_ne!(
            BlockHeader::of_ptr(next),
            BlockHeader::of_ptr(first),
            "must land in a fresh block"
        );
    }

    #[test]
    fn reserve_prevents_mid_loop_refill() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        arena.reserve(100 * 40);

        let block = BlockHeader::of_ptr(arena.alloc(40));
        for _ in 0..99 {
            let p = arena.alloc(40);
            assert_eq!(BlockHeader::of_ptr(p), block, "reserve was violated");
        }
    }

    #[test]
    #[should_panic(expected = "large objects take the dedicated-run path")]
    fn absurd_size_fails_cleanly_instead_of_wrapping() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        arena.alloc(8); // non-null bump: the fast path is reachable
        arena.alloc(usize::MAX - 64); // must hit the slow-path assert
    }

    #[test]
    fn extend_refuses_absurd_size_instead_of_wrapping() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let buf = arena.alloc(64);
        assert!(!arena.try_extend_in_place(buf, 64, usize::MAX - 64));
        assert!(
            arena.try_extend_in_place(buf, 64, 128),
            "sane size still extends"
        );
    }

    #[test]
    fn extend_in_place_only_at_bump_top() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();

        let buf = arena.alloc(64);
        assert!(arena.try_extend_in_place(buf, 64, 128), "top must extend");

        let _other = arena.alloc(8); // someone allocates after us
        assert!(
            !arena.try_extend_in_place(buf, 128, 256),
            "no longer the top - must refuse"
        );
    }

    #[test]
    fn reset_hands_destructors_and_recycles_blocks() {
        let _g = crate::memory::block_pool::test_guard();
        let pool = BlockPool::global();
        let mut arena = Arena::new();

        let obj = arena.alloc(16) as *mut RcHeader;
        unsafe { obj.write(RcHeader::new(MemoryCategory::RequestArena, 0)) };
        arena.track_destructor(obj);
        let old_block = BlockHeader::of_ptr(obj as *mut u8);

        let mut ran = Vec::new();
        arena.reset(|o| ran.push(o));
        assert_eq!(ran, vec![obj], "destructor list must reach the caller");

        let regions_before = pool.regions_carved();
        let mut second = Arena::new();
        let p = second.alloc(8);
        assert_eq!(
            BlockHeader::of_ptr(p),
            old_block,
            "next arena must reuse the recycled block"
        );
        assert_eq!(pool.regions_carved(), regions_before);
    }
}
