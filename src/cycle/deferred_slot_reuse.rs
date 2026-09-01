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
//! One chain of 64 KiB manager blocks per open trace, stamped
//! `BLOCK_KIND_GC_METADATA` for as long as it is held, drawn at
//! [`ActiveTrace::open`] and returned at that window's close. The chain's
//! control line is the first 64 bytes of the head block's payload, and
//! thread-local storage holds one non-owning pointer to that head block:
//! **null is the closed window**,
//! so no second flag can disagree with the chain's existence
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
//! The first block is drawn when the window opens rather than at the first
//! withheld return, because that is the last instant at which a refusal has an
//! answer. A collection is ordinarily the standard in-line form and meets no
//! refusal anywhere (`rfc/model/gc/cycle/questions.md`, Y14), but on the
//! pressure path it is a refused pool that started it, so both allocation paths
//! refusing is an outcome the open has to carry: there it is a collection that
//! does not start, and nothing is withheld. A draw at the first withheld return
//! would meet the same refusal holding a slot whose rows are live, where
//! returning it is the reuse this module prevents and dropping it loses a
//! physical return, which is refused (`dev/DECISIONS.md`, "an enrolment cannot
//! fail"). Growth past the first block therefore has no answer left and ends
//! the process, which is the funded class's last resort and the same one the
//! queue's overflow buffer reaches (`crate::cycle::queue`). A thread exiting
//! with its window still open ends the process for the same reason
//! ([`dispose_thread_state`]).
//!
//! The block leaving the append position is charged whole. The block still
//! under the cursor is a documented residue: it never stands in the current
//! figure, and the close enters it in the high-water one together with the
//! trace arena's own residue, which is the instant the two are in use at once
//! (`crate::memory::gc_metadata`).

use std::cell::Cell;

use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockHeader};
use crate::memory::gc_metadata;

/// The chain of withheld returns, resident in the head block it describes.
///
/// `Cell` rather than a lock or a `RefCell`: the chain has one writer by
/// construction, the thread whose trace window is open, and the append sits on
/// the free path where a borrow flag buys nothing.
///
/// One 64-byte line, so the records behind it start on the next one and an
/// append writes no line the replay walk reads before it.
#[repr(C, align(64))]
struct DeferredReturnChain {
    /// Where the next record goes.
    cursor: Cell<*mut *mut u8>,
    /// One past the last record position of the block [`Self::cursor`] points
    /// into, which is what the append tests against.
    limit: Cell<*mut *mut u8>,
    /// The last block of the chain, and the one the cursor is inside.
    append_block: Cell<*mut BlockHeader>,
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

/// Records one block of the chain holds.
///
/// Every block reserves the control line, not only the head that uses it: one
/// capacity rather than two costs 64 bytes in a chain that has grown, and the
/// growth itself is the path this module documents as unreachable in practice.
/// The figure is the candidate queue's overflow capacity for the same reason —
/// a 65,280-byte payload less one 64-byte control line, in eight-byte records.
const RECORDS_PER_BLOCK: usize =
    (BLOCK_PAYLOAD - size_of::<DeferredReturnChain>()) / size_of::<*mut u8>();

const _: () = assert!(
    RECORDS_PER_BLOCK * size_of::<*mut u8>() + size_of::<DeferredReturnChain>() == BLOCK_PAYLOAD
);

thread_local! {
    /// The head block of this thread's withheld returns while its trace window
    /// is open, and null otherwise. Non-owning: the chain belongs to the
    /// [`ActiveTrace`] that drew it.
    static DEFERRED_RETURNS: Cell<*mut BlockHeader> = const { Cell::new(std::ptr::null_mut()) };
}

/// The control line of a chain, which is its head block's first line.
#[inline]
fn chain_of(head: *mut BlockHeader) -> *const DeferredReturnChain {
    BlockHeader::payload_start(head) as *const DeferredReturnChain
}

/// Bytes of `block` in use while it holds `records` records.
///
/// The head's control line counts and a later block's reserved one does not:
/// the ledger's figure is what a structure has written into a block it holds,
/// and the reservation behind that is outside it
/// (`crate::memory::gc_metadata::GcMemoryStats::current_bytes_in_use`).
#[inline]
fn used_bytes(block: *mut BlockHeader, head: *mut BlockHeader, records: usize) -> usize {
    let control = if block == head {
        size_of::<DeferredReturnChain>()
    } else {
        0
    };

    control + records * size_of::<*mut u8>()
}

/// The first record position of any block of a chain.
#[inline]
fn records_of(block: *mut BlockHeader) -> *mut *mut u8 {
    unsafe {
        BlockHeader::payload_start(block).add(size_of::<DeferredReturnChain>()) as *mut *mut u8
    }
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
    /// The chain's head, which carries the control line of all of it.
    head: *mut BlockHeader,
}

impl WithheldReturns {
    /// Draw the chain's first block, or `None` when neither allocation path
    /// answers.
    fn open() -> Option<Self> {
        let (block, from_reserve) = draw_block();
        if block.is_null() {
            return None;
        }

        let records = records_of(block);
        unsafe {
            (&raw mut (*block).next).write(std::ptr::null_mut());
            (BlockHeader::payload_start(block) as *mut DeferredReturnChain).write(
                DeferredReturnChain {
                    cursor: Cell::new(records),
                    limit: Cell::new(records.add(RECORDS_PER_BLOCK)),
                    append_block: Cell::new(block),
                    from_reserve: Cell::new(usize::from(from_reserve)),
                    published: Cell::new(0),
                },
            );
        }

        Some(Self { head: block })
    }

    fn chain(&self) -> &DeferredReturnChain {
        unsafe { &*chain_of(self.head) }
    }

    /// Bytes of the block under the cursor, which no growth has charged.
    fn residue(&self) -> usize {
        let chain = self.chain();
        let append_block = chain.append_block.get();
        let records = (chain.cursor.get() as usize - records_of(append_block) as usize)
            / size_of::<*mut u8>();
        used_bytes(append_block, self.head, records)
    }

    /// Take this thread's window down.
    ///
    /// Idempotent, and called from two places for one reason: the ordered
    /// close calls it after the row sweep, and [`Drop`] calls it again for the
    /// unwind that never reached the ordered close. A window left standing
    /// over a released block is a free path writing a record through a cursor
    /// into memory the pool has handed out again.
    fn close_window(&self) {
        DEFERRED_RETURNS.with(|head| {
            if head.get() == self.head {
                head.set(std::ptr::null_mut());
            }
        });
    }

    /// Return every withheld slot through `ll_free`, oldest first.
    ///
    /// Called with the window already closed, so a return that reaches
    /// [`defer_reuse_if_tracing`] again is refused there and proceeds
    /// physically.
    ///
    /// The cursor bounds the records of the block it points into; every block
    /// before that one holds [`RECORDS_PER_BLOCK`], a block leaving the append
    /// position only when it is full.
    fn replay(&self) {
        let chain = self.chain();
        let append_block = chain.append_block.get();
        let cursor = chain.cursor.get();

        let mut block = self.head;
        while !block.is_null() {
            let records = records_of(block);
            let held = if block == append_block {
                (cursor as usize - records as usize) / size_of::<*mut u8>()
            } else {
                RECORDS_PER_BLOCK
            };

            for index in 0..held {
                // Safety: each record is one entity slot whose observable
                // teardown completed before `defer_reuse_if_tracing` accepted
                // the return. Replaying it once through `ll_free` is
                // the return it still owes.
                unsafe { crate::memory::stdapi::ll_free(*records.add(index)) };
            }

            block = unsafe { (*block).next };
        }
    }
}

impl Drop for WithheldReturns {
    /// Give the chain's blocks back, what the reserve lent through the reserve
    /// allocation path and the rest to the pool.
    ///
    /// The reserve is served first for the reason the trace arena serves it
    /// first: the retry that follows an abort wants an allocation path that
    /// serves.
    fn drop(&mut self) {
        self.close_window();

        let chain = self.chain();
        let published = chain.published.get();
        let mut owed_to_reserve = chain.from_reserve.get();
        gc_metadata::discharge(published);

        let mut block = self.head;
        while !block.is_null() {
            let next = unsafe { (*block).next };
            if owed_to_reserve > 0 {
                owed_to_reserve -= 1;
                gc_metadata::release_to_critical(block);
            } else {
                gc_metadata::release(block);
            }

            block = next;
        }
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
    arena: crate::cycle::arena::TraceScratchArena,
    returns: WithheldReturns,
    // A window belongs to the TLS state of the thread that opened it. Moving
    // the guard would close another thread's window and strand this one's.
    _not_send: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl ActiveTrace {
    /// Open this thread's one trace window, or `None` when neither allocation
    /// path can fund the chain that holds its withheld returns.
    ///
    /// `None` is a collection that does not start: no window is open, no return
    /// has been withheld, and the caller's own abort path has nothing to undo.
    pub(crate) fn open() -> Option<Self> {
        assert!(
            DEFERRED_RETURNS.with(Cell::get).is_null(),
            "a thread runs at most one trace at a time"
        );

        let returns = WithheldReturns::open()?;
        DEFERRED_RETURNS.with(|head| head.set(returns.head));

        Some(Self {
            arena: crate::cycle::arena::TraceScratchArena::new(),
            returns,
            _not_send: std::marker::PhantomData,
        })
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
        // Both of this collection's residues stand together here and nowhere
        // else: the arena's reset charges its own and discharges it in the
        // same call, so a mark after that call would enter the two separately
        // and the high-water figure would miss a collection that held rows and
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
/// The refusal has no answer left. The open trace's rows still address every
/// slot the caller is holding, so returning this one is the reuse the window
/// exists to prevent; dropping the record loses a physical return, which is
/// refused (`dev/DECISIONS.md`, "an enrolment cannot fail"). Nothing can report
/// it either: `ll_free` holds no frame that can fail.
///
/// Reached only after 8,152 slots have died inside one trace window while both
/// allocation paths refuse a single block.
#[cold]
fn grow(head: *mut BlockHeader, chain: &DeferredReturnChain) {
    let (block, from_reserve) = draw_block();
    if block.is_null() {
        std::process::abort();
    }

    // The block the cursor is leaving is full by construction, so what it holds
    // is exact here. Charged after both allocation paths have answered: a
    // refusal leaves the chain where it stands.
    let filled = used_bytes(chain.append_block.get(), head, RECORDS_PER_BLOCK);
    chain.published.set(chain.published.get() + filled);
    gc_metadata::charge(filled);

    let records = records_of(block);
    unsafe {
        (&raw mut (*block).next).write(std::ptr::null_mut());
        (&raw mut (*chain.append_block.get()).next).write(block);
        chain.limit.set(records.add(RECORDS_PER_BLOCK));
    }

    chain.append_block.set(block);
    chain.cursor.set(records);
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
/// With one open and room in the append block: three loads — the thread-local
/// head, the cursor and the limit — two branches and two stores, with no
/// atomic, no allocator call and no pool call. The pool is asked in [`grow`]
/// alone, and the cursor is read a second time only there.
///
/// # Safety
/// `ptr` is a dead entity slot whose teardown has completed and which this call
/// owns until either the function returns `false` or the window closes.
#[inline]
pub(crate) unsafe fn defer_reuse_if_tracing(ptr: *mut u8) -> bool {
    let head = DEFERRED_RETURNS.with(Cell::get);
    if head.is_null() {
        return false;
    }

    let chain = unsafe { &*chain_of(head) };
    let mut cursor = chain.cursor.get();
    if cursor == chain.limit.get() {
        grow(head, chain);
        cursor = chain.cursor.get();
    }

    unsafe {
        cursor.write(ptr);
        chain.cursor.set(cursor.add(1));
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
    let head = DEFERRED_RETURNS.with(Cell::get);
    if head.is_null() {
        return 0;
    }

    let chain = unsafe { &*chain_of(head) };
    let append_block = chain.append_block.get();
    let cursor = chain.cursor.get();
    let mut count = 0;
    let mut block = head;
    while !block.is_null() && block != append_block {
        count += RECORDS_PER_BLOCK;
        block = unsafe { (*block).next };
    }

    count + (cursor as usize - records_of(append_block) as usize) / size_of::<*mut u8>()
}

#[cfg(test)]
mod tests;
