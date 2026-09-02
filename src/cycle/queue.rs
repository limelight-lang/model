//! The per-thread candidate queue: where a non-final decrement leaves a
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
//! # The three storage paths
//!
//! A candidate is written through exactly one of them, tried in order:
//!
//! 1. the write segment, while it has room;
//! 2. a segment taken from a spare cell, or from the critical reserve
//!    with both cells empty, which then becomes the write segment;
//! 3. the base block's bounded overflow buffer, which cannot refuse.
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
//! allocation paths dispense (`rfc/model/gc/cycle/questions.md`, Y12
//! clause 3), and the queue is a chain of them threaded through
//! [`BlockHeader::next`] — the same use the shadow arena makes of that
//! field, and the reason it exists.
//!
//! ```text
//! write ──next──▶ full ──next──▶ full ──next──▶ null
//!  │               │
//!  │               └─ 8160 entries, by construction: a segment leaves
//!  │                  the write position only when it is full
//!  └─ `write_len` entries, held in the base block's control line rather
//!     than in this block
//! ```
//!
//! **The write segment's fill is stored beside the chain and not inside
//! the block, and it is the only bound on any segment's contents.** While
//! the owner is the only one moving segments, a full segment is the only
//! one that leaves the write position, so every segment behind the head
//! holds exactly [`SEGMENT_CAPACITY`] entries and the chain needs no
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
//! Candidate registration sits on the release path, so the write may not
//! allocate, lock or copy (Y12 clause 3). Growth is therefore a pointer
//! swap into a segment somebody else took from the pool: the thread
//! holds **two spares** in cells it fills at thread init and at every
//! safepoint poll, both through the ordinary allocation path. With both
//! cells empty the growth draws the critical reserve
//! (`memory::critical`), which is the draw clause 6 funds. **Reserve mode
//! itself is not built here** — clause 6 asks that the runtime stay in it
//! until every queued root has been walked, and nothing in this crate
//! carries such a state, the poll refilling the reserve unconditionally
//! at the next safepoint. What exists is the draw and the arming.
//!
//! # Why a registration cannot fail
//!
//! Edmond ruled on 2026-08-28 that nothing may be lost, so below the
//! reserve sits a tier that cannot refuse: the **overflow buffer**, whose
//! storage is the **base block** — one 64 KiB pool block the thread holds
//! for one init→exit life and writes entries into directly. A refused
//! entry is stored there by a store and an increment, which keeps clause
//! 3's three prohibitions through the last tier, and
//! [`register_candidate`] therefore answers nothing — it has no failure to
//! report.
//!
//! **What makes the storage certain is the draw, not the address space.**
//! The overflow buffer was a fixed array in this same thread-local until
//! 2026-08-28, and it was almost the whole of the crate's
//! zero-initialised TLS image, which every thread pays at birth whether
//! it registers a candidate or not: `.tbss` measures 472 bytes with the
//! base block on 2026-09-01 against the 65 784 measured without it on
//! 2026-08-29 (`dev/BENCHMARKS.md`, both entries; the figure moves with
//! the toolchain and it is the ratio the two arms establish). The base
//! block is drawn instead, at thread init and before the best-effort
//! fills, and its refusal is a thread that never starts
//! (`rfc/dev/DECISIONS.md`, "the baseline overflow segment is allocator-issued",
//! which is this block). The invariant every later tier rests on comes
//! out of that coupling: every registered thread has a base block,
//! because a thread whose base block was refused is a thread the runtime
//! never registered.
//!
//! Entity work also reaches threads the runtime never registered —
//! self-initialising allocation, a releaser-only FFI consumer — and such
//! a thread draws its base block at its first registration instead,
//! through the ordinary allocation path ([`append_with_new_segment`]).
//! That draw refusing aborts, which is the funded class's last resort
//! reached one step earlier than the overflow buffer's own bound below.
//!
//! The overflow buffer is emptied at the next safepoint poll, which is
//! also where the thread does what the ruling asks: collect, or wait for
//! the collector. Nothing happens inside `ll_release`, and that is not
//! timidity — a collection mid-mutation walks a stale edge and frees a
//! live object (`rfc/model/gc/cycle/questions.md`, Y14, "Where it fires,
//! and where it must not"). The overflow buffer is what carries the root
//! from the refusal to the first lawful instant.
//!
//! After the base block's one 64-byte control line it holds 8,152
//! entries, eight fewer than an ordinary segment. The between-polls
//! guarantee is therefore derived from that smaller capacity, not
//! borrowed from the segment shape. It quantifies over loops the compiler
//! emits, and one loop it does not —
//! `ll_release_vector`, whose count is the caller's — broke it, so that
//! loop now carries a poll of its own every [`POLL_STRIDE`] iterations.
//! Filling the overflow buffer therefore takes sustained pool refusal
//! rather than a long run.
//!
//! The write segment is a cell too: a thread holds none until its first
//! registration, which finds no room by construction and takes the growth
//! path. So a thread that registers no candidate holds its base block and
//! two spare segments rather than three segments, and the empty-queue
//! case needs no arm of its own.
//!
//! # The other block a thread holds for its whole life
//!
//! The collection workspace is the arena's memory and this module's cell: its
//! address lives in the base block's control line rather than in a
//! thread-local of its own, which is what the line reserved a word for. It is
//! drawn at the thread's first collection, not at init, and goes back beside
//! the base block at exit ([`lend_workspace_base`]).
//!
//! # What the poll does for this module
//!
//! Three things, and [`crate::gc::ll_gc_maybe_collect`] does them in
//! order. It refills the spare cells, asking [`needs_spares`] — the count
//! itself, never a flag a draw sets, because a thread whose fill at init
//! was refused has never drawn and would never be asked again. It then
//! drains the overflow buffer into the queue, which is why the refill
//! comes first. And it fires a collection when `gc::take_due` answers
//! true, which a reserve draw or an overflow append arms.
use std::cell::Cell;

use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockHeader};
use crate::memory::gc_metadata;
use crate::refcount::RcHeader;

/// Entries one segment holds. Every segment but the write segment holds
/// exactly this many, which is what lets the chain carry no length.
pub(crate) const SEGMENT_CAPACITY: usize = BLOCK_PAYLOAD / size_of::<*mut RcHeader>();

/// Entries the overflow buffer holds, after the base block's
/// manager-owned control line.
///
/// The capacity is the 65,280-byte payload less one 64-byte
/// [`OwnerCycleState`]: 8,152 pointers. [`POLL_STRIDE`] is derived from
/// this figure and statically checked, so the control migration cannot
/// silently leave the old between-polls bound in force.
pub(crate) const OVERFLOW_CAPACITY: usize =
    (BLOCK_PAYLOAD - size_of::<OwnerCycleState>()) / size_of::<*mut RcHeader>();

/// Iterations a runtime-owned bulk loop may run between two safepoint
/// polls of its own.
///
/// Half the overflow buffer, so that a loop obeying it can never fill the
/// buffer between two of its polls whatever the compiler's own bound turns
/// out to be. The loop that needs it is `object::ll_release_vector`, whose
/// count is the caller's and whose body the compiler never sees inside:
/// without a poll of its own it registers candidates without bound and
/// reaches the abort below with memory free (`rfc/dev/DECISIONS.md`,
/// "a runtime loop carries the poll contract it broke").
pub(crate) const POLL_STRIDE: usize = OVERFLOW_CAPACITY / 2;

/// Spare segments a thread keeps ahead of the next growth.
///
/// Two, which covers the two consumptions one interval between polls can
/// hold: one growth, and one in-line collection whose own request to
/// the pool was refused (`rfc/model/gc/cycle/questions.md`, Y12
/// clause 3). Beyond the two the critical reserve answers, which is what
/// it is for.
pub(crate) const SPARE_SEGMENTS: usize = 2;

/// A thread's queue and the spares behind it, resident in the base block.
///
/// Cells rather than a `RefCell`: the candidate write is the hottest
/// path in the runtime and a borrow flag on it buys nothing, the queue
/// having one writer by contract and no path that re-enters it. Nothing
/// here has drop glue, so thread exit frees it by hand
/// ([`release_queue_segments`]) rather than through a destructor whose order is
/// unspecified (`memory::heap::ll_thread_exit`).
#[repr(C, align(64))]
struct OwnerCycleState {
    /// The segment being written, or null before the first registration.
    /// The rest of the chain hangs off its [`BlockHeader::next`].
    write_segment: Cell<*mut BlockHeader>,
    /// Entries written into [`OwnerCycleState::write_segment`]. Meaningless
    /// when null.
    write_len: Cell<usize>,
    /// Segments taken ahead of the next growth, `spare_count` of them valid.
    spares: [Cell<*mut BlockHeader>; SPARE_SEGMENTS],
    spare_count: Cell<usize>,
    /// Entries in the base block no allocation path could fund a segment
    /// for, the oldest first. The tier that cannot refuse, so that a
    /// candidate registration cannot fail (`rfc/dev/DECISIONS.md`, "an
    /// enrolment cannot fail").
    overflow_len: Cell<usize>,
    /// This thread's collection workspace, in three states: null before the
    /// thread's first collection, the block's address while the workspace is
    /// idle, and that address with [`WORKSPACE_LENT`] set while an arena is
    /// bumping in it ([`lend_workspace_base`]).
    workspace_base: Cell<*mut BlockHeader>,
    /// Reserved for S36.12/S37.4's cold lane/phase descriptor.
    _future_cold_state: Cell<usize>,
}

thread_local! {
    /// Non-owning locator only. The state and every pointer it owns are
    /// stored in the manager-issued base block to which this points.
    static OWNER_STATE: Cell<*mut OwnerCycleState> = const { Cell::new(std::ptr::null_mut()) };
}

const _: () = assert!(size_of::<OwnerCycleState>() == 64);
const _: () = assert!(align_of::<OwnerCycleState>() == 64);
const _: () = assert!(POLL_STRIDE * 2 <= OVERFLOW_CAPACITY);
const _: () = assert!(SEGMENT_CAPACITY > OVERFLOW_CAPACITY);

impl OwnerCycleState {
    const fn new() -> Self {
        Self {
            write_segment: Cell::new(std::ptr::null_mut()),
            write_len: Cell::new(0),
            spares: [const { Cell::new(std::ptr::null_mut()) }; SPARE_SEGMENTS],
            spare_count: Cell::new(0),
            overflow_len: Cell::new(0),
            workspace_base: Cell::new(std::ptr::null_mut()),
            _future_cold_state: Cell::new(0),
        }
    }
}

#[inline]
fn owner_state() -> *mut OwnerCycleState {
    OWNER_STATE.with(Cell::get)
}

#[inline]
unsafe fn owner_state_ref<'a>(state: *mut OwnerCycleState) -> &'a OwnerCycleState {
    unsafe { &*state }
}

#[inline]
fn queue_base_of(state: *mut OwnerCycleState) -> *mut BlockHeader {
    BlockHeader::of_ptr(state as *const u8)
}

/// Where an ordinary segment's entries begin. The base block has a separate
/// address calculation because its first cache line is owner control.
#[inline]
fn segment_entries(segment: *mut BlockHeader) -> *mut *mut RcHeader {
    BlockHeader::payload_start(segment) as *mut *mut RcHeader
}

/// Where the base block's overflow buffer begins, one control line past its
/// payload.
///
/// Every entry this answers is outside the control line, so `state` must
/// carry the provenance of the whole base block — the form
/// [`try_ensure_queue_base`] produces and [`OWNER_STATE`] holds.
#[inline]
fn overflow_entries(state: *mut OwnerCycleState) -> *mut *mut RcHeader {
    unsafe { (state as *mut u8).add(size_of::<OwnerCycleState>()) as *mut *mut RcHeader }
}

/// Put an entity in this thread's queue.
///
/// The caller has already set [`crate::refcount::CANDIDATE_BIT`] in the
/// entity's flags, which Y12 clause 4 requires to happen before the
/// write: a bit set afterwards lets a second decrement register the same
/// entity twice in the window between the two.
///
/// **It cannot fail, and answers nothing.** Every allocation path refusing
/// writes the entry to the overflow buffer instead, because a root that
/// leaves no entry behind is a garbage ring no later collection can name,
/// registration being edge-triggered (`rfc/model/gc/cycle/questions.md`,
/// Y6), and
/// Edmond ruled on 2026-08-28 that nothing may be lost.
///
/// # Safety
/// `entity` points to a live heap entity beginning with `RcHeader`, and
/// stays live at least until this thread's next safepoint.
pub(crate) unsafe fn register_candidate(entity: *mut RcHeader) {
    let mut state = owner_state();
    if state.is_null() {
        state = ensure_queue_base_or_abort();
    }
    let q = unsafe { owner_state_ref(state) };
    let write_segment = q.write_segment.get();
    let write_len = q.write_len.get();

    if write_segment.is_null() || write_len == SEGMENT_CAPACITY {
        unsafe { append_with_new_segment(state, entity) };
        return;
    }

    unsafe { segment_entries(write_segment).add(write_len).write(entity) };
    q.write_len.set(write_len + 1);
}

/// The growth path: put a fresh segment in the write position and write
/// the entry into it.
///
/// The full segment stays reachable through the fresh one's
/// [`BlockHeader::next`], so growth links and never copies. No allocation
/// path funding one writes the entry to the overflow buffer, which is why
/// this answers nothing either.
unsafe fn append_with_new_segment(state: *mut OwnerCycleState, entity: *mut RcHeader) {
    // `register_candidate` established the base block before reaching here.
    // Drawing it at the first refusal would be too late: every other
    // allocation path would already have found the pool empty.
    let q = unsafe { owner_state_ref(state) };
    let full = q.write_segment.get();

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
            // already takes one step earlier, at its base block
            // (`dev/DECISIONS.md`, "what the first touch of a
            // thread-local with drop glue may cost").
            let block = gc_metadata::adopt(crate::memory::critical::draw());
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
        unsafe { append_to_overflow(state, entity) };
        // The overflow append arms on its own: the refill the poll
        // performs is unconditional, so what the arming buys here is the
        // fire, not the cells.
        crate::gc::arm();
        return;
    }

    unsafe { (*fresh).next = full };
    if !full.is_null() {
        // The segment leaving the write position holds its whole payload,
        // and from this instant that payload is in use. The assertion
        // pins the premise rather than trusting it:
        // `release_queue_segments` discharges a
        // payload for every segment behind the head, so a part-filled one
        // there would discharge bytes nothing charged. The invariant is
        // today's single mover's, and the module doc names where it ends.
        debug_assert_eq!(q.write_len.get(), SEGMENT_CAPACITY);
        gc_metadata::charge(BLOCK_PAYLOAD);
    }

    unsafe { segment_entries(fresh).write(entity) };
    q.write_segment.set(fresh);
    q.write_len.set(1);
}

/// The tier below the reserve: store the entry where nothing can refuse
/// it, and count it.
///
/// Aborts when the overflow buffer is full, which is the last resort the funded
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
/// `state` is this thread's base-block pointer as [`OWNER_STATE`] holds it,
/// carrying the provenance of the whole block. A pointer reconstructed from
/// a `&OwnerCycleState` covers the control line alone and cannot address
/// the overflow buffer behind it.
unsafe fn append_to_overflow(state: *mut OwnerCycleState, entity: *mut RcHeader) {
    let q = unsafe { owner_state_ref(state) };
    let overflow_len = q.overflow_len.get();
    if overflow_len == OVERFLOW_CAPACITY {
        // Nothing to report it through: `ll_release` holds no frame, and
        // the poll that would raise is what this thread has not reached.
        std::process::abort();
    }

    // The control pointer is inside this thread's non-null base block,
    // which `register_candidate` established before taking any growth path.
    unsafe { overflow_entries(state).add(overflow_len).write(entity) };
    q.overflow_len.set(overflow_len + 1);
    gc_metadata::charge(size_of::<*mut RcHeader>());
}

/// Give this thread a base block, or answer null when the manager refuses.
/// The returned pointer is the control plane inside that block.
fn try_ensure_queue_base() -> *mut OwnerCycleState {
    let present = owner_state();
    if !present.is_null() {
        return present;
    }

    let block = gc_metadata::acquire();
    if block.is_null() {
        return std::ptr::null_mut();
    }

    // **The TLS pointer is read again rather than trusted across the draw.**
    // `BlockPool::get` raises a record, and a thread's first record runs
    // `ll_thread_init` from inside the journal (`journal::mod`, "A thread
    // can reach a record site without ever having initialised the
    // runtime") — which comes back here and installs a base block of its own.
    // Writing over it would strand that block for the life of the
    // process, one per registered thread. The two memory reserves are
    // safe from the same re-entry for a reason this cell does not have:
    // their `RefCell` is borrowed across the draw, so the inner call
    // refuses and returns.
    let installed = owner_state();
    if !installed.is_null() {
        gc_metadata::release_to_critical(block);
        return installed;
    }

    let state = BlockHeader::payload_start(block) as *mut OwnerCycleState;
    unsafe { state.write(OwnerCycleState::new()) };
    // Publish last: any re-entry after this point must see fully initialised
    // control and will use this exact base block.
    OWNER_STATE.with(|cell| cell.set(state));
    // One charge per base block: the re-entrant frame above finds a block
    // already published, gives its own block back and returns without
    // reaching this line.
    gc_metadata::charge(size_of::<OwnerCycleState>());
    state
}

/// Draw the base block of a thread the runtime never registered, and abort
/// when it cannot be drawn.
///
/// Two refusals answer the same way, because there is no continuation
/// from here that keeps the root: [`crate::refcount::CANDIDATE_BIT`] is set
/// before this call and nothing unsets it, so a registration that returned
/// without an entry would be Y6's permanent miss with the bit left
/// standing. The abort is the funded class's last resort, reached one step
/// earlier than [`append_to_overflow`]'s own
/// (`rfc/dev/DECISIONS.md`, "the baseline overflow segment is allocator-issued", which is
/// this base block).
///
/// **Asking whether the exit will run is also what arms it**
/// (`crate::memory::heap::thread_exit_will_run`), which is what this
/// thread needs: nothing else has registered a guard for it, and without
/// one the base block would be a block the process never sees again. The
/// registration is a TLS destructor, and its first touch on this
/// platform can end the process rather than report — the same edge
/// [`crate::memory::critical::draw`] meets two lines below, and the
/// same one this call is about to take anyway (`dev/DECISIONS.md`, "what
/// the first touch of a thread-local with drop glue may cost").
fn ensure_queue_base_or_abort() -> *mut OwnerCycleState {
    if !crate::memory::heap::thread_exit_will_run() {
        // Past `ll_thread_exit`, with nothing left to run another: the
        // base block would go back to no one.
        std::process::abort();
    }

    let state = try_ensure_queue_base();
    if state.is_null() {
        std::process::abort();
    }

    state
}

/// Ensure this thread has a base block, and report whether it has one.
///
/// `false` is the thread that never starts: the base block is the one stock
/// a later poll cannot make good, because the guarantee it carries — that a
/// candidate registration cannot fail — would be suspended between birth and
/// that poll. [`crate::memory::heap::ll_thread_init`] calls it before its
/// best-effort fills and reports the refusal to its own caller.
pub(crate) fn initialize_queue_base() -> bool {
    !try_ensure_queue_base().is_null()
}

/// Set in [`OwnerCycleState::workspace_base`] while an arena holds the block.
/// A block address never carries it, blocks being 64 KiB-aligned.
const WORKSPACE_LENT: usize = 1;

/// Hand this thread's collection workspace to an opening arena, drawing it on
/// the thread's first collection.
///
/// **Null on two conditions**: the pool refused the draw, and a thread the
/// runtime never registered, which holds no state for this cell to live in.
/// What the caller does with null is [`crate::cycle::arena`]'s.
///
/// The block is the thread's from here until [`release_queue_base`], and the
/// caller borrows it rather than owning it (`dev/DECISIONS.md`, "the workspace
/// base is drawn at the first collection, not at thread init").
///
/// **The ordinary allocation path and nothing else.** A reserve block that
/// became a bump arena for the life of a thread would be the reserve spent as
/// ordinary memory (`rfc/model/memory/critical-reserve.md`, "Allocation
/// paths"), and the pressure path has the critical reserve for the growth
/// past this block instead (`crate::cycle::arena`).
///
/// A second lend before the first is returned fails an assertion rather than
/// granting the same bytes twice. The release profile ends the process on it;
/// the test profile unwinds, which is what lets a case state the refusal.
pub(crate) fn lend_workspace_base() -> *mut BlockHeader {
    let state = owner_state();
    if state.is_null() {
        return std::ptr::null_mut();
    }

    let q = unsafe { owner_state_ref(state) };
    let installed = q.workspace_base.get();
    assert_eq!(
        installed as usize & WORKSPACE_LENT,
        0,
        "a thread bumps one collection workspace at a time"
    );

    // The draw needs no re-read of the cell across it, unlike the base block's
    // in `try_ensure_queue_base`: the re-entry that guard exists for is a
    // thread's first journal record running `ll_thread_init`, and a thread
    // that already has its control line has recorded already.
    let base = if installed.is_null() {
        gc_metadata::acquire()
    } else {
        installed
    };

    if !base.is_null() {
        q.workspace_base
            .set((base as usize | WORKSPACE_LENT) as *mut BlockHeader);
    }

    base
}

/// Take the workspace back from the arena that is closing, leaving it idle for
/// the thread's next collection.
///
/// **Answers nothing when the thread has no state left.** One sequence reaches
/// that: [`release_queue_base`] has taken the state out of the thread-local and
/// then failed one of its own assertions, and this call is running in the
/// unwind. Asserting here would be a second panic on that path and nothing on
/// any other, and the block is already the pool's business rather than a
/// closing arena's.
pub(crate) fn return_workspace_base(base: *mut BlockHeader) {
    let state = owner_state();
    if state.is_null() {
        return;
    }

    let q = unsafe { owner_state_ref(state) };
    assert_eq!(
        q.workspace_base.get() as usize,
        base as usize | WORKSPACE_LENT,
        "the closing arena returns the block it was lent"
    );
    q.workspace_base.set(base);
}

/// Draw this thread's workspace ahead of any collection, so that a test
/// counting blocks across one counts the collection and not the draw.
///
/// Tests only. The block is out of the pool for the rest of the thread's life
/// either way; what this moves is the instant, from the middle of a test into
/// its fixture.
#[cfg(test)]
pub(crate) fn warm_workspace_base() {
    let base = lend_workspace_base();
    assert!(!base.is_null(), "the pool served this thread's workspace");
    return_workspace_base(base);
}

/// Give back both blocks a thread holds for its whole life — the collection
/// workspace, if it ever collected, and then the base block — leaving the
/// thread without either.
///
/// Called by `memory::heap` at thread exit, after
/// [`release_queue_segments`], and again in `ll_thread_init`'s rollback of a
/// thread whose exit will never run. Both blocks are per life, while
/// the segment release is also how a test starts from a known queue — a
/// running thread stripped of its base block there would draw a second one
/// at its next registration and hold two, and one stripped of its workspace
/// would draw a second at its next collection.
///
/// Through [`crate::memory::critical::give_back`], the route the segments
/// take, so a reserve below capacity is refilled before the pool sees
/// anything.
pub(crate) fn release_queue_base() {
    let state = OWNER_STATE.with(|cell| cell.replace(std::ptr::null_mut()));
    if state.is_null() {
        return;
    }

    let q = unsafe { owner_state_ref(state) };
    assert!(
        q.write_segment.get().is_null(),
        "release follows segment release"
    );
    assert_eq!(q.spare_count.get(), 0, "release follows spare release");
    assert_eq!(q.overflow_len.get(), 0, "release follows overflow release");

    // The workspace goes back ahead of the block whose control line names it,
    // and to the pool rather than through the reserve: what the reserve lent
    // goes back to the reserve, and the reserve never funded this one
    // ([`lend_workspace_base`]).
    let workspace = q.workspace_base.replace(std::ptr::null_mut());
    assert_eq!(
        workspace as usize & WORKSPACE_LENT,
        0,
        "release follows the collection's close"
    );
    gc_metadata::release(workspace);

    gc_metadata::discharge(size_of::<OwnerCycleState>());
    gc_metadata::release_to_critical(queue_base_of(state));
}

/// Move overflow entries back into the queue, as far as the room a poll
/// has just made allows.
///
/// The poll calls it after the cells are refilled and before it fires, and
/// it takes no allocation path of its own: with the cells still empty an
/// entry would be written straight back to the overflow buffer, so the move
/// stops instead and waits for the collection the same poll is about to run.
pub(crate) fn drain_overflow() {
    let state = owner_state();
    if state.is_null() {
        return;
    }
    let q = unsafe { owner_state_ref(state) };
    while q.overflow_len.get() > 0 {
        let write_segment = q.write_segment.get();
        let has_room = !write_segment.is_null() && q.write_len.get() < SEGMENT_CAPACITY;
        if !has_room && q.spare_count.get() == 0 {
            break;
        }

        let overflow_len = q.overflow_len.get() - 1;
        // The base block exists wherever the count is above zero, one
        // having been drawn before the first entry was written.
        let entity = unsafe { overflow_entries(state).add(overflow_len).read() };
        q.overflow_len.set(overflow_len);
        // Per entry rather than once for the run: the re-registration below
        // can fill a segment and charge its payload, and a discharge held to
        // the end would leave the overflow buffer's bytes standing over
        // entries it no longer holds — a high-water figure counting the same
        // memory twice.
        gc_metadata::discharge(size_of::<*mut RcHeader>());
        unsafe { register_candidate(entity) };
    }
}

/// Take one spare, or null when both cells are empty.
#[inline]
fn take_spare(q: &OwnerCycleState) -> *mut BlockHeader {
    let spare_count = q.spare_count.get();
    if spare_count == 0 {
        return std::ptr::null_mut();
    }

    q.spare_count.set(spare_count - 1);
    q.spares[spare_count - 1].replace(std::ptr::null_mut())
}

/// Whether this thread's spare cells are below their stock and want a
/// poll to fill them.
///
/// The count itself rather than a flag a draw sets, which is the rule
/// both memory reserves learned the hard way: a thread whose fill at
/// init was refused holds nothing, has never drawn, and a flag would
/// leave it unasked for the rest of its life (`memory::reserve`,
/// `is_drawn`).
pub(crate) fn needs_spares() -> bool {
    let state = owner_state();
    state.is_null() || unsafe { owner_state_ref(state) }.spare_count.get() < SPARE_SEGMENTS
}

/// Fill the spare cells through the ordinary allocation path, answering
/// false when they could not be filled completely.
///
/// Best-effort by construction, and called where a refusal is already
/// reported by something else: at thread init, where the thread's first
/// allocation returns null, and at the safepoint poll, which comes back.
pub(crate) fn refill_spares() -> bool {
    let state = owner_state();
    if state.is_null() {
        return false;
    }
    let q = unsafe { owner_state_ref(state) };
    while q.spare_count.get() < SPARE_SEGMENTS {
        let block = gc_metadata::acquire();
        if block.is_null() {
            return false;
        }

        // The count is read again after the draw, and a full pair
        // sends the block straight back: the record `BlockPool::get`
        // raises can run `ll_thread_init` on this thread, and that
        // call fills these same cells, so an index taken before the
        // draw would be past the end of the array
        // ([`try_ensure_queue_base`] carries the same re-entry and why).
        let spare_count = q.spare_count.get();
        if spare_count == SPARE_SEGMENTS {
            gc_metadata::release_to_critical(block);
            return true;
        }

        q.spares[spare_count].set(block);
        q.spare_count.set(spare_count + 1);
    }

    true
}

/// Give every segment and every spare back, and leave the queue empty.
///
/// Thread exit calls it in production and the tests call it to start
/// from a known queue: the queue holds pool blocks, and a dying thread
/// must not take them with it.
///
/// **The entries go with the segments — the overflow buffer's too — and
/// their entities keep the candidate bit, which is a permanent miss and not
/// a deferral.** A block
/// with live occupants is handed to the abandoned list and adopted by
/// another thread (`memory::heap::ll_thread_exit`), so the entity
/// outlives its queue carrying a bit that names an entry nobody holds —
/// and [`crate::refcount::CANDIDATE_GATE_MASK`] refuses every later
/// decrement of it, for the life of the process. Clearing the bits here
/// is not available to this step: an entry may name a slot already freed,
/// and reading it to clear a bit would touch returned memory. S39.1 is
/// the step that chooses the fate, and this is the cost it is choosing
/// against.
///
/// Through [`crate::memory::critical::give_back`] rather than straight
/// to the pool, so a reserve below capacity is refilled before the pool
/// sees anything.
pub(crate) fn release_queue_segments() {
    let state = owner_state();
    if state.is_null() {
        return;
    }
    let q = unsafe { owner_state_ref(state) };
    let mut segment = q.write_segment.replace(std::ptr::null_mut());
    let write_len = q.write_len.replace(0);

    // The candidate write charges nothing, so the write segment's fill is
    // the one residue the ledger carries, and this is the transition that
    // ends it. It goes to the high-water figure alone: the bytes are
    // being released in the same breath, and a charge would show another
    // thread a current figure holding a segment that is already gone.
    gc_metadata::mark_peak(write_len * size_of::<*mut RcHeader>());

    // The head is the write segment, whose fill was never published; every
    // segment behind it left that position full and carries its payload.
    let mut left_the_write_position = false;
    while !segment.is_null() {
        let next = unsafe { (*segment).next };
        unsafe { (*segment).next = std::ptr::null_mut() };
        if left_the_write_position {
            gc_metadata::discharge(BLOCK_PAYLOAD);
        }

        left_the_write_position = true;
        gc_metadata::release_to_critical(segment);
        segment = next;
    }

    let spare_count = q.spare_count.replace(0);
    for cell in &q.spares[..spare_count] {
        let block = cell.replace(std::ptr::null_mut());
        gc_metadata::release_to_critical(block);
    }

    // The overflow buffer empties by its count, which is the only bound on
    // the base block's contents exactly as `write_len` is the only bound on
    // the write segment's. The base block itself stays: it belongs to the
    // thread's life rather than to the queue's contents, and
    // [`release_queue_base`] is what ends that life.
    let overflow_len = q.overflow_len.replace(0);
    gc_metadata::discharge(overflow_len * size_of::<*mut RcHeader>());
}

/// Entries this thread's overflow buffer holds.
#[cfg(test)]
pub(crate) fn overflow_len() -> usize {
    let state = owner_state();
    if state.is_null() {
        0
    } else {
        unsafe { owner_state_ref(state) }.overflow_len.get()
    }
}

/// Entries this thread's queue holds, walking the chain.
#[cfg(test)]
pub(crate) fn candidate_count() -> usize {
    let state = owner_state();
    if state.is_null() {
        return 0;
    }
    let q = unsafe { owner_state_ref(state) };
    let write_segment = q.write_segment.get();
    if write_segment.is_null() {
        return 0;
    }

    let mut count = q.write_len.get();
    let mut segment = unsafe { (*write_segment).next };
    while !segment.is_null() {
        count += SEGMENT_CAPACITY;
        segment = unsafe { (*segment).next };
    }

    count
}

/// Segments this thread's queue holds.
#[cfg(test)]
pub(crate) fn segment_count() -> usize {
    let state = owner_state();
    if state.is_null() {
        return 0;
    }
    let q = unsafe { owner_state_ref(state) };
    let mut count = 0;
    let mut segment = q.write_segment.get();
    while !segment.is_null() {
        count += 1;
        segment = unsafe { (*segment).next };
    }

    count
}

/// Spares this thread holds.
#[cfg(test)]
pub(crate) fn spare_count() -> usize {
    let state = owner_state();
    if state.is_null() {
        0
    } else {
        unsafe { owner_state_ref(state) }.spare_count.get()
    }
}

/// This thread's base block, or null when it holds none. One block, out of
/// the pool for the thread's whole life, so an exact `blocks_out` names it.
#[cfg(test)]
pub(crate) fn queue_base() -> *mut BlockHeader {
    let state = owner_state();
    if state.is_null() {
        std::ptr::null_mut()
    } else {
        queue_base_of(state)
    }
}

/// The workspace cell verbatim: null before this thread's first collection,
/// the block while it is idle, and the block with [`WORKSPACE_LENT`] set while
/// an arena holds it.
///
/// Unmasked on purpose. A case that asserts the cell is empty has to be able
/// to see a bit standing over a null block, which is the one wrong state the
/// mask would hide.
#[cfg(test)]
pub(crate) fn workspace_base() -> *mut BlockHeader {
    let state = owner_state();
    if state.is_null() {
        return std::ptr::null_mut();
    }

    unsafe { owner_state_ref(state) }.workspace_base.get()
}

#[cfg(test)]
pub(crate) fn write_segment() -> *mut BlockHeader {
    let state = owner_state();
    if state.is_null() {
        std::ptr::null_mut()
    } else {
        unsafe { owner_state_ref(state) }.write_segment.get()
    }
}

/// Fill the write segment to capacity with `filler`, so the next
/// registration grows the queue.
///
/// The shorthand exists because the honest way to reach the growth path is
/// 8160 releases, which is a fixture rather than a test: the branch it
/// reaches is three lines and the entries before it prove nothing about
/// them. **It writes the entries rather than only the count**, because
/// the count is what bounds a segment's contents and a test that lied
/// about it would hand a reader applying the zero-count rule 8159 recycled
/// words to dereference.
#[cfg(test)]
pub(crate) fn fill_write_segment(filler: *mut RcHeader) {
    let state = owner_state();
    assert!(!state.is_null(), "no queue base block");
    let q = unsafe { owner_state_ref(state) };
    let write_segment = q.write_segment.get();
    assert!(!write_segment.is_null(), "no write segment to fill");
    for index in q.write_len.get()..SEGMENT_CAPACITY {
        unsafe { segment_entries(write_segment).add(index).write(filler) };
    }

    q.write_len.set(SEGMENT_CAPACITY);
}

/// The nth entry of the write segment, counting from the oldest.
#[cfg(test)]
pub(crate) fn write_segment_entry(index: usize) -> *mut RcHeader {
    let state = owner_state();
    assert!(!state.is_null(), "no queue base block");
    let q = unsafe { owner_state_ref(state) };
    let write_segment = q.write_segment.get();
    assert!(!write_segment.is_null(), "no write segment");
    assert!(index < q.write_len.get(), "entry {index} is past the fill");
    unsafe { segment_entries(write_segment).add(index).read() }
}

#[cfg(test)]
mod tests;
