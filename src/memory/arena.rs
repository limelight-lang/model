//! Request arena: bump allocation over pooled 64 KB blocks.
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
    Escapees,
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
    /// Escapees: request-arena objects that a longer-lived container
    /// referenced (`rfc/model/memory/arenas.md`, "The dangerous
    /// direction"). Append-only list of the **entities themselves**, not
    /// their holder slots — the live external-reference count lives in each
    /// entity's `refcount` (the [`IS_ESCAPEE`](crate::refcount::IS_ESCAPEE)
    /// hold-count), so reset never dereferences a holder slot and cannot
    /// dangle. Fate of each escapee (promote or drop) is decided at reset
    /// from its count.
    escapees: *mut LogSegment,
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
            escapees: std::ptr::null_mut(),
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

    /// A marker that changes whenever this arena allocates. The bump
    /// pointer already moves on every `alloc` (and jumps on a fresh block),
    /// so it *is* the "did anyone allocate" flag — at zero cost on the hot
    /// path. Arena reset snapshots it around a destructor round to tell a
    /// **dirty** destructor (one that allocated, so it may have stored a new
    /// object into a survivor) from a **pure** one (see
    /// `rfc/model/memory/arena-reset.md` and `crate::promote`).
    #[inline]
    pub(crate) fn bump_cursor(&self) -> usize {
        self.bump as usize
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

    /// Barrier hook: `entity` became an escapee (a longer-lived container
    /// took a reference to this request-arena object for the first time —
    /// its `IS_ESCAPEE` count went 0 → 1). Record it so reset can decide
    /// its fate. Append-only: an escapee whose count later returns to zero
    /// is simply skipped at reset, and a re-escape appends again (harmless,
    /// deduplicated by the reset-time subgraph mark). The count itself is
    /// maintained in the entity's `refcount`, not here.
    pub fn log_escapee(&mut self, entity: *mut RcHeader) {
        self.log_push(Log::Escapees, entity as usize);
    }

    /// Barrier hook: a heap entity was stored into one of this arena's
    /// containers. The log owns exactly one release per record; the
    /// barrier never releases a displaced value in an arena container.
    pub fn log_release_at_reset(&mut self, entity: *mut RcHeader) {
        self.log_push(Log::ReleaseAtReset, entity as usize);
    }

    /// End of request: run pre-destructors via the callback, then
    /// return every block to the pool. O(blocks + log records), not
    /// O(objects). The full promotion discipline (validation, trace,
    /// per-block retention) lives in `crate::promote`; this is the
    /// bare-mechanics variant used when no object model is in play.
    pub fn reset(&mut self, run_destructor: impl FnMut(*mut RcHeader)) {
        self.reset_with(run_destructor, |_| {});
    }

    /// [`reset`] with an escape handler receiving every escapee entity.
    /// Composition of the reset primitives below.
    pub fn reset_with(
        &mut self,
        mut run_destructor: impl FnMut(*mut RcHeader),
        mut handle_escapee: impl FnMut(*mut RcHeader),
    ) {
        // Loops: a destructor may track new destructors or create new
        // escapes (the fixpoint discipline of arena-reset.md).
        loop {
            let mut progress = false;
            self.drain_destructors(|o| {
                progress = true;
                run_destructor(o);
            });
            self.drain_escapees(|e| {
                progress = true;
                handle_escapee(e);
            });
            if !progress {
                break;
            }
        }
        self.drain_release_log(|entity| unsafe {
            if crate::refcount::ll_release(entity) {
                // Bare-mechanics reset has no teardown layer; the
                // promote path dispatches real entity teardown.
            }
        });
        self.finish_reset(|_| false);
    }

    // --- Reset primitives (composed by `crate::promote`) -----------------

    /// One-shot drain of the destructor log: takes the current chain;
    /// entries tracked *during* the drain start a fresh chain for the
    /// caller's next round.
    pub fn drain_destructors(&mut self, mut f: impl FnMut(*mut RcHeader)) {
        let head = self.destructors;
        self.destructors = std::ptr::null_mut();
        Self::drain_log(head, |rec| f(rec as *mut RcHeader));
    }

    /// One-shot drain of the escapee list (same take semantics): yields
    /// each recorded escapee entity.
    pub fn drain_escapees(&mut self, mut f: impl FnMut(*mut RcHeader)) {
        let head = self.escapees;
        self.escapees = std::ptr::null_mut();
        Self::drain_log(head, |rec| f(rec as *mut RcHeader));
    }

    /// One-shot drain of the release-at-reset log: exactly one release
    /// is owed per record (the barrier skipped overwrite releases).
    /// The caller performs the release and owns teardown dispatch.
    pub fn drain_release_log(&mut self, mut f: impl FnMut(*mut RcHeader)) {
        let head = self.release_at_reset;
        self.release_at_reset = std::ptr::null_mut();
        Self::drain_log(head, |rec| f(rec as *mut RcHeader));
    }

    /// Final step: free OS-direct payloads, return blocks to the pool
    /// (except those `keep_block` claims — retained survivor blocks,
    /// whose new kind the caller has already stamped), null the bump.
    /// All logs must be drained first: their memory lives in these
    /// blocks.
    pub fn finish_reset(&mut self, mut keep_block: impl FnMut(*mut BlockHeader) -> bool) {
        debug_assert!(
            self.destructors.is_null()
                && self.escapees.is_null()
                && self.release_at_reset.is_null(),
            "logs must be drained before finish_reset"
        );

        let larges = self.larges;
        self.larges = std::ptr::null_mut();
        Self::drain_log(larges, |rec| unsafe {
            crate::memory::stdapi::ll_free(rec as *mut u8)
        });

        // Read the chain link before `put` — the pool reuses the field.
        let pool = BlockPool::global();
        let mut block = self.blocks;
        self.blocks = std::ptr::null_mut();
        while !block.is_null() {
            let next = unsafe { (*block).next };
            if !keep_block(block) {
                pool.put(block);
            }
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
            Log::Escapees => self.escapees,
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
            Log::Escapees => self.escapees = head,
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

        // Derived from BLOCK_PAYLOAD, not hardcoded: the count is whatever
        // exactly fills one block, and spelling it as a literal silently
        // pinned this test to a 32 KB block. Changing BLOCK_SIZE then failed
        // here with "block must be exactly full", which reads like an arena
        // bug rather than a stale constant.
        let slots = BLOCK_PAYLOAD / 8;
        let first = arena.alloc(8);
        for _ in 0..slots - 1 {
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
