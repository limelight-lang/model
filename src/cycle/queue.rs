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
//!  └─ `filled` entries, held in the queue floor's control line rather
//!     than in this block
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
//! it enrols or not: `.tbss` measures 472 bytes with the floor on
//! 2026-09-01 against the 65 784 measured without it on 2026-08-29
//! (`dev/BENCHMARKS.md`, both entries; the figure moves with the
//! toolchain and it is the ratio the two arms establish). The floor is
//! drawn instead, at thread init and before the best-effort fills, and
//! its refusal is a thread that never starts
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
//! After the floor's one 64-byte control line it holds 8,152 entries,
//! eight fewer than an ordinary segment. The between-polls guarantee is
//! therefore derived from that smaller capacity, not borrowed from the
//! segment shape. It quantifies over loops the compiler emits, and one
//! loop it does not —
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
//! first. And it fires a collection when `gc::take_due` answers
//! true, which a reserve draw or an escrow landing arms.

use std::cell::Cell;

use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockHeader};
use crate::memory::gc_metadata::{self, GcBlockRole};
use crate::refcount::RcHeader;

/// Entries one segment holds. Every segment but the live one holds
/// exactly this many, which is what lets the chain carry no length.
pub(crate) const SEGMENT_CAPACITY: usize = BLOCK_PAYLOAD / size_of::<*mut RcHeader>();

/// Entries the escrow holds after the floor's manager-owned control line.
///
/// The capacity is the 65,280-byte payload less one 64-byte
/// [`OwnerCycleState`]: 8,152 pointers. [`POLL_STRIDE`] is derived from
/// this figure and statically checked, so the control migration cannot
/// silently leave the old between-polls bound in force.
pub(crate) const ESCROW_ENTRIES: usize =
    (BLOCK_PAYLOAD - size_of::<OwnerCycleState>()) / size_of::<*mut RcHeader>();

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

/// A thread's queue and the spares behind it, resident in the floor.
///
/// Cells rather than a `RefCell`: the enrolment write is the hottest
/// path in the runtime and a borrow flag on it buys nothing, the queue
/// having one writer by contract and no path that re-enters it. Nothing
/// here has drop glue, so thread exit frees it by hand
/// ([`drain`]) rather than through a destructor whose order is
/// unspecified (`memory::heap::ll_thread_exit`).
#[repr(C, align(64))]
struct OwnerCycleState {
    /// The segment being written, or null before the first enrolment.
    /// The rest of the chain hangs off its [`BlockHeader::next`].
    live: Cell<*mut BlockHeader>,
    /// Entries written into [`OwnerCycleState::live`]. Meaningless when null.
    filled: Cell<usize>,
    /// Segments taken ahead of an overflow, `held` of them valid.
    spares: [Cell<*mut BlockHeader>; SPARE_SEGMENTS],
    held: Cell<usize>,
    /// Entries in the floor no door could fund a segment for, the oldest
    /// first. The tier that cannot refuse, so that an enrolment cannot
    /// fail (`rfc/dev/DECISIONS.md`, "an enrolment cannot fail").
    escrowed: Cell<usize>,
    /// Reserved for S36.10's persistent base pointer without growing the hot
    /// line or changing the escrow budget again.
    _future_workspace: Cell<*mut BlockHeader>,
    /// Reserved for S36.12/S37.4's cold lane/phase descriptor.
    _future_cold_state: Cell<usize>,
}

thread_local! {
    /// Non-owning locator only. The state and every pointer it owns live in
    /// the manager-issued queue floor to which this points.
    static OWNER: Cell<*mut OwnerCycleState> = const { Cell::new(std::ptr::null_mut()) };
}

const _: () = assert!(size_of::<OwnerCycleState>() == 64);
const _: () = assert!(align_of::<OwnerCycleState>() == 64);
const _: () = assert!(POLL_STRIDE * 2 <= ESCROW_ENTRIES);
const _: () = assert!(SEGMENT_CAPACITY > ESCROW_ENTRIES);

impl OwnerCycleState {
    const fn new() -> Self {
        Self {
            live: Cell::new(std::ptr::null_mut()),
            filled: Cell::new(0),
            spares: [const { Cell::new(std::ptr::null_mut()) }; SPARE_SEGMENTS],
            held: Cell::new(0),
            escrowed: Cell::new(0),
            _future_workspace: Cell::new(std::ptr::null_mut()),
            _future_cold_state: Cell::new(0),
        }
    }
}

#[inline]
fn owner() -> *mut OwnerCycleState {
    OWNER.with(Cell::get)
}

#[inline]
unsafe fn owner_ref<'a>(owner: *mut OwnerCycleState) -> &'a OwnerCycleState {
    unsafe { &*owner }
}

#[inline]
fn floor_of(owner: *mut OwnerCycleState) -> *mut BlockHeader {
    BlockHeader::of_ptr(owner as *const u8)
}

/// Where an ordinary segment's entries begin. The floor has a separate
/// address calculation because its first cache line is owner control.
#[inline]
fn entries(segment: *mut BlockHeader) -> *mut *mut RcHeader {
    BlockHeader::payload_start(segment) as *mut *mut RcHeader
}

/// Where the floor's escrow begins, one control line past its payload.
///
/// Every entry this answers lies outside the control line, so `owner`
/// must carry the provenance of the whole floor block — the form
/// [`draw_floor`] produces and [`OWNER`] holds.
#[inline]
fn escrow_entries(owner: *mut OwnerCycleState) -> *mut *mut RcHeader {
    unsafe { (owner as *mut u8).add(size_of::<OwnerCycleState>()) as *mut *mut RcHeader }
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
    let mut owner = owner();
    if owner.is_null() {
        owner = draw_floor_or_abort();
    }
    let q = unsafe { owner_ref(owner) };
    let live = q.live.get();
    let filled = q.filled.get();

    if live.is_null() || filled == SEGMENT_CAPACITY {
        unsafe { grow_and_write(owner, entity) };
        return;
    }

    unsafe { entries(live).add(filled).write(entity) };
    q.filled.set(filled + 1);
}

/// The overflow path: put a fresh segment in the live position and write
/// the entry into it.
///
/// The full segment stays reachable through the fresh one's
/// [`BlockHeader::next`], so growth links and never copies. No door
/// funding one puts the entry in the escrow, which is why this answers
/// nothing either.
unsafe fn grow_and_write(owner: *mut OwnerCycleState, entity: *mut RcHeader) {
    // `enrol` established the floor before reaching here. Drawing it at
    // the first refusal would be too late: every other door would already
    // have found the pool empty.
    let q = unsafe { owner_ref(owner) };
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
            let block =
                gc_metadata::adopt(crate::memory::critical::draw(), GcBlockRole::QueueSegment);
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
        unsafe { escrow(owner, entity) };
        // The escrow landing arms on its own: the refill the poll
        // performs is unconditional, so what the arming buys here is the
        // fire, not the cells.
        crate::gc::arm();
        return;
    }

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
///
/// # Safety
/// `owner` is this thread's floor pointer as [`OWNER`] holds it, carrying
/// the provenance of the whole block. A pointer reconstructed from a
/// `&OwnerCycleState` covers the control line alone and cannot address
/// the escrow behind it.
unsafe fn escrow(owner: *mut OwnerCycleState, entity: *mut RcHeader) {
    let q = unsafe { owner_ref(owner) };
    let escrowed = q.escrowed.get();
    if escrowed == ESCROW_ENTRIES {
        // Nothing to report it through: `ll_release` holds no frame, and
        // the poll that would raise is what this thread has not reached.
        std::process::abort();
    }

    // The control pointer is inside this thread's non-null floor, which
    // `enrol` established before taking any growth door.
    unsafe { escrow_entries(owner).add(escrowed).write(entity) };
    q.escrowed.set(escrowed + 1);
}

/// Put a floor under this thread, or answer null when the manager refuses.
/// The returned pointer is the control plane inside that floor.
fn draw_floor() -> *mut OwnerCycleState {
    let present = owner();
    if !present.is_null() {
        return present;
    }

    let block = gc_metadata::acquire(GcBlockRole::QueueFloor);
    if block.is_null() {
        return std::ptr::null_mut();
    }

    // **The TLS pointer is read again rather than trusted across the draw.**
    // `BlockPool::get` raises a record, and a thread's first record runs
    // `ll_thread_init` from inside the journal (`journal::mod`, "A thread
    // can reach a record site without ever having initialised the
    // runtime") — which comes back here and installs a floor of its own.
    // Writing over it would strand that block for the life of the
    // process, one per registered thread. The two memory reserves are
    // safe from the same re-entry for a reason this cell does not have:
    // their `RefCell` is borrowed across the draw, so the inner call
    // refuses and returns.
    let installed = owner();
    if !installed.is_null() {
        gc_metadata::release_to_critical(block, GcBlockRole::QueueFloor);
        return installed;
    }

    let state = BlockHeader::payload_start(block) as *mut OwnerCycleState;
    unsafe { state.write(OwnerCycleState::new()) };
    // Publish last: any re-entry after this point must see fully initialised
    // control and will use this exact floor.
    OWNER.with(|owner| owner.set(state));
    state
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
fn draw_floor_or_abort() -> *mut OwnerCycleState {
    if !crate::memory::heap::thread_exit_will_run() {
        // Past `ll_thread_exit`, with nothing left to run another: the
        // floor would go back to no one.
        std::process::abort();
    }

    let owner = draw_floor();
    if owner.is_null() {
        std::process::abort();
    }

    owner
}

/// Take this thread's floor, and report whether it has one.
///
/// `false` is the thread that never starts: the floor is the one stock a
/// later poll cannot make good, because the guarantee it carries — that
/// an enrolment cannot fail — would be suspended between birth and that
/// poll. [`crate::memory::heap::ll_thread_init`] calls it before its
/// best-effort fills and reports the refusal to its own caller.
pub(crate) fn take_floor() -> bool {
    !draw_floor().is_null()
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
    let owner = OWNER.with(|owner| owner.replace(std::ptr::null_mut()));
    if owner.is_null() {
        return;
    }

    let q = unsafe { owner_ref(owner) };
    assert!(q.live.get().is_null(), "release follows queue drain");
    assert_eq!(q.held.get(), 0, "release follows spare drain");
    assert_eq!(q.escrowed.get(), 0, "release follows escrow drain");
    gc_metadata::release_to_critical(floor_of(owner), GcBlockRole::QueueFloor);
}

/// Move escrowed entries back into the queue, as far as the room a poll
/// has just made allows.
///
/// The poll calls it after the cells are refilled and before it fires,
/// and it takes no door of its own: with the cells still empty an entry
/// would land straight back in the escrow, so the drain stops instead and
/// waits for the collection the same poll is about to run.
pub(crate) fn drain_escrow() {
    let owner = owner();
    if owner.is_null() {
        return;
    }
    let q = unsafe { owner_ref(owner) };
    while q.escrowed.get() > 0 {
        let live = q.live.get();
        let has_room = !live.is_null() && q.filled.get() < SEGMENT_CAPACITY;
        if !has_room && q.held.get() == 0 {
            return;
        }

        let escrowed = q.escrowed.get() - 1;
        // The floor exists wherever the count is above zero, one
        // having been drawn before the first entry was written.
        let entity = unsafe { escrow_entries(owner).add(escrowed).read() };
        q.escrowed.set(escrowed);
        unsafe { enrol(entity) };
    }
}

/// Take one spare, or null when both cells are empty.
#[inline]
fn take_spare(q: &OwnerCycleState) -> *mut BlockHeader {
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
    let owner = owner();
    owner.is_null() || unsafe { owner_ref(owner) }.held.get() < SPARE_SEGMENTS
}

/// Fill the spare cells through the ordinary door, answering false when
/// they could not be filled completely.
///
/// Best-effort by construction, and called where a refusal is already
/// reported by something else: at thread init, where the thread's first
/// allocation returns null, and at the safepoint poll, which comes back.
pub(crate) fn replenish() -> bool {
    let owner = owner();
    if owner.is_null() {
        return false;
    }
    let q = unsafe { owner_ref(owner) };
    while q.held.get() < SPARE_SEGMENTS {
        let block = gc_metadata::acquire(GcBlockRole::QueueSegment);
        if block.is_null() {
            return false;
        }

        // The count is read again after the draw, and a full pair
        // sends the block straight back: the record `BlockPool::get`
        // raises can run `ll_thread_init` on this thread, and that
        // call fills these same cells, so an index taken before the
        // draw would be past the end of the array
        // ([`draw_floor`] carries the same re-entry and why).
        let held = q.held.get();
        if held == SPARE_SEGMENTS {
            gc_metadata::release_to_critical(block, GcBlockRole::QueueSegment);
            return true;
        }

        q.spares[held].set(block);
        q.held.set(held + 1);
    }

    true
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
    let owner = owner();
    if owner.is_null() {
        return;
    }
    let q = unsafe { owner_ref(owner) };
    let mut segment = q.live.replace(std::ptr::null_mut());
    q.filled.set(0);

    while !segment.is_null() {
        let next = unsafe { (*segment).next };
        unsafe { (*segment).next = std::ptr::null_mut() };
        gc_metadata::release_to_critical(segment, GcBlockRole::QueueSegment);
        segment = next;
    }

    let held = q.held.replace(0);
    for cell in &q.spares[..held] {
        let block = cell.replace(std::ptr::null_mut());
        gc_metadata::release_to_critical(block, GcBlockRole::QueueSegment);
    }

    // The escrow empties by its count, which is the only bound on the
    // floor's contents exactly as `filled` is the only bound on the
    // live segment's. The floor itself stays: it belongs to the
    // thread's life rather than to the queue's contents, and
    // [`release_floor`] is what ends that life.
    q.escrowed.set(0);
}

/// Entries this thread's escrow is holding.
#[cfg(test)]
pub(crate) fn escrowed_count() -> usize {
    let owner = owner();
    if owner.is_null() {
        0
    } else {
        unsafe { owner_ref(owner) }.escrowed.get()
    }
}

/// Entries this thread's queue holds, walking the chain.
#[cfg(test)]
pub(crate) fn enrolled_count() -> usize {
    let owner = owner();
    if owner.is_null() {
        return 0;
    }
    let q = unsafe { owner_ref(owner) };
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
}

/// Segments this thread's queue holds.
#[cfg(test)]
pub(crate) fn segment_count() -> usize {
    let owner = owner();
    if owner.is_null() {
        return 0;
    }
    let q = unsafe { owner_ref(owner) };
    let mut count = 0;
    let mut segment = q.live.get();
    while !segment.is_null() {
        count += 1;
        segment = unsafe { (*segment).next };
    }

    count
}

/// Spares this thread holds.
#[cfg(test)]
pub(crate) fn spares_held() -> usize {
    let owner = owner();
    if owner.is_null() {
        0
    } else {
        unsafe { owner_ref(owner) }.held.get()
    }
}

/// This thread's floor, or null when it holds none. One block, out of
/// the pool for the thread's whole life, so an exact `blocks_out` names
/// it.
#[cfg(test)]
pub(crate) fn floor() -> *mut BlockHeader {
    let owner = owner();
    if owner.is_null() {
        std::ptr::null_mut()
    } else {
        floor_of(owner)
    }
}

#[cfg(test)]
pub(crate) fn live_segment() -> *mut BlockHeader {
    let owner = owner();
    if owner.is_null() {
        std::ptr::null_mut()
    } else {
        unsafe { owner_ref(owner) }.live.get()
    }
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
    let owner = owner();
    assert!(!owner.is_null(), "no queue floor");
    let q = unsafe { owner_ref(owner) };
    let live = q.live.get();
    assert!(!live.is_null(), "no live segment to fill");
    for index in q.filled.get()..SEGMENT_CAPACITY {
        unsafe { entries(live).add(index).write(filler) };
    }

    q.filled.set(SEGMENT_CAPACITY);
}

/// The nth entry of the live segment, counting from the oldest.
#[cfg(test)]
pub(crate) fn live_entry(index: usize) -> *mut RcHeader {
    let owner = owner();
    assert!(!owner.is_null(), "no queue floor");
    let q = unsafe { owner_ref(owner) };
    let live = q.live.get();
    assert!(!live.is_null(), "no live segment");
    assert!(index < q.filled.get(), "entry {index} is past the fill");
    unsafe { entries(live).add(index).read() }
}

#[cfg(test)]
mod tests;
