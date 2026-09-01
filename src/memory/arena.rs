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
    Weak,
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
    /// Arena-resident objects that took a weak reference while alive
    /// here. Reset walks the list after the destructor fixpoint, nulling
    /// each dying entry's weak cell before the pages are reused —
    /// otherwise `$weak->get()` would return a pointer into recycled bump
    /// memory (`rfc/model/weak-references.md`, "Death notification").
    /// Append-only; duplicates and promoted survivors are tolerated by
    /// the walk's own tests.
    weak: *mut LogSegment,
    /// Carving cursor for log segments cut from a reserve block, kept
    /// apart from `bump` on purpose: a reserve block must never become
    /// the arena's allocation front, or the next ordinary `alloc` would
    /// spend the memory that exists so the barrier cannot fail
    /// (`crate::memory::reserve`).
    log_bump: *mut u8,
    log_limit: *mut u8,
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
            weak: std::ptr::null_mut(),
            log_bump: std::ptr::null_mut(),
            log_limit: std::ptr::null_mut(),
        }
    }

    /// The hot path. Sizes are rounded to 8; on a constant size the
    /// rounding folds away at compile time.
    ///
    /// **Null for a size past one block payload**, which is a refusal and
    /// not an abort: the arena bump-packs into blocks, so a slot that
    /// large has no home here, and the size arrives from the program
    /// through `ll_arena_alloc`. A caller whose size comes from a
    /// program-visible count wants one of the two doors that split at the
    /// same bound and take the dedicated-run path above it:
    /// [`Arena::alloc_body`] for the bytes an entity owns, and
    /// `Arena::alloc_entity` for an entity slot — named rather than
    /// linked, being crate-private where this is public.
    #[inline]
    pub fn alloc(&mut self, size: usize) -> *mut u8 {
        let size = round_up_8(size);
        // The bound is this allocator's own — `routing::slot_limit`
        // reports it to callers that are not the allocator, and asking it
        // from here would make the arena depend on the module that
        // depends on the arena.
        if size > BLOCK_PAYLOAD {
            return std::ptr::null_mut();
        }

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

    /// Bytes the current block still bump-serves, for a test that has to
    /// leave the arena a known amount of room. The alternative is to
    /// exhaust the bump and hope the remainder lands in the window a
    /// refusal needs, which is a coin flip on the block payload and on
    /// every size the allocation path rounds.
    #[cfg(test)]
    pub(crate) fn room_left(&self) -> usize {
        if self.bump.is_null() {
            return 0;
        }

        self.limit as usize - self.bump as usize
    }

    #[cold]
    fn alloc_slow(&mut self, size: usize) -> *mut u8 {
        // The invariant, stated where the block is taken: `alloc` above
        // refuses this size, so reaching here with it means a new caller
        // bypassed the refusal and is about to bump past a block's end.
        debug_assert!(
            size <= BLOCK_PAYLOAD,
            "large objects take the dedicated-run path, not the arena — a \
             caller whose size comes from the program wants alloc_body"
        );

        if !self.fresh_block() {
            return std::ptr::null_mut();
        }

        let p = self.bump;
        self.bump = p.wrapping_add(size);
        p
    }

    /// Compiler batch hook: guarantee `bytes` of headroom so a loop of
    /// allocations runs without limit checks.
    /// Best-effort: if the pool is empty and the OS refuses, the headroom
    /// is simply not there and the following `alloc` reports the failure.
    /// Reserving is an optimization, so it has nothing of its own to say.
    pub fn reserve(&mut self, bytes: usize) {
        assert!(bytes <= BLOCK_PAYLOAD, "reserve larger than a block");
        if self.remaining() < bytes {
            let _ = self.fresh_block();
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
    /// Null when the OS refuses. Nothing is logged in that case, so reset
    /// has no phantom run to free.
    pub fn alloc_large(&mut self, size: usize) -> *mut u8 {
        assert!(size > BLOCK_PAYLOAD, "block-sized allocations use alloc");
        let p = unsafe { crate::memory::stdapi::ll_alloc(size, 16) };
        if p.is_null() {
            return p;
        }

        assert!(
            self.log_push(Log::Larges, p as usize),
            "out of memory recording an arena large run — the record cannot be \n             dropped without leaking the run at reset"
        );
        p
    }

    /// Allocate an **entity** slot of `size` bytes: bump-packed into a
    /// shared block while it fits, and past one block payload a
    /// block-aligned allocation of its own from `memory::large_entity`,
    /// logged as one of this arena's runs.
    ///
    /// A door of its own rather than a lifted bound on [`Arena::alloc`],
    /// because that one serves `ll_arena_alloc` straight from the C ABI,
    /// where an entity and a byte buffer are the same request; it keeps
    /// refusing. And not [`Arena::alloc_large`], which allocates through
    /// `ll_alloc` and would stamp the raw-buffer kind on memory the
    /// collector reads as an `RcHeader`
    /// (`rfc/model/memory/large-entities.md`).
    ///
    /// Null on refusal. The run is freed by the reset with every other
    /// record in the log, unless promotion takes it out of the log first.
    pub(crate) fn alloc_entity(&mut self, size: usize) -> *mut u8 {
        if size <= BLOCK_PAYLOAD {
            return self.alloc(size);
        }

        let entity = crate::memory::large_entity::alloc(size);
        if entity.is_null() {
            return entity;
        }

        if !self.log_push(Log::Larges, entity as usize) {
            // Where `alloc_large` asserts, this door reports: nothing is
            // published into the slot yet, so handing the run straight back
            // leaves no zero-count member and no leak, and the factory above
            // raises on it the way it raises on any other refusal.
            unsafe { crate::memory::stdapi::ll_free(entity) };
            return std::ptr::null_mut();
        }

        entity
    }

    /// Allocate an entity's out-of-line body of `size` bytes, in-block
    /// while it fits and as a dedicated run above that. Either way the
    /// arena owns it and the reset frees it.
    ///
    /// The split has to be made by somebody: [`Arena::alloc`] asserts on a
    /// size over a block payload and [`Arena::alloc_large`] asserts on one
    /// under it, so a caller that allocates a body of program-visible size
    /// and does not split kills the process on the first body that crosses
    /// the line — in a release build by abort, the profile not unwinding.
    /// This is the request-arena counterpart of
    /// `buffer_arena::buffer_alloc_longlived_payload`.
    pub fn alloc_body(&mut self, size: usize) -> *mut u8 {
        if size <= BLOCK_PAYLOAD {
            self.alloc(size)
        } else {
            self.alloc_large(size)
        }
    }

    /// Give up ownership of an OS-direct run this arena allocated: the
    /// reset will not free it, and the caller becomes responsible for it.
    ///
    /// This exists for one caller — promotion carrying a surviving
    /// entity's out-of-line payload out of the arena
    /// (`rfc/model/strings.md`, "An arena string that survives the reset
    /// takes its payload with it"). For a payload above `BLOCK_PAYLOAD`
    /// the transfer is what the design asks for and is also what removes
    /// the failure mode: nothing is allocated, so nothing can be refused
    /// at a point where there is no caller left to report to.
    ///
    /// The record is zeroed rather than unlinked;
    /// [`drain_log`](Self::drain_log) skips zeros. Returns false when `ptr` was
    /// not one of this arena's runs.
    pub fn forget_large(&mut self, ptr: *mut u8) -> bool {
        let mut seg = self.larges;
        while !seg.is_null() {
            unsafe {
                for i in 0..(*seg).count {
                    let slot = (*seg).records.as_mut_ptr().add(i);
                    if slot.read() == ptr as usize {
                        slot.write(0);
                        return true;
                    }
                }

                seg = (*seg).next;
            }
        }

        false
    }

    /// Objects with side-effect destructors register here; `reset`
    /// hands them back to the caller (the object-lifecycle layer owns
    /// the actual `__destruct` protocol).
    ///
    /// **False when the record could not be written.** Unlike the escape
    /// and release logs, a lost destructor record does not dangle — it
    /// silently skips a `__destruct`, which is a semantic break. The
    /// answer is not to abort but to fail the creation that asked for it:
    /// registration happens once `__construct` has returned, so a refusal
    /// raises memory-exhausted at the creation site, which is exactly the
    /// path a throwing constructor already takes
    /// (`rfc/runtime/object-lifecycle.md`).
    #[must_use]
    pub fn track_destructor(&mut self, obj: *mut RcHeader) -> bool {
        self.log_push(Log::Destructors, obj as usize)
    }

    /// Barrier hook: `entity` became an escapee (a longer-lived container
    /// took a reference to this request-arena object for the first time —
    /// its `IS_ESCAPEE` count went 0 → 1). Record it so reset can decide
    /// its fate. Append-only: an escapee whose count later returns to zero
    /// is simply skipped at reset, and a re-escape appends again (harmless,
    /// deduplicated by the reset-time subgraph mark). The count itself is
    /// maintained in the entity's `refcount`, not here.
    pub fn log_escapee(&mut self, entity: *mut RcHeader) {
        assert!(
            self.log_push(Log::Escapees, entity as usize),
            "out of memory recording an arena escapee — the record cannot be \n             dropped without dangling at reset"
        );
    }

    /// Barrier hook: a heap entity was stored into one of this arena's
    /// containers. The log owns exactly one release per record; the
    /// barrier never releases a displaced value in an arena container.
    pub fn log_release_at_reset(&mut self, entity: *mut RcHeader) {
        assert!(
            self.log_push(Log::ReleaseAtReset, entity as usize),
            "out of memory recording an arena release-at-reset — the record \n             cannot be dropped without leaking the entity"
        );
    }

    /// Weak-machinery hook: `obj` (arena-resident) took its first weak
    /// reference. Reset's weak walk nulls its cell before the pages are
    /// reused. A lost record would dangle — the cell would keep
    /// resolving into recycled memory — so refusal aborts like the
    /// escapee log's.
    pub fn log_weak(&mut self, obj: *mut RcHeader) {
        assert!(
            self.log_push(Log::Weak, obj as usize),
            "out of memory recording an arena weak referent — the record cannot \n             be dropped without a weak cell dangling at reset"
        );
    }

    /// One-shot drain of the weak log (same take semantics): yields each
    /// recorded weakly-referenced arena entity.
    pub fn drain_weak_log(&mut self, mut f: impl FnMut(*mut RcHeader)) {
        let head = self.weak;
        self.weak = std::ptr::null_mut();
        Self::drain_log(head, |rec| f(rec as *mut RcHeader));
    }

    /// End of request: run pre-destructors via the callback, then
    /// return every block to the pool. O(blocks + log records), not
    /// O(objects). The full promotion discipline (validation, trace,
    /// per-block retention) lives in `crate::promote`; this is the
    /// bare-mechanics variant used when no object model is in play.
    pub fn reset(&mut self, run_destructor: impl FnMut(*mut RcHeader)) {
        self.reset_with(run_destructor, |_| {});
    }

    /// [`reset`](Self::reset) with an escape handler receiving every escapee
    /// entity. Composition of the reset primitives below.
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

        // The weak walk — after the destructor fixpoint, before the pages
        // go back. Unlike teardown, this is not optional mechanics: a
        // skipped walk leaves cells resolving into recycled memory. The
        // call into `weak` is the same kind of peer-service call as
        // `ll_release` above.
        unsafe { crate::weak::drain_arena_weak_log(self) };
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
                && self.release_at_reset.is_null()
                && self.weak.is_null(),
            "logs must be drained before finish_reset"
        );

        let larges = self.larges;
        self.larges = std::ptr::null_mut();
        Self::drain_log(larges, |rec| unsafe {
            let ptr = rec as *mut u8;
            // An entity in a block of its own is the one record here that
            // is not a byte payload, and it dies the way every unpromoted
            // arena entity dies: wholesale, with the memory it sat in.
            // What differs is that its memory leaves through the free
            // door, which asks an entity slot for the refcount-0 header a
            // death leaves behind — and an arena entity is uncounted, so
            // it still carries the count its factory wrote. Zeroing it
            // here is that death, stated where it happens.
            let kind = crate::memory::block_pool::load_block_kind(
                &raw const (*BlockHeader::of_ptr(ptr)).kind,
            );
            if crate::memory::large_entity::is_large_entity(kind) {
                (ptr as *mut u64).write(0);
            }

            crate::memory::stdapi::ll_free(ptr)
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
        // The carving cursor pointed into a block that just went home.
        self.log_bump = std::ptr::null_mut();
        self.log_limit = std::ptr::null_mut();
    }

    /// Append a record to an in-arena log, growing the segment chain
    /// from the arena's own bump memory.
    ///
    /// ## Why the growth branch is a `#[cold]` call
    ///
    /// Two of the four callers are the store barrier's arena-log hooks
    /// (`log_escapee`, `log_release_at_reset`), which sit inside
    /// `ref_store`. With the growth branch inline, each of them dragged
    /// `Arena::alloc` -> `BlockPool::get` and, as the code stood then,
    /// an out-of-memory panic into the barrier: 308 IR lines and two
    /// `alloca [48 x i8]` for a
    /// branch taken once per `LOG_SEG_RECORDS` records. Generated code
    /// calls the barrier on every unresolved reference store, so its
    /// size is the thing that decides whether the store inlines at all.
    #[inline]
    #[must_use]
    fn log_push(&mut self, which: Log, value: usize) -> bool {
        let head = match which {
            Log::Destructors => self.destructors,
            Log::Larges => self.larges,
            Log::Escapees => self.escapees,
            Log::ReleaseAtReset => self.release_at_reset,
            Log::Weak => self.weak,
        };

        let head = if head.is_null() || unsafe { (*head).count } == LOG_SEG_RECORDS {
            let grown = self.grow_log(head);
            if grown.is_null() {
                return false;
            }

            grown
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
            Log::Weak => self.weak = head,
        }

        true
    }

    /// The full segment: carve a fresh one from the arena's own bump
    /// memory and link it in front of `head`. Runs once per
    /// `LOG_SEG_RECORDS` pushes — see [`log_push`](Self::log_push) for
    /// why it is out of line.
    #[cold]
    #[inline(never)]
    fn grow_log(&mut self, head: *mut LogSegment) -> *mut LogSegment {
        let mut seg = self.alloc(size_of::<LogSegment>()) as *mut LogSegment;
        if seg.is_null() {
            // Ordinary memory is gone, which is exactly what the thread's
            // reserve is held back for (`crate::memory::reserve`).
            seg = self.carve_log_from_reserve() as *mut LogSegment;
        }

        // Past the reserve there is nothing left to try. The refusal is
        // reported rather than resolved here, because what a lost record
        // means differs per log: for escapees, release-at-reset and large
        // runs it is a broken invariant and the caller aborts; a lost
        // destructor record only skips a side effect, and that one fails
        // the object's creation instead.
        if seg.is_null() {
            return std::ptr::null_mut();
        }

        unsafe {
            (*seg).next = head;
            (*seg).count = 0;
        }

        seg
    }

    /// Cut one log segment out of the thread's reserve.
    ///
    /// The reserve block is linked into this arena's block list, so reset
    /// hands it back to the pool like any other, but it is deliberately
    /// **not** installed as `bump`/`limit`: ordinary allocation must keep
    /// reporting null while the reserve lasts, because that null is what
    /// lets a Limelight frame raise memory-exhausted. Instead the block
    /// is carved by `log_bump`/`log_limit`, which only this path uses —
    /// so one block yields ~15 segments rather than one.
    #[cold]
    #[inline(never)]
    fn carve_log_from_reserve(&mut self) -> *mut u8 {
        let size = round_up_8(size_of::<LogSegment>());

        if self.log_bump.is_null() || self.log_bump.wrapping_add(size) > self.log_limit {
            let block = crate::memory::reserve::draw();
            if block.is_null() {
                return std::ptr::null_mut();
            }

            unsafe {
                (*block).next = self.blocks;
                crate::memory::block_pool::store_block_kind(
                    &raw const (*block).kind,
                    BLOCK_KIND_ARENA,
                );
            }

            self.blocks = block;
            self.log_bump = BlockHeader::payload_start(block);
            self.log_limit = BlockHeader::end(block);
        }

        let p = self.log_bump;
        self.log_bump = p.wrapping_add(size);
        p
    }

    /// Visit every record of a segment chain (newest segment first).
    /// Zero records are skipped: [`forget_large`](Self::forget_large)
    /// zeroes a run it hands to someone else, and no log's payload can
    /// legitimately be a null pointer.
    fn drain_log(head: *mut LogSegment, mut f: impl FnMut(usize)) {
        let mut seg = head;
        while !seg.is_null() {
            unsafe {
                for i in 0..(*seg).count {
                    let record = (*seg).records.as_ptr().add(i).read();
                    if record != 0 {
                        f(record);
                    }
                }

                seg = (*seg).next;
            }
        }
    }

    /// False when the pool has nothing and the OS refused more. The
    /// arena's state is left untouched in that case, so a caller that
    /// reports the failure leaves a usable arena behind.
    #[must_use]
    fn fresh_block(&mut self) -> bool {
        let block = BlockPool::global().get();
        if block.is_null() {
            return false;
        }

        // The kind last and through `store_block_kind`, because the
        // collector acquire-loads that word for every block in every
        // carved region and this one is freshly out of the pool.
        unsafe {
            (*block).next = self.blocks;
            crate::memory::block_pool::store_block_kind(&raw const (*block).kind, BLOCK_KIND_ARENA);
        }

        self.blocks = block;
        self.bump = BlockHeader::payload_start(block);
        self.limit = BlockHeader::end(block);
        true
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
mod tests;
