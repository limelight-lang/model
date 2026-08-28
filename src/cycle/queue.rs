//! The per-thread enrolment queue: where a non-final decrement leaves a
//! cycle candidate for a later trace to read.
//!
//! The contract is `rfc/model/gc/cycle/questions.md`, Y12, and every
//! clause of it is normative here. What this module builds is the
//! **owner's side** of that contract: the write, the growth and the
//! funding. The read side belongs to whoever holds the trace token, and
//! arrives with the mark of `PLAN.md` S35.1; the accelerator that reads
//! another thread's queue arrives at S38.1, with the claim it needs.
//!
//! # The shape
//!
//! One queue per thread, and its writer is the thread that owns it
//! (Y12 clause 1), so the write is uncontended by construction and needs
//! no read-modify-write. An entry is one pointer to an entity header.
//! Slots are sixteen-byte aligned in every size class
//! (`memory::heap::SIZE_CLASSES`), so an entry's low four bits are free
//! and reserved for the marks a dirty reader writes (Y12 clause 7).
//!
//! A **segment is one 64 KiB pool block**, which is the only unit both
//! funding doors dispense (`rfc/model/gc/cycle/questions.md`, Y12
//! clause 3), and the queue is a chain of them threaded through
//! [`BlockHeader::next`] — the same use the shadow arena makes of that
//! field, and the reason it exists.
//!
//! ```text
//! live ──next──▶ full ──next──▶ full ──next──▶ null
//!  │              │
//!  │              └─ 8160 entries, by construction: a segment leaves
//!  │                 the live position only when it is full
//!  └─ `filled` entries, held in the thread's own cell rather than in
//!     the block
//! ```
//!
//! **The live segment's fill lives beside the chain and not inside the
//! block, and it is the only bound on any segment's contents.** While the
//! owner is the only one moving segments, overflow is the only way one
//! leaves the live position, so every segment behind the head holds
//! exactly [`SEGMENT_CAPACITY`] entries and the chain needs no
//! per-segment length. **That is a property of today's single mover and
//! not of the structure**: clause 2's reader detaches a chain whose head
//! is partly filled, and the count that bounds it is in a cell the writer
//! is about to reset. What the two agree on is open
//! (`rfc/model/gc/cycle/questions.md`, Y12 clause 2), and S38.1 is where a
//! second thread swaps the chain for the first time — so a reader written
//! against "every segment is full" is written against an invariant that
//! ends there.
//!
//! # Why the growth path allocates nothing
//!
//! Enrolment sits on the release path, so the write may not allocate,
//! lock or copy (Y12 clause 3). The overflow is therefore a pointer
//! swap into a segment somebody else took from the pool: the thread
//! holds **two spares** in cells it fills at thread init and at every
//! safepoint poll, both through the ordinary door. With both cells empty
//! the overflow draws the critical reserve (`memory::critical`), which is
//! the draw clause 6 funds. **Reserve mode itself is not built here** —
//! clause 6 asks that the runtime stay in it until every queued root has
//! been walked, and nothing in this crate carries such a state, the poll
//! refilling the reserve unconditionally at the next safepoint. What
//! exists is the draw and the arming.
//!
//! # Why an enrolment cannot fail
//!
//! Edmond ruled on 2026-08-28 that nothing may be lost, so below the
//! reserve sits a tier that cannot refuse: the **escrow**, a fixed array
//! in this same thread-local, `const`-constructible and never grown. A
//! refused entry lands there by a store and an increment, which keeps
//! clause 3's three prohibitions through the last tier, and [`enrol`]
//! therefore answers nothing — it has no failure to report.
//!
//! The escrow is emptied at the next safepoint poll, which is also where
//! the thread does what the ruling asks: collect, or wait for the
//! collector. Nothing happens inside `ll_release`, and that is not
//! timidity — a collection mid-mutation walks a stale edge and frees a
//! live object (`rfc/model/gc/cycle/questions.md`, Y14, "Where it fires,
//! and where it must not"). The escrow is what carries the root from the
//! refusal to the first lawful instant.
//!
//! It holds one segment's worth of entries, on the argument clause 3
//! makes for the two cells: a whole segment cannot fill between two
//! polls at any entry size. That argument quantifies over loops the
//! compiler emits, and one loop it does not — `ll_release_vector`, whose
//! count is the caller's — broke it, so that loop now carries a poll of
//! its own every [`POLL_STRIDE`] iterations. Overflowing the escrow
//! therefore takes sustained pool refusal rather than a long run.
//!
//! The live segment is a cell too: a thread holds none until its first
//! enrolment, which finds no room by construction and takes the overflow
//! path. So a thread that never enrols holds two segments and not three,
//! and the empty-queue case needs no arm of its own.
//!
//! # What the poll owes this module
//!
//! Three things, and [`crate::gc::ll_gc_maybe_collect`] does them in
//! order. It refills the spare cells, asking [`is_short`] — the count
//! itself, never a flag a draw sets, because a thread whose fill at init
//! was refused has never drawn and would never be asked again. It then
//! drains the escrow into the queue, which is why the refill comes
//! first. And it fires a collection when [`crate::gc::take_due`] answers
//! true, which a reserve draw or an escrow landing arms.

use std::cell::Cell;

use crate::memory::block_pool::{BLOCK_KIND_ARENA, BLOCK_PAYLOAD, BlockHeader, BlockPool};
use crate::refcount::RcHeader;

/// Entries one segment holds. Every segment but the live one holds
/// exactly this many, which is what lets the chain carry no length.
pub(crate) const SEGMENT_CAPACITY: usize = BLOCK_PAYLOAD / size_of::<*mut RcHeader>();

/// Entries the escrow holds — one segment's worth.
///
/// Sized on the argument [`SPARE_SEGMENTS`] is sized on: a whole segment
/// cannot fill between two polls at any entry size, so a run that
/// overflows the escrow is a run with no poll in it at all. The figure is
/// deliberately generous; what would license a smaller one is the ABI's
/// bound on operations between two polls, which is unwritten
/// (`rfc/runtime/exceptions.md`).
pub(crate) const ESCROW_ENTRIES: usize = SEGMENT_CAPACITY;

/// Iterations a runtime-owned bulk loop may run between two safepoint
/// polls of its own.
///
/// Half the escrow, so that a loop obeying it can never fill the escrow
/// between two of its polls whatever the compiler's own bound turns out
/// to be. The loop that needs it is `object::ll_release_vector`, whose
/// count is the caller's and whose body the compiler never sees inside:
/// without a poll of its own it enrols without bound and reaches the
/// abort below with memory free (`rfc/dev/DECISIONS.md`, "a runtime loop
/// carries the poll contract it broke").
pub(crate) const POLL_STRIDE: usize = ESCROW_ENTRIES / 2;

/// Spare segments a thread keeps ahead of an overflow.
///
/// Two, which covers the two consumptions one interval between polls can
/// hold: one overflow, and one in-line collection whose own request to
/// the pool was refused (`rfc/model/gc/cycle/questions.md`, Y12
/// clause 3). Beyond the two the critical reserve answers, which is what
/// it is for.
pub(crate) const SPARE_SEGMENTS: usize = 2;

/// A thread's queue and the spares behind it.
///
/// Cells rather than a `RefCell`: the enrolment write is the hottest
/// path in the runtime and a borrow flag on it buys nothing, the queue
/// having one writer by contract and no path that re-enters it. Nothing
/// here has drop glue, so thread exit frees it by hand
/// ([`drain`]) rather than through a destructor whose order is
/// unspecified (`memory::heap::ll_thread_exit`).
struct Queue {
    /// The segment being written, or null before the first enrolment.
    /// The rest of the chain hangs off its [`BlockHeader::next`].
    live: Cell<*mut BlockHeader>,
    /// Entries written into [`Queue::live`]. Meaningless when it is null.
    filled: Cell<usize>,
    /// Segments taken ahead of an overflow, `held` of them valid.
    spares: [Cell<*mut BlockHeader>; SPARE_SEGMENTS],
    held: Cell<usize>,
    /// Entries no door could fund a segment for, `escrowed` of them
    /// valid. The tier that cannot refuse, so that an enrolment cannot
    /// fail (`rfc/dev/DECISIONS.md`, "an enrolment cannot fail").
    escrow: [Cell<*mut RcHeader>; ESCROW_ENTRIES],
    escrowed: Cell<usize>,
}

thread_local! {
    static QUEUE: Queue = const {
        Queue {
            live: Cell::new(std::ptr::null_mut()),
            filled: Cell::new(0),
            spares: [const { Cell::new(std::ptr::null_mut()) }; SPARE_SEGMENTS],
            held: Cell::new(0),
            escrow: [const { Cell::new(std::ptr::null_mut()) }; ESCROW_ENTRIES],
            escrowed: Cell::new(0),
        }
    };
}

/// Where a segment's entries begin.
#[inline]
fn entries(segment: *mut BlockHeader) -> *mut *mut RcHeader {
    BlockHeader::payload_start(segment) as *mut *mut RcHeader
}

/// Put an entity in this thread's queue.
///
/// The caller has already set [`crate::refcount::ENROLLED`] in the
/// entity's flags, which Y12 clause 4 requires to happen before the
/// write: a bit set afterwards lets a second decrement enrol the same
/// entity twice in the window between the two.
///
/// **It cannot fail, and answers nothing.** Every door refusing puts the
/// entry in the escrow instead, because a root that leaves no entry
/// behind is a garbage ring no later collection can name, enrolment
/// being edge-triggered (`rfc/model/gc/cycle/questions.md`, Y6), and
/// Edmond ruled on 2026-08-28 that nothing may be lost.
///
/// # Safety
/// `entity` points to a live heap entity beginning with `RcHeader`, and
/// stays live at least until this thread's next safepoint.
pub(crate) unsafe fn enrol(entity: *mut RcHeader) {
    QUEUE.with(|q| {
        let live = q.live.get();
        let filled = q.filled.get();

        if live.is_null() || filled == SEGMENT_CAPACITY {
            unsafe { grow_and_write(q, entity) };
            return;
        }

        unsafe { entries(live).add(filled).write(entity) };
        q.filled.set(filled + 1);
    })
}

/// The overflow path: put a fresh segment in the live position and write
/// the entry into it.
///
/// The full segment stays reachable through the fresh one's
/// [`BlockHeader::next`], so growth links and never copies. No door
/// funding one puts the entry in the escrow, which is why this answers
/// nothing either.
unsafe fn grow_and_write(q: &Queue, entity: *mut RcHeader) {
    let full = q.live.get();

    let fresh = match take_spare(q) {
        s if !s.is_null() => s,
        _ => {
            // Both cells empty, so the reserve — the draw clause 6
            // funds. It is a fixed-array pop on any thread that has
            // touched `memory::critical` before, which `ll_thread_init`
            // arranges for every thread it runs on; what the very first
            // touch of that thread-local costs is the platform's, its
            // payload having drop glue and therefore a destructor to
            // register. A thread that never ran `ll_thread_init` reaches
            // that first touch from here, and S34.5 is the step that owes
            // it an answer.
            let block = crate::memory::critical::draw();
            if !block.is_null() {
                // A draw is pressure, and pressure is what asks for a
                // collection. Armed here rather than beside the refusal
                // below so that the two paths arm independently: the
                // criterion names them separately and a later tier
                // between them would lose one silently.
                crate::gc::arm();
            }

            block
        }
    };

    if fresh.is_null() {
        escrow(q, entity);
        // The escrow landing arms on its own: the refill the poll
        // performs is unconditional, so what the arming buys here is the
        // fire, not the cells.
        crate::gc::arm();
        return;
    }

    // Stamped here as well as at acquisition, because the block may have
    // come from the reserve rather than from a cell, and the reserve
    // stamps its own on the way in but not on the way out. `ARENA`
    // because what matters is that it is not `ENTITY` — a trace never
    // enters a block of any other kind (`crate::cycle::row`).
    unsafe {
        crate::memory::block_pool::store_block_kind(&raw const (*fresh).kind, BLOCK_KIND_ARENA)
    };
    unsafe { (*fresh).next = full };

    unsafe { entries(fresh).write(entity) };
    q.live.set(fresh);
    q.filled.set(1);
}

/// The tier below the reserve: park the entry where nothing can refuse
/// it, and count it.
///
/// Aborts when the escrow is full, which is the last resort the funded
/// class already keeps (`rfc/runtime/exceptions.md`, the store barrier's
/// reserve). What stands between an ordinary program and it is the poll
/// contract, which every loop obeys — the compiler's emitted ones and,
/// since it is a loop the compiler cannot see inside,
/// `object::ll_release_vector`'s own ([`POLL_STRIDE`]). What is left
/// behind that is a conjunction: the pool refusing across polls, and
/// either a gate closed for the whole run or a collection that ran and
/// lost, and then thousands of further non-final decrements.
fn escrow(q: &Queue, entity: *mut RcHeader) {
    let escrowed = q.escrowed.get();
    if escrowed == ESCROW_ENTRIES {
        // Nothing to report it through: `ll_release` holds no frame, and
        // the poll that would raise is what this thread has not reached.
        std::process::abort();
    }

    q.escrow[escrowed].set(entity);
    q.escrowed.set(escrowed + 1);
}

/// Move escrowed entries back into the queue, as far as the room a poll
/// has just made allows.
///
/// The poll calls it after the cells are refilled and before it fires,
/// and it takes no door of its own: with the cells still empty an entry
/// would land straight back in the escrow, so the drain stops instead and
/// waits for the collection the same poll is about to run.
pub(crate) fn drain_escrow() {
    QUEUE.with(|q| {
        while q.escrowed.get() > 0 {
            let live = q.live.get();
            let has_room = !live.is_null() && q.filled.get() < SEGMENT_CAPACITY;
            if !has_room && q.held.get() == 0 {
                return;
            }

            let escrowed = q.escrowed.get() - 1;
            let entity = q.escrow[escrowed].replace(std::ptr::null_mut());
            q.escrowed.set(escrowed);
            unsafe { enrol(entity) };
        }
    });
}

/// Take one spare, or null when both cells are empty.
#[inline]
fn take_spare(q: &Queue) -> *mut BlockHeader {
    let held = q.held.get();
    if held == 0 {
        return std::ptr::null_mut();
    }

    q.held.set(held - 1);
    q.spares[held - 1].replace(std::ptr::null_mut())
}

/// Whether this thread's spare cells are short and want a poll to fill
/// them.
///
/// The count itself rather than a flag a draw sets, which is the rule
/// both memory reserves learned the hard way: a thread whose fill at
/// init was refused holds nothing, has never drawn, and a flag would
/// leave it unasked for the rest of its life (`memory::reserve`,
/// `is_drawn`).
pub(crate) fn is_short() -> bool {
    QUEUE.with(|q| q.held.get() < SPARE_SEGMENTS)
}

/// Fill the spare cells through the ordinary door, answering false when
/// they could not be filled completely.
///
/// Best-effort by construction, and called where a refusal is already
/// reported by something else: at thread init, where the thread's first
/// allocation returns null, and at the safepoint poll, which comes back.
pub(crate) fn replenish() -> bool {
    QUEUE.with(|q| {
        while q.held.get() < SPARE_SEGMENTS {
            let block = BlockPool::global().get();
            if block.is_null() {
                return false;
            }

            // Stamped at acquisition rather than at the swap, which is
            // what `memory::critical` does and for the same reason: a
            // block held in a cell is out of the pool and the collector
            // acquire-loads the kind of every block in every carved
            // region, so a spare left reading `FREE` is a block counted
            // out and read as free.
            unsafe {
                crate::memory::block_pool::store_block_kind(
                    &raw const (*block).kind,
                    BLOCK_KIND_ARENA,
                )
            };

            let held = q.held.get();
            q.spares[held].set(block);
            q.held.set(held + 1);
        }

        true
    })
}

/// Give every segment and every spare back, and leave the queue empty.
///
/// Thread exit calls it in production and the tests call it to start
/// from a known queue: the queue holds pool blocks, and a dying thread
/// must not take them with it.
///
/// **The entries go with the segments — the escrow's too — and their
/// entities keep the enrolled bit, which is a permanent miss and not a
/// deferral.** A block
/// with live occupants is handed to the abandoned list and adopted by
/// another thread (`memory::heap::ll_thread_exit`), so the entity
/// outlives its queue carrying a bit that names an entry nobody holds —
/// and [`crate::refcount::ENROLMENT_GATE_MASK`] refuses every later
/// decrement of it, for the life of the process. Clearing the bits here
/// is not available to this step: an entry may name a slot already freed,
/// and reading it to clear a bit would touch returned memory. S39.1 is
/// the step that chooses the fate, and this is the cost it is choosing
/// against.
///
/// Through [`crate::memory::critical::give_back`] rather than straight
/// to the pool, so a reserve below capacity is refilled before the pool
/// sees anything.
pub(crate) fn drain() {
    QUEUE.with(|q| {
        let mut segment = q.live.replace(std::ptr::null_mut());
        q.filled.set(0);

        while !segment.is_null() {
            let next = unsafe { (*segment).next };
            unsafe { (*segment).next = std::ptr::null_mut() };
            crate::memory::critical::give_back(segment);
            segment = next;
        }

        let held = q.held.replace(0);
        for cell in &q.spares[..held] {
            let block = cell.replace(std::ptr::null_mut());
            crate::memory::critical::give_back(block);
        }

        let escrowed = q.escrowed.replace(0);
        for cell in &q.escrow[..escrowed] {
            cell.set(std::ptr::null_mut());
        }
    });
}

/// Entries this thread's escrow is holding.
#[cfg(test)]
pub(crate) fn escrowed_count() -> usize {
    QUEUE.with(|q| q.escrowed.get())
}

/// Entries this thread's queue holds, walking the chain.
#[cfg(test)]
pub(crate) fn enrolled_count() -> usize {
    QUEUE.with(|q| {
        let live = q.live.get();
        if live.is_null() {
            return 0;
        }

        let mut count = q.filled.get();
        let mut segment = unsafe { (*live).next };
        while !segment.is_null() {
            count += SEGMENT_CAPACITY;
            segment = unsafe { (*segment).next };
        }

        count
    })
}

/// Segments this thread's queue holds.
#[cfg(test)]
pub(crate) fn segment_count() -> usize {
    QUEUE.with(|q| {
        let mut count = 0;
        let mut segment = q.live.get();
        while !segment.is_null() {
            count += 1;
            segment = unsafe { (*segment).next };
        }

        count
    })
}

/// Spares this thread holds.
#[cfg(test)]
pub(crate) fn spares_held() -> usize {
    QUEUE.with(|q| q.held.get())
}

/// Fill the live segment to capacity with `filler`, so the next
/// enrolment overflows.
///
/// The shorthand exists because the honest way to reach the overflow is
/// 8160 releases, which is a fixture rather than a test: the branch it
/// reaches is three lines and the entries before it prove nothing about
/// them. **It writes the entries rather than only the count**, because
/// the count is what bounds a segment's contents and a test that lied
/// about it would hand the first reader — S34.3's corpse rule — 8159
/// recycled words to dereference.
#[cfg(test)]
pub(crate) fn fill_live_segment(filler: *mut RcHeader) {
    QUEUE.with(|q| {
        let live = q.live.get();
        assert!(!live.is_null(), "no live segment to fill");
        for index in q.filled.get()..SEGMENT_CAPACITY {
            unsafe { entries(live).add(index).write(filler) };
        }

        q.filled.set(SEGMENT_CAPACITY);
    });
}

/// The nth entry of the live segment, counting from the oldest.
#[cfg(test)]
pub(crate) fn live_entry(index: usize) -> *mut RcHeader {
    QUEUE.with(|q| {
        let live = q.live.get();
        assert!(!live.is_null(), "no live segment");
        assert!(index < q.filled.get(), "entry {index} is past the fill");
        unsafe { entries(live).add(index).read() }
    })
}

#[cfg(test)]
mod tests;
