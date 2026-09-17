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
//! There are three independent reasons a dead slot may still be named:
//!
//! - a candidate-queue entry, represented by
//!   [`crate::refcount::CANDIDATE_BIT`];
//! - this window, represented by a non-null [`DEFERRED_RETURNS`], while mark
//!   or scan may still use a shadow row for the slot;
//! - a trace on another thread, represented by this thread's token being
//!   held while no window of its own is open, which may hold the slot's
//!   address from before its death (below, "A foreign holder of the token").
//!
//! Every attempted return goes through `memory::stdapi::ll_free`. That entry
//! point first refuses the queue window and then calls
//! [`withhold_under_a_trace_or_make_returns`] for this one. Closing a trace gives its withheld
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
//! disagree with the window's existence (`dev/DECISIONS.md`, "the trace's
//! withheld returns are manager memory, drawn where a refusal can still be
//! answered"). That line is the whole of
//! the region and the whole of what the module holds: one head, one flag, and
//! every withheld return in the dying entity it belongs to.
//!
//! There is no TLS drop glue: thread-exit order is owned explicitly by
//! `memory::heap::ll_thread_exit`, and a runtime structure first touched by a
//! destructor may not depend on the platform's TLS destructor order
//! (`dev/DECISIONS.md`, "thread exit owns the order its per-thread state dies
//! in").
//!
//! # A foreign holder of the token
//!
//! A trace on another thread holds this thread's token and reads its blocks
//! (`rfc/model/gc/rc-cycle.md`, "The deferral's contract"). While it does,
//! **every death of this thread's is withheld and no stamp is read**
//! ([`withhold_under_a_foreign_trace`]): between reading a cell and meeting
//! the child's row that trace holds an address in a block that carries no
//! stamp yet, so the stamp cannot say which slots it holds. The stack is the
//! window's own shape — threaded through the dead entities, the head one
//! thread-local word — and it draws nothing, so a thread that never
//! collected withholds without a workspace. The mutator makes the returns once
//! it reads the token free ([`make_returns_withheld_under_a_foreign_trace`]):
//! at its next free, at the safepoint poll and before its exit; a holder
//! that arrives meanwhile leaves them standing. A cross-thread free of an
//! entity slot is a return of the mutator's memory made at the mutator's reclaim
//! of its remote stack, and that reclaim waits the same way
//! ([`returns_are_withheld`]). What this costs is the churn one trace lasts,
//! measured in `dev/BENCHMARKS.md`, "S38.3 what a foreign holder costs the
//! mutator".
//!
//! **The deaths are one of three stacks**, because a trace holds addresses
//! into more than entity slots (`rfc/model/gc/rc-cycle.md`, "The deferral's
//! contract"). A buffer chunk an array's growth or a mutator's death would
//! free waits on the second, threaded through the chunk's first word with
//! its capacity packed above the link ([`withhold_chunk_under_a_foreign_trace`]);
//! a whole block — an arena's at its reset, a buffer arena's or an entity
//! heap's when it empties, a retained one's when its last occupant and
//! payload are gone, the reset's whole-block sentinel among them — and an
//! OS-direct run wait on the third, threaded through the header word the
//! pool links by ([`withhold_block_under_a_foreign_trace`]). The block's
//! gate stands in the pool's own `put`, which is the one entry every block
//! return reaches, and the run's in the run arm of `ll_free`. The mutator
//! makes all three at the same three moments, the slots first, because a
//! slot's return can empty its block and reach the pool.
//!
//! The link the stack is threaded through is the dead object's class word
//! and the dead array's version word, both of which such a trace loads, so
//! it is written as a release and read as an acquire ([`withheld_link`]); a
//! reader that finds the link where a class was re-reads the count as zero
//! and strides nothing (`crate::cells::trace_cells`).
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
//! - **a row does** — the slot goes on the window's stack, which the close
//!   pops one slot at a time ([`WithheldReturns::dispose_withheld`]).
//!
//! The stack is the whole of the withholding, and no header bit is read to
//! find a withheld slot. `ll_free` marks both arms alike at its head, the mark
//! being its own guard against a second free and no part of this decision
//! ([`crate::refcount::DEAD_IN_PLACE`]).
//!
//! **The close walks no block's slots**, whoever owns the block: the bump
//! cursor that would bound such a walk is the mutator's to move, and reading a
//! slot the mutator is publishing races that store. What the window itself reads
//! of a block is the one word the stamp stands in; what the return then reads
//! is `ll_free`'s own. What the stack costs the close moves with the deaths
//! withheld rather than with the blocks the collection touched, and the pop
//! learns the next address from the slot it is freeing, so the returns cannot
//! overlap (`dev/BENCHMARKS.md`, "S43.1 the sweep's walk against the withheld
//! chain" and "S44.4 the close against the chain").
//! A thread exiting with its window still open ends the process
//! ([`dispose_thread_state`]); the module's other assertions refuse a
//! window misused by its own caller — a second trace on the thread, a second
//! detach, a batch handed on twice — and every one of them stands in every
//! build.
//!
//! The workspace's region enters no byte figure, being memory the thread
//! holds whether or not a collection is running
//! (`crate::cycle::arena::TraceScratchArena::residue`), and the module holds no
//! manager memory besides. So it moves the manager's ledger by nothing
//! (`crate::memory::gc_metadata`).

use std::cell::Cell;

use crate::cycle::arena::find_initialized_row;
use crate::cycle::row::{EdgeTarget, resolve_edge_target};
use crate::cycle::shadow::{self, Color};
use crate::memory::block_pool::{BLOCK_KIND_ENTITY, BLOCK_KIND_RETAINED, BlockHeader};

/// The head of the withheld returns and the words the close reads beside it,
/// resident in the workspace's fixed prefix.
///
/// `Cell` rather than a lock or a `RefCell`: the head has one writer by
/// construction, the thread whose trace window is open, and the push sits on
/// the free path where a borrow flag buys nothing.
///
/// One 64-byte line of its own, so a push writes no line another reader is on.
/// The line holds two words and is not packed further: the region it heads is
/// the workspace's fixed prefix, and a prefix under 64 bytes would move the
/// collection's bump off a line boundary for nothing.
#[repr(C, align(64))]
struct WindowControl {
    /// Newest withheld slot, or null while this window has withheld none.
    /// Each names the next through [`withheld_link`] and the oldest names
    /// null, a stack rather than a list because a slot is pushed once and no
    /// word of it has to answer "stacked?".
    withheld: Cell<*mut u8>,
    /// Whether this collection's rows are gone, which is what decides between
    /// returning the marks and abandoning them.
    ///
    /// False until [`ActiveTrace`]'s drop has swept, so an unwind before the
    /// sweep abandons: a slot handed back with a row still naming it is the
    /// reuse this window exists to prevent, and the fact is read once at the
    /// close rather than inferred from where the unwind came from.
    swept: Cell<bool>,
}

const _: () = assert!(size_of::<WindowControl>() == 64);
const _: () = assert!(align_of::<WindowControl>() == 64);

/// Bytes the withheld returns take out of the workspace: the control line the
/// stack's head stands in, and nothing besides — every withheld return is held
/// in the dying entity itself.
pub(crate) const RETURNS_BASE_BYTES: usize = size_of::<WindowControl>();

thread_local! {
    /// The control line of this thread's withheld returns while its trace
    /// window is open, and null otherwise. Non-owning: the region belongs to
    /// the workspace the [`ActiveTrace`]'s arena holds.
    static DEFERRED_RETURNS: Cell<*mut WindowControl> =
        const { Cell::new(std::ptr::null_mut()) };
}

/// The stack a trace's withheld returns are pushed onto, and the mutator that
/// clears what an unwind leaves standing.
///
/// A holder of its own rather than a field the enclosing drop unwinds by hand:
/// an unwind out of the close would otherwise skip the clearing below, and a
/// stacked slot that outlives its window is one no window returns.
/// [`crate::cycle::arena::TraceScratchArena`] is re-entrant for the same
/// reason.
struct WithheldReturns {
    /// The control line, in the workspace region this window was opened over.
    control: *mut WindowControl,
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
    /// 64, and stays the caller's for as long as this window is used.
    unsafe fn open(region: *mut u8) -> Self {
        let control = region as *mut WindowControl;

        // Field by field and written rather than assigned: the region is
        // memory with no value in it, so an assignment would drop a
        // `WindowControl` that was never constructed.
        unsafe {
            (&raw mut (*control).withheld).write(Cell::new(std::ptr::null_mut()));
            (&raw mut (*control).swept).write(Cell::new(false));
        }

        Self { control }
    }

    fn control(&self) -> &WindowControl {
        unsafe { &*self.control }
    }

    /// Record that this collection's rows are gone, which is what lets an
    /// unwind return the marks it finds rather than abandon them.
    ///
    /// Called by [`ActiveTrace`]'s drop the instant
    /// [`crate::cycle::arena::TraceScratchArena::sweep_rows`] returns, and by
    /// nothing else: every path that returns memory stands on this word.
    fn rows_are_gone(&self) {
        self.control().swept.set(true);
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

    /// Take the newest withheld slot off the stack, or **None** when nothing
    /// is stacked.
    ///
    /// **The head moves before the slot is disposed of**, which is what makes
    /// a panic inside one return cost that one slot: the slots below it are
    /// still named by the head, so the drop's own pass finds them. A pop that
    /// moved the head after the return would leave the whole stack named by a
    /// slot the free list has taken back.
    fn pop_withheld(&self) -> Option<*mut u8> {
        let slot = self.control().withheld.get();
        if slot.is_null() {
            return None;
        }

        // Safety: a withheld slot is one `ll_free` took and this window
        // stacked, and nothing has returned; what keeps its memory readable
        // differs by population.
        // A size-class slot reached no free list and stands below its block's
        // bump cursor, still counted in the block's `used`
        // (`crate::memory::heap::Heap::free`); a retained survivor is still a
        // live occupant of its block, which therefore cannot go home
        // (`crate::memory::retained::occupant_freed`); a large entity is the
        // one occupant its block or mapping waits for. None of the three moves
        // when a block changes hands: adoption writes the owner word and no
        // slot's state.
        self.control().withheld.set(unsafe { withheld_next(slot) });
        Some(slot)
    }

    /// Dispose of every slot this window withheld, newest first, by
    /// `disposition` ([`Disposition`]).
    ///
    /// Called with the window closed, so a return that reaches
    /// [`withhold_under_a_trace_or_make_returns`] again is not withheld a second time and
    /// proceeds physically. The pop itself reads
    /// no word of any block, only the dead slot's own link; what reads the
    /// block is the return behind it, `ll_free` posting onto the block's stack
    /// of cross-thread frees while the block is another thread's and taking
    /// the ordinary mutator path where this thread owns it.
    ///
    /// **The link is read before the return overwrites it**, the free list
    /// linking through the same word ([`withheld_link`]); the pop takes the
    /// next address off the slot and only then hands the slot over.
    fn dispose_withheld(&self, disposition: Disposition) {
        while let Some(slot) = self.pop_withheld() {
            note_slot_popped();
            unsafe { dispose_of(slot, disposition) };
        }
    }
}

/// What the close does with a slot it pops.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// Hand the slot back and make the return it deferred, which is the close.
    Return,
    /// Return nothing, which is the unwind that reached the window's drop
    /// before the rows were swept ([`WithheldReturns::drop`]). The slot stays
    /// exactly where the withholding found it: out of circulation, holding its
    /// block or mapping with it, and still carrying the bit `ll_free` took —
    /// which is true of it, nobody having handed it back. That is the price of
    /// a window that lost its collection's rows before it could give anything
    /// back.
    Abandon,
}

// Slots the close has popped on this thread, one per withheld return. What it
// is for is the size of the close: a reading tells a collection that withheld
// nothing from one that withheld and gave back, where the free lists afterwards
// read alike.
#[cfg(test)]
thread_local! {
    static SLOTS_POPPED: Cell<usize> = const { Cell::new(0) };
}

/// Count one slot the close popped, and nothing at all without `cfg(test)`:
/// the pop calls it either way.
#[inline]
fn note_slot_popped() {
    #[cfg(test)]
    SLOTS_POPPED.with(|popped| popped.set(popped.get() + 1));
}

/// What the probe holds for this thread, zeroed by the read.
#[cfg(test)]
pub(crate) fn take_slots_popped() -> usize {
    SLOTS_POPPED.with(|popped| popped.replace(0))
}

/// The word a stacked slot names the next one through: the eight bytes a free
/// slot links through (`crate::memory::heap::FREE_LIST_LINK_OFFSET`), which
/// hold nothing the mutator reads while the slot is dead and which the return
/// overwrites.
///
/// **Written and read atomically, and the write is a release**, because in
/// an object the word is the class word and in an array the head's version,
/// both of which a trace on another thread loads: such a trace may hold the
/// dead entity's address from before its death and read the word after it.
/// The release orders the count's fall before the link, so a reader whose
/// acquire load of the class word returned the link re-reads the count as
/// zero and expands nothing (`crate::cells::trace_cells`); a version read
/// as the link fails the bracket or finds the null storage the dispose left.
/// On the mutator's own window the stack has one writer and one reader and the
/// atomics cost nothing.
///
/// # Safety
/// `slot` is a dead entity slot of at least the free list's two words.
#[inline]
unsafe fn withheld_link(slot: *mut u8) -> &'static std::sync::atomic::AtomicPtr<u8> {
    unsafe {
        &*(slot.add(crate::memory::heap::FREE_LIST_LINK_OFFSET)
            as *const std::sync::atomic::AtomicPtr<u8>)
    }
}

/// The next slot `slot` names on its stack ([`withheld_link`]).
///
/// # Safety
/// As [`withheld_link`].
#[inline]
unsafe fn withheld_next(slot: *mut u8) -> *mut u8 {
    unsafe { withheld_link(slot) }.load(std::sync::atomic::Ordering::Acquire)
}

/// Name `next` from `slot` ([`withheld_link`]).
///
/// # Safety
/// As [`withheld_link`].
#[inline]
unsafe fn set_withheld_next(slot: *mut u8, next: *mut u8) {
    unsafe { withheld_link(slot) }.store(next, std::sync::atomic::Ordering::Release);
}

/// Put `slot` on this window's stack of withheld returns.
///
/// # Safety
/// As [`withheld_link`], and `control` is this thread's open window.
unsafe fn push_withheld(control: &WindowControl, slot: *mut u8) {
    unsafe { set_withheld_next(slot, control.withheld.get()) };
    control.withheld.set(slot);
}

/// Make the return this window deferred, where the disposition is
/// [`Disposition::Return`].
///
/// **The slot is handed back first, and only here.** The return re-enters
/// `ll_free`, which refuses a free of a slot it already holds
/// (`crate::memory::stdapi::hand_back_and_free`; `crate::refcount::DEAD_IN_PLACE`).
/// [`Disposition::Abandon`] hands nothing back and frees nothing.
///
/// # Safety
/// `slot` is a dead entity this close has taken off the window's stack.
unsafe fn dispose_of(slot: *mut u8, disposition: Disposition) {
    if disposition == Disposition::Return {
        unsafe { crate::memory::stdapi::hand_back_and_free(slot) };
    }
}

impl Drop for WithheldReturns {
    /// Take the window down and dispose of every slot still on its stack,
    /// which is all this window owns: its control line stands in the workspace
    /// region the arena hands back, its withheld returns stand in the dead
    /// entities themselves, and no path of this module holds memory of the
    /// manager's.
    ///
    /// **Which of the two dispositions it takes is
    /// [`WindowControl::swept`]'s to say.** The ordered close sets that
    /// word the instant the rows are gone, so:
    ///
    /// - **a panic raised after the sweep** — inside
    ///   [`crate::cycle::arena::TraceScratchArena::reset`]'s hand-back of its
    ///   own blocks, or inside one of the close's own returns — is survived
    ///   for every slot still on the stack: those are returned, the rows that
    ///   would have made the returns a reuse being gone;
    /// - **a panic raised before the sweep** is not survived at all: the
    ///   memory stays out of circulation, a slot handed back under a live row
    ///   being the reuse this window exists to prevent.
    ///
    /// **No panic site of the crate stands in that second case**, [`ActiveTrace`]'s
    /// drop sweeping ahead of everything that can raise (`dev/DECISIONS.md`,
    /// "the row sweep runs ahead of the candidate restore"). The word is read
    /// rather than the order inferred, so a close reordered later cannot make
    /// this drop return under a row that still names the slot.
    ///
    /// **What no panic recovers is the one return it interrupted.** A raising
    /// `ll_free` leaves its own slot with the mark handed back and the return
    /// unmade, on no free list, below its block's cursor and counted in its
    /// block's occupancy. Nothing else of that pass is lost: the pop takes the head
    /// off the stack before it hands the slot over, so the slots behind the
    /// raising one are still named by the head and this pass makes their
    /// returns ([`pop_withheld`](WithheldReturns::pop_withheld)).
    ///
    /// A panic raised by these returns themselves is a panic during an unwind
    /// and ends the process, which is why this pass makes no return the
    /// ordered close would not have made and reaches no assertion it failed.
    fn drop(&mut self) {
        self.close_window();
        let disposition = if self.control().swept.get() {
            Disposition::Return
        } else {
            Disposition::Abandon
        };

        self.dispose_withheld(disposition);
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
    /// The entries this collection read out of the ring as its batch, until
    /// the window closes. `None` before [`ActiveTrace::read_candidates`].
    ///
    /// A count over entries that stay in the ring: the close disposes of them
    /// in place, and a close that chose no disposition leaves them where they
    /// are for the retirement pass that follows it
    /// (`crate::cycle::queue::dispose_candidates`).
    batch: Option<crate::cycle::queue::Batch>,
    /// Declared before the arena, and therefore dropped before it: the stack's
    /// control line stands in a region of the workspace, which the arena's drop
    /// hands back to the thread.
    ///
    /// Defensive rather than load-bearing, and worth the line for what
    /// it costs: the drop below pops before either field dies, and
    /// `queue::return_workspace_base` leaves the block in the thread's own cell
    /// rather than handing it to the pool, so a reversed order would read
    /// memory nobody else can have yet. It becomes load-bearing the day that
    /// call gives the block back.
    returns: WithheldReturns,
    arena: crate::cycle::arena::TraceScratchArena,
    /// Whether [`close`](Self::close) has run, so that [`Drop`] runs it once.
    closed: bool,
    /// The commit count the marked pass saw; `Some` selects the deferred
    /// disposition at the close ([`Self::dispose_batch_on_close`]).
    defer_at_commits: Option<u64>,
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
            closed: false,
            defer_at_commits: None,
            _not_send: std::marker::PhantomData,
        })
    }

    /// Read this thread's ring as this trace's batch, once, before the
    /// first mark.
    ///
    /// The draw the window needed is behind it — the reading itself asks for
    /// nothing and cannot be refused (`crate::cycle::queue::read_batch`)
    /// — so a collection that reaches this line has all the memory its roots
    /// cost.
    pub(crate) fn read_candidates(&mut self) {
        assert!(self.batch.is_none(), "a trace reads its batch once");
        self.batch = Some(crate::cycle::queue::read_batch());
    }

    /// Read P alone as this trace's batch, the collection over the
    /// collector's verdicts (`crate::cycle::queue::read_batch_of_verdicts`).
    pub(crate) fn read_verdicts(&mut self) {
        assert!(self.batch.is_none(), "a trace reads its batch once");
        self.batch = Some(crate::cycle::queue::read_batch_of_verdicts());
    }

    /// The arena and the batch in one answer, because a trace reads
    /// the batch's roots while writing the arena's rows and two calls would
    /// borrow this window twice.
    ///
    /// # Panics
    /// When no batch has been read, which is a caller that skipped
    /// [`ActiveTrace::read_candidates`].
    pub(crate) fn rows_and_roots(
        &mut self,
    ) -> (
        &mut crate::cycle::arena::TraceScratchArena,
        &crate::cycle::queue::Batch,
    ) {
        let batch = self
            .batch
            .as_ref()
            .expect("the trace has no candidate batch");
        (&mut self.arena, batch)
    }

    /// Arm this window's close to harvest, so that its sweep writes the
    /// entities the scan left unreachable into the thread's member list
    /// ([`crate::cycle::members`]).
    ///
    /// **False when a list is already standing on this thread**, which is a
    /// collection a destructor of another collection's teardown started: it
    /// closes the way an ordinary one does and takes no members, the region
    /// holding one list and the outer driver still reading it.
    ///
    /// Armed after the trace answers `Complete` and never before. A trace that
    /// gave up leaves no colour that is a verdict, so a harvest of its rows
    /// would name entities no scan classified (`crate::cycle::trace`).
    ///
    /// `capacity` is what this harvest takes before it gives up, and the
    /// driver of an ordinary collection arms nothing at all: that path keeps
    /// its arena through the teardown and reads the rows themselves.
    pub(crate) fn arm_harvest(&mut self, capacity: u32) -> bool {
        self.arena.arm_harvest(capacity)
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

    /// Close a pressure trace without choosing its candidate disposition, then
    /// hand the batch to the pressure driver.
    ///
    /// A panic before the hand-over leaves `closed` false, so [`Drop`] closes
    /// the window normally, the entries standing in the ring either way. The
    /// caller that receives the batch owns its exact-validation disposition:
    /// defer it, or leave it for the retirement pass.
    pub(crate) fn close_and_take_batch(mut self) -> crate::cycle::queue::Batch {
        assert!(
            self.defer_at_commits.is_none(),
            "a reading that chose the deferred lane cannot hand its batch on"
        );
        self.close(false);
        self.closed = true;
        self.batch.take().expect("a pressure close takes one batch")
    }

    /// Mark every root this collection may defer, and answer how many were
    /// marked.
    ///
    /// A root whose row the scan left [`Color::Live`] is one this collection
    /// read and proved held from outside; a root whose row it left
    /// [`Color::PotentiallyUnreachable`] is one of them too when
    /// `externally_referenced` — the commit's own exact validation read the
    /// proposed set that way. Every other root is left alone: a root of a
    /// set the commit refused is garbage that is not collectible yet, and the
    /// deferred lane would hide it for a whole epoch.
    ///
    /// **The rows are read here because here is where they still stand.** The
    /// close sweeps them, and past that a root's colour cannot be recovered at
    /// any price (`PLAN.md` S37.6).
    pub(crate) fn mark_roots_for_deferral(&mut self, externally_referenced: bool) -> usize {
        let batch = self
            .batch
            .as_mut()
            .expect("only a batch that was read is marked");
        batch.mark_for_deferral(|root| {
            let EdgeTarget::Tracked(key) = (unsafe { resolve_edge_target(root) }) else {
                return false;
            };

            match unsafe { find_initialized_row(key) } {
                Some(row) => match shadow::color(unsafe { *row }) {
                    Color::Live => true,
                    Color::PotentiallyUnreachable => externally_referenced,
                    _ => false,
                },
                None => false,
            }
        })
    }

    /// Select the mutator-side disposition for the trace's original records: the
    /// marked pass, with `at_commits` the commit count the reading that set
    /// those marks saw (`crate::cycle::queue::dispose_candidates`).
    ///
    /// A close that never reaches this leaves the batch in the ring whole,
    /// which is every path that gave up before the commit.
    pub(crate) fn dispose_batch_on_close(&mut self, at_commits: u64) {
        assert!(
            self.batch.is_some(),
            "only a batch that was read has a disposition"
        );
        self.defer_at_commits = Some(at_commits);
    }

    /// The ordered close: sweep the rows, dispose of the batch, make the
    /// withheld returns, give the arena's blocks back. `dispose_batch` is
    /// false for a pressure close that hands its batch on to the driver.
    ///
    /// The sweep comes first, taken whether or not anything was withheld:
    /// after the window falls, a physical return may recommission the block
    /// whose shadow pointer the sweep must null. It stands ahead of the
    /// disposition so that an unwind raised past it — out of a free the
    /// disposition's retirement makes — leaves a drop whose rows are gone and
    /// whose withheld returns can therefore be made rather than abandoned
    /// (`dev/DECISIONS.md`, "the row sweep runs ahead of the candidate
    /// restore"). Every root the disposition keeps stays in the ring, in
    /// order, behind nothing and ahead of whatever the teardown registered;
    /// the disposition reads no row and no withheld slot, and `ll_free`'s
    /// candidate arm reads the entity's own bit rather than the lane its
    /// record stands in, so nothing above or below turns on where it stands
    /// between them. A close that chose no disposition leaves the batch in
    /// the ring for the retirement pass behind it
    /// (`crate::cycle::collect`). The arena's own blocks name no slot, so
    /// they go back after the returns rather than before them, which is what
    /// leaves every return made when a panic in the hand-back sends this frame
    /// into [`WithheldReturns::drop`]. The reset enters its own residue in the
    /// high-water figure as it rewinds
    /// (`crate::cycle::arena::TraceScratchArena`), and this window has no
    /// residue to stand beside it.
    fn close(&mut self, dispose_batch: bool) {
        self.arena.sweep_rows();
        self.returns.rows_are_gone();
        if dispose_batch {
            if let (Some(batch), Some(at_commits)) = (self.batch.take(), self.defer_at_commits) {
                crate::cycle::queue::dispose_candidates(batch, at_commits);
            }
        }

        fire_injected_close_unwind();
        self.returns.close_window();
        self.returns.dispose_withheld(Disposition::Return);
        self.arena.reset();
    }
}

// Whether this thread's next close raises between the row sweep and the
// returns it withheld.
//
// Fault injection, tests only, and for a state that has no other way in: the
// disposition of the batch cannot refuse, and the two calls around it — the
// sweep and the returns — are the ones the case is about. What could raise
// here in production is an assertion inside the pool the merge's own growth
// reaches, which no test can stage from outside.
#[cfg(test)]
thread_local! {
    static PANIC_IN_CLOSE: Cell<bool> = const { Cell::new(false) };
}

/// Arm the injection for **one** close of this thread
/// ([`crate::cycle::testing::ArmedInjection`]).
///
/// What it stages is an unwind out of the close past the sweep: the rows are
/// gone by then, so the withheld returns are made by the drop that runs behind
/// it rather than abandoned (`dev/DECISIONS.md`, "the row sweep runs ahead of
/// the candidate restore").
#[cfg(test)]
pub(crate) fn inject_close_unwind() -> crate::cycle::testing::ArmedInjection {
    crate::cycle::testing::ArmedInjection::arm(&PANIC_IN_CLOSE)
}

/// Raise the armed unwind and disarm it, and do nothing at all without
/// `cfg(test)`.
#[inline]
fn fire_injected_close_unwind() {
    #[cfg(test)]
    if PANIC_IN_CLOSE.with(|armed| armed.replace(false)) {
        panic!("the injected close unwind");
    }
}

impl Drop for ActiveTrace {
    fn drop(&mut self) {
        if self.closed {
            return;
        }

        self.close(true);
    }
}

/// Refuse a physical return while the current trace can still address the
/// slot, withholding the return for the window's close, and answer whether the
/// return was refused.
///
/// **False is a return the caller must make physically**, which is a thread
/// with no window open and no foreign holder of its token, or a death in
/// memory this collection never touched ([`classify`]). On the first of
/// those the returns a foreign holder withheld earlier are made first
/// ([`make_returns_withheld_under_a_foreign_trace`]): the free that finds
/// the token free is one of the three places the mutator makes them.
///
/// Called only after the queue-entry window has refused the same return. A
/// close that still finds `CANDIDATE_BIT` stops before here, because the
/// queue entry itself keeps the slot withheld.
///
/// With no window open the cost is one thread-local load, the token's
/// reading — a fence and an acquire load through the record — and, with
/// nothing withheld, three thread-local reads of empty heads. With a window
/// open, the block's own state is read — one load for a slotted or a
/// retained death, one for a large entity's row — and a withheld death then
/// costs one write into the dying entity's own byte 8 and one store of the
/// head, with no allocator call and no pool call; the link store is a
/// release store, priced in `dev/BENCHMARKS.md`, "S38.3 what a foreign
/// holder costs the mutator".
///
/// # Safety
/// `ptr` is a dead entity slot whose teardown has completed and which this call
/// owns until either the function returns `false` or the window closes.
/// `kind` is the kind `ptr`'s own block reads, and outside the retained
/// sentinel `ptr` addresses an entity rather than the block itself — the push
/// writes the stack link into `ptr`'s byte 8, and a block base passed under
/// any other kind would land it in the block's own header.
#[inline]
pub(crate) unsafe fn withhold_under_a_trace_or_make_returns(ptr: *mut u8, kind: u32) -> bool {
    let control = DEFERRED_RETURNS.with(Cell::get);
    if !control.is_null() {
        let window = unsafe { &*control };
        return unsafe { withhold(window, ptr, kind) };
    }

    // The slot entry is one of the two readers that act on the byte: a
    // request is consented to here, and `POSTED` arms the collection over P
    // (`crate::cycle::token::read_and_act_on_this_thread`).
    if crate::cycle::token::read_and_act_on_this_thread() == crate::cycle::token::Reading::Collector
    {
        // The reset's whole-block sentinel addresses a block header, which
        // has no byte 8 to thread the stack through; the block's return
        // waits at the pool's own entry instead
        // (`withhold_block_under_a_foreign_trace`).
        if kind == BLOCK_KIND_RETAINED && ptr == BlockHeader::of_ptr(ptr) as *mut u8 {
            return false;
        }

        unsafe { withhold_under_a_foreign_trace(ptr) };
        return true;
    }

    unsafe { make_returns_withheld_under_a_foreign_trace() };
    false
}

/// Whether a return of this thread's entity memory would be made under a
/// trace — this thread's own window, or a foreign holder of its token — and
/// so has to wait. For the reclaim of cross-thread frees, which reaches no
/// `ll_free` on the mutator: the slots stay on their block's remote stack until
/// a collect that finds no trace (`crate::memory::heap::Heap::collect_remote`).
#[inline]
pub(crate) fn returns_are_withheld() -> bool {
    !DEFERRED_RETURNS.with(Cell::get).is_null()
        || crate::cycle::token::collector_is_tracing_this_thread()
}

/// The address bits of a packed chunk word, below the size.
const CHUNK_LINK_BITS: u32 = 48;

/// The next chunk a withheld chunk names, and the chunk's own capacity, read
/// off its first word: the address in the low 48 bits and the capacity above
/// them, a chunk being at most one block's payload
/// (`memory::block_pool::BLOCK_PAYLOAD`), which fits the 16 bits left.
///
/// The first word and no other: a trace on another thread may still stride
/// the chunk from a reading it validated before the free, and the words it
/// reads are an element's `+8` and an entry's key — never the chunk's first
/// word, which is an index slot pair in the hash form and the first
/// element's `+0` in the vector form (`crate::cells::trace_cells`).
///
/// # Safety
/// `chunk` is a withheld chunk of this thread's.
#[inline]
unsafe fn chunk_link(chunk: *mut u8) -> (*mut u8, usize) {
    let word = unsafe {
        (*(chunk as *const std::sync::atomic::AtomicU64)).load(std::sync::atomic::Ordering::Acquire)
    };
    (
        (word & ((1 << CHUNK_LINK_BITS) - 1)) as usize as *mut u8,
        (word >> CHUNK_LINK_BITS) as usize,
    )
}

/// Write `next` and `capacity` into `chunk`'s first word ([`chunk_link`]).
///
/// # Safety
/// As [`chunk_link`], and `capacity` is below `1 << 16`.
#[inline]
unsafe fn set_chunk_link(chunk: *mut u8, next: *mut u8, capacity: usize) {
    debug_assert!(
        capacity < 1 << (64 - CHUNK_LINK_BITS),
        "a chunk's capacity fits above the link"
    );
    debug_assert!(
        next as usize >> CHUNK_LINK_BITS == 0,
        "an address fits the link"
    );
    let word = (next as usize as u64) | ((capacity as u64) << CHUNK_LINK_BITS);
    unsafe {
        (*(chunk as *const std::sync::atomic::AtomicU64))
            .store(word, std::sync::atomic::Ordering::Release)
    };
}

/// Where a withheld block's link stands: the header word the pool links by.
const BLOCK_LINK_OFFSET: usize = std::mem::offset_of!(BlockHeader, next);

/// The next block a withheld block names, through the header word the pool
/// links by (`memory::block_pool::BlockHeader::next`), which a block reaching
/// its return is on no list through, and which a run keeps its `size` in —
/// a figure its unmapping does not read.
///
/// # Safety
/// `block` is a withheld block or run of this thread's.
#[inline]
unsafe fn block_link(block: *mut u8) -> *mut u8 {
    unsafe {
        (*(block.add(BLOCK_LINK_OFFSET) as *const std::sync::atomic::AtomicPtr<u8>))
            .load(std::sync::atomic::Ordering::Acquire)
    }
}

/// Name `next` from `block` ([`block_link`]).
///
/// # Safety
/// As [`block_link`].
#[inline]
unsafe fn set_block_link(block: *mut u8, next: *mut u8) {
    unsafe {
        (*(block.add(BLOCK_LINK_OFFSET) as *const std::sync::atomic::AtomicPtr<u8>))
            .store(next, std::sync::atomic::Ordering::Release)
    };
}

/// Whether a return of this thread's memory made now would be made under a
/// foreign holder of its token: the token held and no window of the
/// thread's own open. The mutator's own window withholds by its stamp and
/// frees no chunk and no block while it traces
/// (`rfc/model/gc/rc-cycle.md`, "The deferral's contract").
#[inline]
fn under_a_foreign_holder() -> bool {
    // `try_with`: the pool's `put` runs from a thread-local's drop on the
    // exit path, and a `const` cell with no drop glue is never destroyed
    // before it, so the fallback is the null it would read anyway.
    DEFERRED_RETURNS
        .try_with(Cell::get)
        .unwrap_or(std::ptr::null_mut())
        .is_null()
        && crate::cycle::token::collector_is_tracing_this_thread()
}

/// Withhold a buffer chunk's return while another thread's trace holds this
/// thread's token, and answer whether it was withheld: a trace may stride
/// the chunk from a reading it validated before the free
/// (`memory::buffer_arena::buffer_free_longlived_payload`). The chunk is
/// threaded by its first word, which no stride reads ([`chunk_link`]).
///
/// # Safety
/// `(chunk, capacity)` is one live chunk of this thread's buffer arena, and
/// `capacity` is at most a block's payload.
#[inline]
pub(crate) unsafe fn withhold_chunk_under_a_foreign_trace(chunk: *mut u8, capacity: usize) -> bool {
    if !under_a_foreign_holder() {
        return false;
    }

    CHUNKS_WITHHELD_UNDER_A_FOREIGN_TRACE.with(|head| {
        unsafe { set_chunk_link(chunk, head.get(), capacity) };
        head.set(chunk);
    });
    true
}

/// Withhold a whole block's or an OS-direct run's return while another
/// thread's trace holds this thread's token, and answer whether it was
/// withheld: a trace holds addresses into an arena's blocks as untracked
/// children, into a buffer block's chunks, into a retained block's
/// survivors, and a block the pool recommissioned or a run the system
/// unmapped under it is memory no stale reading survives
/// (`memory::block_pool::BlockPool::put`, `memory::stdapi::ll_free`'s run
/// arm). Threaded by the header's second word ([`block_link`]).
///
/// # Safety
/// `block` is a block header of this thread's on no list, or the header of
/// an OS-direct run about to be unmapped.
#[inline]
pub(crate) unsafe fn withhold_block_under_a_foreign_trace(block: *mut u8) -> bool {
    if !under_a_foreign_holder() {
        return false;
    }

    BLOCKS_WITHHELD_UNDER_A_FOREIGN_TRACE.with(|head| {
        unsafe { set_block_link(block, head.get()) };
        head.set(block);
    });
    true
}

/// Withhold a death while another thread's trace holds this thread's token.
///
/// **Every death is withheld, and no stamp is read**, unlike the mutator's own
/// window ([`classify`]): a trace on another thread holds an address between
/// reading the cell that named it and meeting its row, and in that interval
/// the block carries no stamp yet, so a return made on the strength of a
/// clear stamp could be handed out again under the address the trace still
/// holds. The cost is the churn one trace lasts (`dev/BENCHMARKS.md`, "S38.3
/// what a foreign holder costs the mutator"). The stack is threaded through
/// the dead entities like the window's ([`withheld_link`]), headed in a word
/// of this thread's, and nothing is drawn.
///
/// # Safety
/// As [`withhold_under_a_trace_or_make_returns`], and this thread has no window of its own
/// open.
#[inline]
unsafe fn withhold_under_a_foreign_trace(ptr: *mut u8) {
    WITHHELD_UNDER_A_FOREIGN_TRACE.with(|head| {
        unsafe { set_withheld_next(ptr, head.get()) };
        head.set(ptr);
    });
}

/// Make the returns withheld under a foreign holder, once the token is free.
///
/// Called by the mutator and by nobody else — a free that finds the token
/// free, the safepoint poll, the pressure path under its own token ahead of
/// its allocation retry, and the exit after its wait for the holder —
/// because the slots are the mutator's and the physical return is its heap's.
/// Nothing is made while the token is held: a holder that arrived after the
/// release keeps every return standing, and a pop that finds the token taken
/// between two returns puts the slot back and stops, so a slot is never
/// handed back under a trace and the loop ends whatever the holders do.
///
/// Each return re-enters `ll_free` through the hand-back, which is the one
/// entry a withheld slot goes back through
/// (`crate::memory::stdapi::hand_back_and_free`).
///
/// # Safety
/// This thread has no window of its own open, and every slot on the stack is
/// a dead entity this thread's free withheld.
pub(crate) unsafe fn make_returns_withheld_under_a_foreign_trace() {
    // The whole stack is taken off the head first: each return re-enters
    // the entry that withheld it, which asks this function again, and a head
    // still naming the rest would make the returns a recursion one frame
    // deep per slot. Re-entered with an empty head it makes nothing and
    // answers at once.
    let mut taken = WITHHELD_UNDER_A_FOREIGN_TRACE.with(|head| head.replace(std::ptr::null_mut()));
    while !taken.is_null() {
        if crate::cycle::token::collector_is_tracing_this_thread() {
            // A holder arrived between two returns: what is left goes back
            // on the head, behind whatever the returns so far re-withheld.
            WITHHELD_UNDER_A_FOREIGN_TRACE.with(|head| unsafe {
                splice_behind_the_head(
                    head,
                    taken,
                    |slot| withheld_next(slot),
                    |last, next| set_withheld_next(last, next),
                )
            });
            return;
        }

        let slot = taken;
        taken = unsafe { withheld_next(slot) };
        unsafe { crate::memory::stdapi::hand_back_and_free(slot) };
    }

    // The chunks, then the blocks, each the same way. The slots go first
    // because a slot's return can empty its block and reach the pool,
    // which is where a block would be withheld again under a holder that
    // arrived meanwhile — and then the block list below finds it.
    let mut taken =
        CHUNKS_WITHHELD_UNDER_A_FOREIGN_TRACE.with(|head| head.replace(std::ptr::null_mut()));
    while !taken.is_null() {
        let (next, capacity) = unsafe { chunk_link(taken) };
        if crate::cycle::token::collector_is_tracing_this_thread() {
            CHUNKS_WITHHELD_UNDER_A_FOREIGN_TRACE.with(|head| unsafe {
                splice_behind_the_head(
                    head,
                    taken,
                    |chunk| chunk_link(chunk).0,
                    |last, next| {
                        let (_, last_capacity) = chunk_link(last);
                        set_chunk_link(last, next, last_capacity);
                    },
                )
            });
            return;
        }

        let chunk = taken;
        taken = next;
        unsafe { crate::memory::buffer_arena::buffer_free_longlived_payload(chunk, capacity) };
    }

    let mut taken =
        BLOCKS_WITHHELD_UNDER_A_FOREIGN_TRACE.with(|head| head.replace(std::ptr::null_mut()));
    while !taken.is_null() {
        if crate::cycle::token::collector_is_tracing_this_thread() {
            BLOCKS_WITHHELD_UNDER_A_FOREIGN_TRACE.with(|head| unsafe {
                splice_behind_the_head(
                    head,
                    taken,
                    |block| block_link(block),
                    |last, next| set_block_link(last, next),
                )
            });
            return;
        }

        let block = taken;
        taken = unsafe { block_link(block) };
        unsafe { crate::memory::stdapi::return_withheld_block(block) };
    }
}

/// Put a chain a drain took off `head` back on it, behind what the returns
/// made so far re-withheld: the chain's last link, found through `next_of`,
/// is pointed through `link` at what the head names now, and the head then
/// names the chain.
///
/// # Safety
/// `taken` is a chain this thread's drain took off `head`, threaded through
/// the links `next_of` reads and `link` writes.
unsafe fn splice_behind_the_head(
    head: &Cell<*mut u8>,
    taken: *mut u8,
    next_of: impl Fn(*mut u8) -> *mut u8,
    link: impl Fn(*mut u8, *mut u8),
) {
    let mut last = taken;
    loop {
        let next = next_of(last);
        if next.is_null() {
            break;
        }

        last = next;
    }

    link(last, head.get());
    head.set(taken);
}

/// How a death is withheld, or that it needs no withholding at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Withholding {
    /// No row of this collection addresses the slot, so the caller returns it
    /// physically and this window owes nothing.
    ReturnNow,
    /// The slot goes on the window's stack, threaded through the dead entity
    /// itself.
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
/// the window, so ownership decides nothing here. Why the mutator is no part of
/// the condition: `dev/DECISIONS.md`, "the stamp is the whole condition where
/// the return is not the mutator's" and "one stack through the dead entity
/// holds every withheld return".
///
/// # Safety
/// As [`withhold_under_a_trace_or_make_returns`].
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
/// A death [`classify`] withholds goes on this window's stack
/// ([`push_withheld`]), which the close pops. Nothing is drawn and nothing can
/// refuse, which is what leaves this path with no process end on it at all.
///
/// A withheld slot stays out of the allocator's hands because the physical
/// return was never made: it is on no free list and below its block's bump
/// cursor, and a withheld survivor keeps its block's occupant count above
/// zero, so the block is not the pool's either (module doc).
///
/// **What keeps a withheld slot off its free list is this function's single
/// exit**: a death is either returned here or stacked here, never both. No
/// assertion at the free list's own entrances says so, and none can — a slot
/// reaches them marked whichever way it came, the mark being `ll_free`'s own
/// (`dev/DECISIONS.md`, "a second `ll_free` of an entity is refused, and the
/// mark is the bit it is refused on").
///
/// # Safety
/// As [`withhold_under_a_trace_or_make_returns`], and `control` is this thread's open window.
unsafe fn withhold(control: &WindowControl, ptr: *mut u8, kind: u32) -> bool {
    if unsafe { classify(ptr, kind) } == Withholding::ReturnNow {
        return false;
    }

    unsafe { push_withheld(control, ptr) };
    true
}

/// Refuse a thread exit that would abandon an open trace window.
///
/// A live window at exit would leave a trace using blocks whose mutator is being
/// abandoned; that is outside the protocol, and this ends the process rather
/// than letting it happen — `ll_thread_exit` is `extern "C"` and has no caller
/// that could act on a refusal. No path from user code reaches it: an exit a
/// destructor asks for during a collection waits for the thread's top
/// (`crate::memory::heap::thread_exit_pending`), and a foreign holder of the
/// token is waited for before this runs
/// (`crate::cycle::collect::collect_before_exit`). What remains is the crate's
/// own misuse, an exit called with a window open on the calling frame's stack.
/// The window itself needs no disposal here: it belongs to the
/// [`ActiveTrace`], whose drop is what closes it.
pub(crate) fn dispose_thread_state() {
    assert!(
        DEFERRED_RETURNS.with(Cell::get).is_null(),
        "a thread cannot exit inside its trace window"
    );
    assert!(
        WITHHELD_UNDER_A_FOREIGN_TRACE.with(Cell::get).is_null()
            && CHUNKS_WITHHELD_UNDER_A_FOREIGN_TRACE
                .with(Cell::get)
                .is_null()
            && BLOCKS_WITHHELD_UNDER_A_FOREIGN_TRACE
                .with(Cell::get)
                .is_null(),
        "a thread cannot exit with returns withheld under a foreign trace"
    );
}

thread_local! {
    /// Newest return withheld because another thread holds this thread's
    /// trace token, or null; each names the next through [`withheld_link`].
    /// No control line stands behind it: the stack is threaded through the
    /// dead entities, and the head is this one word, so a thread that never
    /// collected — and so never drew a workspace — withholds without drawing
    /// anything on its free path. No drop glue, as every thread-local the
    /// exit reaches (`memory::heap::ll_thread_exit`).
    static WITHHELD_UNDER_A_FOREIGN_TRACE: Cell<*mut u8> = const { Cell::new(std::ptr::null_mut()) };
}

thread_local! {
    /// Newest buffer chunk withheld under a foreign holder, or null; each
    /// names the next through its own first word, packed with its size
    /// ([`chunk_link`]).
    static CHUNKS_WITHHELD_UNDER_A_FOREIGN_TRACE: Cell<*mut u8> = const { Cell::new(std::ptr::null_mut()) };
    /// Newest whole block or OS-direct run withheld under a foreign holder,
    /// or null; each names the next through its header's second word, the
    /// pool's own link ([`block_link`]).
    static BLOCKS_WITHHELD_UNDER_A_FOREIGN_TRACE: Cell<*mut u8> = const { Cell::new(std::ptr::null_mut()) };
}

/// How many chunks this thread is withholding under a foreign holder.
#[cfg(test)]
pub(crate) fn foreign_withheld_chunks() -> usize {
    let mut count = 0;
    let mut chunk = CHUNKS_WITHHELD_UNDER_A_FOREIGN_TRACE.with(Cell::get);
    while !chunk.is_null() {
        count += 1;
        chunk = unsafe { chunk_link(chunk) }.0;
    }

    count
}

/// How many blocks and runs this thread is withholding under a foreign
/// holder.
#[cfg(test)]
pub(crate) fn foreign_withheld_blocks() -> usize {
    let mut count = 0;
    let mut block = BLOCKS_WITHHELD_UNDER_A_FOREIGN_TRACE.with(Cell::get);
    while !block.is_null() {
        count += 1;
        block = unsafe { block_link(block) };
    }

    count
}

/// How many returns this thread is withholding under a foreign holder of its
/// token, by walking that stack.
#[cfg(test)]
pub(crate) fn foreign_withheld_count() -> usize {
    let mut count = 0;
    let mut slot = WITHHELD_UNDER_A_FOREIGN_TRACE.with(Cell::get);
    while !slot.is_null() {
        count += 1;
        slot = unsafe { withheld_next(slot) };
    }

    count
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
        slot = unsafe { withheld_next(slot) };
    }

    count
}

#[cfg(test)]
mod tests;
