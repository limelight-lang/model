//! The per-thread candidate queue: where a non-final decrement leaves a
//! cycle candidate for a later trace to read.
//!
//! The contract is `rfc/model/gc/cycle/questions.md`, Y12, and every
//! clause of it is normative here. What this module builds is the
//! **owner's side** of that contract: the write, the growth and the
//! funding. The read side belongs to whoever holds the trace token:
//! `cycle::mark` traces from one root, and the collection that reads those
//! roots out of this queue is `cycle::collect`. A collector thread reads
//! the ring behind this owner's writer without a detach
//! (`rfc/dev/DECISIONS.md`, "the candidate queue is read behind its writer,
//! and the collector's verdicts come back by a second ring"); its batch is
//! `crate::cycle::worker`'s, under the token, and the in-line collection is
//! the same reader on the owner's own thread.
//!
//! # The three storage paths
//!
//! A candidate is written through exactly one of them, tried in order:
//!
//! 1. the tail block of the ring, while it has room, or the next block of
//!    the circle when it is consumed;
//! 2. a block taken from a spare cell, or from the critical reserve
//!    with both cells empty, which is linked into the circle after the tail;
//! 3. the base block's bounded overflow buffer, which cannot refuse.
//!
//! # The shape
//!
//! One queue per thread, and its writer is the thread that owns it
//! (Y12 clause 1), so the write is uncontended by construction and needs
//! no read-modify-write. An entry is one pointer to an entity header, stored
//! as its address. Slots are sixteen-byte aligned in every size class
//! (`memory::heap::SIZE_CLASSES`), so an entry's low four bits are free and
//! carry the marks a reading writes over an entry it read. **Bit 0 is
//! the close's**, which is where it says a root belongs to the deferred lane
//! ([`DEFERRED_MARK`]); bits 1 to 3 are unused. The mark is written over the
//! entries a collection read and read once, by the pass that disposes of
//! them: every walk that hands an entry out as an address masks it
//! ([`ENTRY_MARK_BITS`]).
//!
//! **The active lane is a ring R of the form `crate::ring` builds**: 64 KiB
//! pool blocks linked in a circle, each carrying its own `front` and `tail`
//! on separate lines, so that a reader can stand behind the writer without
//! a detach and neither touches the other's index. R's two block pointers
//! stand in the owner's record ([`OwnerRecord::candidate_ring`]) — the front
//! block on the collector's line and the tail block on the owner's — and
//! the blocks themselves are pool blocks, the only unit both allocation
//! paths dispense (`rfc/model/gc/cycle/questions.md`, Y12 clause 3). A
//! consumed block is not returned: the writer reaches it again around the
//! circle. Every count of what the ring holds is `(tail − front) mod cap`
//! summed over its blocks, and no block's contents are bounded by anything
//! but its own two indices.
//!
//! **The deferred lane is a chain of the same blocks** ([`ring::Chain`]),
//! filled by the owner alone at a collection's close and re-offered at the
//! epoch's turn by a splice into R after the tail block, with no copy and
//! no block drawn ([`reoffer_deferred_candidates`]).
//!
//! # Why the growth path allocates nothing
//!
//! Candidate registration sits on the release path, so the write may not
//! allocate, lock or copy (Y12 clause 3). Growth is therefore a link of a
//! block somebody else took from the pool: the thread holds **two spares**
//! in cells it fills at thread init and at every safepoint poll, both
//! through the ordinary allocation path. With both cells empty the growth
//! draws the critical reserve (`memory::critical`), which is the draw
//! clause 6 funds. **Reserve mode itself is not built here** — clause 6 asks
//! that the runtime stay in it until every queued root has been walked, and
//! nothing in this crate carries such a state, the poll refilling the
//! reserve unconditionally at the next safepoint. What exists is the draw
//! and the arming.
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
//! A buffer in the thread-local would be paid by every thread at birth,
//! whether it registers a candidate or not, and what that cost was measured at
//! is `dev/BENCHMARKS.md`, "the escrow's move out of TLS". The base
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
//! through the ordinary allocation path, and its owner record beside it
//! through the registry's lock — the one lock the registration path takes,
//! once per such thread, and clause 3's one exception
//! ([`ensure_queue_base_or_abort`]). Either draw refusing aborts, which is
//! the funded class's last resort reached one step earlier than the
//! overflow buffer's own bound below.
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
//! entries — seventeen more than a ring block, whose three control lines
//! cost it more. The between-polls guarantee is derived from the buffer's
//! own capacity and borrows nothing from the block shape. It quantifies over the loops the compiler
//! emits and over the one runtime loop that carries a poll of its own
//! ([`POLL_STRIDE`]), so filling the overflow buffer takes sustained pool
//! refusal rather than a long run.
//!
//! The ring holds no block until the thread's first registration, which
//! finds no room by construction and takes the growth path. So a thread
//! that registers no candidate holds its base block and two spare blocks
//! rather than three blocks, and the empty-queue case needs no arm of its
//! own.
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
//! Five things, and [`crate::gc::ll_gc_maybe_collect`] does them in order.
//! It refills the spare cells, asking [`needs_spares`] — the count itself,
//! never a flag a draw sets, because a thread whose fill at init was refused
//! has never drawn and would never be asked again. It then drains the overflow
//! buffer into the queue, which is why the refill comes first; compares the
//! full-width epoch against the deferred lane's mirror and re-offers that lane
//! where it moved; behind an open gate, disposes of the prefix of P up to
//! the first proposed root ([`verdicts`]); and, armed, fires a collection.
//! A reserve draw, an overflow append, a due deferred re-offer, or a
//! proposal standing in P arms it.
//!
//! # The second ring, P
//!
//! The collector's verdicts about the roots it took from R come back by a
//! ring of the same form with the roles swapped, one block per thread,
//! drawn with the record and never grown. The owner alone reads it: a
//! collection's batch is R's entries and the proposed roots standing in P
//! ([`Batch`]), and every reduction of state a verdict leads to is made on
//! the owner's own re-reading, at the close or at the poll ([`verdicts`]).
//!
//! # What the in-line collection does with the rings
//!
//! It reads every entry from the front block to the tail, and every root
//! standing in P, as its batch ([`read_batch`]) — the collecting word in
//! the record keeps the collector out for the collection's whole length —
//! traces, and at its close **compacts the ring in place** ([`compaction`]):
//! an entry it disposed of is dropped, every other entry is kept in order,
//! and the blocks' `tail` indices and the tail block are lowered, every one
//! of them the owner's own words on its own thread. Nothing is taken out, so
//! nothing is merged back; a registration the collection's own destructors
//! make lands at the tail, behind the batch, and the compaction keeps it.
//! The batch's prefix of P is disposed of by the same pass and P's front
//! advanced past it. The overflow buffer is compacted by the same pass and
//! never traced: its entries are the next collection's, once the poll has
//! drained them.
use std::cell::{Cell, UnsafeCell};

use crate::cycle::owner_record::{self, OwnerRecord};
use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockHeader};
use crate::memory::gc_metadata;
use crate::refcount::RcHeader;
use crate::ring::{self, Chain, Quiescent, Writer};

/// Entries the overflow buffer holds, after the base block's
/// manager-owned control line.
///
/// The capacity is the 65,280-byte payload less one 64-byte
/// [`OwnerCycleState`]: 8,152 pointers. [`POLL_STRIDE`] is derived from
/// this figure and statically checked.
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

/// Spare blocks a thread keeps ahead of the next growth.
///
/// Two, which covers the two consumptions one interval between polls can
/// hold: one overflow of the tail block, and the growth of the deferred
/// lane at a collection's close, which takes a spare per block it fills
/// (`rfc/model/gc/cycle/questions.md`, Y12 clause 3). Beyond the two the
/// critical reserve answers, which is what it is for.
pub(crate) const SPARE_SEGMENTS: usize = 2;

/// A thread's queue and the spares behind it, resident in the base block.
///
/// Cells rather than a `RefCell`: the candidate write is the hottest
/// path in the runtime and a borrow flag on it buys nothing, the queue
/// having one writer by contract and no path that re-enters it. Nothing
/// here has drop glue, so thread exit frees it by hand
/// ([`release_queue_segments`]) rather than through a destructor whose order is
/// unspecified (`memory::heap::ll_thread_exit`). R's own two words are not
/// here: they stand in the owner's record, where a collector reaches them
/// (`crate::cycle::owner_record`).
#[repr(C, align(64))]
struct OwnerCycleState {
    /// This thread's collection workspace, in three states: null before the
    /// thread's first collection, the block's address while the workspace is
    /// idle, and that address with [`WORKSPACE_LENT`] set while an arena is
    /// bumping in it ([`lend_workspace_base`]).
    workspace_base: Cell<*mut BlockHeader>,
    /// Blocks taken ahead of the next growth, `spare_count` of them valid.
    spares: [Cell<*mut BlockHeader>; SPARE_SEGMENTS],
    /// Full-width commit count as it stood when the deferred lane last became
    /// nonempty or was re-offered. Read against the poll's own count, which is
    /// what tells a turnover from a commit ([`reoffer_deferred_if_epoch_moved`]).
    turnover_mirror: Cell<u64>,
    /// The deferred lane: candidates a later turnover rather than a decrement
    /// offers to a trace again. Written and read by the owner alone, at a
    /// collection's close and at the re-offer.
    deferred: UnsafeCell<Chain>,
    /// Entries in the base block no allocation path could fund a block
    /// for, written oldest first, which is the order every walk of the buffer
    /// reads them in. [`drain_overflow`] empties it from the other end, so
    /// that a drain the ring leaves no room for costs no move; what
    /// it leaves behind is the oldest prefix, and nothing about a candidate
    /// depends on its age. The tier that cannot refuse, so that a
    /// candidate registration cannot fail (`rfc/dev/DECISIONS.md`, "an
    /// enrolment cannot fail").
    overflow_len: Cell<u16>,
    spare_count: Cell<u8>,
}

thread_local! {
    /// Non-owning locator only. The state and every pointer it owns are
    /// stored in the manager-issued base block to which this points.
    static OWNER_STATE: Cell<*mut OwnerCycleState> = const { Cell::new(std::ptr::null_mut()) };
}

const _: () = assert!(size_of::<OwnerCycleState>() == 64);
const _: () = assert!(align_of::<OwnerCycleState>() == 64);
const _: () = assert!(POLL_STRIDE * 2 <= OVERFLOW_CAPACITY);
const _: () = assert!(ring::BLOCK_ENTRIES > OVERFLOW_CAPACITY / 2);

impl OwnerCycleState {
    const fn new() -> Self {
        Self {
            workspace_base: Cell::new(std::ptr::null_mut()),
            spares: [const { Cell::new(std::ptr::null_mut()) }; SPARE_SEGMENTS],
            turnover_mirror: Cell::new(0),
            deferred: UnsafeCell::new(Chain::empty()),
            spare_count: Cell::new(0),
            overflow_len: Cell::new(0),
        }
    }

    /// The deferred lane, for the owner's own thread alone.
    #[allow(clippy::mut_from_ref)]
    fn deferred(&self) -> &mut Chain {
        // The owner is the one thread that reaches this cell, and no caller
        // holds one borrow across a call that takes another.
        unsafe { &mut *self.deferred.get() }
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

#[inline]
fn stored_len(len: usize) -> u16 {
    u16::try_from(len).expect("the overflow buffer fits in u16")
}

/// This thread's record, which every thread with a base block has: the
/// record is drawn beside the block on both paths that draw one.
#[inline]
fn this_thread_record_ref<'a>() -> &'a OwnerRecord {
    let record = owner_record::this_thread_record();
    debug_assert!(!record.is_null(), "a thread with a base block has a record");
    unsafe { &*record }
}

/// The ring's entry for `entity`: its address, with the provenance exposed
/// so that [`entry_entity`] can give it back.
#[inline]
fn entity_entry(entity: *mut RcHeader) -> usize {
    entity.expose_provenance()
}

/// The entity a ring entry names, its mark taken off.
#[inline]
fn entry_entity(entry: usize) -> *mut RcHeader {
    std::ptr::with_exposed_provenance_mut(entry & !ENTRY_MARK_BITS)
}

/// The entity an entry of R names, for the collector's batch, which reads
/// R's entries as the owner's walk does (`crate::cycle::worker`).
#[inline]
pub(crate) fn entry_root(entry: usize) -> *mut RcHeader {
    entry_entity(entry)
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

    unsafe { append_entry(state, entity) };
}

/// Write one entry into the ring of the thread `state` belongs to, growing the
/// circle where the tail block is full and the next block is the reader's.
///
/// The entry alone: no flag is read and none is written, so an entity already
/// carrying its bit is written exactly as the live one [`register_candidate`]
/// admits.
///
/// # Safety
/// `state` is this thread's base-block pointer as [`OWNER_STATE`] holds it,
/// carrying the provenance of the whole block ([`append_to_overflow`] reaches
/// past the control line through it).
unsafe fn append_entry(state: *mut OwnerCycleState, entity: *mut RcHeader) {
    let owner_state = unsafe { owner_state_ref(state) };
    // `register_candidate` established the base block before reaching here,
    // and the record beside it. Drawing either at the first refusal would be
    // too late: every other allocation path would already have found the
    // pool empty.
    let writer = unsafe { Writer::new(this_thread_record_ref().candidate_ring()) };
    if writer
        .push(entity_entry(entity), || fresh_block(owner_state))
        .is_err()
    {
        unsafe { append_to_overflow(state, entity) };
        // The overflow append arms on its own: the refill the poll
        // performs is unconditional, so what the arming buys here is the
        // fire, not the cells.
        crate::gc::arm();
    }
}

/// The growth path's block: a spare, or the critical reserve with both
/// cells empty, or null when neither has one. A block this answers is
/// charged as the ring's from here.
fn fresh_block(owner_state: &OwnerCycleState) -> *mut BlockHeader {
    let mut block = take_spare(owner_state);
    if block.is_null() {
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
        block = gc_metadata::adopt(crate::memory::critical::draw());
        if !block.is_null() {
            // A draw is pressure, and pressure is what asks for a
            // collection. Armed here rather than beside the refusal
            // in `append_entry` so that the two paths arm independently: the
            // criterion names them separately and a later tier
            // between them would lose one silently.
            crate::gc::arm();
        }
    }

    if !block.is_null() {
        charge_block();
    }

    block
}

/// Enter one block into the ledger as the queue's: a block in R or in the
/// deferred lane is charged whole, at the transition that links it in, and
/// discharged at the one that takes it out ([`discharge_block`]). A spare
/// is a reservation and carries no charge (`memory::gc_metadata`).
#[inline]
fn charge_block() {
    gc_metadata::charge(BLOCK_PAYLOAD);
}

#[inline]
fn discharge_block() {
    gc_metadata::discharge(BLOCK_PAYLOAD);
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
    let owner_state = unsafe { owner_state_ref(state) };
    let overflow_len = owner_state.overflow_len.get();
    if usize::from(overflow_len) == OVERFLOW_CAPACITY {
        // Nothing to report it through: `ll_release` holds no frame, and
        // the poll that would raise is what this thread has not reached.
        std::process::abort();
    }

    // The control pointer is inside this thread's non-null base block,
    // which `register_candidate` established before taking any growth path.
    unsafe {
        overflow_entries(state)
            .add(usize::from(overflow_len))
            .write(entity)
    };
    owner_state.overflow_len.set(overflow_len + 1);
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
/// [`crate::memory::critical::draw`] meets, and the
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

    // The record beside the base block, as `ll_thread_init` draws it, and
    // claimable at once, this draw being the whole of the thread's
    // initialisation. The registry's lock is the one lock on the
    // registration path, paid once per such thread
    // (`crate::cycle::owner_record`, "When a thread takes its record").
    match owner_record::draw_thread_record() {
        owner_record::RecordDraw::Drawn => unsafe { owner_record::make_thread_record_claimable() },
        owner_record::RecordDraw::Present => {}
        owner_record::RecordDraw::AllocationFailed => std::process::abort(),
    }

    state
}

/// Whether this thread holds a base block now.
pub(crate) fn queue_base_present() -> bool {
    !owner_state().is_null()
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

    let owner_state = unsafe { owner_state_ref(state) };
    let installed = owner_state.workspace_base.get();
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
        owner_state
            .workspace_base
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

    let owner_state = unsafe { owner_state_ref(state) };
    assert_eq!(
        owner_state.workspace_base.get() as usize,
        base as usize | WORKSPACE_LENT,
        "the closing arena returns the block it was lent"
    );
    owner_state.workspace_base.set(base);
}

/// Draw this thread's workspace ahead of any collection, so that a test
/// counting blocks across one counts the collection and not the draw.
///
/// The block is out of the pool for the rest of the thread's life
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
/// The base block goes back the way the ring's blocks do, through
/// `gc_metadata::release_to_critical`, so a reserve below capacity is refilled
/// before the pool sees anything. The workspace does not: the reason is at the
/// line that releases it.
pub(crate) fn release_queue_base() {
    let state = OWNER_STATE.with(|cell| cell.replace(std::ptr::null_mut()));
    if state.is_null() {
        return;
    }

    let owner_state = unsafe { owner_state_ref(state) };
    let record = owner_record::this_thread_record();
    assert!(
        record.is_null() || !unsafe { Quiescent::new((*record).candidate_ring()) }.has_blocks(),
        "release follows the ring's release"
    );
    assert!(
        owner_state.deferred().is_empty(),
        "release follows deferred-lane release"
    );
    assert_eq!(
        owner_state.spare_count.get(),
        0,
        "release follows spare release"
    );
    assert_eq!(
        owner_state.overflow_len.get(),
        0,
        "release follows overflow release"
    );

    // The workspace goes back ahead of the block whose control line names it,
    // and to the pool rather than through the reserve: what the reserve lent
    // goes back to the reserve, and the reserve never funded this one
    // ([`lend_workspace_base`]).
    let workspace = owner_state.workspace_base.replace(std::ptr::null_mut());
    assert_eq!(
        workspace as usize & WORKSPACE_LENT,
        0,
        "release follows the collection's close"
    );
    gc_metadata::release(workspace);

    gc_metadata::discharge(size_of::<OwnerCycleState>());
    gc_metadata::release_to_critical(queue_base_of(state));
}

/// Refill the spare cells where they are short, then drain the overflow
/// buffer into the room the refill made.
///
/// The sequence the safepoint poll and the exit's collection share, in the
/// one order that works: a drain with no room writes the entries straight
/// back (`rfc/model/gc/cycle/questions.md`, Y12 clause 3). The refill runs
/// only when the cells are short, which is what asks for it — a count rather
/// than a flag, so a thread whose fill at init was refused is still asked
/// ([`needs_spares`]). The poll replenishes the critical reserve before this,
/// so that a growth with both cells empty has its path open again; the exit
/// does not, because its own end drains that reserve a few calls later and a
/// growth it cannot fund goes to the overflow buffer, which the next round
/// drains.
pub(crate) fn refill_and_drain() {
    if needs_spares() {
        let _ = refill_spares();
    }

    drain_overflow();
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
    let owner_state = unsafe { owner_state_ref(state) };
    while owner_state.overflow_len.get() > 0 {
        // Room is a tail block with a free slot, or a spare to link in after
        // a full one; the writer's own move into a consumed block of the
        // circle is room this reading does not see, and costs one more round
        // through the buffer at the next poll.
        let has_room =
            unsafe { Writer::new(this_thread_record_ref().candidate_ring()) }.tail_block_has_room();
        if !has_room && owner_state.spare_count.get() == 0 {
            break;
        }

        let overflow_len = owner_state.overflow_len.get() - 1;
        // The base block exists wherever the count is above zero, one
        // having been drawn before the first entry was written.
        let entity = unsafe {
            overflow_entries(state)
                .add(usize::from(overflow_len))
                .read()
        };
        owner_state.overflow_len.set(overflow_len);
        // Per entry rather than once for the run: the re-registration below
        // can link a block and charge its payload, and a discharge held to
        // the end would leave the overflow buffer's bytes standing over
        // entries it no longer holds — a high-water figure counting the same
        // memory twice.
        gc_metadata::discharge(size_of::<*mut RcHeader>());
        unsafe { register_candidate(entity) };
    }
}

/// The entries one in-line collection read as its roots: the first `len` of
/// R from its front, which is every entry the ring held when the collection
/// read it, and the first `verdicts` of P, of which the proposed and the
/// unwalked are roots ([`read_batch`]).
///
/// Counts and not a chain: the entries stay where they are, a registration
/// the collection's own destructors make lands behind them in R, a verdict
/// the collector posts lands behind them in P, and the close's compaction
/// is what disposes of them ([`dispose_candidates`]). A batch dropped
/// without a disposition leaves every root registered, since nothing was
/// taken out of either ring.
#[derive(Debug)]
pub(crate) struct Batch {
    len: usize,
    verdicts: usize,
}

impl Batch {
    /// Whether the batch holds nothing for a collection to do: no entry of
    /// R and no entry of P. A batch of verdicts without a root among them is
    /// not empty — its collection proposes nothing and its close is what
    /// defers, retires and writes back those verdicts and advances P past
    /// them; and so is one of entries already answered for, whose close is
    /// the advance.
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0 && self.verdicts == 0
    }

    /// Take every root in the batch, oldest first — P's, which were R's
    /// front before the collector took them, then R's — and stop at the
    /// first `visit` that answers false. **False** when it stopped early.
    /// The order is what a bounded trace reads: the collector's shortlist
    /// ahead of the roots nobody has read (`rfc/model/gc/rc-cycle.md`,
    /// "In-line collection is the same reader").
    ///
    /// A root may name an entity that has since been torn down: the entry is
    /// what keeps that slot out of the allocator's hands, and nothing retires
    /// it at the death (`rfc/model/gc/cycle/questions.md`, Y12 clause 7). The
    /// reader is what applies the zero-count rule, which for the trace is
    /// `crate::cycle::mark`'s.
    pub(crate) fn walk_roots(&self, mut visit: impl FnMut(*mut RcHeader) -> bool) -> bool {
        let mut stopped = false;
        self.walk_verdicts(|entry| {
            if verdicts::is_batch_root(entry) {
                stopped = !visit(verdicts::verdict_entity(entry));
            }
            !stopped
        });
        if stopped {
            return false;
        }

        let Some(ring) = candidate_ring() else {
            return true;
        };

        let mut left = self.len;
        ring.walk(|entry| {
            if left == 0 {
                return false;
            }

            left -= 1;
            stopped = !visit(entry_entity(entry));
            !stopped
        });
        !stopped
    }

    /// `visit` over the batch's first `verdicts` entries of P as they stand,
    /// disposed ones included, stopping at the first false.
    fn walk_verdicts(&self, mut visit: impl FnMut(usize) -> bool) {
        let Some(ring) = verdicts::verdict_ring() else {
            return;
        };

        let mut left = self.verdicts;
        ring.walk(|entry| {
            if left == 0 {
                return false;
            }

            left -= 1;
            visit(entry)
        });
    }

    /// Mark every root whose entity `deferrable` answers true for, in both
    /// rings, and answer how many were marked.
    ///
    /// The entity handed to the predicate carries no mark, and a mark already
    /// standing on an entry is overwritten rather than kept: it is one an
    /// unwound close left behind, and this close's reading is the one that
    /// decides. P's entries that are not roots — read live, zero-count,
    /// disposed — are not asked and not marked.
    pub(crate) fn mark_for_deferral(
        &mut self,
        mut deferrable: impl FnMut(*mut RcHeader) -> bool,
    ) -> usize {
        let Some(ring) = candidate_ring() else {
            return 0;
        };

        let mut marked = 0;
        ring.map_prefix_in_place(self.len, |slot| {
            let entity = *slot & !ENTRY_MARK_BITS;
            *slot = if deferrable(entry_entity(*slot)) {
                marked += 1;
                entity | DEFERRED_MARK
            } else {
                entity
            };
        });
        if let Some(ring) = verdicts::verdict_ring() {
            ring.map_prefix_in_place(self.verdicts, |slot| {
                let entry = *slot;
                let unmarked = entry & !verdicts::VERDICT_DEFER_MARK;
                *slot = if verdicts::is_batch_root(entry)
                    && deferrable(verdicts::verdict_entity(entry))
                {
                    marked += 1;
                    unmarked | verdicts::VERDICT_DEFER_MARK
                } else {
                    unmarked
                };
            });
        }
        marked
    }
}

/// The bit a close's disposition reads off an entry: the root it names
/// belongs to the deferred lane rather than to the active one.
///
/// Bit 0 of the stored address, which an entity header never carries: the
/// smallest size class is sixteen bytes, so the low four bits are clear and
/// the module doc's ledger says which of them belongs to whom. It is written
/// by [`Batch::mark_for_deferral`] after the commit and read once, by
/// the pass that disposes of the batch; every path that hands an entry to
/// anything else masks it off first (`crate::cycle::queue::compaction`).
pub(crate) const DEFERRED_MARK: usize = 1;

/// The low bits of an entry that carry a mark, masked off wherever an entry
/// is handed out as an address. The one bit written and not the four a
/// slot's alignment frees: a fixture's header stands on any eight-byte
/// boundary, and a mask over bits nothing writes would fold two such headers
/// into one.
pub(crate) const ENTRY_MARK_BITS: usize = DEFERRED_MARK;

/// The owner's handle over R while no reader runs, or `None` for a thread
/// with no record — one that never registered.
///
/// The exclusion is the caller's: the collecting word in the record keeps a
/// collector out for an in-line collection's whole length, and the token
/// does for a retirement outside one (`crate::cycle::collect`).
fn candidate_ring<'a>() -> Option<Quiescent<'a>> {
    let record = owner_record::this_thread_record();
    if record.is_null() {
        return None;
    }

    Some(unsafe { Quiescent::new((*record).candidate_ring()) })
}

/// Read this thread's two rings as one collection's batch: every entry of R
/// from the front to the tail, and every entry of P the collector has
/// posted, counted and left where they are. The caller holds the token or
/// the collecting word, so P is quiescent under the reading.
///
/// **It draws nothing and cannot be refused.** Nothing is taken out, so the
/// next registration finds the tail where the writer left it, and a
/// collection that reaches this line has all the memory its roots cost
/// (`dev/DECISIONS.md`, "the detach of a candidate chain draws no segment"
/// states the requirement this form meets by construction).
///
/// **The overflow buffer stays out of the batch.** Its entries are the next
/// collection's: the poll drains it into the ring before it fires, so on the
/// ordinary path it is empty here, and on the pressure path what it holds
/// keeps its bits and its records where they are.
///
/// An empty batch is the answer for a thread that has registered nothing,
/// and for one with no queue state at all.
pub(crate) fn read_batch() -> Batch {
    Batch {
        len: candidate_ring().map_or(0, |ring| ring.count()),
        verdicts: verdicts::verdict_ring().map_or(0, |ring| ring.count()),
    }
}

/// Dispose of a traced batch at the close: an entry whose entity completed
/// its death is retired, an entry [`Batch::mark_for_deferral`] marked
/// goes to the deferred lane, and every other entry of R stays in the ring,
/// in order; the batch's entries of P are disposed of the same way, the
/// ones the close cannot dispose of written back into R, and P's front
/// advances past all of them ([`verdicts`]).
///
/// `at_commits` is the process's commit count as the reading that decided the
/// marks saw it, and it is recorded only where the deferred lane goes from
/// empty to occupied — the oldest deferred record is what decides when the
/// owner owes a re-offer, as it is for [`defer_candidates`].
///
/// **A marked entry stays in the ring when the deferred lane cannot take
/// it.** The lane grows by a spare block per block it fills, and both cells
/// can stand empty — step 4's own registrations draw them. The root is then
/// offered to the next collection instead of waiting for the turnover, which
/// costs recall on that root and nothing else: the one destination that
/// cannot refuse is the one the fallback names, so no token is ever in no
/// lane (`rfc/model/gc/cycle/questions.md`, Y12 clause 8).
pub(crate) fn dispose_candidates(batch: Batch, at_commits: u64) {
    compaction::compact(Some(at_commits), false, Some(batch.verdicts));
}

/// Move a traced batch whole into this owner's deferred lane, sweeping out of
/// that lane the records whose entities completed their deaths on the way.
///
/// `at_commits` is the process's commit count as the reading that found the
/// component live saw it, and the caller takes it at that instant rather than
/// letting this read the global: the two collection paths dispose of a batch
/// on opposite sides of their own commit's increment, and a mirror taken here
/// would put the same event one whole epoch apart between them. It is recorded
/// only when the lane goes from empty to occupied — the oldest deferred record
/// is what decides when the owner owes a re-offer.
///
/// The deferred lane is swept before it receives the batch, so a record
/// naming an entity that is already dead in place gives its slot back here
/// instead of holding it for an epoch (`rfc/model/gc/cycle/questions.md`,
/// Y12 clause 8, "may sweep its own deferred-candidate buffer for zero-count
/// entities first"). The entries behind the batch and the overflow buffer are
/// kept by the same pass and not deferred: nothing that was not traced may
/// be.
pub(crate) fn defer_candidates(mut batch: Batch, at_commits: u64) {
    if batch.is_empty() {
        return;
    }

    batch.mark_for_deferral(|_| true);
    compaction::compact(Some(at_commits), true, Some(batch.verdicts));
}

/// Re-offer every deferred record: at an owner poll whose epoch moved, and
/// before each round of the exit's collection, which is the thread's last
/// turnover (`crate::cycle::collect::collect_before_exit`).
///
/// The caller owns the epoch comparison. The move is a splice of the lane's
/// blocks into R after the tail block, with no copy and no block drawn
/// (`crate::ring::Writer::splice_after_tail`); it leaves no record in the
/// deferred lane. A reader that wants the count moved takes
/// [`deferred_count`] before the call: nothing is counted here.
pub(crate) fn reoffer_deferred_candidates() {
    let state = owner_state();
    if state.is_null() {
        return;
    }
    let owner_state = unsafe { owner_state_ref(state) };
    let Some((first, last)) = owner_state.deferred().take() else {
        return;
    };

    let writer = unsafe { Writer::new(this_thread_record_ref().candidate_ring()) };
    unsafe { writer.splice_after_tail(first, last) };
}

/// Re-offer the deferred lane exactly once after `commits` stands in a later
/// epoch than the mirror this owner recorded. Returns whether it moved any
/// records.
///
/// The caller is the safepoint poll. What the comparison asks is whether a
/// turnover has closed since the mirror, not whether a commit has: a deferred
/// record waits out its epoch, and re-offering it at the next commit of any
/// thread would give back the whole recall the deferral buys. The count is
/// full-width rather than the header's two epoch bits so that four turnovers
/// slept through read as four, and the mirror advances only with the
/// owner-side move, so a refused collection cannot make the lane disappear.
pub(crate) fn reoffer_deferred_if_epoch_moved(commits: u64) -> bool {
    let state = owner_state();
    if state.is_null() {
        return false;
    }
    let owner_state = unsafe { owner_state_ref(state) };
    if owner_state.deferred().is_empty()
        || crate::cycle::epoch::turnovers_of(owner_state.turnover_mirror.get())
            == crate::cycle::epoch::turnovers_of(commits)
    {
        return false;
    }

    owner_state.turnover_mirror.set(commits);
    reoffer_deferred_candidates();
    true
}

/// Retire completed deaths at the owner's exact reading, compacting the ring
/// and the overflow buffer in place without drawing memory, and retiring
/// in place — the entry nulled, P's front unmoved — every completed death
/// a verdict of P names. A live or unfinished death stays registered, in
/// order.
///
/// # Safety
/// No membership or shadow reader can still name an entry being retired, no
/// arena reset is open, and no collector reads R or writes P: the caller
/// holds the token, or the collecting word excludes the collector. Every
/// entry still names its own held allocation.
pub(crate) unsafe fn retire_candidates() {
    compaction::compact(None, false, None);
}

mod compaction;
pub(crate) mod verdicts;

/// Put `entity` in the deferred lane, taking a block from a spare cell when
/// the lane's last block is full or there is none; [`ring::NoBlock`] with
/// both cells empty, and the entity is nowhere.
///
/// The block comes from a spare cell and never from the reserve: a block the
/// deferred lane keeps is one the reserve does not get back, and a draw at
/// a collection's close would be a request under the pressure that can have
/// started it (`rfc/model/gc/cycle/questions.md`, Y12 clause 8). `at_commits`
/// is recorded as the turnover mirror only where the lane goes from empty to
/// occupied — the oldest deferred record is what decides when the owner owes
/// a re-offer — and `None` records nothing.
fn defer_entry(
    owner_state: &OwnerCycleState,
    entity: *mut RcHeader,
    at_commits: Option<u64>,
) -> Result<(), ring::NoBlock> {
    let lane_was_empty = owner_state.deferred().is_empty();
    owner_state.deferred().push(entity_entry(entity), || {
        let block = take_spare(owner_state);
        if !block.is_null() {
            charge_block();
        }
        block
    })?;

    if lane_was_empty && let Some(at_commits) = at_commits {
        owner_state.turnover_mirror.set(at_commits);
    }

    Ok(())
}

/// Put a block the queue no longer holds where the next growth finds it: a
/// spare cell, or the critical reserve with both cells full.
fn return_surplus_block(owner_state: &OwnerCycleState, block: *mut BlockHeader) {
    let spare_count = owner_state.spare_count.get();
    if usize::from(spare_count) < SPARE_SEGMENTS {
        owner_state.spares[usize::from(spare_count)].set(block);
        owner_state.spare_count.set(spare_count + 1);
    } else {
        gc_metadata::release_to_critical(block);
    }
}

#[cfg(test)]
/// Work whose cost changes when retirement moves within a collection.
///
/// A record pass is one compaction over the ring and the overflow buffer;
/// the reads and the writes are per entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct QueueWork {
    pub(crate) record_passes: usize,
    pub(crate) records_read: usize,
    pub(crate) records_moved: usize,
}

#[cfg(test)]
thread_local! {
    static QUEUE_WORK: Cell<QueueWork> = const { Cell::new(QueueWork {
        record_passes: 0,
        records_read: 0,
        records_moved: 0,
    }) };
}

#[inline]
fn note_queue_work(_passes: usize, _read: usize, _moved: usize) {
    #[cfg(test)]
    let _ = QUEUE_WORK.try_with(|work| {
        let mut value = work.get();
        value.record_passes += _passes;
        value.records_read += _read;
        value.records_moved += _moved;
        work.set(value);
    });
}

#[cfg(test)]
/// Return this thread's queue work since the previous reading and zero it.
pub(crate) fn take_queue_work() -> QueueWork {
    QUEUE_WORK.with(|work| work.replace(QueueWork::default()))
}

/// Take one spare, or null when both cells are empty.
#[inline]
fn take_spare(owner_state: &OwnerCycleState) -> *mut BlockHeader {
    let spare_count = owner_state.spare_count.get();
    if spare_count == 0 {
        return std::ptr::null_mut();
    }

    owner_state.spare_count.set(spare_count - 1);
    owner_state.spares[usize::from(spare_count - 1)].replace(std::ptr::null_mut())
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
    state.is_null()
        || usize::from(unsafe { owner_state_ref(state) }.spare_count.get()) < SPARE_SEGMENTS
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
    let owner_state = unsafe { owner_state_ref(state) };
    while usize::from(owner_state.spare_count.get()) < SPARE_SEGMENTS {
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
        let spare_count = owner_state.spare_count.get();
        if usize::from(spare_count) == SPARE_SEGMENTS {
            gc_metadata::release_to_critical(block);
            return true;
        }

        owner_state.spares[usize::from(spare_count)].set(block);
        owner_state.spare_count.set(spare_count + 1);
    }

    true
}

/// Give every block of the ring, of the deferred lane and of the spare cells
/// back, and leave the queue empty.
///
/// Thread exit calls it in production and the tests call it to start
/// from a known queue: the queue holds pool blocks, and a dying thread
/// must not take them with it.
///
/// **The entries go with the blocks — the overflow buffer's and the
/// deferred lane's too — and their entities keep the candidate bit, which is
/// a permanent miss and not a deferral.** A block
/// with live occupants is handed to the abandoned list and adopted by
/// another thread (`memory::heap::ll_thread_exit`), so the entity
/// outlives its queue carrying a bit that names an entry nobody holds —
/// and [`crate::refcount::CANDIDATE_GATE_MASK`] refuses every later
/// decrement of it, for the life of the process. Clearing the bits here
/// is not available: an entry may name a slot already freed, and reading
/// it to clear a bit would touch returned memory. What keeps the miss
/// bounded is the collection the exit runs before this call, which takes
/// every ring the thread's own trace can and reports what it could not
/// (`crate::cycle::collect::collect_before_exit`); the entries this finds
/// are that residue, or a test's leavings.
///
/// Through [`crate::memory::critical::give_back`] rather than straight
/// to the pool, so a reserve below capacity is refilled before the pool
/// sees anything.
pub(crate) fn release_queue_segments() {
    let state = owner_state();
    if state.is_null() {
        return;
    }
    let owner_state = unsafe { owner_state_ref(state) };

    let give_back = |block: *mut BlockHeader| {
        discharge_block();
        gc_metadata::release_to_critical(block);
    };
    if let Some(ring) = candidate_ring() {
        ring.dismantle(give_back);
    }

    owner_state.deferred().dismantle(give_back);

    let spare_count = owner_state.spare_count.replace(0);
    for cell in &owner_state.spares[..usize::from(spare_count)] {
        let block = cell.replace(std::ptr::null_mut());
        gc_metadata::release_to_critical(block);
    }

    // The overflow buffer empties by its count, which is the only bound on
    // the base block's contents. The base block itself stays: it belongs to
    // the thread's life rather than to the queue's contents, and
    // [`release_queue_base`] is what ends that life.
    let overflow_len = owner_state.overflow_len.replace(0);
    gc_metadata::discharge(usize::from(overflow_len) * size_of::<*mut RcHeader>());
}

/// Entries this thread's overflow buffer holds.
pub(crate) fn overflow_len() -> usize {
    let state = owner_state();
    if state.is_null() {
        0
    } else {
        usize::from(unsafe { owner_state_ref(state) }.overflow_len.get())
    }
}

/// Registrations this thread holds by lane — the ring, the deferred lane,
/// the overflow buffer, and the verdicts standing in P — by the indices and
/// the counts; P's entries are read for the null of a disposed one and
/// dereferenced no more than any other entry.
///
/// What the exit reports as its residue, summed, and what a round of that
/// collection is measured against by lane
/// (`crate::cycle::collect::collect_before_exit`): a retirement lowers a
/// count, and a deferral moves a root from one lane to another and leaves
/// the sum where it was, which the next round's re-offer makes progress.
pub(crate) fn registered_by_lane() -> [usize; 4] {
    let state = owner_state();
    if state.is_null() {
        return [0; 4];
    }
    let owner_state = unsafe { owner_state_ref(state) };
    [
        candidate_ring().map_or(0, |ring| ring.count()),
        owner_state.deferred().len(),
        usize::from(owner_state.overflow_len.get()),
        standing_verdict_count(),
    ]
}

/// Entries of P the owner has not answered for, by a walk of its slots:
/// registrations in transit, which the exit's residue counts and a round of
/// its collection moves.
fn standing_verdict_count() -> usize {
    let mut count = 0;
    if let Some(ring) = verdicts::verdict_ring() {
        ring.walk(|entry| {
            count += usize::from(!verdicts::is_disposed(entry));
            true
        });
    }
    count
}

/// Entries this thread's ring holds, by its indices.
#[cfg(test)]
pub(crate) fn candidate_count() -> usize {
    if owner_state().is_null() {
        return 0;
    }

    candidate_ring().map_or(0, |ring| ring.count())
}

/// Every candidate token this thread's queue holds, appended to `out`: the
/// ring from its front, then the deferred lane oldest first, then the
/// overflow buffer oldest entry first, then the verdicts standing in P.
///
/// [`candidate_count`] answers the ring alone, and a count of one lane
/// can state neither half of the rule this exists for — a `CANDIDATE_BIT`
/// standing over no record anywhere, and one entity holding a record in two
/// lanes at once.
///
/// **What comes back says nothing about which lane a token is in**, the three
/// being concatenated. A caller that asserts residence pairs this with
/// [`candidate_count`] and [`deferred_count`]: the multiset gives identity and
/// those two give the split.
///
/// The entries are read and not dereferenced, exactly as the queue reads them:
/// an entry may name an entity that has since been torn down
/// (`rfc/model/gc/cycle/questions.md`, Y12 clause 7).
#[cfg(test)]
pub(crate) fn collect_lane_tokens(out: &mut Vec<*mut RcHeader>) {
    let state = owner_state();
    if state.is_null() {
        return;
    }

    let owner_state = unsafe { owner_state_ref(state) };
    if let Some(ring) = candidate_ring() {
        ring.walk(|entry| {
            out.push(entry_entity(entry));
            true
        });
    }

    owner_state
        .deferred()
        .walk(|entry| out.push(entry_entity(entry)));

    for index in 0..usize::from(owner_state.overflow_len.get()) {
        out.push(unsafe { overflow_entries(state).add(index).read() });
    }

    if let Some(ring) = verdicts::verdict_ring() {
        ring.walk(|entry| {
            if !verdicts::is_disposed(entry) {
                out.push(verdicts::verdict_entity(entry));
            }
            true
        });
    }
}

/// Blocks in this thread's ring.
#[cfg(test)]
pub(crate) fn segment_count() -> usize {
    if owner_state().is_null() {
        return 0;
    }

    candidate_ring().map_or(0, |ring| ring.block_count())
}

/// Blocks in this thread's deferred lane.
#[cfg(test)]
pub(crate) fn deferred_segment_count() -> usize {
    let state = owner_state();
    if state.is_null() {
        return 0;
    }

    unsafe { owner_state_ref(state) }.deferred().block_count()
}

/// Spares this thread holds.
#[cfg(test)]
pub(crate) fn spare_count() -> usize {
    let state = owner_state();
    if state.is_null() {
        0
    } else {
        usize::from(unsafe { owner_state_ref(state) }.spare_count.get())
    }
}

/// The commit count this owner recorded when its deferred lane last became
/// nonempty or was re-offered, which is what [`reoffer_deferred_if_epoch_moved`]
/// compares its argument against.
///
/// A case reads it rather than [`crate::cycle::epoch::commits`] because the
/// counter is process-global: another thread's commit between the deferral and
/// the reading would make the case's own arithmetic answer about a mirror it
/// does not hold.
#[cfg(test)]
pub(crate) fn deferred_turnover_mirror() -> u64 {
    let state = owner_state();
    if state.is_null() {
        return 0;
    }

    unsafe { owner_state_ref(state) }.turnover_mirror.get()
}

/// Records standing in this thread's deferred lane. Read by the census and
/// by the `bench-loads` hook before a re-offer, which is what keeps the count
/// out of the re-offer itself.
#[cfg(any(test, feature = "bench-loads"))]
pub(crate) fn deferred_count() -> usize {
    let state = owner_state();
    if state.is_null() {
        return 0;
    }

    unsafe { owner_state_ref(state) }.deferred().len()
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

/// The block of R this thread registers into, or null before the first
/// registration.
#[cfg(test)]
pub(crate) fn tail_block() -> *mut BlockHeader {
    let record = owner_record::this_thread_record();
    if record.is_null() {
        return std::ptr::null_mut();
    }

    unsafe { (*record).candidate_ring() }
        .tail_block
        .load(std::sync::atomic::Ordering::Relaxed)
}

/// Fill the tail block to capacity with `filler`, so the next
/// registration moves the writer to another block.
///
/// The shorthand exists because the honest way to reach the block change is
/// 8160 releases, which is a fixture rather than a test: the branch it
/// reaches is three lines and the entries before it prove nothing about
/// them. **It writes the entries rather than only the index**, because
/// the index is what bounds a block's contents and a test that lied
/// about it would hand a reader applying the zero-count rule 8159 recycled
/// words to dereference.
#[cfg(test)]
pub(crate) fn fill_tail_block(filler: *mut RcHeader) {
    assert!(!owner_state().is_null(), "no queue base block");
    let ring = candidate_ring().expect("a thread with a base block has a record");
    assert!(ring.has_blocks(), "no tail block to fill");
    ring.fill_tail_block(entity_entry(filler));
}

/// The nth entry of the ring, counting from the front.
#[cfg(test)]
pub(crate) fn entry_at(index: usize) -> *mut RcHeader {
    assert!(!owner_state().is_null(), "no queue base block");
    let ring = candidate_ring().expect("a thread with a base block has a record");
    let mut found = None;
    let mut position = 0;
    ring.walk(|entry| {
        if position == index {
            found = Some(entry_entity(entry));
            return false;
        }

        position += 1;
        true
    });
    found.unwrap_or_else(|| panic!("entry {index} is past the ring's count"))
}

#[cfg(test)]
mod tests;
