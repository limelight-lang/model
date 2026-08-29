//! The per-thread enrolment queue: where a non-final decrement leaves a
//! cycle candidate for a later trace to read.
//!
//! The contract is `rfc/model/gc/cycle/questions.md`, Y12, and every
//! clause of it is normative here. What this module builds is the
//! **owner's side** of that contract: the write, the growth and the
//! funding. The read side belongs to whoever holds the trace token:
//! `cycle::mark` traces from one root, and the collection that draws
//! those roots out of this queue is S36.7's. The accelerator that reads
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
//! reserve sits a tier that cannot refuse: the **escrow**, whose storage
//! is the **floor** — one 64 KiB pool block the thread holds for one
//! init→exit life and writes entries into directly. A refused entry
//! lands there by a store and an increment, which keeps clause 3's three
//! prohibitions through the last tier, and [`enrol`] therefore answers
//! nothing — it has no failure to report.
//!
//! **What makes the storage certain is the draw, not the address space.**
//! The escrow was a fixed array in this same thread-local until
//! 2026-08-28, and it was almost the whole of the crate's
//! zero-initialised TLS image, which every thread pays at birth whether
//! it enrols or not: `.tbss` measures 496 bytes with the floor against
//! 65 784 without it (`dev/BENCHMARKS.md`, "the escrow's move out of
//! TLS"). The floor is drawn instead, at thread init and before the
//! best-effort fills, and its refusal is a thread that never starts
//! (`rfc/dev/DECISIONS.md`, "the escrow's floor is allocator-issued").
//! The invariant every later tier rests on comes out of that coupling:
//! every registered thread has a floor, because a thread whose floor was
//! refused is a thread the runtime never registered.
//!
//! Entity work also reaches threads the runtime never registered —
//! self-initialising allocation, a releaser-only FFI consumer — and such
//! a thread draws its floor at its first enrolment instead, through the
//! ordinary door ([`grow_and_write`]). That draw refusing aborts, which
//! is the funded class's last resort reached one door earlier than the
//! escrow's own overflow below.
//!
//! The escrow is emptied at the next safepoint poll, which is also where
//! the thread does what the ruling asks: collect, or wait for the
//! collector. Nothing happens inside `ll_release`, and that is not
//! timidity — a collection mid-mutation walks a stale edge and frees a
//! live object (`rfc/model/gc/cycle/questions.md`, Y14, "Where it fires,
//! and where it must not"). The escrow is what carries the root from the
//! refusal to the first lawful instant.
//!
//! It holds one segment's worth of entries, which is what a block holds
//! and what clause 3's argument for the two cells asks for: a whole
//! segment cannot fill between two polls at any entry size. That argument
//! quantifies over loops the compiler emits, and one loop it does not —
//! `ll_release_vector`, whose count is the caller's — broke it, so that
//! loop now carries a poll of its own every [`POLL_STRIDE`] iterations.
//! Overflowing the escrow therefore takes sustained pool refusal rather
//! than a long run.
//!
//! The live segment is a cell too: a thread holds none until its first
//! enrolment, which finds no room by construction and takes the overflow
//! path. So a thread that never enrols holds its floor and two spare
//! segments rather than three segments, and the empty-queue case needs no
//! arm of its own.
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

/// Entries the escrow holds — one block's worth, the floor being one
/// block.
///
/// The size the ruling asks for is the size the storage has: a whole
/// segment cannot fill between two polls at any entry size, which is the
/// argument [`SPARE_SEGMENTS`] is sized on, so a run that overflows the
/// escrow is a run with no poll in it at all. The figure is deliberately
/// generous; what would license a smaller one is the ABI's bound on
/// operations between two polls, which is unwritten
/// (`rfc/runtime/exceptions.md`) — and a smaller one buys nothing while
/// the storage is a whole block either way.
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
    /// The escrow's storage, or null on a thread that has neither run
    /// `ll_thread_init` nor enrolled. Held for one init→exit life and
    /// given back by [`release_floor`].
    floor: Cell<*mut BlockHeader>,
    /// Entries in the floor no door could fund a segment for, the oldest
    /// first. The tier that cannot refuse, so that an enrolment cannot
    /// fail (`rfc/dev/DECISIONS.md`, "an enrolment cannot fail").
    escrowed: Cell<usize>,
}

thread_local! {
    static QUEUE: Queue = const {
        Queue {
            live: Cell::new(std::ptr::null_mut()),
            filled: Cell::new(0),
            spares: [const { Cell::new(std::ptr::null_mut()) }; SPARE_SEGMENTS],
            held: Cell::new(0),
            floor: Cell::new(std::ptr::null_mut()),
            escrowed: Cell::new(0),
        }
    };
}

/// Where a block's entries begin — a segment's, and the floor's, which
/// hold the same thing and are the same size.
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
    // The floor before anything else, because the tier at the bottom of
    // this function is the one that may not refuse. A registered thread
    // has held one since `ll_thread_init`, so this is a predictable
    // branch on a cold path; a thread that never ran init draws here, at
    // its first enrolment, which is this path by construction — the live
    // segment is a cell, so a thread's first enrolment finds no room.
    // Drawing at the first refusal instead would draw exactly when every
    // other door has already found the pool empty.
    let floor = match q.floor.get() {
        f if !f.is_null() => f,
        _ => draw_floor_or_abort(q),
    };

    let full = q.live.get();

    let fresh = match take_spare(q) {
        s if !s.is_null() => s,
        _ => {
            // Both cells empty, so the reserve — the draw clause 6
            // funds. It is a fixed-array pop on any thread that has
            // touched `memory::critical` before, which `ll_thread_init`
            // arranges for every thread it runs on. A thread that never
            // ran it reaches that first touch from here, and on glibc the
            // registration it performs kills the process when it cannot
            // allocate 32 bytes — which is the abort this same thread
            // already takes one door earlier, at its floor
            // (`dev/DECISIONS.md`, "what the first touch of a
            // thread-local with drop glue may cost").
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
        escrow(q, floor, entity);
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
fn escrow(q: &Queue, floor: *mut BlockHeader, entity: *mut RcHeader) {
    let escrowed = q.escrowed.get();
    if escrowed == ESCROW_ENTRIES {
        // Nothing to report it through: `ll_release` holds no frame, and
        // the poll that would raise is what this thread has not reached.
        std::process::abort();
    }

    // The floor is this thread's and non-null, which is the caller's to
    // establish and the reason it is a parameter rather than a second
    // load: every path here comes through [`grow_and_write`], which draws
    // one before it takes any door.
    unsafe { entries(floor).add(escrowed).write(entity) };
    q.escrowed.set(escrowed + 1);
}

/// Put a floor under this thread, or answer null when the pool refuses.
///
/// Through the ordinary door, and idempotent: a thread that holds one
/// answers with it and draws nothing.
fn draw_floor(q: &Queue) -> *mut BlockHeader {
    let floor = q.floor.get();
    if !floor.is_null() {
        return floor;
    }

    let block = BlockPool::global().get();
    if block.is_null() {
        return block;
    }

    // Stamped for the reason a spare is stamped at acquisition: the block
    // is out of the pool, and the collector acquire-loads the kind of
    // every block in every carved region, so a floor left reading `FREE`
    // is a block counted out and read as free.
    unsafe {
        crate::memory::block_pool::store_block_kind(&raw const (*block).kind, BLOCK_KIND_ARENA)
    };

    // **The cell is read again rather than trusted across the draw.**
    // `BlockPool::get` raises a record, and a thread's first record runs
    // `ll_thread_init` from inside the journal (`journal::mod`, "A thread
    // can reach a record site without ever having initialised the
    // runtime") — which comes back here and installs a floor of its own.
    // Writing over it would strand that block for the life of the
    // process, one per registered thread. The two memory reserves are
    // safe from the same re-entry for a reason this cell does not have:
    // their `RefCell` is borrowed across the draw, so the inner call
    // refuses and returns.
    if !q.floor.get().is_null() {
        crate::memory::critical::give_back(block);
        return q.floor.get();
    }

    q.floor.set(block);
    block
}

/// Draw the floor of a thread the runtime never registered, and abort
/// when it cannot be drawn.
///
/// Two refusals answer the same way, because there is no continuation
/// from here that keeps the root: [`crate::refcount::ENROLLED`] is set
/// before this call and nothing unsets it, so an enrolment that returned
/// without an entry would be Y6's permanent miss with the bit left
/// standing. The abort is the funded class's last resort, reached
/// one door earlier than [`escrow`]'s own (`rfc/dev/DECISIONS.md`, "the
/// escrow's floor is allocator-issued").
///
/// **Asking whether the exit will run is also what arms it**
/// (`crate::memory::heap::thread_exit_will_run`), which is what this
/// thread needs: nothing else has registered a guard for it, and without
/// one the floor would be a block the process never sees again. The
/// registration is a TLS destructor, and its first touch on this
/// platform can end the process rather than report — the same edge
/// [`crate::memory::critical::draw`] stands on two lines below, and the
/// same one this call is about to take anyway (`dev/DECISIONS.md`, "what
/// the first touch of a thread-local with drop glue may cost").
fn draw_floor_or_abort(q: &Queue) -> *mut BlockHeader {
    if !crate::memory::heap::thread_exit_will_run() {
        // Past `ll_thread_exit`, with nothing left to run another: the
        // floor would go back to no one.
        std::process::abort();
    }

    let floor = draw_floor(q);
    if floor.is_null() {
        std::process::abort();
    }

    floor
}

/// Take this thread's floor, and report whether it has one.
///
/// `false` is the thread that never starts: the floor is the one stock a
/// later poll cannot make good, because the guarantee it carries — that
/// an enrolment cannot fail — would be suspended between birth and that
/// poll. [`crate::memory::heap::ll_thread_init`] calls it before its
/// best-effort fills and reports the refusal to its own caller.
pub(crate) fn take_floor() -> bool {
    QUEUE.with(|q| !draw_floor(q).is_null())
}

/// Give the floor back, leaving the thread without one.
///
/// Called by `memory::heap::retire_the_journal` after [`drain`], and by
/// nothing else: the floor is per life, while [`drain`] is also how a
/// test starts from a known queue — a live thread stripped of its floor
/// there would draw a second one at its next enrolment and hold two.
///
/// Through [`crate::memory::critical::give_back`], the route the segments
/// take, so a reserve below capacity is refilled before the pool sees
/// anything.
pub(crate) fn release_floor() {
    QUEUE.with(|q| {
        let floor = q.floor.replace(std::ptr::null_mut());
        if floor.is_null() {
            return;
        }

        crate::memory::critical::give_back(floor);
    });
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
            // The floor exists wherever the count is above zero, one
            // having been drawn before the first entry was written.
            let entity = unsafe { entries(q.floor.get()).add(escrowed).read() };
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

            // The count is read again after the draw, and a full pair
            // sends the block straight back: the record `BlockPool::get`
            // raises can run `ll_thread_init` on this thread, and that
            // call fills these same cells, so an index taken before the
            // draw would be past the end of the array
            // ([`draw_floor`] carries the same re-entry and why).
            let held = q.held.get();
            if held == SPARE_SEGMENTS {
                crate::memory::critical::give_back(block);
                return true;
            }

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

        // The escrow empties by its count, which is the only bound on the
        // floor's contents exactly as `filled` is the only bound on the
        // live segment's. The floor itself stays: it belongs to the
        // thread's life rather than to the queue's contents, and
        // [`release_floor`] is what ends that life.
        q.escrowed.set(0);
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

/// This thread's floor, or null when it holds none. One block, out of
/// the pool for the thread's whole life, so an exact `blocks_out` names
/// it.
#[cfg(test)]
pub(crate) fn floor() -> *mut BlockHeader {
    QUEUE.with(|q| q.floor.get())
}

/// Fill the live segment to capacity with `filler`, so the next
/// enrolment overflows.
///
/// The shorthand exists because the honest way to reach the overflow is
/// 8160 releases, which is a fixture rather than a test: the branch it
/// reaches is three lines and the entries before it prove nothing about
/// them. **It writes the entries rather than only the count**, because
/// the count is what bounds a segment's contents and a test that lied
/// about it would hand a reader applying the corpse rule 8159 recycled
/// words to dereference.
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
