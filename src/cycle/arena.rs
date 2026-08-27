//! The collection's working memory: a bump arena over 64 KiB blocks,
//! taken for one collection and returned whole at its end.
//!
//! **Two doors, in this order: the ordinary block pool, then the
//! thread's critical reserve** (`rfc/model/memory/critical-reserve.md`,
//! "The three customers"). The in-line collection is the standard form
//! rather than the emergency one, so most runs begin with no refusal
//! anywhere and a full trace's rows are far beyond any reserve; the
//! critical door is the fallback, and on the pressure path of Y14 it is
//! the first draw, the pool's refusal being what triggered the
//! collection.
//!
//! **A refusal at both doors aborts the collection, and never the
//! process.** That is why the memory is asked for a block at a time
//! through a call that can answer null, rather than reserved as a
//! mapping materialised page by page: a page that fails to materialise
//! reports nothing a caller can catch, and the release profile is built
//! `panic = "abort"` (`rfc/model/gc/rc-cycle.md`, "Where the shadow
//! count lives"; the ruling of 2026-08-27 in `dev/DECISIONS.md`).
//!
//! # What the arena owes back
//!
//! Every block, at the end of the collection and on the abort path
//! alike, and what the critical door lent goes back to the reserve
//! before the pool sees a block — the retry that follows an abort wants
//! a door that is open.
//!
//! **The shadow-row pointers are nulled earlier than that, and the
//! instant is fixed by the design rather than by convenience.**
//! [`ShadowArena::sweep_touched`] runs at the end of scan, where the
//! trace token is released and where the last touch of any shadow row
//! has already happened. Everything after that store runs untokened, and
//! the slot returns are among it — so a block may reach the pool and be
//! recommissioned while this collection's teardown is still running, and
//! a sweep left until then would write into another collection's header
//! word (`rfc/model/gc/rc-cycle.md`, "Concurrency" and "Death while
//! enrolled"). [`ShadowArena::reset`] sweeps too, and that is the abort
//! path: an abort can only be raised where memory is asked for, which is
//! inside mark and scan, so an aborting collection has not reached the
//! release instant.
//!
//! # What it does not hold
//!
//! A `Vec`, a `HashMap`, or anything else that reaches the global
//! allocator. Both of the arena's own lists live in its own memory: the
//! blocks thread through their headers, and the touched list is a
//! segment chain bumped out of them. A collection that grew a `Vec`
//! would allocate through the very door that has already refused, and an
//! allocation failure inside `Vec` aborts the process
//! (`rfc/model/gc/cycle/questions.md`, Y14, "Its working memory must be
//! sized before it is needed").

use crate::memory::block_pool::{BLOCK_KIND_ARENA, BLOCK_PAYLOAD, BlockHeader, BlockPool};

/// Block addresses one segment of the touched list holds. 512 makes a
/// segment 4 KiB, so a segment costs a sixteenth of a block and a
/// collection that touches few blocks pays for one.
const TOUCHED_PER_SEGMENT: usize = 512;

/// One run of the touched list, bumped out of the arena's own memory.
///
/// The list is walked once, at reset, and never searched, so a chain of
/// filled runs is the whole of what it needs to be.
#[repr(C)]
struct TouchedSegment {
    next: *mut TouchedSegment,
    len: usize,
    /// Block header addresses. Live only below `len`: the rest is
    /// whatever the bump handed over, and is written before it is read.
    blocks: [*mut u8; TOUCHED_PER_SEGMENT],
}

/// One collection's memory. Built on the collecting thread's stack,
/// spent by that collection, and returned by [`ShadowArena::reset`].
pub(crate) struct ShadowArena {
    /// Blocks held, threaded through `BlockHeader::next`, newest first.
    blocks: *mut BlockHeader,
    /// How many of them came through the critical door, and therefore
    /// how many go back to the reserve rather than to the pool. The
    /// arena does not record *which*: a block is a block, and the count
    /// is what restores the reserve's size.
    from_reserve: usize,
    /// The bump cursor into the newest block, and the bytes left in it.
    cursor: *mut u8,
    left: usize,
    /// Newest segment of the touched list, or null while nothing is
    /// stamped.
    touched: *mut TouchedSegment,
}

impl ShadowArena {
    /// An arena holding nothing. Allocates no memory: a collection that
    /// finds no candidate pays for no block.
    pub(crate) fn new() -> Self {
        Self {
            blocks: std::ptr::null_mut(),
            from_reserve: 0,
            cursor: std::ptr::null_mut(),
            left: 0,
            touched: std::ptr::null_mut(),
        }
    }

    /// `bytes` of 8-aligned scratch, or **null when both doors have
    /// refused**, which is the caller's signal to abort the collection.
    ///
    /// A request larger than one block payload is refused outright: no
    /// allocation this arena serves comes near it — a smallest-class
    /// block's rows are 16 320 bytes and a touched segment 4 KiB — and a
    /// run of blocks would give the abort path a second shape to return.
    ///
    /// The memory is **not zeroed**. A row array arrives dirty by
    /// design, its met bitmap being what says which rows have meaning
    /// (`rfc/model/gc/rc-cycle.md`, "The rows are not zeroed greedily").
    pub(crate) fn alloc(&mut self, bytes: usize) -> *mut u8 {
        // Rounded through a checked add, because the refusal below is
        // what the caller reads and an overflow would reach it as a
        // small number: with overflow checks it panics on the one path
        // that must abort a collection instead, and without them it
        // wraps to zero and grants a pointer the caller believes is
        // enormous.
        let Some(rounded) = bytes.checked_add(7) else {
            return std::ptr::null_mut();
        };

        let bytes = rounded & !7;
        if bytes > BLOCK_PAYLOAD {
            return std::ptr::null_mut();
        }

        if bytes > self.left && !self.grow() {
            return std::ptr::null_mut();
        }

        let granted = self.cursor;
        self.cursor = unsafe { granted.add(bytes) };
        self.left -= bytes;
        granted
    }

    /// Enrol `block` for the sweep, **before** its shadow-row pointer is
    /// stamped. False when the list cannot grow, which aborts the
    /// collection like any other refusal.
    ///
    /// **The order is the contract, and the other one has a hole.** This
    /// call can fail; the stamp cannot. Stamp first and a refusal here
    /// leaves a block pointing at rows the abort is about to give back to
    /// the pool, which is the stale pointer the list exists to prevent —
    /// and it fails exactly when memory is short, which is when the abort
    /// runs. Enrolling first costs nothing: the sweep of a block that was
    /// never stamped stores null over null.
    ///
    /// A block enrolled twice is swept twice, which is the same store
    /// again. One never enrolled is the defect.
    ///
    /// # Safety
    /// `block` must be the header of a live `BLOCK_KIND_ENTITY` block,
    /// and must stay mapped until [`sweep_touched`](Self::sweep_touched)
    /// runs — which it does, a trace in flight being what keeps a block
    /// from reaching the pool (`rfc/model/gc/rc-cycle.md`, "Death while
    /// enrolled"). It may go home at any time after that.
    pub(crate) unsafe fn note_touched(&mut self, block: *mut u8) -> bool {
        let full = self.touched.is_null() || unsafe { (*self.touched).len } == TOUCHED_PER_SEGMENT;
        if full {
            let segment = self.alloc(size_of::<TouchedSegment>()) as *mut TouchedSegment;
            if segment.is_null() {
                return false;
            }

            // Field by field and written rather than assigned: the bump
            // hands over memory with no value in it, so a `*segment = ..`
            // that dropped the old contents would read what was never
            // written.
            unsafe {
                (&raw mut (*segment).next).write(self.touched);
                (&raw mut (*segment).len).write(0);
            }
            self.touched = segment;
        }

        unsafe {
            let segment = self.touched;
            let filled = (*segment).len;
            (&raw mut (*segment).blocks[filled]).write(block);
            (&raw mut (*segment).len).write(filled + 1);
        }
        true
    }

    /// End the collection's hold on memory: give every block back, the
    /// reserve first, having swept anything
    /// [`sweep_touched`](Self::sweep_touched) has not.
    ///
    /// This is the whole of the abort path. A collection that gave up
    /// halfway calls it and has left nothing behind — the trace writes
    /// into no entity, so the heap is byte-identical and the only state
    /// to undo is the pointers the sweep nulls.
    ///
    /// Idempotent, and [`Drop`] calls it, so a collection that unwinds
    /// under a profile that unwinds leaks no block. The release profile
    /// aborts instead, so on that build every exit of a collection owes
    /// this call explicitly.
    pub(crate) fn reset(&mut self) {
        self.sweep_touched();

        // What the reserve lent goes back to the reserve, and the rest
        // to the pool. Returning everything through the reserve's door
        // would refill it out of ordinary memory a collection happened
        // to be holding, which is the safepoint's job and not this one;
        // returning everything to the pool would leave the reserve empty
        // for the retry that follows an abort, and the retry is why the
        // ordering exists at all.
        // The arena's own state moves ahead of each hand-over rather
        // than after the loop: `BlockPool::put` takes a mutex, and a
        // thread that unwinds out of a poisoned one leaves `Drop` to run
        // `reset` again — over a list whose head was already returned.
        self.cursor = std::ptr::null_mut();
        self.left = 0;
        while !self.blocks.is_null() {
            let block = self.blocks;
            self.blocks = unsafe { (*block).next };
            if self.from_reserve > 0 {
                self.from_reserve -= 1;
                crate::memory::critical::give_back(block);
            } else {
                BlockPool::global().put(block);
            }
        }

        self.from_reserve = 0;
    }

    /// Null the shadow-row pointer of every block this collection
    /// enrolled, and empty the list.
    ///
    /// **Called at the end of scan**, where the trace token is released:
    /// that is the last instant at which the blocks are guaranteed still
    /// to be this collection's, because the slot returns that follow the
    /// release can hand one to the pool and another collection can
    /// recommission it (module doc). [`reset`](Self::reset) calls it
    /// again, which is the abort path and a second call over an emptied
    /// list.
    ///
    /// The rows themselves need no undoing: mark and scan write into no
    /// entity, so the pointer is the whole of what a collection leaves in
    /// the heap.
    pub(crate) fn sweep_touched(&mut self) {
        let mut segment = self.touched;
        // Emptied first: the walk below runs to the end of the chain,
        // and a second call must find nothing rather than repeat it.
        self.touched = std::ptr::null_mut();

        while !segment.is_null() {
            let filled = unsafe { (*segment).len };
            for i in 0..filled {
                let block = unsafe { (*segment).blocks[i] };
                unsafe { crate::memory::heap::clear_block_shadow(block) };
            }

            segment = unsafe { (*segment).next };
        }
    }

    /// Take one more block, or answer false when both doors refuse.
    ///
    /// What is left of the previous block is abandoned. A bump that
    /// searched its older blocks for a fit would be a free list, and the
    /// arena's whole life is one collection.
    fn grow(&mut self) -> bool {
        let mut block = BlockPool::global().get();
        if block.is_null() {
            block = crate::memory::critical::draw();
            if block.is_null() {
                return false;
            }

            self.from_reserve += 1;
        } else {
            // Through `store_block_kind` for the reason every other
            // commissioning uses it: the collector acquire-loads the kind
            // of every block of every carved region, so a plain store to
            // that word is a data race. A reserve block already carries
            // this kind.
            unsafe {
                crate::memory::block_pool::store_block_kind(
                    &raw const (*block).kind,
                    BLOCK_KIND_ARENA,
                )
            };
        }

        unsafe { (&raw mut (*block).next).write(self.blocks) };
        self.blocks = block;
        self.cursor = BlockHeader::payload_start(block);
        self.left = BLOCK_PAYLOAD;
        true
    }

    /// Blocks this arena holds. Tests only: the number is what a leak
    /// looks like from outside.
    #[cfg(test)]
    pub(crate) fn blocks_held(&self) -> usize {
        let mut count = 0;
        let mut block = self.blocks;
        while !block.is_null() {
            count += 1;
            block = unsafe { (*block).next };
        }
        count
    }
}

impl Drop for ShadowArena {
    /// The net under an unwind. On the contract path the collection has
    /// already called [`reset`](ShadowArena::reset) and this finds
    /// nothing; a test that panics mid-collection would otherwise leave
    /// the pool short for every test after it.
    fn drop(&mut self) {
        self.reset();
    }
}

#[cfg(test)]
mod tests;
