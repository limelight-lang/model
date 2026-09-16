//! The owner's record: the words of one mutator thread that a collector
//! thread reaches, in storage that outlives the thread
//! (`rfc/model/gc/rc-cycle.md`, "Worker-to-owner handoff"; `dev/DECISIONS.md`,
//! "the token stands in a record the process keeps, and the exit's claim on
//! it is never released"). Four lines: the trace token with the owner's
//! note of who holds it; the reader's line, the collector's words of the
//! two rings; the writer's line, the owner's words of them and its count
//! of freeing dispositions; and the hold line, the word under
//! which a collector reads the rings' blocks before its claim and the word
//! that names the owner's collector (`rfc/dev/DECISIONS.md`, "the candidate
//! queue is read behind its writer, and the collector's verdicts come back
//! by a second ring"). The first three lines are each written by one
//! party, so a registration's store and a batch's load never share a line;
//! the hold line is the one two parties write. Both rings' words are read
//! by `crate::cycle::queue` through `crate::ring`.
//!
//! # P's one block is drawn with the record
//!
//! P's one block is drawn beside the record and installed in both of P's
//! words before the record is the thread's, and given back with the record
//! at the exit; its refusal is the record's refusal. Why P is one block,
//! what it carries and how the owner reads it is
//! `crate::cycle::queue::verdicts`.
//!
//! # When a thread takes its record
//!
//! At `ll_thread_init`, beside the base block and before anything the
//! initialisation would have to undo: the registry's refusal is a thread
//! that never starts, as the base block's is
//! ([`draw_thread_record`]). The token stays held until the initialisation
//! is complete ([`make_thread_record_claimable`]), so a collector claims no
//! half-built thread. A thread the runtime never registered takes its
//! record at its first candidate registration, through the registry's
//! lock, beside the base block that path draws for it
//! (`crate::cycle::queue`, the base block's draw for an unregistered
//! thread); that lock is the one exception to the registration path's ban
//! on locking, paid once per thread. The exit releases the record last of
//! the collector's structures, after the queue whose registrations its
//! rounds could still make (`crate::memory::heap::ll_thread_exit`).
//!
//! # Why the storage outlives the thread
//!
//! A collector's first access to an owner is a load of a word of its record,
//! made before it holds any token. Nothing in the design covers a read made
//! under no claim except the lifetime of what is read: a word in a
//! thread-local dies with the thread, and a word in a block the exit returns
//! can be read after the pool has reissued the block. So the records stand
//! in a chain of GC-metadata blocks the process never returns, and a record
//! a thread has finished with goes to a free list for the next thread rather
//! than back to the pool.
//!
//! # The blocks a collector reads before its claim are held
//!
//! The idle test reads past the record into the rings' blocks — R's front
//! block and P's one block — and those the exit does return. So the
//! collector takes them to itself for the reading, the way the memory
//! manager moves a block between owners (`rfc/dev/DECISIONS.md`, "the
//! collector takes P's block to itself for the reading it makes before its
//! claim"): it sets the record's hold word ([`take_for_reading`]), reads,
//! and clears it ([`hand_back_reading`]). An exit that reaches a ring's
//! return under the hold leaves that ring to the holder, noting which
//! ([`leave_to_holder_if_held`]), and the hand-back returns what was left;
//! the registry hands out no record whose hold word is not clear, so a
//! re-taken record never installs a block over one still held or left.
//!
//! # The token is what says whether a record is anyone's
//!
//! A record's token is held from the moment the registry carves it until the
//! thread that took it has finished its initialisation — noted as the
//! owner's own claim, so the free path withholds nothing under it — and
//! again from the exit's final claim until the next thread's initialisation
//! ends. A collector's
//! claim is a compare-and-swap from free, so it fails on a record nobody has
//! taken yet, on one an exit has released, and on one whose next thread is
//! not yet ready — without a liveness word of its own, which would have to be
//! ordered against the token anyway. The exit's claim is never released: the
//! record goes to the free list held, and the taker releases it
//! ([`make_thread_record_claimable`]). Until that claim — through the static
//! blocks' teardown, the exit's first step — the token is free and a claim
//! succeeds, which is what the claim's wait is for. A thread that never
//! exits — one the runtime never registered, taking its record at its first
//! collection — keeps its record claimable for the life of the process.
//!
//! **The exit draws no record.** A thread that reaches its exit without one
//! is reached by no collector, so its claim is empty, and a record taken by
//! one of the exit's rounds would be released by that round's guard and go
//! to the free list free — a record a collector could then claim with nobody
//! in it. [`ensure_thread_record`] answers null while the exit runs.
//!
//! # The owner's own claim, told from a foreign one
//!
//! The free path withholds a return while a foreign holder has the token
//! (`crate::cycle::deferred_slot_reuse`, "A foreign holder of the token"),
//! and the exit's rounds of collection run under the owner's own claim, so
//! the word alone does not say which. [`OwnerRecord::owner_holds`] is the
//! owner's note to itself, written beside its take and its release and read
//! only when the token reads held, so the free path's common case stays one
//! load. The ruling that made the token one bit dissolved the holder kind
//! because no reader needed it (`rfc/dev/DECISIONS.md`, "the trace token
//! covers the trace alone, and the accelerator hands off by buffer swap");
//! the exit's held claim is the reader that does, and its note is the
//! owner's rather than the word's, so a collector still reads one bit.
//!
//! The collector thread that makes the round, and the round itself, are
//! `crate::cycle::worker`; the round reaches every record through
//! [`for_each_record`].

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicUsize, Ordering};

use crate::cycle::token::TraceToken;
use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockHeader};
use crate::memory::gc_metadata;

/// One mutator thread's record: four 64-byte lines — the token's, the
/// reader's and the writer's each written by one party, the hold line
/// shared — so that the collector's loads of one owner touch nothing of
/// another's and nothing the owner's registration stores into.
#[repr(C, align(64))]
pub(crate) struct OwnerRecord {
    /// The trace token, taken by a collector thread around its trace of this
    /// owner's graph and by the owner around its own.
    pub(crate) token: TraceToken,
    /// The next free record, meaningful while this one is on the registry's
    /// free list and written under its lock alone.
    free_link: Cell<*mut OwnerRecord>,
    /// Whether the holder of [`OwnerRecord::token`] is the owner itself.
    /// Written by the owner alone, beside its take and its release, and
    /// read by the owner alone, so relaxed on both sides.
    owner_holds: AtomicBool,
    /// Whether a case has asked the registry to leave this record on the
    /// free list: other tests' threads start and exit under the parallel
    /// harness, and a record they could pop is one no case can read after
    /// its own thread's exit.
    #[cfg(test)]
    pinned: AtomicBool,
    /// The collector's words of the two rings.
    reader: ReaderLine,
    /// The owner's words of the two rings.
    writer: WriterLine,
    /// The collector's hold over the rings' blocks for its pre-claim reading,
    /// and the exit's note of what it left to that hold.
    hold: HoldLine,
}

/// The line the collector writes: where it reads R from, where it posts
/// verdicts into P, and how many roots it takes per batch. Loaded by the
/// owner only where the ring's rules say so (`crate::ring`, the writer's
/// read of the front block on a full tail block).
#[repr(C, align(64))]
struct ReaderLine {
    /// The block of R the collector reads from (`crate::ring::Slots`).
    r_front_block: AtomicPtr<BlockHeader>,
    /// The block of P the collector posts into: P's one block, the same
    /// [`WriterLine::p_front_block`] names, for the thread's whole life.
    p_tail_block: AtomicPtr<BlockHeader>,
    /// Roots the collector takes from this owner per batch, halved on a
    /// batch that met its budget and doubled back on a completed one
    /// (`crate::cycle::worker`); zero before the first batch, which reads
    /// it as the starting size.
    batch: AtomicUsize,
    /// The value of [`WriterLine::freeing_dispositions`] the collector last
    /// read, its own copy, so that each party writes its own line: a
    /// difference is a disposition that freed something since the last
    /// round ([`OwnerRecord::note_freeing_disposition`]).
    freeing_dispositions_seen: AtomicU32,
}

/// The line the owner writes: where it registers into R, where it reads
/// verdicts from P, and whether it is collecting in line.
#[repr(C, align(64))]
struct WriterLine {
    /// The block of R the owner registers into (`crate::ring::Slots`).
    r_tail_block: AtomicPtr<BlockHeader>,
    /// The block of P the owner reads verdicts from: P's one block.
    p_front_block: AtomicPtr<BlockHeader>,
    /// Whether an in-line collection is running on the owner, from before
    /// its take of the token to the last store of its close. The owner's
    /// gate against a second collection on its own thread, and what keeps a
    /// collector out of R for the collection's whole length rather than for
    /// its trace: the collector reads it with acquire after its own claim of
    /// the token and, finding it set, releases and skips. The clear is a
    /// release store and the close's last, so a collector that reads it
    /// clear reads the compaction's entries and indices behind it
    /// (`rfc/dev/DECISIONS.md`, "the candidate queue is read behind its
    /// writer, and the collector's verdicts come back by a second ring", "Who
    /// reads R").
    collecting: AtomicBool,
    /// Dispositions of P at this owner's poll that freed something, counted
    /// up by the owner and never cleared: the collector compares it with
    /// its own copy to shorten its fallback interval
    /// (`crate::cycle::worker`, "The thread, and the round over the
    /// records").
    freeing_dispositions: AtomicU32,
}

/// The line the collector, the exit and the registry share.
#[repr(C, align(64))]
struct HoldLine {
    /// [`READING`] while a collector reads the rings' blocks before its
    /// claim; [`R_LEFT`] and [`P_LEFT`] while a ring's blocks an exit found
    /// held wait for the hand-back to return them; [`RETURNING`] from the
    /// exit's first return of a ring nobody held until the registry hands
    /// the record out again, so that no reading begins against a ring the
    /// exit is returning. Zero is a record whose blocks are the owner's
    /// alone; the registry hands out a record with nothing but
    /// `RETURNING` set, and clears it. The collector's take is a
    /// compare-and-swap, the exit's leave one too; the hand-back and the
    /// registry store.
    reading: AtomicU8,
    /// The collector thread this owner is named to, as a slot index of
    /// `crate::cycle::worker`'s: zero is the elder, and a fresh record's.
    /// Written by a collector at a handover and read by the owner's poll,
    /// which wakes the collector it names, and by every collector's round,
    /// which serves the owners named to it. On this line because it is the
    /// one word a collector writes into a record it does not read for.
    collector: AtomicU8,
}

/// A collector is reading the rings' blocks under no claim.
const READING: u8 = 1;
/// The exit left R's blocks in the record for the reading's hand-back.
const R_LEFT: u8 = 2;
/// The exit left P's block in the record for the reading's hand-back.
const P_LEFT: u8 = 4;
/// The exit is returning, or has returned, a ring nobody held.
const RETURNING: u8 = 8;

/// Which ring an exit leaves to a collector's hold.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Ring {
    Candidates,
    Verdicts,
}

impl Ring {
    const fn left_flag(self) -> u8 {
        match self {
            Ring::Candidates => R_LEFT,
            Ring::Verdicts => P_LEFT,
        }
    }
}

impl ReaderLine {
    const fn empty() -> Self {
        Self {
            r_front_block: AtomicPtr::new(std::ptr::null_mut()),
            p_tail_block: AtomicPtr::new(std::ptr::null_mut()),
            batch: AtomicUsize::new(0),
            freeing_dispositions_seen: AtomicU32::new(0),
        }
    }

    /// Empty the line in place, for a record taken off the free list.
    fn reset(&self) {
        self.r_front_block
            .store(std::ptr::null_mut(), Ordering::Relaxed);
        self.p_tail_block
            .store(std::ptr::null_mut(), Ordering::Relaxed);
        self.batch.store(0, Ordering::Relaxed);
        self.freeing_dispositions_seen.store(0, Ordering::Relaxed);
    }
}

impl WriterLine {
    const fn empty() -> Self {
        Self {
            r_tail_block: AtomicPtr::new(std::ptr::null_mut()),
            p_front_block: AtomicPtr::new(std::ptr::null_mut()),
            collecting: AtomicBool::new(false),
            freeing_dispositions: AtomicU32::new(0),
        }
    }

    /// Empty the line in place, for a record taken off the free list.
    fn reset(&self) {
        self.r_tail_block
            .store(std::ptr::null_mut(), Ordering::Relaxed);
        self.p_front_block
            .store(std::ptr::null_mut(), Ordering::Relaxed);
        self.collecting.store(false, Ordering::Relaxed);
        self.freeing_dispositions.store(0, Ordering::Relaxed);
    }
}

// The free link is written under the registry's lock and every other field
// is atomic, which is what lets a record be reached from two threads.
unsafe impl Sync for OwnerRecord {}

const _: () = assert!(size_of::<OwnerRecord>() == 256);
const _: () = assert!(std::mem::offset_of!(OwnerRecord, reader) == 64);
const _: () = assert!(std::mem::offset_of!(OwnerRecord, writer) == 128);
const _: () = assert!(std::mem::offset_of!(OwnerRecord, hold) == 192);
const _: () = assert!(
    !std::mem::needs_drop::<OwnerRecord>(),
    "a record is written in place and never dropped"
);

/// Records one block's payload holds.
const RECORDS_PER_BLOCK: usize = BLOCK_PAYLOAD / size_of::<OwnerRecord>();

/// The process's records: the chain of blocks they stand in, how far the head
/// block is carved, and the records threads have finished with.
struct Registry {
    /// The block records are carved from, and behind it through
    /// [`BlockHeader::next`] every block carved before it.
    head: *mut BlockHeader,
    /// Records carved out of `head` so far.
    carved: usize,
    /// Records released by exited threads, each with its token held.
    free: *mut OwnerRecord,
}

// The pointers are the registry's own and are touched under its lock.
unsafe impl Send for Registry {}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    head: std::ptr::null_mut(),
    carved: 0,
    free: std::ptr::null_mut(),
});

thread_local! {
    /// Non-owning locator of this thread's record, null while it has none.
    /// A `const` cell of a pointer, so the exit path may read it after other
    /// thread-locals are gone.
    static OWNER_RECORD: Cell<*mut OwnerRecord> = const { Cell::new(std::ptr::null_mut()) };
}

impl OwnerRecord {
    /// A record in the state the registry hands out: token held by nobody in
    /// particular.
    const fn taken() -> Self {
        Self {
            token: TraceToken::new_held(),
            free_link: Cell::new(std::ptr::null_mut()),
            owner_holds: AtomicBool::new(false),
            #[cfg(test)]
            pinned: AtomicBool::new(false),
            reader: ReaderLine::empty(),
            writer: WriterLine::empty(),
            hold: HoldLine {
                reading: AtomicU8::new(0),
                collector: AtomicU8::new(0),
            },
        }
    }

    /// The collector this owner is named to (`crate::cycle::worker`).
    #[inline]
    pub(crate) fn collector(&self) -> usize {
        usize::from(self.hold.collector.load(Ordering::Relaxed))
    }

    /// Name this owner to `collector`, on a collector's thread; the owner's
    /// next poll wakes the one named.
    #[inline]
    pub(crate) fn name_to_collector(&self, collector: usize) {
        self.hold.collector.store(
            u8::try_from(collector).expect("a collector slot index"),
            Ordering::Relaxed,
        );
    }

    /// Whether a thread other than the owner holds the token now: a reading,
    /// stale in both directions in the ways [`TraceToken::is_held`] names.
    #[inline]
    pub(crate) fn held_by_another(&self) -> bool {
        self.token.is_held() && !self.owner_holds.load(Ordering::Relaxed)
    }

    /// R's two words: the front block on the reader's line, the tail block
    /// on the writer's.
    #[inline]
    pub(crate) fn candidate_ring(&self) -> crate::ring::Slots<'_> {
        crate::ring::Slots {
            front_block: &self.reader.r_front_block,
            tail_block: &self.writer.r_tail_block,
        }
    }

    /// P's two words: the front block on the writer's line, since the owner
    /// is P's reader, and the tail block on the reader's, the collector being
    /// its writer.
    #[inline]
    pub(crate) fn verdict_ring(&self) -> crate::ring::Slots<'_> {
        crate::ring::Slots {
            front_block: &self.writer.p_front_block,
            tail_block: &self.reader.p_tail_block,
        }
    }

    /// The collector's batch size for this owner, its own word: zero before
    /// the first batch.
    #[inline]
    pub(crate) fn batch_size(&self) -> usize {
        self.reader.batch.load(Ordering::Relaxed)
    }

    #[inline]
    pub(crate) fn set_batch_size(&self, roots: usize) {
        self.reader.batch.store(roots, Ordering::Relaxed);
    }

    /// Whether the owner is collecting in line, as a collector reads it after
    /// its claim of the token: acquire, so that a clear reading carries the
    /// close's stores.
    #[inline]
    pub(crate) fn is_collecting_as_collector(&self) -> bool {
        self.writer.collecting.load(Ordering::Acquire)
    }

    /// Whether the owner is collecting in line, as the owner reads it:
    /// relaxed, the word being the owner's own on that side.
    #[inline]
    pub(crate) fn is_collecting(&self) -> bool {
        self.writer.collecting.load(Ordering::Relaxed)
    }

    /// Raise the collecting word, before the owner takes its token: the take
    /// is what orders the word before a collector's next claim.
    #[inline]
    pub(crate) fn set_collecting(&self) {
        self.writer.collecting.store(true, Ordering::Relaxed);
    }

    /// Clear the collecting word: the close's last store, and a release, so
    /// that a collector reading it clear reads everything the close wrote.
    #[inline]
    pub(crate) fn clear_collecting(&self) {
        self.writer.collecting.store(false, Ordering::Release);
    }

    /// Note, on the owner's thread, that a disposition of P at its poll
    /// freed something: the collector's next round reads it and shortens
    /// its fallback interval.
    #[inline]
    pub(crate) fn note_freeing_disposition(&self) {
        let count = self.writer.freeing_dispositions.load(Ordering::Relaxed);
        self.writer
            .freeing_dispositions
            .store(count.wrapping_add(1), Ordering::Relaxed);
    }

    /// Whether the owner noted a freeing disposition since the collector
    /// last asked, on the collector's thread: the owner's count against the
    /// collector's own copy, which this brings up to date.
    #[inline]
    pub(crate) fn take_freeing_disposition_note(&self) -> bool {
        let count = self.writer.freeing_dispositions.load(Ordering::Relaxed);
        if count
            == self
                .reader
                .freeing_dispositions_seen
                .load(Ordering::Relaxed)
        {
            return false;
        }

        self.reader
            .freeing_dispositions_seen
            .store(count, Ordering::Relaxed);
        true
    }
}

/// Call `visit` on every record the registry has carved so far, in no
/// particular order: the records threads live in, the ones on the free list
/// and the calling thread's own alike, since a record is never returned and
/// a walk cannot tell them apart without a claim. The registry's lock is held
/// for the reading of how far the carve got and not across the visits, so a
/// record carved during the walk is the next walk's.
pub(crate) fn for_each_record(mut visit: impl FnMut(*mut OwnerRecord)) {
    let (head, carved) = {
        let registry = REGISTRY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (registry.head, registry.carved)
    };

    // Every block behind the head is carved whole: a block leaves the head
    // position only once `carved` reaches its capacity.
    let mut block = head;
    let mut records = carved;
    while !block.is_null() {
        let first = BlockHeader::payload_start(block) as *mut OwnerRecord;
        for index in 0..records {
            visit(unsafe { first.add(index) });
        }

        block = unsafe { (*block).next };
        records = RECORDS_PER_BLOCK;
    }
}

/// This thread's record, or null while it has none.
#[inline]
pub(crate) fn this_thread_record() -> *mut OwnerRecord {
    OWNER_RECORD.with(Cell::get)
}

/// This thread's record, taking one from the registry if it has none yet,
/// with P's block drawn and installed. Null when the pool refuses the block
/// a fresh record would stand in or P's block, and the next call asks again.
///
/// The record comes out with its token held; the caller is the one that
/// releases it, or keeps it as its own claim ([`crate::cycle::token::HeldToken`]).
/// The second of the pair says whether the token was just taken, so the
/// caller can tell a record it already lived in from one it has this
/// instant. Every thread
/// that registers a candidate has a record before it does, so a caller on
/// a production path finds one present; the take here is the test's, whose
/// thread asks for its token before anything else.
pub(crate) fn ensure_thread_record() -> (*mut OwnerRecord, bool) {
    let present = this_thread_record();
    if !present.is_null() {
        return (present, false);
    }

    if crate::memory::heap::thread_exit_running() {
        return (std::ptr::null_mut(), true);
    }

    // P's block ahead of the record, so that no record is ever published
    // with a draw still to make behind it. The thread-local is read again
    // after the draw rather than trusted across it: under `debug-journal`
    // the draw's first record site runs `ll_thread_init` on this thread,
    // which takes a record of its own (`crate::cycle::queue`,
    // `try_ensure_queue_base` carries the same re-entry).
    let p_block = gc_metadata::acquire();
    if p_block.is_null() {
        return (std::ptr::null_mut(), true);
    }

    let installed = this_thread_record();
    if !installed.is_null() {
        gc_metadata::release_to_critical(p_block);
        return (installed, false);
    }

    let record = take_record();
    if record.is_null() {
        gc_metadata::release_to_critical(p_block);
        return (record, true);
    }

    gc_metadata::charge(BLOCK_PAYLOAD);
    unsafe { (*record).verdict_ring().install_single_block(p_block) };
    OWNER_RECORD.with(|cell| cell.set(record));
    #[cfg(test)]
    RECORDS_TAKEN.with(|count| count.set(count.get() + 1));
    (record, true)
}

/// What [`draw_thread_record`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RecordDraw {
    /// The thread had a record already, and keeps it as it was: an
    /// initialisation run again on a started thread owes it nothing.
    Present,
    /// This call took one, with its token held; the caller releases the
    /// hold ([`make_thread_record_claimable`]) or gives the record back
    /// ([`release_thread_record`]).
    Drawn,
    /// The registry could not carve one, which the caller treats as it
    /// treats a refused base block.
    AllocationFailed,
}

/// Give this thread a record, with its token held as the owner's own claim:
/// the draw `ll_thread_init` makes beside the base block, and the one a
/// thread the runtime never registered makes at its first registration.
///
/// The hold is noted as the owner's, because the rest of the initialisation
/// runs under it and its returns — a rollback's, a re-entered draw's — go
/// through the free path, which withholds under a foreign holder and not
/// under the owner ([`OwnerRecord::owner_holds`]).
pub(crate) fn draw_thread_record() -> RecordDraw {
    match ensure_thread_record() {
        (record, _) if record.is_null() => RecordDraw::AllocationFailed,
        (record, true) => {
            unsafe { note_owner_holds(record, true) };
            RecordDraw::Drawn
        }
        (_, false) => RecordDraw::Present,
    }
}

/// Make this thread's record claimable: the release of the hold
/// [`draw_thread_record`] took, made once, after the last thing a
/// collector's claim may not overtake.
///
/// # Safety
/// This thread's record was drawn by this thread's initialisation
/// ([`RecordDraw::Drawn`]) and nothing has released it since.
pub(crate) unsafe fn make_thread_record_claimable() {
    let record = this_thread_record();
    debug_assert!(
        !record.is_null() && unsafe { (*record).token.is_held() && owner_holds(record) },
        "the initialisation's hold is what this releases"
    );
    unsafe {
        note_owner_holds(record, false);
        (*record).token.release();
    }
}

/// Note whether the owner holds its own token. Written by
/// the owner beside its take and its release, and by nothing else.
///
/// # Safety
/// `record` is this thread's record and this thread is the holder whose
/// claim the note describes.
#[inline]
pub(crate) unsafe fn note_owner_holds(record: *mut OwnerRecord, holds: bool) {
    unsafe { (*record).owner_holds.store(holds, Ordering::Relaxed) };
}

/// Whether the owner's own claim stands on `record`.
///
/// # Safety
/// `record` is this thread's record.
#[inline]
pub(crate) unsafe fn owner_holds(record: *mut OwnerRecord) -> bool {
    unsafe { (*record).owner_holds.load(Ordering::Relaxed) }
}

/// Give this thread's record back to the registry, for the next thread, and
/// P's block back to the pool.
///
/// The token stays held: the exit's final claim is what stands on it — or
/// the initialisation's own hold, for a thread `ll_thread_init` refused
/// after the draw — and the thread that takes the record next releases it
/// when its own initialisation is complete. A thread that never had a
/// record has nothing to give back. A verdict still standing in P goes with
/// the block: the exit's rounds read P to its end under the claim, so what
/// stands here is what no round could dispose of.
///
/// # Safety
/// This thread holds its record's token and will not touch the record again.
pub(crate) unsafe fn release_thread_record() {
    let record = OWNER_RECORD.with(|cell| cell.replace(std::ptr::null_mut()));
    if record.is_null() {
        return;
    }

    // Under the held token, so no collector posts into the block as it
    // goes; a collector reading it before its claim keeps it until its
    // hand-back.
    if !unsafe { leave_to_holder_if_held(record, Ring::Verdicts) } {
        unsafe { give_back_verdict_ring(record) };
    }

    debug_assert!(
        unsafe { (*record).token.is_held() },
        "a record goes back held: under the exit's claim, or under the \
         initialisation's own hold"
    );
    #[cfg(test)]
    debug_assert!(
        crate::cycle::queue::queue_base().is_null(),
        "the record goes back after the base block, whose queue's rounds \
         could still register"
    );
    let mut registry = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    unsafe { (*record).free_link.set(registry.free) };
    registry.free = record;
}

/// Return P's block, whichever thread does it: the exit, or the collector
/// whose hold the exit found.
///
/// # Safety
/// No collector posts into P: the caller holds the record's token, or the
/// exit left P to the caller's hold.
unsafe fn give_back_verdict_ring(record: *mut OwnerRecord) {
    unsafe { crate::ring::Quiescent::new((*record).verdict_ring()) }.dismantle(|block| {
        gc_metadata::discharge(BLOCK_PAYLOAD);
        gc_metadata::release_to_critical(block);
    });
}

/// Take `record`'s rings' blocks for a reading under no claim: true with the
/// hold set, false where a collector already holds them, an exit has left
/// blocks for one, or an exit has begun returning them — in which case the
/// caller reads nothing of them. A record on the free list is never taken:
/// its exit's returns set [`RETURNING`], which the registry clears when it
/// hands the record out.
///
/// # Safety
/// `record` is a record of the registry's.
pub(crate) unsafe fn take_for_reading(record: *mut OwnerRecord) -> bool {
    unsafe { &(*record).hold.reading }
        .compare_exchange(0, READING, Ordering::AcqRel, Ordering::Relaxed)
        .is_ok()
}

/// End the reading [`take_for_reading`] began, and return the blocks an
/// exit left to it meanwhile: R's through the queue's give-back, P's here.
/// Clearing the hold before the returns is what keeps an exit from leaving
/// more once they begin; the flags go last, so the registry hands the
/// record out only after its blocks are gone. A ring left is a ring an exit
/// returned through this hand-back, so the word ends [`RETURNING`] as it
/// would after the exit's own return: the record is on the free list, or
/// on its way there, and refuses the next reading until the registry hands
/// it out.
///
/// # Safety
/// The calling thread took the hold and has finished reading.
pub(crate) unsafe fn hand_back_reading(record: *mut OwnerRecord) {
    let hold = unsafe { &(*record).hold.reading };
    let left = hold.fetch_and(!READING, Ordering::AcqRel);
    debug_assert!(left & READING != 0, "a hand-back ends a reading");
    if left & R_LEFT != 0 {
        unsafe { crate::cycle::queue::give_back_candidate_ring_left_by_an_exit(record) };
    }

    if left & P_LEFT != 0 {
        unsafe { give_back_verdict_ring(record) };
    }

    if left & (R_LEFT | P_LEFT) == 0 {
        return;
    }

    let _ = hold.fetch_update(Ordering::Release, Ordering::Relaxed, |state| {
        Some((state & !(R_LEFT | P_LEFT)) | RETURNING)
    });
}

/// The exit's question at a ring's return: whether a collector holds the
/// rings' blocks for a reading, in which case `ring`'s blocks stay in the
/// record for the hand-back and this answers true. Both answers are a
/// store into the hold word — the leave sets the ring's flag, the return
/// sets [`RETURNING`] — so that a reading and the exit's decision are
/// ordered one way or the other: a reading that begins after a return's
/// store fails its take, and a return that follows a reading's take sees
/// it and leaves.
///
/// # Safety
/// `record` is this thread's record, and the exit will not touch `ring`'s
/// blocks again where this answers true.
pub(crate) unsafe fn leave_to_holder_if_held(record: *mut OwnerRecord, ring: Ring) -> bool {
    let hold = unsafe { &(*record).hold.reading };
    let mut state = hold.load(Ordering::Acquire);
    #[cfg(test)]
    at_the_exits_hold_check();
    loop {
        let held = state & READING != 0;
        let next = if held {
            state | ring.left_flag()
        } else {
            state | RETURNING
        };
        match hold.compare_exchange_weak(state, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return held,
            Err(now) => state = now,
        }
    }
}

/// What an exit runs between its load of the hold word and its store, for
/// the case whose collector takes the blocks in that window; on the exiting
/// thread, once.
#[cfg(test)]
static AT_THE_EXITS_HOLD_CHECK: Mutex<Option<Box<dyn FnOnce() + Send>>> = Mutex::new(None);

#[cfg(test)]
pub(crate) fn at_the_next_exits_hold_check(act: Box<dyn FnOnce() + Send>) {
    *AT_THE_EXITS_HOLD_CHECK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(act);
}

#[cfg(test)]
fn at_the_exits_hold_check() {
    let act = AT_THE_EXITS_HOLD_CHECK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(act) = act {
        act();
    }
}

/// Whether an exit left `ring`'s blocks in `record` for a collector's
/// hand-back.
pub(crate) fn ring_left_to_a_holder(record: *mut OwnerRecord, ring: Ring) -> bool {
    unsafe { &(*record).hold.reading }.load(Ordering::Acquire) & ring.left_flag() != 0
}

/// Whether no reading holds `record`'s blocks and no exit left any: what
/// the registry requires of a record it hands out. [`RETURNING`] alone is
/// the state an exit leaves a record in, and the take clears it.
fn blocks_are_the_owners(record: *mut OwnerRecord) -> bool {
    unsafe { &(*record).hold.reading }.load(Ordering::Acquire) & (READING | R_LEFT | P_LEFT) == 0
}

/// Take a record out of the registry: a released one first, then one carved
/// out of the head block, then one out of a block drawn for it. Null when
/// the pool refuses that draw.
fn take_record() -> *mut OwnerRecord {
    #[cfg(test)]
    if REFUSE_DRAWS.with(Cell::get) {
        return std::ptr::null_mut();
    }

    let mut registry = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let released = first_free_record(&mut registry);
    if !released.is_null() {
        // In place rather than a fresh `taken()`: a collector's pointer to the
        // token outlives the last life, and the word it will compare must be
        // the held one the exit left rather than a rewritten one.
        unsafe {
            (*released).owner_holds.store(false, Ordering::Relaxed);
            (*released).free_link.set(std::ptr::null_mut());
            (*released).reader.reset();
            (*released).writer.reset();
            (*released).hold.collector.store(0, Ordering::Relaxed);
            // Last, with release: the next reading's take is what sees the
            // lines above as reset.
            (*released).hold.reading.store(0, Ordering::Release);
        }
        return released;
    }

    if registry.head.is_null() || registry.carved == RECORDS_PER_BLOCK {
        let block = gc_metadata::acquire();
        if block.is_null() {
            return std::ptr::null_mut();
        }

        #[cfg(test)]
        BLOCKS_CARVED.with(|count| count.set(count.get() + 1));
        unsafe { (*block).next = registry.head };
        registry.head = block;
        registry.carved = 0;
    }

    let record = unsafe {
        (BlockHeader::payload_start(registry.head) as *mut OwnerRecord).add(registry.carved)
    };
    registry.carved += 1;
    #[cfg(test)]
    RECORDS_CARVED.with(|count| count.set(count.get() + 1));
    gc_metadata::charge(size_of::<OwnerRecord>());
    unsafe { record.write(OwnerRecord::taken()) };
    record
}

/// Unlink and answer the first record of the free list whose blocks are the
/// owner's, or null: a record a collector is reading, or holds blocks of
/// that an exit left, stays on the list until its hand-back. In the test
/// build a case's pin and its named record narrow the walk further
/// (`skipped_by_a_case`).
fn first_free_record(registry: &mut Registry) -> *mut OwnerRecord {
    #[cfg(test)]
    let wanted = TAKE_THIS.with(|cell| cell.replace(std::ptr::null_mut()));
    let mut link: *mut *mut OwnerRecord = &raw mut registry.free;
    loop {
        let record = unsafe { *link };
        if record.is_null() {
            return record;
        }

        #[cfg(test)]
        let skipped = !blocks_are_the_owners(record) || skipped_by_a_case(record, wanted);
        #[cfg(not(test))]
        let skipped = !blocks_are_the_owners(record);
        if skipped {
            link = unsafe { (*record).free_link.as_ptr() };
            continue;
        }

        unsafe { *link = (*record).free_link.get() };
        return record;
    }
}

/// Whether a case keeps `record` on the free list: every pinned record is
/// skipped, unless the taking thread named `wanted` with
/// [`take_this_record_for_test`], in which case every record but `wanted`
/// is — so that a case can read what a re-take of one record does while
/// other cases' threads move the list's top.
#[cfg(test)]
fn skipped_by_a_case(record: *mut OwnerRecord, wanted: *mut OwnerRecord) -> bool {
    if wanted.is_null() {
        unsafe { (*record).pinned.load(Ordering::Relaxed) }
    } else {
        record != wanted
    }
}

/// Make this thread's next take pop `record` off the free list, wherever it
/// stands and pinned or not; null when the list does not hold it.
#[cfg(test)]
pub(crate) fn take_this_record_for_test(record: *mut OwnerRecord) {
    TAKE_THIS.with(|cell| cell.set(record));
}

/// Keep `record` on the free list, or let it go again, for a case that
/// reads a record after its thread's exit.
#[cfg(test)]
pub(crate) fn pin_for_test(record: *mut OwnerRecord, pinned: bool) {
    unsafe { (*record).pinned.store(pinned, Ordering::Relaxed) };
}

#[cfg(test)]
thread_local! {
    /// Records this thread has taken out of the registry, for a case that
    /// reads whether an exit drew one.
    static RECORDS_TAKEN: Cell<usize> = const { Cell::new(0) };
    /// Registry blocks this thread's takes drew from the pool, and records
    /// they carved fresh rather than popped: what a take leaves with the
    /// process, read on the thread so that other threads' carves are not in
    /// the figure.
    static BLOCKS_CARVED: Cell<usize> = const { Cell::new(0) };
    static RECORDS_CARVED: Cell<usize> = const { Cell::new(0) };
    /// The one record this thread's next take pops, or null for the list's
    /// first unpinned record; cleared by the take.
    static TAKE_THIS: Cell<*mut OwnerRecord> = const { Cell::new(std::ptr::null_mut()) };
    /// Whether this thread's draws answer null, for a case that reads what
    /// a refused record costs. Every draw while it stands, rather than the
    /// next one: under `debug-journal` the base block's draw runs a second
    /// `ll_thread_init` from inside the journal, whose own draw would spend
    /// a one-shot refusal before the case's init reached its.
    static REFUSE_DRAWS: Cell<bool> = const { Cell::new(false) };
}

/// Refuse this thread's draws of a record, or serve them again, as a
/// registry whose block the pool refused would; the refusal names the draw
/// and nothing else.
#[cfg(test)]
pub(crate) fn refuse_record_draws(refuse: bool) {
    REFUSE_DRAWS.with(|cell| cell.set(refuse));
}

/// Write into `record`'s reader line, for a case that reads whether a
/// re-take empties it: the batch size, which is the one word of the two
/// lines no exit reads. The four block words are left alone, because the
/// exit reads both rings through them and a scribbled pointer would be
/// followed; they are nulled by the rings' dismantle before the record goes
/// back, which is what the reset repeats. The collecting word is left alone
/// too: set, it is the owner's gate, and the exit would wait behind it.
#[cfg(test)]
pub(crate) fn scribble_lines_for_test(record: *mut OwnerRecord) {
    unsafe { (*record).reader.batch.store(7, Ordering::Relaxed) };
}

/// Whether `record`'s reader and writer lines hold what a fresh life starts
/// with: R's words and the batch size empty, the collecting word clear, and
/// P's two words naming one block.
#[cfg(test)]
pub(crate) fn lines_are_fresh(record: *mut OwnerRecord) -> bool {
    let reader = unsafe { &(*record).reader };
    let writer = unsafe { &(*record).writer };
    let p_block = reader.p_tail_block.load(Ordering::Relaxed);
    reader.r_front_block.load(Ordering::Relaxed).is_null()
        && reader.batch.load(Ordering::Relaxed) == 0
        && writer.r_tail_block.load(Ordering::Relaxed).is_null()
        && !writer.collecting.load(Ordering::Relaxed)
        && !p_block.is_null()
        && writer.p_front_block.load(Ordering::Relaxed) == p_block
}

/// The block P stands in, or null for a record with none.
#[cfg(test)]
pub(crate) fn verdict_block(record: *mut OwnerRecord) -> *mut BlockHeader {
    unsafe { (*record).reader.p_tail_block.load(Ordering::Relaxed) }
}

/// How many records this thread has taken out of the registry so far.
#[cfg(test)]
pub(crate) fn records_taken() -> usize {
    RECORDS_TAKEN.with(Cell::get)
}

/// Whether `record` stands on the registry's free list now, for a case that
/// reads the exit's hand-back. Other tests' threads exit under the parallel
/// harness, so a count of the list would move under a case; membership of
/// one record does not.
#[cfg(test)]
pub(crate) fn registry_lists_free(record: *mut OwnerRecord) -> bool {
    let registry = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut free = registry.free;
    while !free.is_null() {
        if free == record {
            return true;
        }

        free = unsafe { (*free).free_link.get() };
    }

    false
}

/// Registry blocks this thread's takes drew and records they carved fresh,
/// for a case that reads what a thread's draw left with the process: a
/// carved record is charged once and stays carved, and the block it stands
/// in stays with the registry.
#[cfg(test)]
pub(crate) fn carved_by_this_thread() -> (usize, usize) {
    (
        BLOCKS_CARVED.with(Cell::get),
        RECORDS_CARVED.with(Cell::get),
    )
}

/// Whether `block` is one of the registry's, for a case that reads where a
/// record stands.
#[cfg(test)]
pub(crate) fn registry_owns_block(block: *mut BlockHeader) -> bool {
    let registry = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut head = registry.head;
    while !head.is_null() {
        if head == block {
            return true;
        }

        head = unsafe { (*head).next };
    }

    false
}

#[cfg(test)]
mod tests;
