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
//! asking the memory manager for anything. Past that region the chain takes
//! 64 KiB manager blocks, stamped `BLOCK_KIND_GC_METADATA` for as long as
//! they are held and returned at the window's close.
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
//! **A window's open asks no allocation path**, the workspace being the one
//! block it stands on and the [`ActiveTrace`] having drawn that already. So
//! the refusal a draw at the first withheld return would meet — holding a
//! slot whose rows are live, where returning it is the reuse this module
//! prevents and dropping it loses a physical return, which is refused
//! (`dev/DECISIONS.md`, "an enrolment cannot fail") — has no way of arriving
//! for as long as the region has room.
//!
//! Growth past the region is where it arrives, and there it ends the process,
//! which is the funded class's last resort and the same one the queue's
//! overflow buffer reaches (`crate::cycle::queue`). Reached only after
//! [`RETURNS_BASE_RECORDS`] slots have died inside one trace window while both
//! allocation paths refuse a single block.
//!
//! **A death in memory a trace has stamped does not reach it**: past the
//! region such a death is marked in the entity's own header
//! ([`crate::refcount::DEAD_IN_PLACE`]), which asks no allocation path and so
//! cannot be refused. The chain keeps every death the region holds, the walk a
//! mark costs the sweep being dearer than an append at every design class
//! (`dev/BENCHMARKS.md`, 2026-09-04, S43.1). What still reaches the growth and
//! the process end behind it is a death in memory no trace stamped, and the
//! reset's whole-block sentinel, which has no header of its own to carry a
//! mark; `PLAN.md` S43.5 has to answer both before it can delete the growth. A
//! thread exiting with its window still open ends the process for a reason of
//! its own ([`dispose_thread_state`]).
//!
//! The block leaving the append position is charged whole. The block still
//! under the cursor is a documented residue: it never stands in the current
//! figure, and the close enters it in the high-water one together with the
//! trace arena's own residue, which is the instant the two are in use at once
//! (`crate::memory::gc_metadata`). The workspace's own region enters neither
//! figure, being memory the thread holds whether or not a collection is
//! running (`crate::cycle::arena::TraceScratchArena::residue`).

use std::cell::Cell;

use crate::cycle::records::{RecordChain, SEGMENT_HEADER_BYTES};
use crate::cycle::shadow::{self, Color};
use crate::memory::block_pool::{
    BLOCK_KIND_ENTITY, BLOCK_KIND_RETAINED, BLOCK_PAYLOAD, BlockHeader,
};
use crate::memory::gc_metadata;

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
    /// The records themselves, over the workspace's region and the blocks the
    /// growth attached past it.
    records: RecordChain<*mut u8>,
    /// Blocks of this chain drawn through the reserve allocation path, and
    /// therefore how many go back to the reserve rather than to the pool. Which
    /// ones is not recorded: a block is a block, and the count is what restores
    /// the reserve's size.
    from_reserve: Cell<usize>,
    /// Bytes of this chain already charged to the manager's ledger, so that the
    /// close discharges exactly what was charged.
    published: Cell<usize>,
}

const _: () = assert!(size_of::<DeferredReturnChain>() == 64);
const _: () = assert!(align_of::<DeferredReturnChain>() == 64);

/// Records the workspace's own region for withheld returns holds.
///
/// The capacity the Sage gate fixed, and the record count past which a
/// growth — and therefore a refusal that ends the process — becomes reachable
/// at all (`PLAN.md`, S36.11).
pub(crate) const RETURNS_BASE_RECORDS: usize = 1_024;

/// Bytes that region takes out of the workspace: the control line, the base
/// segment's header line, and the records behind it.
pub(crate) const RETURNS_BASE_BYTES: usize = size_of::<DeferredReturnChain>()
    + SEGMENT_HEADER_BYTES
    + RETURNS_BASE_RECORDS * size_of::<*mut u8>();

/// Records one block the growth attaches holds.
///
/// The block's payload less the segment's header line, in eight-byte records.
/// The figure is the candidate queue's overflow capacity for the same reason.
const RECORDS_PER_BLOCK: usize = (BLOCK_PAYLOAD - SEGMENT_HEADER_BYTES) / size_of::<*mut u8>();

const _: () =
    assert!(RECORDS_PER_BLOCK * size_of::<*mut u8>() + SEGMENT_HEADER_BYTES == BLOCK_PAYLOAD);

thread_local! {
    /// The control line of this thread's withheld returns while its trace
    /// window is open, and null otherwise. Non-owning: the region belongs to
    /// the workspace the [`ActiveTrace`]'s arena holds.
    static DEFERRED_RETURNS: Cell<*mut DeferredReturnChain> =
        const { Cell::new(std::ptr::null_mut()) };
}

/// Bytes of the segment the chain is filling that stand in the byte ledger.
///
/// The workspace's region enters neither figure, so a chain that has not grown
/// answers zero; a block the growth attached counts its header line and the
/// records written behind it
/// (`crate::memory::gc_metadata::GcMemoryStats::current_bytes_in_use`).
#[inline]
fn ledger_bytes(chain: &DeferredReturnChain) -> usize {
    if chain.records.appends_into_base() {
        return 0;
    }

    SEGMENT_HEADER_BYTES + chain.records.records_in_append_segment() * size_of::<*mut u8>()
}

/// One block for the chain, the ordinary pool first and the critical reserve
/// second, with the flag saying which allocation path answered. A null block
/// means both refused, and then the flag is meaningless.
fn draw_block() -> (*mut BlockHeader, bool) {
    let pooled = gc_metadata::acquire();
    if !pooled.is_null() {
        return (pooled, false);
    }

    (gc_metadata::adopt(crate::memory::critical::draw()), true)
}

/// The chain a trace's withheld returns are written into, and the owner that
/// gives its blocks back.
///
/// A holder of its own rather than a field the enclosing drop unwinds by hand:
/// `BlockPool::put` panics on a poisoned mutex, and a release left to the end
/// of [`ActiveTrace::drop`] would be skipped by an unwind out of the replay,
/// stranding 64 KiB blocks and their ledger entry for the life of the process.
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
            (&raw mut (*control).from_reserve).write(Cell::new(0));
            (&raw mut (*control).published).write(Cell::new(0));
        }

        Self { control }
    }

    fn chain(&self) -> &DeferredReturnChain {
        unsafe { &*self.control }
    }

    /// Bytes of the segment under the cursor, which no growth has charged.
    fn residue(&self) -> usize {
        ledger_bytes(self.chain())
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
}

impl Drop for WithheldReturns {
    /// Give back every block the growth attached, what the reserve lent
    /// through the reserve allocation path and the rest to the pool. The
    /// workspace's own region is the arena's and stays where it is.
    ///
    /// The reserve is served first for the reason the trace arena serves it
    /// first: the retry that follows an abort wants an allocation path that
    /// serves.
    fn drop(&mut self) {
        self.close_window();

        let chain = self.chain();
        gc_metadata::discharge(chain.published.replace(0));
        let mut owed_to_reserve = chain.from_reserve.replace(0);

        chain.records.take_segments_past_base(|segment| {
            let block = BlockHeader::of_ptr(segment);
            if owed_to_reserve > 0 {
                owed_to_reserve -= 1;
                gc_metadata::release_to_critical(block);
            } else {
                gc_metadata::release(block);
            }
        });
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
    /// module owns, and it reads the arena's residue beforehand to enter it in
    /// the ledger beside its own — an arena reset by its caller enters that
    /// residue separately and loses the one instant the two stand together.
    pub(crate) fn arena(&mut self) -> &mut crate::cycle::arena::TraceScratchArena {
        &mut self.arena
    }
}

impl Drop for ActiveTrace {
    fn drop(&mut self) {
        // First, and before the rows die: a batch still here was disposed of by
        // nothing, so every root in it keeps its registration and its records go
        // back to the lane they came out of. It depends on neither the sweep nor
        // the replay, and `ll_free`'s candidate arm reads the entity's bit rather
        // than the lane its record stands in.
        if let Some(batch) = self.batch.take() {
            crate::cycle::queue::restore_candidates(batch);
        }

        // Both of this collection's residues stand together here and nowhere
        // else: the arena's reset enters its own alone, and the high-water
        // figure takes the larger of two marks rather than their sum, so a
        // mark left to that call would miss a collection that held rows and
        // withheld returns at once.
        gc_metadata::mark_peak(self.arena.residue() + self.returns.residue());

        // First and unconditionally: after the window falls, a physical
        // return may recommission the block whose shadow pointer this sweep
        // must null.
        self.arena.reset();

        self.returns.close_window();
        self.returns.replay();
    }
}

/// Move the append position into a block of its own, or end the process.
///
/// The open trace's rows still address every slot the caller is holding, so
/// returning this one is the reuse the window exists to prevent; dropping the
/// record loses a physical return, which is refused (`dev/DECISIONS.md`, "an
/// enrolment cannot fail"). Nothing can report it either: `ll_free` holds no
/// frame that can fail. **S43.5 deletes this function**, and it owes two cases
/// first: a death past a full region in memory no trace stamped, where a mark
/// is one no sweep walks to, and the reset's whole-block sentinel, which
/// addresses a block header and so has no entity header to mark.
#[cold]
fn grow(chain: &DeferredReturnChain) {
    let (block, from_reserve) = draw_block();
    if block.is_null() {
        std::process::abort();
    }

    // The segment the cursor is leaving is full by construction, so what it
    // holds is exact here — nothing for the workspace's region, and a whole
    // payload for a block. Charged after both allocation paths have answered:
    // a refusal leaves the chain where it stands.
    let filled = ledger_bytes(chain);
    chain.published.set(chain.published.get() + filled);
    gc_metadata::charge(filled);

    unsafe {
        chain
            .records
            .extend(BlockHeader::payload_start(block), RECORDS_PER_BLOCK)
    };

    if from_reserve {
        chain.from_reserve.set(chain.from_reserve.get() + 1);
    }
}

/// Refuse a physical return while the current trace can still address
/// the slot, recording the return for the window's close.
///
/// Called only after the queue-entry window has refused the same return. A
/// replay that still finds `CANDIDATE_BIT` stops before here, because the
/// queue entry itself remains the record.
///
/// With no window open the whole cost is one thread-local load and one branch.
/// With one open and room in the append segment: three loads — the thread-local
/// control line, the cursor and the limit — two branches and two stores, with
/// no atomic, no allocator call and no pool call. The pool is asked in
/// [`grow`] alone, and the second push below it runs on that path only.
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
    if !chain.records.push(ptr) {
        unsafe { withhold_without_a_record(chain, ptr, kind) };
    }

    true
}

/// Whether a mark in the entity's own header is one a sweep will reach,
/// which is what separates the deaths this module answers with a mark from
/// the deaths it still records.
///
/// The stamp is the half every population shares: rows over the memory mean
/// a collection is walking it, and the memory a collection walks is the
/// memory a sweep of its touched list can reach — which is what `PLAN.md`
/// S43.4 builds, the walk today making one store per touched block and
/// reading no header
/// ([`crate::cycle::arena::TraceScratchArena::clear_touched_rows`]). Where
/// that stamp is written, and what else has to hold, differs by population:
///
/// - **an entity slot** is asked whose block it is as well. The return is
///   the owner's — its free list and its `used` — so a slot marked in a
///   block this thread does not own waits for a sweep this thread never
///   runs;
/// - **a retained survivor** asks the stamp alone, its return being an
///   atomic decrement any thread may perform
///   ([`crate::memory::retained::occupant_freed`]). The reset's whole-block
///   sentinel is refused here instead: it addresses the block header rather
///   than an entity, so there is no header of its own to mark;
/// - **a large entity**, pooled or OS-direct, carries its one row in its own
///   block header, so that row's colour is the stamp
///   ([`crate::memory::large_entity::shadow_row`]). Its header is not a
///   `HeapBlockHeader`, and neither the shadow pointer nor the owner word
///   an entity block carries exists at those offsets to be read.
///
/// Why the retained and large populations are not asked for an owner, and
/// what that leaves open: `dev/DECISIONS.md`, "the stamp is the whole
/// condition where the return is not the owner's".
///
/// # Safety
/// As [`defer_reuse_if_tracing`].
unsafe fn can_carry_the_mark(ptr: *mut u8, kind: u32) -> bool {
    let block = BlockHeader::of_ptr(ptr) as *mut u8;

    // Asked rather than listed, because the two large kinds grow together or
    // not at all (`crate::memory::large_entity::is_large_entity`).
    if crate::memory::large_entity::is_large_entity(kind) {
        let row = unsafe { *crate::memory::large_entity::shadow_row(block) };
        return shadow::color(row) != Color::Untouched;
    }

    // A kind with no arm refuses rather than falls through: the set that
    // reaches here is `stdapi::can_lose_trace_identity`'s, and a kind added
    // there without an arm here would otherwise be marked on the strength of
    // a shadow pointer read at an offset that may be another module's.
    match kind {
        BLOCK_KIND_ENTITY => {
            !unsafe { crate::memory::heap::block_shadow(block) }.is_null()
                && unsafe { crate::memory::heap::block_is_owned_by_this_thread(block) }
        }
        BLOCK_KIND_RETAINED => {
            ptr != block && !unsafe { crate::memory::heap::block_shadow(block) }.is_null()
        }
        _ => false,
    }
}

/// Withhold a return the region has no room to record.
///
/// A death [`can_carry_the_mark`] admits carries the fact in the entity's own
/// header ([`crate::refcount::mark_dead_in_place`]), and what will find it is
/// the sweep `PLAN.md` S43.4 builds over the memory the same trace stamped.
/// Nothing is drawn and nothing can refuse, which is what takes the process
/// end off that path (`PLAN.md` S43.2, S43.3). Every other death still takes a record, and what is left
/// there is a death in memory no trace stamped, plus the reset's whole-block
/// sentinel — the two `PLAN.md` S43.5 has to answer before it can delete the
/// growth.
///
/// A marked slot stays out of the allocator's hands the way a recorded one
/// does — it is on no free list and below its block's bump cursor — and a
/// marked survivor keeps its block's occupant count above zero, so the block
/// is not the pool's either. What returns them is `PLAN.md` S43.4's sweep,
/// which is unbuilt: until it lands a mark holds the memory for the life of
/// the process.
///
/// # Safety
/// As [`defer_reuse_if_tracing`], whose refused push this answers.
#[cold]
unsafe fn withhold_without_a_record(chain: &DeferredReturnChain, ptr: *mut u8, kind: u32) {
    if unsafe { can_carry_the_mark(ptr, kind) } {
        unsafe {
            crate::refcount::mark_dead_in_place(ptr as *mut crate::refcount::RcHeader);
        }

        return;
    }

    grow(chain);
    if !chain.records.push(ptr) {
        // The same last resort [`grow`] reaches, and by the same call rather
        // than by a panic: a record this call drops is a physical return
        // lost, and `ll_free`'s caller is `extern "C"`, so an unwind out of
        // here is outside the protocol on every profile that unwinds. The
        // segment a growth opens is empty and holds [`RECORDS_PER_BLOCK`], so
        // only a defect in the chain gets here.
        std::process::abort();
    }
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
