//! Request arena: bump allocation over pooled 32 KB blocks.
//!
//! The hot path is `alloc` — four instructions after inlining. The
//! slow path takes a block from the global pool. Nothing is freed
//! per-object: `reset` hands the destructor list to the caller and
//! returns every block to the pool.
//!
//! **The arena is self-contained** (`rfc/model/memory/arenas.md`
//! implementation note): its bookkeeping lives in memory it already
//! owns, not in side `Vec`s. The block list threads through the block
//! headers themselves; the destructor and large-payload logs are
//! segment chains allocated from the arena's own bump. Everything dies
//! together at reset, for free.

use crate::memory::block_pool::{BLOCK_KIND_ARENA, BLOCK_PAYLOAD, BlockHeader, BlockPool};
use crate::refcount::RcHeader;

#[inline]
pub(crate) fn round_up_8(size: usize) -> usize {
    // Saturating: a near-usize::MAX size must stay huge (and fail the
    // bump check) rather than wrap to a small number.
    size.saturating_add(7) & !7
}

/// Records per log segment: 16-byte header + 500 words = 4016 bytes,
/// comfortably within a block payload. Segments chain newest-first;
/// they are never copied (unlike a doubling buffer, a chain has no
/// upper bound from the single-block alloc limit).
const LOG_SEG_RECORDS: usize = 500;

#[repr(C)]
struct LogSegment {
    next: *mut LogSegment,
    count: usize,
    records: [usize; LOG_SEG_RECORDS],
}

/// Which in-arena log a record goes to.
#[derive(Clone, Copy)]
enum Log {
    Destructors,
    Larges,
    RememberedSet,
    ReleaseAtReset,
}

pub struct Arena {
    bump: *mut u8,
    limit: *mut u8,
    /// Newest-first chain of owned blocks, linked through the block
    /// headers' own `next` field.
    blocks: *mut BlockHeader,
    /// Objects awaiting a pre-destructor at reset (in-arena log).
    destructors: *mut LogSegment,
    /// OS-direct payloads owned by this arena — buffers larger than a
    /// block — freed at reset like everything else (in-arena log).
    larges: *mut LogSegment,
    /// Remembered set: slots in longer-lived containers that received a
    /// reference into this arena (`rfc/model/memory/arenas.md`). Fate
    /// of the escapees is decided at reset.
    remembered: *mut LogSegment,
    /// Heap entities referenced from this arena's containers. The log
    /// owns exactly one release per record — the barrier deliberately
    /// does NOT release a displaced value on overwrite
    /// (`rfc/model/memory/arenas.md`, "Why no release on overwrite").
    release_at_reset: *mut LogSegment,
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
            blocks: std::ptr::null_mut(),
            destructors: std::ptr::null_mut(),
            larges: std::ptr::null_mut(),
            remembered: std::ptr::null_mut(),
            release_at_reset: std::ptr::null_mut(),
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
        self.log_push(Log::Larges, p as usize);
        p
    }

    /// Objects with side-effect destructors register here; `reset`
    /// hands them back to the caller (the object-lifecycle layer owns
    /// the actual `__destruct` protocol).
    pub fn track_destructor(&mut self, obj: *mut RcHeader) {
        self.log_push(Log::Destructors, obj as usize);
    }

    /// Barrier hook: a longer-lived container received a reference into
    /// this arena; remember the slot for reset-time promotion.
    pub fn log_escape(&mut self, slot: *mut *mut RcHeader) {
        self.log_push(Log::RememberedSet, slot as usize);
    }

    /// Barrier hook: a heap entity was stored into one of this arena's
    /// containers. The log owns exactly one release per record; the
    /// barrier never releases a displaced value in an arena container.
    pub fn log_release_at_reset(&mut self, entity: *mut RcHeader) {
        self.log_push(Log::ReleaseAtReset, entity as usize);
    }

    /// End of request: run pre-destructors via the callback, then
    /// return every block to the pool. O(blocks + log records), not
    /// O(objects).
    pub fn reset(&mut self, run_destructor: impl FnMut(*mut RcHeader)) {
        self.reset_with(run_destructor, |_| {});
    }

    /// [`reset`] with an escape handler: `handle_escape` receives every
    /// remembered-set slot (a longer-lived location holding a reference
    /// into this arena). Validation and per-block promotion per
    /// `rfc/model/memory/arena-reset.md` belong to the object-lifecycle
    /// layer; until it lands the raw slots are handed to the caller.
    pub fn reset_with(
        &mut self,
        mut run_destructor: impl FnMut(*mut RcHeader),
        mut handle_escape: impl FnMut(*mut *mut RcHeader),
    ) {
        // 1. Destructors. Taken in a loop: a destructor may track new
        //    destructors (allocating into this arena is still legal
        //    here), which start a fresh chain.
        loop {
            let head = self.destructors;
            if head.is_null() {
                break;
            }
            self.destructors = std::ptr::null_mut();
            Self::drain_log(head, |rec| run_destructor(rec as *mut RcHeader));
        }

        // 2. Escaped references — after destructors (they may create
        //    new escapes; the fixpoint discipline of arena-reset.md).
        loop {
            let head = self.remembered;
            if head.is_null() {
                break;
            }
            self.remembered = std::ptr::null_mut();
            Self::drain_log(head, |rec| handle_escape(rec as *mut *mut RcHeader));
        }

        // 3. Deferred releases of heap entities the arena referenced:
        //    exactly one release per log record (the barrier skipped
        //    the overwrite releases). Death here means a heap entity
        //    whose last reference was from this arena.
        let releases = self.release_at_reset;
        self.release_at_reset = std::ptr::null_mut();
        Self::drain_log(releases, |rec| unsafe {
            if crate::refcount::ll_release(rec as *mut RcHeader) {
                // TODO(object-lifecycle): run teardown for the dying
                // entity; until then the memory is not reclaimed.
            }
        });

        // 4. OS-direct payloads (their log lives in blocks that are
        //    still alive at this point).
        let larges = self.larges;
        self.larges = std::ptr::null_mut();
        Self::drain_log(larges, |rec| unsafe {
            crate::memory::stdapi::ll_free(rec as *mut u8)
        });

        // 3. Blocks, the logs' own memory included. Read the chain link
        //    before `put` — the pool reuses the same field.
        let pool = BlockPool::global();
        let mut block = self.blocks;
        self.blocks = std::ptr::null_mut();
        while !block.is_null() {
            let next = unsafe { (*block).next };
            pool.put(block);
            block = next;
        }

        self.bump = std::ptr::null_mut();
        self.limit = std::ptr::null_mut();
    }

    /// Append a record to an in-arena log, growing the segment chain
    /// from the arena's own bump memory.
    fn log_push(&mut self, which: Log, value: usize) {
        let head = match which {
            Log::Destructors => self.destructors,
            Log::Larges => self.larges,
            Log::RememberedSet => self.remembered,
            Log::ReleaseAtReset => self.release_at_reset,
        };

        let head = if head.is_null() || unsafe { (*head).count } == LOG_SEG_RECORDS {
            let seg = self.alloc(size_of::<LogSegment>()) as *mut LogSegment;
            unsafe {
                (*seg).next = head;
                (*seg).count = 0;
            }
            seg
        } else {
            head
        };

        unsafe {
            let c = (*head).count;
            (*head).records.as_mut_ptr().add(c).write(value);
            (*head).count = c + 1;
        }

        match which {
            Log::Destructors => self.destructors = head,
            Log::Larges => self.larges = head,
            Log::RememberedSet => self.remembered = head,
            Log::ReleaseAtReset => self.release_at_reset = head,
        }
    }

    /// Visit every record of a segment chain (newest segment first).
    fn drain_log(head: *mut LogSegment, mut f: impl FnMut(usize)) {
        let mut seg = head;
        while !seg.is_null() {
            unsafe {
                for i in 0..(*seg).count {
                    f((*seg).records.as_ptr().add(i).read());
                }
                seg = (*seg).next;
            }
        }
    }

    fn fresh_block(&mut self) {
        let block = BlockPool::global().get();
        unsafe {
            (*block).kind = BLOCK_KIND_ARENA;
            (*block).next = self.blocks;
        }
        self.blocks = block;
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
    fn destructor_log_survives_segment_growth() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();

        // Three segments' worth of tracked objects.
        let n = LOG_SEG_RECORDS * 2 + 137;
        let objs: Vec<*mut RcHeader> = (0..n)
            .map(|_| {
                let obj = arena.alloc(16) as *mut RcHeader;
                unsafe { obj.write(RcHeader::new(MemoryCategory::RequestArena, 0)) };
                arena.track_destructor(obj);
                obj
            })
            .collect();

        let mut ran = Vec::new();
        arena.reset(|o| ran.push(o));

        assert_eq!(ran.len(), n, "every tracked destructor must be delivered");
        let expected: std::collections::HashSet<_> = objs.iter().map(|p| *p as usize).collect();
        let got: std::collections::HashSet<_> = ran.iter().map(|p| *p as usize).collect();
        assert_eq!(got, expected, "same set of objects, order unspecified");
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
