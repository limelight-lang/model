//! The trace window over physical entity-slot return.
//!
//! A trace's shadow rows are indexed by slot. If an entity dies after its row
//! has been met and the allocator reuses that slot before the trace finishes,
//! the new occupant inherits the dead occupant's row: visited state, working
//! count and verdict all name the address rather than an allocation identity.
//! Observable teardown still happens at refcount zero; only the slot's return
//! to its heap waits here (`rfc/model/gc/rc-cycle.md`, "Zero-count entities
//! pending slot reuse").
//!
//! There are two independent reasons a dead slot may still be named:
//!
//! - a candidate-queue entry, represented by
//!   [`crate::refcount::CANDIDATE_BIT`];
//! - this window, represented by a non-null [`DEFERRED_RETURNS`], while mark
//!   or scan may still use a shadow row for the slot.
//!
//! Every attempted return goes through `memory::stdapi::ll_free`. That entry
//! point first refuses the queue window and then calls
//! [`defer_reuse_if_tracing`] for this one. Closing a trace replays its
//! withheld returns through the same entry point, so an entry still standing
//! keeps the slot withheld without a second record. Conversely, retiring an
//! entry while the trace still runs reaches this list. The two windows can
//! therefore close in either order.
//!
//! # What it owns and for how long
//!
//! **The first [`RETURNS_BASE_RECORDS`] records are a fixed region of the
//! thread's workspace**, laid out behind the worklist's
//! ([`crate::cycle::arena`]), so a window opens on memory the thread already
//! holds and the ordinary collection withholds every return it has without
//! asking the memory manager for anything. The region is all the memory this
//! module ever holds: a death the region has no room for is answered in the
//! dying entity's own memory rather than out of a new block.
//!
//! The chain's control line is the first 64 bytes of that region, and
//! thread-local storage holds one non-owning pointer to it: **null is the
//! closed window**, so no second flag can disagree with the chain's existence
//! (`PLAN.md`, S36.9, "TLS holds only the non-owning pointer that finds the
//! owner state").
//!
//! There is no TLS drop glue: thread-exit order is owned explicitly by
//! `memory::heap::ll_thread_exit`, and a runtime structure first touched by a
//! destructor may not depend on the platform's TLS destructor order
//! (`dev/DECISIONS.md`, "thread exit owns the order its per-thread state dies
//! in").
//!
//! # What a refusal costs, and where it is answered
//!
//! **No path of this module asks an allocation path**, so no path of it can
//! be refused. A window's open stands on the workspace region the
//! [`ActiveTrace`] has already drawn; a death the region can record costs one
//! append; and a death past the region is answered out of memory the dying
//! entity itself occupies. The refusal a draw here would meet — holding a
//! slot whose rows are live, where returning it is the reuse this module
//! prevents and dropping it loses a physical return, which is refused
//! (`dev/DECISIONS.md`, "an enrolment cannot fail") — therefore has no way of
//! arriving.
//!
//! Past the region the state of the dying slot's block decides which of three
//! answers it takes ([`classify_past_the_region`]):
//!
//! - **no row of this collection addresses the block** — the return proceeds
//!   physically and this window owes nothing. What the window prevents is a
//!   new occupant inheriting a row that has been met, and a block this
//!   collection never touched holds no such row;
//! - **a row does, and the block is this thread's to walk** — the slot takes
//!   the mark ([`crate::refcount::DEAD_IN_PLACE`]) and its block goes on the
//!   window's list of marked blocks, which the close walks slot by slot
//!   ([`WithheldReturns::return_marked`]);
//! - **a row does, and the block is another thread's** — the slot takes the
//!   mark and goes on the window's stack of foreign slots, threaded through
//!   the dead slots themselves ([`WithheldReturns::return_foreign`]). Its
//!   block is not walked: the bump cursor bounding such a walk is the owner's
//!   to move, and reading a slot the owner is publishing races that store.
//!
//! The chain keeps every death the region has room for, the per-slot walk a
//! mark costs the close being dearer than an append at every design class
//! (`dev/BENCHMARKS.md`, 2026-09-04, S43.1); that is why the mark answers a
//! full region rather than replacing the chain. A thread exiting with its
//! window still open ends the process, which is the one process end this
//! module holds and has a reason of its own ([`dispose_thread_state`]).
//!
//! The workspace's region enters no byte figure, being memory the thread
//! holds whether or not a collection is running
//! (`crate::cycle::arena::TraceScratchArena::residue`), and past it the module
//! holds no manager memory to enter. So it moves the manager's ledger by
//! nothing (`crate::memory::gc_metadata`).

use std::cell::Cell;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use crate::cycle::records::{RecordChain, SEGMENT_HEADER_BYTES};
use crate::cycle::shadow::{self, Color};
use crate::memory::block_pool::{BLOCK_KIND_ENTITY, BLOCK_KIND_RETAINED, BlockHeader};

/// The chain of withheld returns, resident in the workspace region it
/// describes.
///
/// `Cell` rather than a lock or a `RefCell`: the chain has one writer by
/// construction, the thread whose trace window is open, and the append sits on
/// the free path where a borrow flag buys nothing.
///
/// One 64-byte line, so the base segment's own header starts on the next one
/// and an append writes no line the replay walk reads before it.
#[repr(C, align(64))]
struct DeferredReturnChain {
    /// The records themselves, over the workspace's one region.
    records: RecordChain<*mut u8>,
    /// Newest block of the list of blocks holding a mark, or null while this
    /// window has taken none. Every block on it names the next through one
    /// word of its own header and the last names itself, so a block's
    /// membership is that word alone ([`marked_link`]).
    ///
    /// A `Cell` beside the atomic links, and for the same reason the chain
    /// is one: the head has one writer, the thread whose window is open.
    marked: Cell<*mut u8>,
    /// Newest marked slot of a block this thread does not own, or null while
    /// this window has stacked none. Each names the next through
    /// [`foreign_link`] and the oldest names null, a stack rather than a list
    /// because a slot is pushed once and no word of it has to answer
    /// "stacked?".
    foreign: Cell<*mut u8>,
}

const _: () = assert!(size_of::<DeferredReturnChain>() == 64);
const _: () = assert!(align_of::<DeferredReturnChain>() == 64);

/// Records the workspace's own region for withheld returns holds.
///
/// The capacity the Sage gate fixed, and the record count past which a death
/// is marked, stacked or returned on its block's own state rather than
/// recorded (`PLAN.md`, S36.11, [`classify_past_the_region`]).
pub(crate) const RETURNS_BASE_RECORDS: usize = 1_024;

/// Bytes that region takes out of the workspace: the control line, the base
/// segment's header line, and the records behind it.
pub(crate) const RETURNS_BASE_BYTES: usize = size_of::<DeferredReturnChain>()
    + SEGMENT_HEADER_BYTES
    + RETURNS_BASE_RECORDS * size_of::<*mut u8>();

thread_local! {
    /// The control line of this thread's withheld returns while its trace
    /// window is open, and null otherwise. Non-owning: the region belongs to
    /// the workspace the [`ActiveTrace`]'s arena holds.
    static DEFERRED_RETURNS: Cell<*mut DeferredReturnChain> =
        const { Cell::new(std::ptr::null_mut()) };
}

/// The chain a trace's withheld returns are written into, and the owner that
/// clears what an unwind leaves standing.
///
/// A holder of its own rather than a field the enclosing drop unwinds by hand:
/// an unwind out of the replay would otherwise skip the clearing below, and a
/// mark or a stacked slot that outlives its window is one no window returns.
/// [`crate::cycle::arena::TraceScratchArena`] is re-entrant for the same
/// reason.
struct WithheldReturns {
    /// The control line, in the workspace region this chain was opened over.
    control: *mut DeferredReturnChain,
}

impl WithheldReturns {
    /// Open a chain over `region`, which is the workspace's own region for
    /// withheld returns.
    ///
    /// Infallible: the region is memory the arena already holds, so a window
    /// opens wherever a collection does.
    ///
    /// # Safety
    /// `region` addresses [`RETURNS_BASE_BYTES`] writable bytes, aligned to
    /// 64, and stays the caller's for as long as this chain is used.
    unsafe fn open(region: *mut u8) -> Self {
        let control = region as *mut DeferredReturnChain;
        let records = unsafe { region.add(size_of::<DeferredReturnChain>()) };

        // Field by field and written rather than assigned: the region is
        // memory with no value in it, so an assignment would drop a
        // `DeferredReturnChain` that was never constructed.
        unsafe {
            (&raw mut (*control).records).write(RecordChain::over(records, RETURNS_BASE_RECORDS));
            (&raw mut (*control).marked).write(Cell::new(std::ptr::null_mut()));
            (&raw mut (*control).foreign).write(Cell::new(std::ptr::null_mut()));
        }

        Self { control }
    }

    fn chain(&self) -> &DeferredReturnChain {
        unsafe { &*self.control }
    }

    /// Take this thread's window down.
    ///
    /// Idempotent, and called from two places for one reason: the ordered
    /// close calls it after the row sweep, and [`Drop`] calls it again for the
    /// unwind that never reached the ordered close. A window left standing
    /// over a released block is a free path writing a record through a cursor
    /// into memory the pool has handed out again.
    fn close_window(&self) {
        DEFERRED_RETURNS.with(|control| {
            if control.get() == self.control {
                control.set(std::ptr::null_mut());
            }
        });
    }

    /// Return every withheld slot through `ll_free`, oldest first.
    ///
    /// Called with the window already closed, so a return that reaches
    /// [`defer_reuse_if_tracing`] again is refused there and proceeds
    /// physically.
    fn replay(&self) {
        self.chain().records.walk(|slot| {
            // Safety: each record is one entity slot whose observable
            // teardown completed before `defer_reuse_if_tracing` accepted
            // the return. Replaying it once through `ll_free` is
            // the return it still owes.
            unsafe { crate::memory::stdapi::ll_free(slot) };
        });
    }

    /// Take the newest listed block off the list, or **None** when nothing
    /// is listed.
    ///
    /// The head moves before the block is touched, so the blocks behind the
    /// one being disposed of are still named by the head and the drop's own
    /// pass finds them. A walk that took the head whole would leave them
    /// listed with nothing naming them, and a listed block refuses every
    /// later mark of itself (`dev/POSTMORTEM.md`, "a repair moved ownership
    /// into a `Drop` and left the state that names it behind": a clearing
    /// written twice in two places is a clearing that will be true in one of
    /// them).
    ///
    /// **What that order costs is the block under the walk**: a panic inside
    /// the disposal of one of its slots leaves the marks the walk had not
    /// reached standing, named by nothing, and the drop's pass cannot find
    /// them either. `PLAN.md` S43.6 owns that half.
    fn take_marked_block(&self) -> Option<(*mut u8, u32)> {
        let block = self.chain().marked.get();
        if block.is_null() {
            return None;
        }

        // Safety: a listed block is one this window marked a slot in, and no
        // return has retired it — the replay may have returned other slots
        // of it, but every marked slot is itself a hold, through `used`, the
        // occupant count, or the one entity a large block carries.
        let kind = unsafe { crate::memory::block_pool::load_block_kind(block as *const AtomicU32) };
        let link = unsafe { &*marked_link(block, kind) };
        let next = link.load(Ordering::Acquire);
        link.store(std::ptr::null_mut(), Ordering::Release);

        // The last block names itself, which is what keeps "listed" a test
        // of one word against null.
        self.chain().marked.set(if next == block {
            std::ptr::null_mut()
        } else {
            next
        });

        Some((block, kind))
    }

    /// Return every slot a mark holds, newest block first.
    ///
    /// Called where [`replay`](Self::replay) is called and under the same
    /// closed window, **after** [`return_foreign`](Self::return_foreign). A
    /// record and a mark never name one slot, a death taking one or the
    /// other; a mark and a stacked slot can, and the order is what separates
    /// them. A block foreign when one of its slots was stacked can be this
    /// thread's by the time the next slot of it dies — `Heap::adopt` runs on
    /// the ordinary refill path, inside the window — and that slot lists the
    /// block. The stack walk runs first, so every stacked slot reads free by
    /// the time this walk reaches its block, and the block cannot retire
    /// under the walk either: the mark that listed it is a hold of its own.
    ///
    /// **A block leaves the list before its slots are returned**, because the
    /// return that spends its last hold gives the block to the pool and its
    /// header to the next owner.
    fn return_marked(&self) {
        while let Some((block, kind)) = self.take_marked_block() {
            unsafe { dispose_marks_of(block, kind, Disposition::Return) };
        }
    }

    /// Clear every mark still listed and return none of the memory.
    ///
    /// The unwind's half of [`return_marked`](Self::return_marked): a panic
    /// out of the replay, or out of the walk itself, reaches
    /// [`Drop`](WithheldReturns::drop) with blocks still listed, and both
    /// halves of what they carry have to go. The link, because a listed
    /// block refuses every later mark of itself and no window could ever
    /// return those slots. The mark, because a mark that outlives its window
    /// is a slot an exiting thread hands to an adopting one, which is the
    /// case `crate::refcount::DEAD_IN_PLACE` says cannot arise and the two
    /// guards in `crate::memory::heap` assert.
    ///
    /// **This reaches the blocks still listed and no others.** A panic raised
    /// inside a disposal leaves the block that walk had taken off the list
    /// carrying whatever marks it had not reached, which is
    /// [`take_marked_block`](Self::take_marked_block)'s note and `PLAN.md`
    /// S43.6's.
    ///
    /// What is left is a slot on no free list, below its block's cursor and
    /// counted in the block's occupancy: a leak of the same shape and the
    /// same size as the records the same unwind loses, and `PLAN.md` S43.6's
    /// subject. **The memory is not returned here**, and the reason is the
    /// order this runs in: an unwind out of `TraceScratchArena::reset` gets
    /// here with rows still standing over these blocks, so a slot handed
    /// back now is one a new occupant could take under a live row — the
    /// reuse the whole window exists to prevent.
    fn abandon_marked(&self) {
        while let Some((block, kind)) = self.take_marked_block() {
            unsafe { dispose_marks_of(block, kind, Disposition::Abandon) };
        }
    }

    /// Take the newest stacked slot off the stack, or **None** when nothing
    /// is stacked.
    ///
    /// The head moves before the slot is disposed of, for the reason
    /// [`take_marked_block`](Self::take_marked_block) moves it first: a panic
    /// inside a disposal leaves the slots below it still named by the head,
    /// so the drop's own pass finds them.
    fn take_foreign_slot(&self) -> Option<*mut u8> {
        let slot = self.chain().foreign.get();
        if slot.is_null() {
            return None;
        }

        // Safety: a stacked slot is one this window marked and nothing has
        // returned. The owner never heard of the death, so the slot is on no
        // free list, stands below its block's cursor and is still counted in
        // the block's `used` — which is what keeps the block out of the pool
        // and these bytes readable (`crate::memory::heap::Heap::free`). That
        // holds whether or not this thread has adopted the block since:
        // adoption moves the owner word and no slot's state.
        self.chain()
            .foreign
            .set(unsafe { foreign_link(slot).read() });
        Some(slot)
    }

    /// Return every stacked slot through `ll_free`, newest first.
    ///
    /// Called under the same closed window as
    /// [`return_marked`](Self::return_marked) and before it, which is the
    /// order that keeps one slot out of both walks. The walk itself reads no
    /// word of any block: each return goes through `ll_free`, which posts
    /// onto the block's own stack of cross-thread frees while the block is
    /// still another thread's, and takes the ordinary owner path once this
    /// thread has adopted it.
    fn return_foreign(&self) {
        while let Some(slot) = self.take_foreign_slot() {
            note_slot_visited();
            unsafe { dispose_of(slot, Disposition::Return) };
        }
    }

    /// Clear the mark of every stacked slot and return none of the memory.
    ///
    /// The unwind's half of [`return_foreign`](Self::return_foreign), and
    /// what it leaves is what [`abandon_marked`](Self::abandon_marked)
    /// leaves, in a block of another thread: a slot on no free list, below
    /// its block's cursor and counted in a `used` its owner will never see
    /// decremented. `PLAN.md` S43.6 owns both.
    ///
    /// **It runs before the block walk for symmetry rather than for
    /// soundness.** The close's order is load-bearing because the block walk
    /// returns memory, and a stacked slot returned as an ordinary marked one
    /// puts a free-list link where the stack's own link stood; an abandoning
    /// walk returns nothing, so neither order can move a link the other
    /// reads. A change that gave this path memory back would make the order
    /// load-bearing here too.
    fn abandon_foreign(&self) {
        while let Some(slot) = self.take_foreign_slot() {
            note_slot_visited();
            unsafe { dispose_of(slot, Disposition::Abandon) };
        }
    }
}

/// What the walk does with a mark it finds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// Clear the mark and make the return it deferred, which is the close.
    Return,
    /// Clear the mark and return nothing, which is the unwind
    /// ([`WithheldReturns::abandon_marked`]).
    Abandon,
}

// Slots the close's three walks have read on this thread: the block walk's
// stride, the retained block's survivor list, and the stack of foreign slots.
// What it is for is the claim that an ordinary collection pays nothing for
// the mark — a close that reads no slot is one that took no mark of either
// kind — so the stacked half counts here too, and a reading of one says a
// mark was taken rather than that a block was listed.
#[cfg(test)]
thread_local! {
    static MARKED_SLOTS_VISITED: Cell<usize> = const { Cell::new(0) };
}

/// Count one slot the marked walk read, and nothing at all without
/// `cfg(test)`: the three walks call it either way.
#[inline]
fn note_slot_visited() {
    #[cfg(test)]
    MARKED_SLOTS_VISITED.with(|visited| visited.set(visited.get() + 1));
}

/// What the probe holds for this thread, zeroed by the read.
#[cfg(test)]
pub(crate) fn take_marked_slots_visited() -> usize {
    MARKED_SLOTS_VISITED.with(|visited| visited.replace(0))
}

/// The word that says whether a block is on the marking window's list, in
/// the header the block's kind gives it: the collector line of a heap block,
/// and the first line of a large-entity block, which is a header of its own
/// with no collector line at that offset.
///
/// # Safety
/// `block` is the header of a live block whose kind is `kind`, for as long
/// as the returned pointer is used.
unsafe fn marked_link(block: *mut u8, kind: u32) -> *const AtomicPtr<u8> {
    if crate::memory::large_entity::is_large_entity(kind) {
        return unsafe { crate::memory::large_entity::marked_link(block) };
    }

    unsafe { crate::memory::heap::marked_link(block) }
}

/// Put `block` on this window's list of blocks holding a mark, unless an
/// earlier mark of the same window already did.
///
/// # Safety
/// As [`marked_link`], and `chain` is this thread's open window.
unsafe fn list_marked_block(chain: &DeferredReturnChain, block: *mut u8, kind: u32) {
    let link = unsafe { &*marked_link(block, kind) };
    if !link.load(Ordering::Relaxed).is_null() {
        return;
    }

    let head = chain.marked.get();
    let next = if head.is_null() { block } else { head };
    link.store(next, Ordering::Release);
    chain.marked.set(block);
}

/// The word a stacked slot names the next one through: the eight bytes a free
/// slot links through (`crate::memory::heap::FREE_LIST_LINK_OFFSET`), which
/// hold nothing while the slot is dead and which the return overwrites.
///
/// Plain rather than atomic: the stack has one writer and one reader, the
/// thread whose window is open, and the owner of the block never reads these
/// bytes until it receives the return.
///
/// # Safety
/// `slot` is a dead entity slot of at least the free list's two words.
#[inline]
unsafe fn foreign_link(slot: *mut u8) -> *mut *mut u8 {
    unsafe { slot.add(crate::memory::heap::FREE_LIST_LINK_OFFSET) as *mut *mut u8 }
}

/// Put `slot` on this window's stack of marked slots in blocks this thread
/// does not own.
///
/// # Safety
/// As [`foreign_link`], and `chain` is this thread's open window.
unsafe fn stack_foreign_slot(chain: &DeferredReturnChain, slot: *mut u8) {
    unsafe { foreign_link(slot).write(chain.foreign.get()) };
    chain.foreign.set(slot);
}

/// Dispose of every dead-in-place slot of one listed block: `Return` frees
/// it through `stdapi::ll_free`, which is the funnel the replay uses — the
/// queue window, the reset's arms and the kind dispatch all get their say a
/// second time — and `Abandon` only takes the mark off.
///
/// **A returning walk of a block ends at the return that can retire it**,
/// which is the return of its last hold: past that instant the block may be
/// the pool's and its slots another owner's. Every marked slot is one hold,
/// so a block down to its last one holds no mark this walk has not seen. An
/// abandoning walk retires nothing and needs no such stop.
///
/// # Safety
/// `block` is a block this window listed, its kind is `kind`, and the caller
/// has taken it off the list.
unsafe fn dispose_marks_of(block: *mut u8, kind: u32, disposition: Disposition) {
    if crate::memory::large_entity::is_large_entity(kind) {
        let (entity, _) = unsafe { crate::memory::large_entity::occupant(block) };
        note_slot_visited();
        // Asked here as in the two arms below, though one statement both
        // marks this entity and lists its block: an arm that frees whatever
        // it is handed would free a live entity the day a block is listed
        // for another reason.
        if unsafe { is_marked(entity) } {
            unsafe { dispose_of(entity, disposition) };
        }

        return;
    }

    match kind {
        BLOCK_KIND_ENTITY => {
            let (first, stride, slots) = unsafe { crate::memory::heap::entity_block_slots(block) };
            for index in 0..slots {
                let slot = unsafe { first.add(index * stride) };
                note_slot_visited();
                if !unsafe { is_marked(slot) } {
                    continue;
                }

                // `used` has one writer, the owner, which is this thread: a
                // cross-thread free posts to `remote_free` and moves nothing
                // here (`crate::memory::heap::Heap::free`). So a reading of
                // one taken before the return still holds at the return, and
                // the block that reaches zero by it is the pool's — nothing
                // of it may be read afterwards. The retained arm below,
                // whose count any thread may spend, holds a pin instead.
                let last = disposition == Disposition::Return
                    && unsafe { crate::memory::heap::block_occupancy(block) } == 1;
                unsafe { dispose_of(slot, disposition) };
                if last {
                    return;
                }
            }
        }
        BLOCK_KIND_RETAINED => {
            // Non-null by construction: a listless block has no index space,
            // so no row can address it and no stamp can stand on it
            // (`crate::memory::retained`, `crate::cycle::row`).
            let (list, count) = unsafe { crate::memory::heap::block_survivor_list(block) };
            debug_assert!(
                !list.is_null(),
                "a retained block with no survivor list carried a mark"
            );

            // **A returning walk holds the block itself while it reads it**,
            // and the hold stands from here to the release below, so a panic
            // in the list read above leaks none. A retained block's count is
            // spent by whichever thread frees, so a reading of it taken
            // before a return can be spent by another thread between the two
            // and the block would go to the pool under the walk. The hold
            // answers that: no return inside the walk can empty the block,
            // and the release below decides the emptiness on this thread,
            // after the last read.
            let returning = disposition == Disposition::Return;
            if returning {
                unsafe { crate::memory::retained::pin(block as usize) };
            }

            for index in 0..count {
                let survivor = unsafe { list.add(index).read() } as *mut u8;
                note_slot_visited();
                if unsafe { is_marked(survivor) } {
                    unsafe { dispose_of(survivor, disposition) };
                }
            }

            // The sentinel return of an emptied retained block, which is the
            // form `ll_free` answers off the count word: the hold above was
            // the last thing holding it.
            if returning && unsafe { crate::memory::retained::hold_released(block as usize) } {
                unsafe { crate::memory::stdapi::ll_free(block) };
            }
        }
        // A kind with no arm is a block this walk cannot read the slots of,
        // and `classify_past_the_region` lists none such.
        _ => debug_assert!(false, "a kind with no arm reached the marked walk"),
    }
}

/// Whether the entity at `slot` is one this walk owes a return.
///
/// # Safety
/// `slot` addresses a published entity header, or a slot of an entity block
/// at or below its bump cursor.
unsafe fn is_marked(slot: *mut u8) -> bool {
    let state = unsafe { crate::refcount::slot_state(slot as *const crate::refcount::RcHeader) };
    state == crate::refcount::SlotState::DeadInPlace
}

/// Take the mark off, and make the return it deferred where the disposition
/// is [`Disposition::Return`].
///
/// The clear comes first because the return reads the same word: a slot that
/// reached its free list marked would be handed out as an occupant, and a
/// retained survivor or a large entity would be found by a later window's
/// walk and handed back twice.
///
/// # Safety
/// `slot` is a dead-in-place entity whose block this walk has taken off the
/// list.
unsafe fn dispose_of(slot: *mut u8, disposition: Disposition) {
    unsafe { crate::refcount::clear_dead_in_place(slot as *mut crate::refcount::RcHeader) };
    if disposition == Disposition::Return {
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }
}

impl Drop for WithheldReturns {
    /// Take the window down and leave no mark standing, which is all this
    /// chain owns: its records and its control line stand in the workspace
    /// region the arena hands back, and no path of this module holds memory
    /// of the manager's.
    fn drop(&mut self) {
        self.close_window();
        self.abandon_foreign();
        self.abandon_marked();
    }
}

/// An open in-line trace, the arena whose rows it protects and the returns it
/// withholds.
///
/// The arena is owned rather than borrowed independently so the close order is
/// structural: its sweep nulls every row and releases every scratch block
/// before the window comes down and any entity slot is replayed. Dropping is
/// the abort path too, so a trace that gives up cannot strand the slots whose
/// reuse it delayed.
#[must_use = "dropping the trace window closes the slot-reuse barrier"]
pub(crate) struct ActiveTrace {
    /// The candidate chain this collection detached, until the window closes.
    /// `None` before [`ActiveTrace::detach_candidates`].
    ///
    /// **There is no way to take it out, and that is deliberate.** A
    /// disposition that keeps some roots owes two operations at once — taking
    /// the batch and giving its segments back — and half of that pair is a
    /// batch nothing can end: `restore_candidates` refuses a lane a destructor
    /// has refilled and the drop refuses a batch that still holds a chain.
    /// S36.7 builds the pair with the driver that needs it.
    batch: Option<crate::cycle::queue::InFlightBatch>,
    /// Declared before the arena, and therefore dropped before it: this
    /// chain's control line, its base segment and its first 1,024 records all
    /// stand in a region of the workspace, which the arena's drop hands back
    /// to the thread.
    ///
    /// Defensive rather than load-bearing today, and worth the line for what
    /// it costs: the drop below replays before either field dies, and
    /// `queue::return_workspace_base` leaves the block in the thread's own cell
    /// rather than handing it to the pool, so a reversed order would read
    /// memory nobody else can have yet. It becomes load-bearing the day that
    /// call gives the block back.
    returns: WithheldReturns,
    arena: crate::cycle::arena::TraceScratchArena,
    // A window belongs to the TLS state of the thread that opened it. Moving
    // the guard would close another thread's window and strand this one's.
    _not_send: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl ActiveTrace {
    /// Open this thread's one trace window, or `None` when the memory it
    /// stands on cannot be had: the thread's workspace, on the first
    /// collection of its life.
    ///
    /// `None` is a collection that does not start: no window is open, no return
    /// has been withheld, and the caller's own abort path has nothing to undo.
    /// A thread that has collected once holds its workspace until it exits, so
    /// every window after the first opens without asking the memory manager.
    pub(crate) fn open() -> Option<Self> {
        assert!(
            DEFERRED_RETURNS.with(Cell::get).is_null(),
            "a thread runs at most one trace at a time"
        );

        let arena = crate::cycle::arena::TraceScratchArena::open()?;
        let returns = unsafe { WithheldReturns::open(arena.withheld_returns_region()) };
        DEFERRED_RETURNS.with(|control| control.set(returns.control));

        Some(Self {
            batch: None,
            returns,
            arena,
            _not_send: std::marker::PhantomData,
        })
    }

    /// Take this thread's candidate chain into this trace, once, before the
    /// first mark.
    ///
    /// The draw the window needed is behind it — the detach itself asks for
    /// nothing and cannot be refused (`crate::cycle::queue::detach_candidates`)
    /// — so a collection that reaches this line has all the memory its roots
    /// cost.
    pub(crate) fn detach_candidates(&mut self) {
        assert!(
            self.batch.is_none(),
            "a trace detaches its candidate chain once"
        );
        self.batch = Some(crate::cycle::queue::detach_candidates());
    }

    /// The arena and the detached batch in one answer, because a trace reads
    /// the batch's roots while writing the arena's rows and two calls would
    /// borrow this window twice.
    ///
    /// # Panics
    /// When no batch has been detached, which is a caller that skipped
    /// [`ActiveTrace::detach_candidates`].
    pub(crate) fn rows_and_roots(
        &mut self,
    ) -> (
        &mut crate::cycle::arena::TraceScratchArena,
        &crate::cycle::queue::InFlightBatch,
    ) {
        let batch = self
            .batch
            .as_ref()
            .expect("the trace has no candidate batch");
        (&mut self.arena, batch)
    }

    /// The trace's working memory. No arena reference can outlive the window,
    /// which is what makes the close order above enforceable by the type.
    ///
    /// **The collection does not reset it.** The close does, in an order this
    /// module owns: the reset nulls every row, and only then may a withheld
    /// return be replayed into memory the allocator can hand out again.
    pub(crate) fn arena(&mut self) -> &mut crate::cycle::arena::TraceScratchArena {
        &mut self.arena
    }
}

impl Drop for ActiveTrace {
    fn drop(&mut self) {
        // First, and before the rows die: a batch still here was disposed of by
        // nothing, so every root in it keeps its registration and its records go
        // back to the lane they came out of. It depends on neither the row
        // sweep nor the returns below it, and `ll_free`'s candidate arm reads the entity's bit rather
        // than the lane its record stands in.
        if let Some(batch) = self.batch.take() {
            crate::cycle::queue::restore_candidates(batch);
        }

        // First and unconditionally: after the window falls, a physical
        // return may recommission the block whose shadow pointer this sweep
        // must null. The reset enters its own residue in the high-water
        // figure as it rewinds (`crate::cycle::arena::TraceScratchArena`),
        // and this window has no residue to stand beside it.
        self.arena.reset();

        self.returns.close_window();
        self.returns.replay();
        self.returns.return_foreign();
        self.returns.return_marked();
    }
}

/// Refuse a physical return while the current trace can still address the
/// slot, recording the return for the window's close, and answer whether the
/// return was refused.
///
/// **False is a return the caller must make physically**, which is either a
/// thread with no window open or a death past the region in memory this
/// collection never touched ([`classify_past_the_region`]).
///
/// Called only after the queue-entry window has refused the same return. A
/// replay that still finds `CANDIDATE_BIT` stops before here, because the
/// queue entry itself remains the record.
///
/// With no window open the whole cost is one thread-local load and one branch.
/// With one open and room in the region: three loads — the thread-local
/// control line, the cursor and the limit — two branches and two stores, with
/// no atomic, no allocator call and no pool call. Past the region the block's
/// own state is read, which is the cold tail below.
///
/// # Safety
/// `ptr` is a dead entity slot whose teardown has completed and which this call
/// owns until either the function returns `false` or the window closes.
/// `kind` is the kind `ptr`'s own block reads, and outside the retained
/// sentinel `ptr` addresses an entity rather than the block itself — the mark
/// is a write into the header at `ptr`, and a block base passed under any
/// other kind would land it in the block's own header.
#[inline]
pub(crate) unsafe fn defer_reuse_if_tracing(ptr: *mut u8, kind: u32) -> bool {
    let control = DEFERRED_RETURNS.with(Cell::get);
    if control.is_null() {
        return false;
    }

    let chain = unsafe { &*control };
    if chain.records.push(ptr) {
        return true;
    }

    unsafe { withhold_without_a_record(chain, ptr, kind) }
}

/// How a death past the region is withheld, or that it needs no withholding
/// at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PastTheRegion {
    /// No row of this collection addresses the slot, so the caller returns it
    /// physically and this window owes nothing.
    ReturnNow,
    /// The slot takes the mark and its block goes on the window's list, which
    /// the close walks slot by slot.
    MarkAndListBlock,
    /// The slot takes the mark and goes on the window's stack of foreign
    /// slots, the block belonging to another thread.
    MarkAndStack,
}

/// Which of [`PastTheRegion`]'s three answers a death takes, read off the
/// state of the block it stands in.
///
/// **The stamp decides whether the death has to be withheld at all.** Rows
/// over the memory mean this collection has met the block, so a slot returned
/// there could be handed out again under a row that names it; a block this
/// collection never touched carries no row for any occupant, and its slots
/// are the ones the allocator has been handing out all through the trace
/// anyway. The stamp is not what *finds* a mark — the window's list and stack
/// are — and
/// [`crate::cycle::arena::TraceScratchArena::clear_touched_rows`] reads no
/// header for it.
///
/// **The owner decides how a withheld death is found again**, and only the
/// slotted population is asked:
///
/// - **an entity slot** in a block this thread owns is marked and its block
///   listed, the close walking the block by stride. A slot in a block another
///   thread owns is marked and stacked instead: such a walk is bounded by a
///   bump cursor its owner moves, and a slot read across that store is read
///   through a race ([`crate::memory::heap::entity_block_slots`]);
/// - **a retained survivor** is marked and its block listed, its return being
///   an atomic decrement any thread may perform
///   ([`crate::memory::retained::occupant_freed`]) and its walk bounded by a
///   published survivor list rather than by a cursor. The reset's whole-block
///   sentinel is separated here: it addresses the block header rather than an
///   entity, so there is no header of its own to mark;
/// - **a large entity**, pooled or OS-direct, carries its one row in its own
///   block header, so that row's colour is the stamp
///   ([`crate::memory::large_entity::shadow_row`]) and the block holds the
///   one occupant the close reads. Its header is not a `HeapBlockHeader`, and
///   neither the shadow pointer nor the owner word an entity block carries
///   exists at those offsets to be read.
///
/// Why the retained and large populations are not asked for an owner, and
/// what that leaves open: `dev/DECISIONS.md`, "the stamp is the whole
/// condition where the return is not the owner's".
///
/// # Safety
/// As [`defer_reuse_if_tracing`].
unsafe fn classify_past_the_region(ptr: *mut u8, kind: u32) -> PastTheRegion {
    let block = BlockHeader::of_ptr(ptr) as *mut u8;

    // Asked rather than listed, because the two large kinds grow together or
    // not at all (`crate::memory::large_entity::is_large_entity`).
    if crate::memory::large_entity::is_large_entity(kind) {
        let row = unsafe { *crate::memory::large_entity::shadow_row(block) };
        if shadow::color(row) == Color::Untouched {
            return PastTheRegion::ReturnNow;
        }

        return PastTheRegion::MarkAndListBlock;
    }

    // A kind with no arm returns rather than falls through: the set that
    // reaches here is `stdapi::can_lose_trace_identity`'s, and a kind added
    // there without an arm here would otherwise be marked on the strength of
    // a shadow pointer read at an offset that may be another module's.
    match kind {
        BLOCK_KIND_ENTITY => {
            if unsafe { crate::memory::heap::block_shadow(block) }.is_null() {
                return PastTheRegion::ReturnNow;
            }

            if unsafe { crate::memory::heap::block_is_owned_by_this_thread(block) } {
                PastTheRegion::MarkAndListBlock
            } else {
                PastTheRegion::MarkAndStack
            }
        }
        BLOCK_KIND_RETAINED => {
            // The reset's whole-block sentinel, and it needs no withholding:
            // `promote::retain_block` clears the collector line before it
            // publishes the kind, and between that and the sentinel free the
            // thread is inside `promote::arena_reset_full`, which runs no
            // trace step. A window open around that reset finished its mark
            // before the teardown that drives it, and a window opened inside
            // it by a destructor closed with the destructor's frame — so
            // either way no row of this thread's addresses the block.
            if ptr == block {
                debug_assert!(
                    unsafe { crate::memory::heap::block_shadow(block) }.is_null(),
                    "the reset's whole-block sentinel reached a stamped block"
                );
                return PastTheRegion::ReturnNow;
            }

            if unsafe { crate::memory::heap::block_shadow(block) }.is_null() {
                return PastTheRegion::ReturnNow;
            }

            PastTheRegion::MarkAndListBlock
        }
        _ => {
            debug_assert!(false, "a kind with no arm reached the classifier");
            PastTheRegion::ReturnNow
        }
    }
}

/// Withhold a return the region has no room to record, and answer whether it
/// was withheld at all.
///
/// A death [`classify_past_the_region`] withholds carries the fact in the
/// entity's own header ([`crate::refcount::mark_dead_in_place`]) and goes on
/// this window's list of blocks ([`list_marked_block`]) or its stack of
/// foreign slots ([`stack_foreign_slot`]), both of which the close walks.
/// Nothing is drawn and nothing can refuse, which is what takes every process
/// end off this path (`PLAN.md` S43.2, S43.3, S43.5).
///
/// A marked slot stays out of the allocator's hands the way a recorded one
/// does — it is on no free list and below its block's bump cursor — and a
/// marked survivor keeps its block's occupant count above zero, so the block
/// is not the pool's either. Both hold until the close, and no longer.
///
/// # Safety
/// As [`defer_reuse_if_tracing`], whose refused push this answers.
#[cold]
unsafe fn withhold_without_a_record(chain: &DeferredReturnChain, ptr: *mut u8, kind: u32) -> bool {
    let placement = unsafe { classify_past_the_region(ptr, kind) };
    if placement == PastTheRegion::ReturnNow {
        return false;
    }

    unsafe { crate::refcount::mark_dead_in_place(ptr as *mut crate::refcount::RcHeader) };
    if placement == PastTheRegion::MarkAndStack {
        unsafe { stack_foreign_slot(chain, ptr) };
    } else {
        unsafe { list_marked_block(chain, BlockHeader::of_ptr(ptr) as *mut u8, kind) };
    }

    true
}

/// Refuse a thread exit that would abandon an open trace window.
///
/// A live window at exit would leave a trace using blocks whose owner is being
/// abandoned; that is outside the protocol, and this ends the process rather
/// than letting it happen — `ll_thread_exit` is `extern "C"` and has no caller
/// that could act on a refusal. The chain itself needs no disposal here: it
/// belongs to the [`ActiveTrace`], whose drop is what returns it.
pub(crate) fn dispose_thread_state() {
    assert!(
        DEFERRED_RETURNS.with(Cell::get).is_null(),
        "a thread cannot exit inside its trace window"
    );
}

#[cfg(test)]
pub(crate) fn deferred_slot_count() -> usize {
    let control = DEFERRED_RETURNS.with(Cell::get);
    if control.is_null() {
        return 0;
    }

    unsafe { &*control }.records.used()
}

#[cfg(test)]
mod tests;
