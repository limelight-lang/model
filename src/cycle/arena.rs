//! The collection's working memory: a bump arena that opens over the thread's
//! own workspace and grows into 64 KiB blocks taken for one collection.
//!
//! **The first block is the thread's, and the arena borrows it.** The
//! workspace is drawn at the thread's first collection, through the ordinary
//! allocation path alone, and held until the thread exits, so a collection's
//! close rewinds the bump over it rather than giving it back and every
//! collection after the first opens on memory already in hand
//! ([`crate::cycle::queue::lend_workspace_base`]; `dev/DECISIONS.md`, "the
//! workspace base is drawn at the first collection, not at thread init").
//!
//! **Its first [`WORKSPACE_PREFIX_BYTES`] bytes are two fixed regions rather
//! than bump**, and the bump opens behind them. The first is one line, the
//! control line of the withheld returns
//! ([`crate::cycle::deferred_slot_reuse`]), whose stack is threaded through
//! the dying entities themselves — which is what lets an ordinary collection
//! withhold every return it has without asking the memory manager for
//! anything. The second is the member list a pressure collection harvests into
//! ([`crate::cycle::members`]), which is read after the arena has given every
//! block back. The arena holds neither, only the addresses they are opened
//! over: both outlive its reset, one dying with the collection and the other
//! with the teardown that reads it.
//!
//! The trace's worklist ([`crate::cycle::stack`]) has no region and takes its
//! segments from the bump, one at the first push. The arena holds it for the
//! reason it holds the bump: the memory is one block, and its reset is one
//! call.
//!
//! **Growth past the workspace has two allocation paths, in this order: the
//! ordinary block pool, then the thread's critical reserve**
//! (`rfc/model/memory/critical-reserve.md`,
//! "Reserve users"). The in-line collection is the standard form rather than
//! the emergency one, so most runs begin with no refusal anywhere and a full
//! trace's rows are far beyond any reserve; the reserve allocation path is the
//! fallback, and on the pressure path of Y14 it is the first draw, the pool's
//! refusal being what triggered the collection.
//!
//! **A refusal on both allocation paths aborts the collection, and never the
//! process.** That is why the memory is asked for a block at a time
//! through a call that can answer null, rather than reserved as a
//! mapping materialised page by page: a page that fails to materialise
//! reports nothing a caller can catch, and the release profile is built
//! `panic = "abort"` (`rfc/model/gc/rc-cycle.md`, "Where the shadow
//! count lives"; `dev/DECISIONS.md`, "the shadow arena asks the pool
//! first and the critical reserve second, and the virtual reservation
//! goes").
//!
//! # What the arena owes back
//!
//! Every block it drew, at the end of the collection and on the abort path
//! alike, and what the reserve allocation path lent goes back to the reserve
//! before the pool sees a block — the retry that follows an abort wants an
//! allocation path that serves. The workspace is not among them: it goes back
//! to the thread when the arena drops, and to the memory manager only at
//! thread exit.
//!
//! **The shadow-row pointers are nulled earlier than that, and the
//! instant is fixed by the design rather than by convenience.**
//! [`TraceScratchArena::clear_touched_rows`] runs at the end of scan, where the
//! trace token is released and where the last touch of any shadow row
//! has already happened. Everything after that store runs untokened, and
//! the slot returns are among it — so a block may reach the pool and be
//! recommissioned while this collection's teardown is still running, and
//! a sweep left until then would write into another collection's header
//! word (`rfc/model/gc/rc-cycle.md`, "Concurrency" and "Death while
//! enrolled"). [`TraceScratchArena::reset`] sweeps too, and that is the abort
//! path: an abort can only be raised where memory is asked for, which is
//! inside mark and scan, so an aborting collection has not reached the
//! release instant.
//!
//! # What it does not hold
//!
//! A `Vec`, a `HashMap`, or anything else that reaches the global allocator.
//! All three of the arena's own lists live in its own memory: the blocks
//! thread through their headers, the touched list threads through the row
//! arrays themselves, and the worklist's segments thread through their own
//! headers in the bump. A collection that grew a `Vec` would allocate through the very
//! allocation path that has already refused, and an allocation failure inside
//! `Vec` aborts the process (`rfc/model/gc/cycle/questions.md`, Y14, "Its
//! working memory must be sized before it is needed").
//!
//! # Enrolment cannot fail after the rows exist
//!
//! A block's rows and its entry in the touched list are **one
//! allocation**: the entry is a 24-byte prologue on the row array
//! (`crate::cycle::shadow`). One refusal point serves both, and it
//! stands before either exists, so the state the sweep exists to undo —
//! a block stamped with rows the abort has given back — cannot be
//! reached. The recorded alternative is a segment chain of its own beside
//! the arrays, 512 entries to a segment: it allocates a second time, and that allocation's
//! refusal arrives after the stamp, which is the state above; it also
//! costs 4 KiB at the first touched block against the prologue's 24
//! bytes.
//!
//! A large entity is the one population with no array, its single row
//! being a word of its own block header, and it takes a prologue with no
//! rows behind it for the sake of the sweep. There the refusal is kept
//! harmless by an ordering instead: the row is written only after the
//! enrolment is in hand, so a refused enrolment leaves the row at zero
//! ([`TraceScratchArena::ensure_row`]).

use crate::cycle::deferred_slot_reuse::RETURNS_BASE_BYTES;
use crate::cycle::members::MEMBERS_BASE_BYTES;
use crate::cycle::row::{Population, RowKey};
use crate::cycle::shadow::{self, Color, RowArray};
use crate::cycle::stack::{SEGMENT_BYTES, TraceStack, WorklistEntry};
#[cfg(test)]
use crate::memory::block_pool::BlockPool;
use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockHeader};
use crate::memory::gc_metadata;

/// What one meeting of an entity answers: its row, or the two reasons
/// there is none.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RowLookup {
    /// The entity's row, met — its colour is not
    /// [`Color::Untouched`] and its working count is the refcount less
    /// whatever the trace has already subtracted. The caller writes
    /// through it.
    ///
    /// **`first_visit` is the answer to "had this collection seen the
    /// entity before?"**, and it is carried out of the meeting because
    /// the meeting is what destroys it: after the call the row's colour
    /// says met whichever reach this was. The mark's descent turns on
    /// it — an edge into an entity already expanded takes the
    /// decrement and stops, and a trace that expanded again on every
    /// in-edge would not terminate on a ring
    /// (`rfc/model/gc/rc-cycle.md`, "Removed full-census structures").
    Ready { row: *mut u32, first_visit: bool },
    /// The block cannot place this index, which is a retained block
    /// whose survivor list does not name the entity. The caller counts
    /// the edge as an external live reference, the same answer
    /// `row::resolve_edge_target` gives an address it cannot place.
    Untracked,
    /// Both allocation paths refused. The caller aborts the collection,
    /// which costs nothing beyond the work already done: the trace
    /// writes into no entity.
    AllocationFailed,
}

/// The thread's workspace for as long as one arena holds it, given back to
/// the thread when this drops.
///
/// A holder of its own rather than a raw field the enclosing drop returns by
/// hand: [`TraceScratchArena::reset`] reaches `BlockPool::put`, which panics on
/// a poisoned mutex, and a return written after that call in
/// [`TraceScratchArena::drop`] would be skipped by the unwind. The cell would
/// then stay lent until thread exit, where
/// [`crate::cycle::queue::release_queue_base`] fails its assertion inside
/// `heap::ll_thread_exit` — an `extern "C"` frame, so the panic aborts the
/// process whether or not anything is unwinding, and the report is a signal
/// rather than a named test. [`crate::cycle::deferred_slot_reuse`]'s control
/// line is held this way for the same reason.
///
/// **What it converts is one path.** A lent cell that reaches thread exit by
/// any other route — an arena that is forgotten rather than dropped — still
/// aborts there, because the frame that raises is the one that cannot unwind.
struct LentWorkspace {
    block: *mut BlockHeader,
}

impl LentWorkspace {
    /// The block, never null for as long as the holder exists.
    fn block(&self) -> *mut BlockHeader {
        self.block
    }
}

impl Drop for LentWorkspace {
    fn drop(&mut self) {
        crate::cycle::queue::return_workspace_base(self.block);
    }
}

/// Bytes at the head of the workspace its two fixed regions take, before the
/// bump opens: the withheld returns' control line, then the member list.
pub(crate) const WORKSPACE_PREFIX_BYTES: usize = RETURNS_BASE_BYTES + MEMBERS_BASE_BYTES;

/// Bytes of the workspace the bump may grant.
pub(crate) const WORKSPACE_BUMP_BYTES: usize = BLOCK_PAYLOAD - WORKSPACE_PREFIX_BYTES;

// What the prefix costs the bump, pinned: two regions, and everything else of
// the block grantable. The first assertion is the prefix's own and fires when
// it grows again; the second is the pair of it and the payload, and fires on
// either, so a reading of it names both.
const _: () = assert!(WORKSPACE_PREFIX_BYTES == 8_320);
const _: () = assert!(WORKSPACE_BUMP_BYTES == 56_960);

// What the bump keeps for rows, stated where the prefix is chosen: three of
// the widest arrays a block can need, which is what a trace over three
// smallest-class blocks reserves. A fourth has never fitted the workspace,
// prefix or none (`crate::cycle::shadow::bytes_for`).
const _: () = assert!(3 * 16_408 < WORKSPACE_BUMP_BYTES);

// The regions begin on a line, which their control lines need to be aligned at
// all: a payload starts `LINE_SIZE` into a block the pool aligns to
// `BLOCK_SIZE`, so an offset that is a multiple of 64 is 64-aligned.
const _: () = assert!(RETURNS_BASE_BYTES % 64 == 0);
const _: () = assert!(MEMBERS_BASE_BYTES % 64 == 0);
const _: () = assert!(crate::memory::block_pool::LINE_SIZE % 64 == 0);
const _: () = assert!(crate::memory::block_pool::BLOCK_SIZE % 64 == 0);

// Whether the next reset on this thread raises where the hand-back does.
//
// Fault injection, tests only. The reset's own panic sites are an underflowed
// ledger and a poisoned pool mutex, and a test can produce neither without
// taking every test after it down with the poison — so the case that reads a
// marked slot back after such a panic has no other way of being staged
// (`crate::cycle::deferred_slot_reuse::ActiveTrace`, the close's order).
#[cfg(test)]
thread_local! {
    static PANIC_IN_RESET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Arm the injection for **one** reset of this thread, and disarm it when this
/// guard dies — including on the unwind the injected panic itself raises, so
/// nothing of it reaches the next test on the thread.
///
/// The reset it interrupts is left half-done, which is the state a poisoned
/// hand-back leaves: the rows are swept and the blocks are still the arena's.
/// [`TraceScratchArena::drop`] runs the reset again and gives them back.
#[cfg(test)]
pub(crate) struct InjectedResetFailure;

#[cfg(test)]
impl InjectedResetFailure {
    pub(crate) fn arm() -> Self {
        PANIC_IN_RESET.with(|armed| armed.set(true));
        Self
    }
}

#[cfg(test)]
impl Drop for InjectedResetFailure {
    fn drop(&mut self) {
        PANIC_IN_RESET.with(|armed| armed.set(false));
    }
}

/// Raise the armed failure and disarm it, and do nothing at all without
/// `cfg(test)`.
#[inline]
fn fire_injected_reset_failure() {
    #[cfg(test)]
    if PANIC_IN_RESET.with(|armed| armed.replace(false)) {
        panic!("the injected reset failure");
    }
}

/// One collection's memory: the thread's workspace for as long as the arena
/// lives, and the blocks the bump grew into past it, which
/// [`TraceScratchArena::reset`] returns.
pub(crate) struct TraceScratchArena {
    /// The thread's workspace. Outside both `blocks` and `from_reserve`: it is
    /// not this collection's to return, and the reserve never funded it.
    base: LentWorkspace,
    /// Blocks the bump took above the workspace, threaded through
    /// `BlockHeader::next`, newest first.
    blocks: *mut BlockHeader,
    /// How many of them came through the reserve allocation path, and therefore
    /// how many go back to the reserve rather than to the pool. The
    /// arena does not record *which*: a block is a block, and the count
    /// is what restores the reserve's size.
    from_reserve: usize,
    /// The bump cursor into the newest block, and the bytes left in it.
    cursor: *mut u8,
    left: usize,
    /// Bytes the bump may grant out of the block under the cursor: the whole
    /// payload of a drawn block, and [`WORKSPACE_BUMP_BYTES`] of the
    /// workspace, whose head the fixed region holds.
    open_capacity: usize,
    /// Newest array of the touched list, or null while no block has
    /// been touched.
    touched: *mut RowArray,
    /// The trace's worklist, whose segments are this bump's
    /// ([`crate::cycle::stack`]).
    worklist: TraceStack,
    /// Bytes of this arena's bump already charged to the manager's
    /// ledger, so that [`reset`](TraceScratchArena::reset) discharges exactly
    /// what was charged and a re-entered reset discharges nothing.
    published: usize,
}

impl TraceScratchArena {
    /// An arena over this thread's workspace, or **`None` when the thread has
    /// no workspace to give**: the pool refused the draw, or the runtime never
    /// registered the thread. Either is a collection that does not start — no
    /// window is open, no root has been taken, and the caller's abort path has
    /// nothing to undo.
    ///
    /// The workspace is drawn at the thread's first collection and held until
    /// the thread exits, so this call goes to the memory manager once in a
    /// thread's life and every later collection opens on memory already in
    /// hand ([`crate::cycle::queue::lend_workspace_base`]). A thread bumps one
    /// workspace at a time, and a second arena opened over a live one ends the
    /// process rather than granting the same bytes twice.
    pub(crate) fn open() -> Option<Self> {
        let base = crate::cycle::queue::lend_workspace_base();
        if base.is_null() {
            return None;
        }

        let payload = BlockHeader::payload_start(base);
        Some(Self {
            base: LentWorkspace { block: base },
            blocks: std::ptr::null_mut(),
            from_reserve: 0,
            cursor: unsafe { payload.add(WORKSPACE_PREFIX_BYTES) },
            left: WORKSPACE_BUMP_BYTES,
            open_capacity: WORKSPACE_BUMP_BYTES,
            touched: std::ptr::null_mut(),
            worklist: TraceStack::new(),
            published: 0,
        })
    }

    /// Bytes of the block under the bump, which no [`grow`](Self::grow) has
    /// charged and which [`reset`](Self::reset) enters in the high-water
    /// figure. Zero on a rewound arena, which is the state a re-entered reset
    /// finds.
    ///
    /// Measured against what the bump may grant rather than against the whole
    /// payload: the workspace's fixed region is memory the thread holds
    /// whether or not a collection is running, and counting it here would
    /// charge every collection for it again.
    pub(crate) fn residue(&self) -> usize {
        self.open_capacity - self.left
    }

    /// The workspace region a trace window opens its control line over:
    /// [`RETURNS_BASE_BYTES`] bytes at the head of the payload.
    ///
    /// The window is not this arena's: its stack is popped after
    /// [`sweep_rows`](Self::sweep_rows) and before [`reset`](Self::reset), so
    /// what the arena guarantees is the region's address and that the
    /// workspace stays lent until the arena drops — which is after the window
    /// has died (`crate::cycle::deferred_slot_reuse::ActiveTrace`).
    pub(crate) fn withheld_returns_region(&self) -> *mut u8 {
        BlockHeader::payload_start(self.base.block())
    }

    /// The workspace region a harvest writes its members into:
    /// [`MEMBERS_BASE_BYTES`] bytes behind the withheld returns' control line.
    ///
    /// The list is not this arena's either, and it outlives it the other way
    /// round: the sweep fills it while the arena still stands, and the driver
    /// reads it after every block has gone back
    /// (`crate::cycle::members::StandingMembers`). What the arena guarantees
    /// is the address, and that its own bump never grants these bytes.
    pub(crate) fn member_region(&self) -> *mut u8 {
        unsafe { BlockHeader::payload_start(self.base.block()).add(RETURNS_BASE_BYTES) }
    }

    /// Charge `bytes` of this arena's bump as memory in use, and remember
    /// them for the discharge.
    fn publish(&mut self, bytes: usize) {
        self.published += bytes;
        gc_metadata::charge(bytes);
    }

    /// `bytes` of 8-aligned scratch, or **null when both allocation paths have
    /// refused**, which is the caller's signal to abort the collection.
    ///
    /// A request larger than one block payload is refused outright: no
    /// allocation this arena serves comes near it — a smallest-class
    /// block's array is 16 408 bytes, the largest any population asks
    /// for — and a run of blocks would give the abort path a second
    /// shape to return.
    ///
    /// The memory is **not zeroed**. A row array arrives dirty by design,
    /// its row-initialization bitmap being what says which rows have meaning
    /// (`rfc/model/gc/rc-cycle.md`, "Rows are initialized lazily.").
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

    /// The shadow row of one entity the trace has just reached, **met**:
    /// initialised from `refcount` on this collection's first reach of it
    /// and left as it stands on every later one, so a second edge into
    /// the same entity subtracts from the count rather than restoring it.
    ///
    /// `row` comes from
    /// [`row::resolve_edge_target`](crate::cycle::row::resolve_edge_target) and
    /// `refcount` is the entity's, read by the caller: this module places rows
    /// and knows nothing about entity headers.
    ///
    /// **Change this, change [`find_initialized_row`] too:** the two find the
    /// same row in the same two places, and only this one may create it.
    ///
    /// The block's rows are reserved here, at its first touch, and the
    /// block enrolled for the sweep with them — one allocation, so a
    /// refusal cannot land between the two (module doc). What it does not
    /// do is zero the rows: only the touched group is written, and the
    /// group bitmap is what says which groups those are
    /// (`crate::cycle::shadow`).
    ///
    /// **A large entity is safe by an ordering rather than by that
    /// structure**, its row being a block header word that exists from
    /// the block's commissioning: the colour is tested, then the
    /// enrolment allocated, then the colour written, so a refusal leaves
    /// the row at zero and an unenrolled block is also an unwritten one.
    /// An edit that wrote the row before enrolling would leave the next
    /// collection reading this one's count.
    ///
    /// # Safety
    /// `row` must name a live entity of the collected heap, resolved
    /// from its own address by `resolve_edge_target`, and its block must stay
    /// mapped until [`clear_touched_rows`](Self::clear_touched_rows) runs —
    /// which it does, a trace in flight being what keeps a block from reaching
    /// the pool (`rfc/model/gc/rc-cycle.md`, "Zero-count entities pending slot reuse").
    pub(crate) unsafe fn ensure_row(&mut self, row: RowKey, refcount: u32) -> RowLookup {
        let block = row.block as *mut u8;
        let word = if row.population == Population::SingleEntity {
            // The one population whose row is a block header word rather
            // than an array, so its enrolment has no allocation to ride
            // on and takes a prologue of its own. The row's own colour is
            // what says whether that has happened: nothing but a meeting
            // writes it, and the tail below writes it before returning.
            debug_assert_eq!(
                row.index,
                crate::cycle::row::SINGLE_ENTITY_INDEX,
                "a large entity's block holds one row and this names another"
            );
            let word = unsafe { crate::memory::large_entity::shadow_row(block) };
            if shadow::color(unsafe { *word }) == Color::Untouched
                && self
                    .allocate_and_attach_row_array(block, 0, Population::SingleEntity)
                    .is_null()
            {
                return RowLookup::AllocationFailed;
            }

            word
        } else {
            let mut array = unsafe { crate::memory::heap::block_shadow(block) } as *mut RowArray;
            if array.is_null() {
                let Some(row_count) = (unsafe { index_space(row) }) else {
                    return RowLookup::Untracked;
                };

                array = self.allocate_and_attach_row_array(block, row_count, row.population);
                if array.is_null() {
                    return RowLookup::AllocationFailed;
                }

                // After the enrolment and never before: this store cannot
                // fail and the one above can, so the other order stamps a
                // block the abort would then leave pointing at memory it
                // has given back (module doc).
                unsafe { crate::memory::heap::set_block_shadow(block, array as *mut u8) };
            }

            if row.index >= unsafe { (*array).row_count } {
                // A retained block whose survivor list has been rebuilt
                // under this trace is the only way here, and the trace
                // token forbids it. Conservative rather than fatal all
                // the same: an edge with no row keeps its referent alive.
                debug_assert!(false, "row {} is past the block's index space", row.index);
                return RowLookup::Untracked;
            }

            unsafe { shadow::ensure_group_initialized(array, row.index) };
            unsafe { shadow::row(array, row.index) }
        };

        let first_visit = shadow::color(unsafe { *word }) == Color::Untouched;
        if first_visit {
            unsafe { word.write(shadow::compose(Color::Unclassified, refcount)) };
        }

        RowLookup::Ready {
            row: word,
            first_visit,
        }
    }

    /// Reserve `row_count` rows for `block` and enrol it for the sweep, or
    /// null when both allocation paths have refused.
    ///
    /// The array is linked into the touched list here, which is the
    /// enrolment: it is the same memory, so the two cannot come apart.
    /// `row_count` is zero for a large entity, whose row is elsewhere and
    /// whose prologue is enrolment alone.
    fn allocate_and_attach_row_array(
        &mut self,
        block: *mut u8,
        row_count: u32,
        population: Population,
    ) -> *mut RowArray {
        let array = self.alloc(shadow::bytes_for(row_count)) as *mut RowArray;
        if array.is_null() {
            return array;
        }

        unsafe { shadow::init(array, block, row_count, population, self.touched) };
        self.touched = array;
        array
    }

    /// End this collection's claim on the heap and give no block back: drain
    /// the worklist, then null the shadow-row pointer of every block the
    /// collection stamped.
    ///
    /// **This is what a withheld return waits for**, and the whole of it. A
    /// slot handed back while a row still names it is one the allocator can
    /// give to an occupant that inherits the row, which is the reuse the trace
    /// window exists to prevent; the blocks the arena itself holds name no
    /// slot and go back after the returns
    /// (`crate::cycle::deferred_slot_reuse::ActiveTrace`).
    ///
    /// Idempotent, over a rewound worklist and an emptied touched list.
    pub(crate) fn sweep_rows(&mut self) {
        // Ahead of the sweep, which owes an empty worklist: an abort is raised
        // with entities still queued, and every one of them carries a row
        // pointer into an array this call is about to unstamp.
        self.worklist.rewind();
        self.clear_touched_rows();
    }

    /// End the collection's hold on memory: give every block back, the
    /// reserve first, having swept anything
    /// [`clear_touched_rows`](Self::clear_touched_rows) has not.
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
    ///
    /// **Change this, change the worklist too:** a
    /// [`TraceStack`](crate::cycle::stack::TraceStack) that drew
    /// segments from this arena names memory the pool has taken back
    /// from the moment this returns, and its own `reset` is what says
    /// so.
    ///
    /// **A withheld return waits for [`sweep_rows`](Self::sweep_rows) and not
    /// for this call**, which is the split
    /// `crate::cycle::deferred_slot_reuse::ActiveTrace`'s drop makes: what a
    /// return may not outrun is the unstamping, and the blocks below are the
    /// arena's own.
    pub(crate) fn reset(&mut self) {
        // Kept here although the ordered close has swept already, because the
        // other caller is [`Drop`] and it has not: an unwind reaches this from
        // anywhere in a collection. Over a rewound worklist and an emptied
        // touched list the second pass reads two null heads and stores
        // nothing.
        self.sweep_rows();

        // The block still under the bump has no further grant coming, and it
        // is being released in the same breath — rewound if it is the
        // workspace, handed over below if it is not — so its consumption goes
        // into the high-water figure alone rather than through a charge whose
        // discharge follows it (`memory::gc_metadata::mark_peak`). The blocks
        // before it were charged as `grow` left each of them.
        gc_metadata::mark_peak(self.residue());

        gc_metadata::discharge(self.published);
        self.published = 0;

        // What the reserve lent goes back to the reserve, and the rest
        // to the pool. Returning everything through the reserve allocation path
        // would refill it out of ordinary memory a collection happened
        // to be holding, which is the safepoint's job and not this one;
        // returning everything to the pool would leave the reserve empty
        // for the retry that follows an abort, and the retry is why the
        // ordering exists at all.
        // The arena's own state moves ahead of each hand-over rather
        // than after the loop: `BlockPool::put` takes a mutex, and a
        // thread that unwinds out of a poisoned one leaves `Drop` to run
        // `reset` again — over a list whose head was already returned. A
        // rewound bump reads a residue of zero, which is what that second pass
        // needs to see.
        self.cursor =
            unsafe { BlockHeader::payload_start(self.base.block()).add(WORKSPACE_PREFIX_BYTES) };
        self.left = WORKSPACE_BUMP_BYTES;
        self.open_capacity = WORKSPACE_BUMP_BYTES;

        fire_injected_reset_failure();

        while !self.blocks.is_null() {
            let block = self.blocks;
            self.blocks = unsafe { (*block).next };
            if self.from_reserve > 0 {
                self.from_reserve -= 1;
                gc_metadata::release_to_critical(block);
            } else {
                gc_metadata::release(block);
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
    pub(crate) fn clear_touched_rows(&mut self) {
        // The ordered close owes an empty worklist: every entry carries a row
        // pointer into an array this call unstamps, and an entry surviving the
        // sweep would name a row the next collection is granting. Checked and
        // not enforced, and by a `debug_assert` — this module answers a
        // refusal rather than ending the process, and the release build of a
        // caller that got the order wrong loses nothing here for as long as it
        // pops nothing afterwards, the rows staying where they are until
        // [`reset`](Self::reset) rewinds over them.
        debug_assert!(
            self.worklist.is_empty(),
            "the worklist is drained before the rows it points into are unstamped"
        );

        let mut array = self.touched;
        // Emptied first: the walk below runs to the end of the chain,
        // and a second call must find nothing rather than repeat it.
        self.touched = std::ptr::null_mut();

        // Read once for the whole sweep rather than per block: a harvest is
        // armed before the sweep and released after the teardown, so nothing
        // can arm one between two blocks of this walk
        // (`crate::cycle::members`).
        let harvesting = crate::cycle::members::is_armed();
        // Falls the moment the region refuses a record, and the rest of the
        // walk is the null-only sweep an ordinary collection makes: what an
        // overflowed harvest owes is nothing, and reading further rows would
        // pay for records the driver is going to discard.
        let mut taking = harvesting;

        while !array.is_null() {
            let block = unsafe { (*array).block };
            if taking {
                taking = unsafe { Self::harvest_rows(array, block) };
            }

            match unsafe { (*array).population } {
                // The large entity's row is the block's own header word,
                // so what a stale one costs is not a wild pointer but a
                // count: the next collection would read the entity as met
                // and subtract from a working count this one left behind.
                Population::SingleEntity => unsafe {
                    crate::memory::large_entity::shadow_row(block)
                        .write(shadow::compose(Color::Untouched, 0))
                },
                // Listed rather than a wildcard: a fourth population
                // would otherwise be swept as though its rows hung off
                // the collector line, which is a store into a header word
                // that may be another module's.
                Population::Slotted | Population::Retained => unsafe {
                    crate::memory::heap::clear_block_shadow(block)
                },
            }

            array = unsafe { (*array).next };
        }

        if harvesting {
            crate::cycle::members::end_harvest();
        }
    }

    /// Write every entity of `block` the scan left unreachable into the armed
    /// member list, and answer **false where the list refused one**, which
    /// ends the harvest of the whole sweep rather than of this block.
    ///
    /// The rows are the collection's own memory and stay readable after the
    /// block's shadow pointer is nulled, so the two are in either order; this
    /// one reads the rows first, while the block's header line is still cold
    /// for the store that follows.
    ///
    /// A retained block whose list does not name the row's position is passed
    /// over rather than harvested: the disagreement between the two is
    /// `crate::cycle::row::entity_at`'s to report, and an entity left out of
    /// the list keeps its candidate bit and dies at a later collection.
    ///
    /// # Safety
    /// `array` is an initialised array of a scanned collection, `block` is the
    /// block it was written for, and a harvest is armed on this thread.
    unsafe fn harvest_rows(array: *mut RowArray, block: *mut u8) -> bool {
        let population = unsafe { (*array).population };
        let take =
            |index: u32| match unsafe { crate::cycle::row::entity_at(block, population, index) } {
                Some(entity) => crate::cycle::members::push(entity),
                None => true,
            };

        // The one population whose row is not in the array: a large entity's
        // colour is a word of its own block header, and its array carries the
        // prologue alone (`crate::cycle::shadow::RowArray`).
        if population == Population::SingleEntity {
            let row = unsafe { *crate::memory::large_entity::shadow_row(block) };
            if shadow::color(row) != Color::PotentiallyUnreachable {
                return true;
            }

            return take(crate::cycle::row::SINGLE_ENTITY_INDEX);
        }

        unsafe { shadow::for_each_unreachable(array, take) }
    }

    /// Queue `entry` for expansion, or answer **false** when both allocation
    /// paths refused a worklist segment — which is the caller's signal to
    /// abort the collection, and the only way this can fail.
    ///
    /// The worklist takes [`SEGMENT_BYTES`] of this bump at its first push and
    /// at every boundary the depth crosses after it, so a worklist is refused
    /// exactly where a row array would be.
    pub(crate) fn push_work(&mut self, entry: WorklistEntry) -> bool {
        if self.worklist.push_into_current(entry) {
            return true;
        }

        if !self.worklist.advance_to_kept() {
            let region = self.alloc(SEGMENT_BYTES);
            if region.is_null() {
                return false;
            }

            unsafe { self.worklist.extend(region) };
        }

        self.worklist.push_into_current(entry)
    }

    /// The next entity to expand and the row its meeting found, or `None`
    /// when the closure is exhausted.
    pub(crate) fn pop_work(&mut self) -> Option<WorklistEntry> {
        self.worklist.pop()
    }

    /// Take one more block, or answer false when both allocation paths refuse.
    ///
    /// What is left of the previous block is abandoned. A bump that
    /// searched its older blocks for a fit would be a free list, and the
    /// arena's whole life is one collection.
    fn grow(&mut self) -> bool {
        let mut block = gc_metadata::acquire();
        if block.is_null() {
            block = gc_metadata::adopt(crate::memory::critical::draw());
            if block.is_null() {
                return false;
            }

            self.from_reserve += 1;
        }

        // The block the bump is leaving takes no further grant, so this is the
        // instant its consumption becomes exact — the workspace on the first
        // growth of a collection and a drawn block after it, and the workspace
        // is charged like any other because it stays in use until the reset.
        // Published after both allocation paths have answered: a refusal
        // leaves the bump where it is and the block still open.
        self.publish(self.residue());

        unsafe { (&raw mut (*block).next).write(self.blocks) };
        self.blocks = block;
        self.cursor = BlockHeader::payload_start(block);
        self.left = BLOCK_PAYLOAD;
        self.open_capacity = BLOCK_PAYLOAD;
        true
    }

    /// The newest array of the touched list, or null while no block has
    /// been touched. Every array names the next, so this is the whole
    /// list.
    ///
    /// Handed out so that `crate::cycle::density` can read the rows a
    /// trace left without a second copy of the list's shape. It is a
    /// borrow of the arena's own memory and stays valid until
    /// [`clear_touched_rows`](Self::clear_touched_rows) runs.
    #[cfg(test)]
    pub(crate) fn touched_head(&self) -> *mut RowArray {
        self.touched
    }

    /// Blocks enrolled for the sweep. Tests only, and the instrument for
    /// a defect nothing else reports: a block enrolled twice is swept
    /// twice, which is the same store again, so only the length of the
    /// chain shows it.
    #[cfg(test)]
    pub(crate) fn touched_blocks(&self) -> usize {
        let mut count = 0;
        let mut array = self.touched;
        while !array.is_null() {
            count += 1;
            array = unsafe { (*array).next };
        }

        count
    }

    /// Bytes the bump can still grant out of the block under the cursor.
    /// Tests only, and the instrument a case uses to leave the arena an exact
    /// remainder before forcing a refusal.
    #[cfg(test)]
    pub(crate) fn room_left(&self) -> usize {
        self.left
    }

    /// Segments the worklist holds, the workspace's own region included.
    /// Tests only ([`TraceStack::segment_count`](crate::cycle::stack::TraceStack::segment_count)).
    #[cfg(test)]
    pub(crate) fn worklist_segment_count(&self) -> usize {
        self.worklist.segment_count()
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

/// The shadow row of an entity this collection has **met**, or `None`
/// when it has not: a block the trace never touched, a group it never
/// zeroed, an index past the block's array, or a row still coloured
/// [`Color::Untouched`].
///
/// The read-only twin of [`TraceScratchArena::ensure_row`] — same three
/// populations and same two places a row can be — and it neither allocates nor
/// writes, which is what the scan needs: a meeting would initialise the row of
/// an entity the mark never reached from a refcount nothing has subtracted
/// from, and that row would then be read as unreachable or spared on a
/// count the trace never computed.
///
/// # Safety
/// `row` names a live entity of the collected heap, resolved from its
/// own address by `resolve_edge_target`, and its block is still this
/// collection's.
pub(crate) unsafe fn find_initialized_row(row: RowKey) -> Option<*mut u32> {
    let block = row.block as *mut u8;
    let word = if row.population == Population::SingleEntity {
        debug_assert_eq!(
            row.index,
            crate::cycle::row::SINGLE_ENTITY_INDEX,
            "a large entity's block holds one row and this names another"
        );
        unsafe { crate::memory::large_entity::shadow_row(block) }
    } else {
        let array = unsafe { crate::memory::heap::block_shadow(block) } as *mut RowArray;
        if array.is_null() {
            return None;
        }

        if row.index >= unsafe { (*array).row_count } {
            // The state `ensure_row` asserts on, and it is asserted here for
            // the same reason: only a retained block whose survivor list
            // was rebuilt under this trace reaches it, which the trace
            // token forbids. A silent `None` here would leave the mark
            // aborting loudly on the state and the scan passing over it.
            debug_assert!(false, "row {} is past the block's index space", row.index);
            return None;
        }

        if !unsafe { shadow::group_is_initialized(array, row.index) } {
            return None;
        }

        unsafe { shadow::row(array, row.index) }
    };

    match shadow::color(unsafe { *word }) {
        Color::Untouched => None,
        _ => Some(word),
    }
}

/// How many rows `row`'s block needs, or `None` for a retained block
/// that has no survivor list — a block held for a payload alone, one
/// whose reset has not published it yet, or one whose reset could place
/// no list.
///
/// Both populations answer from the block's own collector line, and
/// neither takes a lock: an entity block states its size class there,
/// and a retained block the length of its survivor list
/// (`memory::retained::occupant_count`).
///
/// # Safety
/// `row`'s block must be commissioned as the population says it is.
unsafe fn index_space(row: RowKey) -> Option<u32> {
    match row.population {
        Population::Slotted => {
            Some(unsafe { crate::memory::heap::collector_block_slots(row.block as *mut u8) })
        }
        Population::Retained => {
            unsafe { crate::memory::retained::occupant_count(row.block) }.map(|count| count as u32)
        }
        // Unreachable: `ensure_row` answers the sole occupant's row from its
        // block header without asking where an array would go.
        Population::SingleEntity => None,
    }
}

impl Drop for TraceScratchArena {
    /// The end of the arena's hold on the thread's workspace, and the net
    /// under an unwind. On the contract path the collection has already
    /// called [`reset`](TraceScratchArena::reset) and that call finds
    /// nothing; a test that panics mid-collection would otherwise leave
    /// the pool short for every test after it.
    ///
    /// The workspace goes back when this arena dies rather than at the reset,
    /// which is what leaves it one holder at a time: an arena that handed it
    /// back and kept bumping would be granting bytes the next collection is
    /// granting too. [`LentWorkspace`] is what performs that return, so an
    /// unwind out of the reset below does not skip it.
    fn drop(&mut self) {
        self.reset();
    }
}

#[cfg(test)]
mod tests;
