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
//! [`defer_reuse_if_tracing`] for this one. Closing a trace gives its withheld
//! returns back through the same entry point, so an entry still standing keeps
//! the slot withheld without a second withholding. Conversely, retiring an
//! entry while the trace still runs reaches this stack. The two windows can
//! therefore close in either order.
//!
//! # What it owns and for how long
//!
//! **A withheld return is held in the dying entity itself.** Each withheld
//! slot is pushed onto one stack threaded through its byte 8, the word a free
//! slot links by (`crate::memory::heap::FREE_LIST_LINK_OFFSET`), which carries
//! nothing any reader of a dead slot reads and which the return of a slotted
//! death overwrites with its free-list link — so the pop takes the next
//! address off a slot before it hands that slot over. The module therefore
//! holds no memory of its own for any number of deaths, and a collection
//! withholds every return it has without asking the memory manager for
//! anything (`dev/DECISIONS.md`, "one stack through the dead entity holds
//! every withheld return").
//!
//! The stack's head stands in a control line, the first 64 bytes of the fixed
//! region at the head of the thread's workspace, ahead of the collection's own
//! bump ([`crate::cycle::arena`]), and thread-local storage holds one non-owning
//! pointer to that line: **null is the closed window**, so no second flag can
//! disagree with the window's existence (`PLAN.md`, S36.9, "TLS holds only the
//! non-owning pointer that finds the owner state"). The rest of the region —
//! the record chain and the [`RETURNS_BASE_RECORDS`] records behind it — takes
//! no death: the open writes the chain's segment header and nothing writes a
//! record, and the whole of it goes with `PLAN.md` S44.2, as do the list of
//! marked blocks and the walks that read it.
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
//! [`ActiveTrace`] has already drawn, and every death it withholds is answered
//! out of memory the dying entity itself occupies. The refusal a draw here
//! would meet — holding a slot whose rows are live, where returning it is the
//! reuse this module prevents and dropping it loses a physical return, which
//! is refused (`dev/DECISIONS.md`, "an enrolment cannot fail") — therefore has
//! no way of arriving.
//!
//! **The state of the dying slot's block decides whether the death is withheld
//! at all**, and that is the whole of the decision ([`classify`]):
//!
//! - **no row of this collection addresses the block** — the return proceeds
//!   physically and this window owes nothing. What the window prevents is a
//!   new occupant inheriting a row that has been met, and a block this
//!   collection never touched holds no such row;
//! - **a row does** — the slot takes the mark
//!   ([`crate::refcount::DEAD_IN_PLACE`]) and goes on the window's stack,
//!   which the close pops one slot at a time
//!   ([`WithheldReturns::dispose_withheld`]).
//!
//! **The close walks no block's slots**, whoever owns the block: the bump
//! cursor that would bound such a walk is the owner's to move, and reading a
//! slot the owner is publishing races that store. What the window itself reads
//! of a block is the one word the stamp stands in; what the return then reads
//! is `ll_free`'s own, which both designs pay alike. What the stack
//! costs the close moves with the deaths withheld rather than with the blocks
//! the collection touched, which is the reading S43.1 took against the walk
//! (`dev/BENCHMARKS.md`, "S43.1 the sweep's walk against the withheld chain").
//! A thread exiting with its window still open ends the process, which is the
//! one process end this module holds and has a reason of its own
//! ([`dispose_thread_state`]).
//!
//! The workspace's region enters no byte figure, being memory the thread
//! holds whether or not a collection is running
//! (`crate::cycle::arena::TraceScratchArena::residue`), and the module holds no
//! manager memory besides. So it moves the manager's ledger by nothing
//! (`crate::memory::gc_metadata`).

use std::cell::Cell;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use crate::cycle::records::{RecordChain, SEGMENT_HEADER_BYTES};
use crate::cycle::shadow::{self, Color};
use crate::memory::block_pool::{BLOCK_KIND_ENTITY, BLOCK_KIND_RETAINED, BlockHeader};

/// The head of the withheld returns and the words the close reads beside it,
/// resident in the workspace region they describe.
///
/// `Cell` rather than a lock or a `RefCell`: the head has one writer by
/// construction, the thread whose trace window is open, and the push sits on
/// the free path where a borrow flag buys nothing.
///
/// One 64-byte line, so the base segment's own header starts on the next one
/// and a push writes no line another reader is on.
#[repr(C, align(64))]
struct DeferredReturnChain {
    /// The region's record chain. Written by nothing, every withheld return
    /// standing on the stack below, and deleted with the region it lies in
    /// (`PLAN.md`, S44.2).
    records: RecordChain<*mut u8>,
    /// Newest block of the list of blocks holding a mark, and null for the
    /// life of every window: nothing lists a block. The list, the walks that
    /// read it and this word go together (`PLAN.md`, S44.2).
    marked: Cell<*mut u8>,
    /// Newest withheld slot, or null while this window has withheld none.
    /// Each names the next through [`withheld_link`] and the oldest names
    /// null, a stack rather than a list because a slot is pushed once and no
    /// word of it has to answer "stacked?".
    withheld: Cell<*mut u8>,
    /// The block a walk can be resumed inside, and null for the life of every
    /// window, no block being listed for a walk to enter (`PLAN.md`, S44.2).
    ///
    /// A walk takes its block off the list before it touches a slot, so
    /// through that stretch this word is the only thing naming the block, and
    /// a panic there would otherwise leave the marks the walk had not reached
    /// named by nothing ([`WithheldReturns::dispose_walking`]).
    ///
    /// **Null is not "no walk in progress".** An arm names its block only
    /// after it has read what it needs of it, so the head of every walk runs
    /// with this word null; and the large-entity arm never names its block at
    /// all, having one disposal with nothing behind it for a resumed walk to
    /// reach. What non-null means is exactly that a resumed walk would be
    /// sound and would have something to do.
    walking: Cell<*mut u8>,
    /// Whether this collection's rows are gone, which is what decides between
    /// returning the marks and abandoning them.
    ///
    /// False until [`ActiveTrace`]'s drop has swept, so an unwind before the
    /// sweep abandons: a slot handed back with a row still naming it is the
    /// reuse this window exists to prevent, and the fact is read once at the
    /// close rather than inferred from where the unwind came from.
    swept: Cell<bool>,
}

const _: () = assert!(size_of::<DeferredReturnChain>() == 64);
const _: () = assert!(align_of::<DeferredReturnChain>() == 64);

/// Records the workspace's own region for withheld returns holds.
///
/// The capacity the Sage gate fixed, over a chain nothing writes into
/// (`PLAN.md`, S36.11, S44.2).
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

/// The stack a trace's withheld returns are pushed onto, and the owner that
/// clears what an unwind leaves standing.
///
/// A holder of its own rather than a field the enclosing drop unwinds by hand:
/// an unwind out of the close would otherwise skip the clearing below, and a
/// mark or a stacked slot that outlives its window is one no window returns.
/// [`crate::cycle::arena::TraceScratchArena`] is re-entrant for the same
/// reason.
struct WithheldReturns {
    /// The control line, in the workspace region this chain was opened over.
    control: *mut DeferredReturnChain,
}

impl WithheldReturns {
    /// Open a window's stack over `region`, which is the workspace's own
    /// region for withheld returns.
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
            (&raw mut (*control).withheld).write(Cell::new(std::ptr::null_mut()));
            (&raw mut (*control).walking).write(Cell::new(std::ptr::null_mut()));
            (&raw mut (*control).swept).write(Cell::new(false));
        }

        Self { control }
    }

    fn chain(&self) -> &DeferredReturnChain {
        unsafe { &*self.control }
    }

    /// Record that this collection's rows are gone, which is what lets an
    /// unwind return the marks it finds rather than abandon them.
    ///
    /// Called by [`ActiveTrace`]'s drop the instant
    /// [`crate::cycle::arena::TraceScratchArena::sweep_rows`] returns, and by
    /// nothing else: every path that returns memory stands on this word.
    fn rows_are_gone(&self) {
        self.chain().swept.set(true);
    }

    /// Take this thread's window down.
    ///
    /// Idempotent, and called from two places for one reason: the ordered
    /// close calls it after the row sweep, and [`Drop`] calls it again for the
    /// unwind that never reached the ordered close. A window left standing
    /// over a released block is a free path pushing a slot onto a head that
    /// stands in memory the pool has handed out again.
    fn close_window(&self) {
        DEFERRED_RETURNS.with(|control| {
            if control.get() == self.control {
                control.set(std::ptr::null_mut());
            }
        });
    }

    /// Return every recorded slot through `ll_free`, oldest first. The chain
    /// it walks is empty for the life of every window, no death being
    /// recorded, and it goes with the chain (`PLAN.md`, S44.2).
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
    /// **The block under the walk is named by
    /// [`DeferredReturnChain::walking`]** for as much of the disposal as a
    /// re-walk can repeat, which is what keeps a panic inside a disposal from
    /// stranding the marks that walk had not reached
    /// ([`dispose_walking`](Self::dispose_walking)). That word is
    /// [`dispose_marks_of`]'s to set, not this function's: an arm reads what
    /// it needs of the block before it names it.
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

    /// Finish the walk a panic left inside a block, if a walk was inside one.
    ///
    /// The block is named by [`DeferredReturnChain::walking`] and by nothing
    /// else, its walk having taken it off the list first. Its slots that were
    /// already disposed of read free rather than marked, so a second walk
    /// makes exactly the returns the first did not reach; the slot whose own
    /// return raised is the one this cannot recover, its mark having come off
    /// before the return ([`dispose_of`]).
    ///
    /// **The block has not retired**, which is what makes reading its header
    /// here sound: a walk nulls this word before any return that can spend the
    /// block's last hold, so a block still named by it holds at least the slot
    /// whose disposal raised.
    ///
    /// **And the walk it repeats reaches no assertion the first walk failed**,
    /// which is what keeps this out of a second panic inside a drop that is
    /// already unwinding — a panic there aborts. An arm names its block only
    /// after it has read what it needs of it, so a walk that raised before its
    /// first disposal leaves this word null and its block unnamed.
    fn dispose_walking(&self, disposition: Disposition) {
        let block = self.chain().walking.get();
        if block.is_null() {
            return;
        }

        // Safety: as above — the block stands, so its header still reads the
        // kind the interrupted walk read from it.
        let kind = unsafe { crate::memory::block_pool::load_block_kind(block as *const AtomicU32) };
        unsafe { dispose_marks_of(self.chain(), block, kind, disposition) };

        // No assertion over the word afterwards, unlike the two loops that
        // call this. A non-null `walking` means an unwind by construction, so
        // every execution of one here would be inside a drop that is already
        // unwinding, where a firing assertion aborts instead of reporting.
    }

    /// Dispose of every mark this window listed a block for, newest block
    /// first: `Return` makes the return each mark deferred, `Abandon` takes
    /// the marks off and gives no memory back.
    ///
    /// Called where [`replay`](Self::replay) is called, under the same closed
    /// window, and after [`dispose_withheld`](Self::dispose_withheld). The
    /// order separated the one slot both structures could name, and no window
    /// lists a block any longer, so this walk finds nothing and the order it
    /// stands in is not load-bearing (`PLAN.md`, S44.2).
    ///
    /// **A block leaves the list before its slots are returned**, because the
    /// return that spends its last hold gives the block to the pool and its
    /// header to the next owner.
    ///
    /// **`Abandon` is what an unwind before the sweep takes**, and both halves
    /// of what a listed block carries have to go there. The link, because a
    /// listed block refuses every later mark of itself and no window could
    /// return those slots. The mark, because a mark that outlives its window
    /// is a slot an exiting thread hands to an adopting one, which is the case
    /// `crate::refcount::DEAD_IN_PLACE` says cannot arise and the two guards
    /// in `crate::memory::heap` assert. **The memory stays out of
    /// circulation** on that path: [`DeferredReturnChain::swept`] read false,
    /// so this collection's rows still stand over these blocks and a slot
    /// handed back would be one a new occupant could take under a row that
    /// names it.
    ///
    /// **The leading [`dispose_walking`](Self::dispose_walking) reaches
    /// nothing under `Abandon`**, and stands there for symmetry: a walk is
    /// named only under the returning disposition, and an abandoning disposal
    /// cannot raise, being a clear of one flag bit.
    fn dispose_marked(&self, disposition: Disposition) {
        self.dispose_walking(disposition);
        while let Some((block, kind)) = self.take_marked_block() {
            unsafe { dispose_marks_of(self.chain(), block, kind, disposition) };
            debug_assert!(
                self.chain().walking.get().is_null(),
                "a walk that ran to its end left its block named"
            );
        }
    }

    /// Take the newest withheld slot off the stack, or **None** when nothing
    /// is stacked.
    ///
    /// **The head moves before the slot is disposed of**, which is what makes
    /// a panic inside one return cost that one slot: the slots below it are
    /// still named by the head, so the drop's own pass finds them. A pop that
    /// moved the head after the return would leave the whole stack named by a
    /// slot the free list has taken back.
    fn pop_withheld(&self) -> Option<*mut u8> {
        let slot = self.chain().withheld.get();
        if slot.is_null() {
            return None;
        }

        // Safety: a withheld slot is one this window marked and nothing has
        // returned, and what keeps its memory readable differs by population.
        // A size-class slot reached no free list and stands below its block's
        // bump cursor, still counted in the block's `used`
        // (`crate::memory::heap::Heap::free`); a retained survivor is still a
        // live occupant of its block, which therefore cannot go home
        // (`crate::memory::retained::occupant_freed`); a large entity is the
        // one occupant its block or mapping waits for. None of the three moves
        // when a block changes hands: adoption writes the owner word and no
        // slot's state.
        self.chain()
            .withheld
            .set(unsafe { withheld_link(slot).read() });
        Some(slot)
    }

    /// Dispose of every slot this window withheld, newest first: `Return`
    /// makes the return through `ll_free`, `Abandon` takes the mark off and
    /// gives no memory back.
    ///
    /// Called with the window closed, so a return that reaches
    /// [`defer_reuse_if_tracing`] again is refused there. The pop itself reads
    /// no word of any block, only the dead slot's own link; what reads the
    /// block is the return behind it, `ll_free` posting onto the block's stack
    /// of cross-thread frees while the block is another thread's and taking
    /// the ordinary owner path where this thread owns it.
    ///
    /// **The link is read before the return overwrites it**, the free list
    /// linking through the same word ([`withheld_link`]); the pop takes the
    /// next address off the slot and only then hands the slot over.
    ///
    /// What `Abandon` leaves is the slot exactly where the withholding found
    /// it: out of circulation, and holding its block or mapping with it, which
    /// is the price of a window that lost its collection's rows before it
    /// could give anything back.
    fn dispose_withheld(&self, disposition: Disposition) {
        while let Some(slot) = self.pop_withheld() {
            note_slot_visited();
            unsafe { dispose_of(slot, disposition) };
        }
    }
}

/// What the walk does with a mark it finds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// Clear the mark and make the return it deferred, which is the close.
    Return,
    /// Clear the mark and return nothing, which is the unwind that reached
    /// the chain before the rows were swept ([`WithheldReturns::drop`]).
    Abandon,
}

// Slots the close has read on this thread, which under the stack is one per
// withheld return. What it is for is the size of the close: a reading tells a
// collection that withheld nothing from one that withheld and gave back, where
// the free lists afterwards read alike.
#[cfg(test)]
thread_local! {
    static MARKED_SLOTS_VISITED: Cell<usize> = const { Cell::new(0) };
}

/// Count one slot the close read, and nothing at all without `cfg(test)`: the
/// pop calls it either way.
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
#[expect(
    dead_code,
    reason = "nothing lists a block since the stack took every withheld return; \
              the list and the walk that reads it go with `PLAN.md` S44.2"
)]
unsafe fn list_marked_block(chain: &DeferredReturnChain, block: *mut u8, kind: u32) {
    let link = unsafe { &*marked_link(block, kind) };

    // Acquire against the release store a walk's unlisting makes: the window
    // that listed this block before may have run on another thread, and this
    // load is what has to see the null it left
    // (`crate::memory::heap::BlockCollector::marked_next`).
    if !link.load(Ordering::Acquire).is_null() {
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
/// thread whose window is open, and no other thread reads these bytes until
/// it receives the return.
///
/// # Safety
/// `slot` is a dead entity slot of at least the free list's two words.
#[inline]
unsafe fn withheld_link(slot: *mut u8) -> *mut *mut u8 {
    unsafe { slot.add(crate::memory::heap::FREE_LIST_LINK_OFFSET) as *mut *mut u8 }
}

/// Put `slot` on this window's stack of withheld returns.
///
/// # Safety
/// As [`withheld_link`], and `chain` is this thread's open window.
unsafe fn push_withheld(chain: &DeferredReturnChain, slot: *mut u8) {
    unsafe { withheld_link(slot).write(chain.withheld.get()) };
    chain.withheld.set(slot);
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
/// **An arm names its block in [`DeferredReturnChain::walking`] after it has
/// read what it needs of the block and before its first disposal, and unnames
/// it where a re-walk stops being possible** — the instant before any return
/// that can retire the block, and the end of the walk otherwise. While the
/// word names the block, a panic inside a disposal leaves the marks the walk
/// has not reached to [`WithheldReturns::dispose_walking`].
///
/// **The order of those two halves is load-bearing and cannot be simplified.**
/// The reads at the head of an arm carry assertions of their own — the size
/// class `crate::memory::heap::entity_block_slot_bounds` indexes by, the survivor
/// list the retained arm asserts on — and a resumed walk repeats everything
/// the raising walk ran before its first disposal. Named earlier, a block
/// whose head-read raised would send the drop's own pass through that same
/// failure, inside an unwind, where a second panic aborts. What a walk that
/// raises there loses instead is every mark of that one block, which
/// [`WithheldReturns::drop`] states.
///
/// **Between a naming and its first disposal nothing may raise either**, for
/// the same reason, and what runs there is `note_slot_visited`,
/// [`is_marked`] through `crate::refcount::slot_state`,
/// `crate::memory::heap::block_occupancy`, `crate::memory::retained::pin` and
/// one read out of the survivor list. Each of the three named calls carries a
/// note back to this one.
///
/// # Safety
/// `block` is a block this window listed, its kind is `kind`, the caller has
/// taken it off the list, and `chain` is the window that listed it.
unsafe fn dispose_marks_of(
    chain: &DeferredReturnChain,
    block: *mut u8,
    kind: u32,
    disposition: Disposition,
) {
    if crate::memory::large_entity::is_large_entity(kind) {
        let (entity, _) = unsafe { crate::memory::large_entity::occupant(block) };
        note_slot_visited();
        // Asked here as in the two arms below, though one statement both
        // marks this entity and lists its block: an arm that frees whatever
        // it is handed would free a live entity the day a block is listed
        // for another reason.
        let marked = unsafe { is_marked(entity) };

        // The arm never names the block: it has one disposal, so there is
        // nothing behind that disposal for a resumed walk to reach, and the
        // return unmaps the block anyway.
        if marked {
            unsafe { dispose_of(entity, disposition) };
        }

        return;
    }

    match kind {
        BLOCK_KIND_ENTITY => {
            let (first, stride, bump) =
                unsafe { crate::memory::heap::entity_block_slot_bounds(block) };

            // Named after the block has been read and not before: a walk that
            // raised in the reading above would send the drop's own pass
            // through the same reading, inside an unwind, where a second
            // panic aborts ([`WithheldReturns::dispose_walking`]).
            chain.walking.set(block);
            for index in 0..bump {
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
                if last {
                    chain.walking.set(std::ptr::null_mut());
                }

                unsafe { dispose_of(slot, disposition) };
                if last {
                    return;
                }
            }

            chain.walking.set(std::ptr::null_mut());
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

            // As the entity arm above, and for the same reason: the reading
            // and the assertion over it stand ahead of the naming.
            chain.walking.set(block);

            // **A returning walk holds the block itself while it reads it**,
            // and the hold stands from here to the guard's death, so a panic
            // in the list read above leaks none. A retained block's count is
            // spent by whichever thread frees, so a reading of it taken
            // before a return can be spent by another thread between the two
            // and the block would go to the pool under the walk. The hold
            // answers that: no return inside the walk can empty the block,
            // and the guard decides the emptiness on this thread, after the
            // last read.
            let hold = (disposition == Disposition::Return)
                .then(|| unsafe { RetainedWalkHold::take(chain, block) });

            for index in 0..count {
                let survivor = unsafe { list.add(index).read() } as *mut u8;
                note_slot_visited();
                if unsafe { is_marked(survivor) } {
                    unsafe { dispose_of(survivor, disposition) };
                }
            }

            // Before the hold dies, whose release can retire the block.
            chain.walking.set(std::ptr::null_mut());
            drop(hold);
        }
        // A kind with no arm is a block this walk cannot read the slots of,
        // and `classify` lists none such. It names no block
        // either, having no slot to dispose of.
        _ => debug_assert!(false, "a kind with no arm reached the marked walk"),
    }
}

/// The hold a returning walk keeps on a retained block, and the emptiness
/// decision that spends it.
///
/// A guard rather than a pair of statements around the walk: a panic between
/// them would otherwise leave the hold standing, and a retained block nothing
/// releases never reaches the pool.
struct RetainedWalkHold<'a> {
    chain: &'a DeferredReturnChain,
    block: *mut u8,
}

impl<'a> RetainedWalkHold<'a> {
    /// Hold `block` for as long as this guard lives.
    ///
    /// # Safety
    /// `block` is the retained block the walk is inside, held by something
    /// else — an occupant or a list — until this returns.
    unsafe fn take(chain: &'a DeferredReturnChain, block: *mut u8) -> Self {
        unsafe { crate::memory::retained::pin(block as usize) };
        Self { chain, block }
    }
}

impl Drop for RetainedWalkHold<'_> {
    /// Spend the hold, and give the block back when the hold was the last
    /// thing holding it — the sentinel return `ll_free` answers off the count
    /// word.
    ///
    /// **On an unwind out of the walk the block is not empty here**: the
    /// survivor whose return raised is still an occupant of it, its
    /// `occupant_freed` never having been made, and so is every survivor the
    /// walk had not reached. So the block stands and
    /// [`DeferredReturnChain::walking`] still names it for the pass that
    /// finishes the walk.
    fn drop(&mut self) {
        if !unsafe { crate::memory::retained::hold_released(self.block as usize) } {
            return;
        }

        // Nulled ahead of the return that retires the block: what a walk
        // would resume into is memory the pool has taken back.
        self.chain.walking.set(std::ptr::null_mut());
        unsafe { crate::memory::stdapi::ll_free(self.block) };
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
/// `slot` is a dead-in-place entity this close has taken off the window's
/// stack, or one of a block it has taken off the window's list.
unsafe fn dispose_of(slot: *mut u8, disposition: Disposition) {
    unsafe { crate::refcount::clear_dead_in_place(slot as *mut crate::refcount::RcHeader) };
    if disposition == Disposition::Return {
        fire_injected_disposal_failure();
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }
}

// Returning disposals of this thread left before the injected failure raises,
// or zero while none is armed.
//
// Fault injection, tests only, and the disposal a walk raises inside has no
// other way of being staged: every other panic case here arms `ll_free`'s
// refusal of a live free by raising a count, and `is_marked` reads that same
// count first, so a slot armed that way is skipped rather than raised on.
#[cfg(test)]
thread_local! {
    static PANIC_AT_DISPOSAL: Cell<usize> = const { Cell::new(0) };
}

/// Arm the injection to raise on the `nth` returning disposal of this thread,
/// counting from one.
///
/// Disarmed by the firing and by this guard's death, so a case that raises
/// before its disposal leaves nothing standing for the thread's next window.
/// The count names a disposal rather than a slot: what the case asserts is
/// which slot the walk skipped, read off that slot's own state.
#[cfg(test)]
pub(crate) struct InjectedDisposalFailure;

#[cfg(test)]
impl InjectedDisposalFailure {
    pub(crate) fn arm(nth: usize) -> Self {
        assert!(nth > 0, "the count names a disposal, from one");
        PANIC_AT_DISPOSAL.with(|left| left.set(nth));
        Self
    }
}

#[cfg(test)]
impl Drop for InjectedDisposalFailure {
    fn drop(&mut self) {
        PANIC_AT_DISPOSAL.with(|left| left.set(0));
    }
}

/// Raise the armed failure at its disposal, and do nothing at all without
/// `cfg(test)`.
///
/// It stands between the clear and the return because that is what the real
/// refusal leaves behind: `ll_free`'s assertion is at its head, so a panic
/// there finds the mark already off and the slot unreturned. Raised before the
/// clear, it would leave a mark the resumed walk returns, and the case would
/// pass on a path no real panic takes.
#[inline]
fn fire_injected_disposal_failure() {
    #[cfg(test)]
    PANIC_AT_DISPOSAL.with(|left| {
        let remaining = left.get();
        if remaining == 0 {
            return;
        }

        if remaining > 1 {
            left.set(remaining - 1);
            return;
        }

        left.set(0);
        panic!("the injected disposal failure");
    });
}

impl Drop for WithheldReturns {
    /// Take the window down and leave no mark standing, which is all this
    /// window owns: its control line stands in the workspace region the arena
    /// hands back, its withheld returns stand in the dead entities themselves,
    /// and no path of this module holds memory of the manager's.
    ///
    /// **Which of the two dispositions it takes is
    /// [`DeferredReturnChain::swept`]'s to say.** The ordered close sets that
    /// word the instant the rows are gone, so:
    ///
    /// - **a panic raised after the sweep** — inside
    ///   [`crate::cycle::arena::TraceScratchArena::reset`]'s hand-back of its
    ///   own blocks, or inside one of the close's own returns — is survived
    ///   for every slot still on the stack: those are returned, the rows that
    ///   would have made the returns a reuse being gone;
    /// - **a panic raised before the sweep** is not survived at all: the marks
    ///   come off and the memory stays out of circulation, a slot handed back
    ///   under a live row being the reuse this window exists to prevent.
    ///
    /// **No panic site of the crate stands in that second case**, [`ActiveTrace`]'s
    /// drop sweeping ahead of everything that can raise (`dev/DECISIONS.md`,
    /// "the row sweep runs ahead of the candidate restore"). The word is read
    /// rather than the order inferred, so a close reordered later cannot make
    /// this drop return under a row that still names the slot.
    ///
    /// **What no panic recovers is the one return it interrupted.** A raising
    /// `ll_free` leaves its own slot with the mark off and the return unmade,
    /// on no free list, below its block's cursor and counted in its block's
    /// occupancy. Nothing else of that pass is lost: the pop takes the head
    /// off the stack before it hands the slot over, so the slots behind the
    /// raising one are still named by the head and this pass makes their
    /// returns ([`pop_withheld`](WithheldReturns::pop_withheld)).
    ///
    /// A panic raised by these returns themselves is a panic during an unwind
    /// and ends the process, which is why the walks make no return the
    /// ordered close would not have made and reach no assertion it failed.
    fn drop(&mut self) {
        self.close_window();
        let disposition = if self.chain().swept.get() {
            Disposition::Return
        } else {
            Disposition::Abandon
        };

        self.dispose_withheld(disposition);
        self.dispose_marked(disposition);
    }
}

/// An open in-line trace, the arena whose rows it protects and the returns it
/// withholds.
///
/// The arena is owned rather than borrowed independently so the close order is
/// structural: its sweep nulls every row before the window comes down and any
/// entity slot goes back, and it gives its own scratch blocks back after
/// those returns are made. Dropping is the abort path too, so a trace that
/// gives up cannot strand the slots whose reuse it delayed.
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
    /// Declared before the arena, and therefore dropped before it: the stack's
    /// control line stands in a region of the workspace, which the arena's drop
    /// hands back to the thread.
    ///
    /// Defensive rather than load-bearing today, and worth the line for what
    /// it costs: the drop below pops before either field dies, and
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
    /// module owns: the sweep nulls every row, and only then may a withheld
    /// return hand memory back to the allocator. The
    /// arena's own blocks go back after those returns
    /// (`crate::cycle::arena::TraceScratchArena::sweep_rows`).
    pub(crate) fn arena(&mut self) -> &mut crate::cycle::arena::TraceScratchArena {
        &mut self.arena
    }
}

impl Drop for ActiveTrace {
    fn drop(&mut self) {
        // First of all, and taken whether or not anything was withheld: after
        // the window falls, a physical return may recommission the block whose
        // shadow pointer this sweep must null. Ahead of the restore below, so
        // that a refusal raised there unwinds into a drop whose rows are gone
        // and whose withheld returns can therefore be made rather than
        // abandoned (`dev/DECISIONS.md`, "the row sweep runs ahead of the
        // candidate restore").
        self.arena.sweep_rows();
        self.returns.rows_are_gone();

        // A batch still here was disposed of by nothing, so every root in it
        // keeps its registration and its records go back to the lane they came
        // out of. It reads no row and no withheld slot, and `ll_free`'s
        // candidate arm reads the entity's own bit rather than the lane its
        // record stands in, so nothing above or below turns on where this
        // stands between them. It stays ahead of the returns, whose `ll_free`
        // is the one call on this path that could refill the lane its
        // assertion wants empty.
        if let Some(batch) = self.batch.take() {
            crate::cycle::queue::restore_candidates(batch);
        }

        self.returns.close_window();
        self.returns.replay();
        self.returns.dispose_withheld(Disposition::Return);
        self.returns.dispose_marked(Disposition::Return);

        // The arena's own blocks name no slot, so they go back after the
        // returns rather than before them — which is what leaves every return
        // made when a panic in the hand-back sends this frame into the drops
        // below (`WithheldReturns::drop`). The reset enters its own residue in
        // the high-water figure as it rewinds
        // (`crate::cycle::arena::TraceScratchArena`), and this window has no
        // residue to stand beside it.
        self.arena.reset();
    }
}

/// Refuse a physical return while the current trace can still address the
/// slot, withholding the return for the window's close, and answer whether the
/// return was refused.
///
/// **False is a return the caller must make physically**, which is either a
/// thread with no window open or a death in memory this collection never
/// touched ([`classify`]).
///
/// Called only after the queue-entry window has refused the same return. A
/// close that still finds `CANDIDATE_BIT` stops before here, because the
/// queue entry itself keeps the slot withheld.
///
/// With no window open the whole cost is one thread-local load and one branch.
/// With one open, the block's own state is read — one load for a slotted or a
/// retained death, one for a large entity's row — and a withheld death then
/// costs the mark, one write into the dying entity's own byte 8 and one store
/// of the head, with no atomic, no allocator call and no pool call.
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
    unsafe { withhold(chain, ptr, kind) }
}

/// How a death is withheld, or that it needs no withholding at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Withholding {
    /// No row of this collection addresses the slot, so the caller returns it
    /// physically and this window owes nothing.
    ReturnNow,
    /// The slot takes the mark and goes on the window's stack, threaded
    /// through the dead entity itself.
    Stack,
}

/// Which of [`Withholding`]'s two answers a death takes, read off the state of
/// the block it stands in.
///
/// **The stamp is the whole of the decision.** Rows over the memory mean this
/// collection has met the block, so a slot returned there could be handed out
/// again under a row that names it; a block this collection never touched
/// carries no row for any occupant, and its slots are the ones the allocator
/// has been handing out all through the trace anyway. The stamp is not what
/// *finds* a withheld slot — the window's stack is — and
/// [`crate::cycle::arena::TraceScratchArena::clear_touched_rows`] reads no
/// header for it.
///
/// **Where the stamp stands is what the arms differ over:**
///
/// - **an entity slot** and **a retained survivor** stand in a block whose
///   collector line carries the shadow pointer
///   ([`crate::memory::heap::block_shadow`]). The reset's whole-block
///   sentinel is separated inside the retained arm: it addresses the block
///   header rather than an entity, so there is no header of its own to mark;
/// - **a large entity**, pooled or OS-direct, carries its one row in its own
///   block header, so that row's colour is the stamp
///   ([`crate::memory::large_entity::shadow_row`]). Its header is not a
///   `HeapBlockHeader`, and the shadow pointer an entity block carries does
///   not stand at that offset to be read.
///
/// **No arm asks who owns the block.** A withheld slot is found again through
/// the dead entity itself, no word of its block being read on either side of
/// the window, so ownership decides nothing here. Why the owner is no part of
/// the condition: `dev/DECISIONS.md`, "the stamp is the whole condition where
/// the return is not the owner's" and "one stack through the dead entity
/// holds every withheld return".
///
/// # Safety
/// As [`defer_reuse_if_tracing`].
unsafe fn classify(ptr: *mut u8, kind: u32) -> Withholding {
    let block = BlockHeader::of_ptr(ptr) as *mut u8;

    // Asked rather than listed, because the two large kinds grow together or
    // not at all (`crate::memory::large_entity::is_large_entity`).
    if crate::memory::large_entity::is_large_entity(kind) {
        let row = unsafe { *crate::memory::large_entity::shadow_row(block) };
        if shadow::color(row) == Color::Untouched {
            return Withholding::ReturnNow;
        }

        return Withholding::Stack;
    }

    // A kind with no arm returns rather than falls through: the set that
    // reaches here is `stdapi::can_lose_trace_identity`'s, and a kind added
    // there without an arm here would otherwise be marked on the strength of
    // a shadow pointer read at an offset that may be another module's.
    match kind {
        BLOCK_KIND_ENTITY => {
            if unsafe { crate::memory::heap::block_shadow(block) }.is_null() {
                return Withholding::ReturnNow;
            }

            Withholding::Stack
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
                return Withholding::ReturnNow;
            }

            if unsafe { crate::memory::heap::block_shadow(block) }.is_null() {
                return Withholding::ReturnNow;
            }

            Withholding::Stack
        }
        _ => {
            debug_assert!(false, "a kind with no arm reached the classifier");
            Withholding::ReturnNow
        }
    }
}

/// Withhold a return, and answer whether it was withheld at all.
///
/// A death [`classify`] withholds carries the fact in the entity's own header
/// ([`crate::refcount::mark_dead_in_place`]) and goes on this window's stack
/// ([`push_withheld`]), which the close pops. Nothing is drawn and nothing can
/// refuse, which is what leaves this path with no process end on it at all.
///
/// A marked slot stays out of the allocator's hands the way a recorded one
/// does — it is on no free list and below its block's bump cursor — and a
/// marked survivor keeps its block's occupant count above zero, so the block
/// is not the pool's either. Both hold until the close, and no longer.
///
/// # Safety
/// As [`defer_reuse_if_tracing`], whose refused push this answers.
unsafe fn withhold(chain: &DeferredReturnChain, ptr: *mut u8, kind: u32) -> bool {
    if unsafe { classify(ptr, kind) } == Withholding::ReturnNow {
        return false;
    }

    unsafe { crate::refcount::mark_dead_in_place(ptr as *mut crate::refcount::RcHeader) };
    unsafe { push_withheld(chain, ptr) };
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

/// How many returns this thread's open window is holding, by walking the
/// stack. Zero with no window open.
#[cfg(test)]
pub(crate) fn deferred_slot_count() -> usize {
    let control = DEFERRED_RETURNS.with(Cell::get);
    if control.is_null() {
        return 0;
    }

    let mut count = 0;
    let mut slot = unsafe { &*control }.withheld.get();
    while !slot.is_null() {
        count += 1;
        slot = unsafe { withheld_link(slot).read() };
    }

    count
}

#[cfg(test)]
mod tests;
