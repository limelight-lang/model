//! The mutator's record: the words of one mutator thread that a collector
//! thread reaches, in storage that outlives the thread
//! (`rfc/model/gc/rc-cycle.md`, "Worker-to-owner handoff"; `dev/DECISIONS.md`,
//! "the token stands in a record the process keeps, and the exit's claim on
//! it is never released"). Four lines: the token line, the trace token's
//! byte and its wait; the reader's line, the collector's words of the
//! two rings; the writer's line, the mutator's words of them and its count
//! of freeing dispositions; and the hold line, the word under
//! which a collector reads the rings' blocks before its claim and the word
//! that names the mutator's collector (`rfc/dev/DECISIONS.md`, "the candidate
//! queue is read behind its writer, and the collector's verdicts come back
//! by a second ring"). The reader's and the writer's lines are each written
//! by one party, so a registration's store and a batch's load never share a
//! line; the token line and the hold line are the two both parties write.
//! Both rings' words are read by `crate::cycle::queue` through `crate::ring`.
//!
//! # P's one block is drawn with the record
//!
//! P's one block is drawn beside the record and installed in both of P's
//! words before the record is the thread's, and given back with the record
//! at the exit; its refusal is the record's refusal. Why P is one block,
//! what it carries and how the mutator reads it is
//! `crate::cycle::queue::verdicts`.
//!
//! # When a thread takes its record
//!
//! At `ll_thread_init`, beside the base block and before anything the
//! initialisation would have to undo: the registry's refusal is a thread
//! that never starts, as the base block's is
//! ([`draw_thread_record`]). The token stays held until the initialisation
//! is complete ([`make_thread_record_claimable`]), so a collector claims no
//! half-built thread. Every thread that registers a candidate has its
//! record before it does, because no thread registers one before its init
//! (`crate::cycle::queue`, a registration with no base block). The exit
//! releases the record last of the collector's structures, after the queue
//! whose registrations its rounds could still make
//! (`crate::memory::heap::ll_thread_exit`).
//!
//! # Why the storage outlives the thread
//!
//! A collector's first access to a mutator is a load of a word of its record,
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
//! manager moves a block between mutators (`rfc/dev/DECISIONS.md`, "the
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
//! mutator's own claim, so the free path withholds nothing under it — and
//! again from the exit's final claim until the next thread's initialisation
//! ends. A collector's
//! claim is a compare-and-swap from free, so it fails on a record nobody has
//! taken yet, on one an exit has released, and on one whose next thread is
//! not yet ready — without a liveness word of its own, which would have to be
//! ordered against the token anyway. The exit's claim is never released: the
//! record goes to the free list held, and the taker releases it
//! ([`make_thread_record_claimable`]). Until that claim — through the static
//! blocks' teardown, the exit's first step — the token is free and a claim
//! succeeds, which is what the claim's wait is for.
//!
//! **The exit draws no record.** A thread that reaches its exit without one
//! is reached by no collector, so its claim is empty, and a record taken by
//! one of the exit's rounds would be released by that round's guard and go
//! to the free list free — a record a collector could then claim with nobody
//! in it. [`draw_thread_record`] refuses while the exit runs.
//!
//! # The mutator's own claim, told from a foreign one
//!
//! The free path withholds a return while a collector has the token
//! (`crate::cycle::deferred_slot_reuse`, "A foreign holder of the token"),
//! and the exit's rounds of collection run under the mutator's own claim.
//! The byte itself says which: `MUTATOR` is written by the mutator alone and
//! `COLLECTOR` by a collector or by the mutator's consent
//! (`crate::cycle::token`), so the free path reads one byte and no note.
//!
//! The collector thread that makes the round, and the round itself, are
//! `crate::cycle::worker`; the round reaches every record through
//! [`for_each_record`].

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{
    AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};

use crate::cycle::token::TraceToken;
use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockHeader};
use crate::memory::gc_metadata;

/// One mutator thread's record: four 64-byte lines — the token's and the
/// hold line written by both parties, the reader's and the writer's each
/// by one — so that the collector's loads of one mutator touch nothing of
/// another's and nothing the mutator's registration stores into.
#[repr(C, align(64))]
pub(crate) struct MutatorRecord {
    /// The trace token, taken by a collector thread around its trace of this
    /// mutator's candidates and the entities the trace reaches, and by the
    /// mutator around its own.
    pub(crate) token: TraceToken,
    /// The collector's request for a turnover of this mutator's epoch, read
    /// beside the token byte at every poll that finds the deferred lane
    /// non-empty (`crate::gc`, the poll; `crate::cycle::queue`, the deferred
    /// lane): stored by the collector's round when this mutator has been
    /// quiet for X (`crate::cycle::worker`), cleared by the mutator at the
    /// poll that jumps its clock and at the fill of an empty lane. Relaxed on
    /// both sides: nothing is published beside it, and a request the fill
    /// cleared or the poll read late costs one turnover early or one poll
    /// late, never a wrong free. A collection of this thread's own between
    /// the ask and the poll — under pressure, or fired — re-defers the lane
    /// and so clears the request, and the next ask waits X again: a thread
    /// whose clock moved is not quiet.
    turnover_requested: AtomicU8,
    /// The next free record, meaningful while this one is on the registry's
    /// free list and written under its lock alone.
    free_link: Cell<*mut MutatorRecord>,
    /// Whether a case has asked the registry to leave this record on the
    /// free list: other tests' threads start and exit under the parallel
    /// harness, and a record they could pop is one no case can read after
    /// its own thread's exit.
    #[cfg(test)]
    pinned: AtomicBool,
    /// The collector's words of the two rings.
    reader: ReaderLine,
    /// The mutator's words of the two rings.
    writer: WriterLine,
    /// The collector's hold over the rings' blocks for its pre-claim reading,
    /// and the exit's note of what it left to that hold.
    hold: HoldLine,
}

/// The line the collector writes: where it reads R from, where it posts
/// verdicts into P, and how many roots it takes per batch. Loaded by the
/// mutator only where the ring's rules say so (`crate::ring`, the writer's
/// read of the front block on a full tail block).
#[repr(C, align(64))]
struct ReaderLine {
    /// The block of R the collector reads from (`crate::ring::Slots`).
    r_front_block: AtomicPtr<BlockHeader>,
    /// The block of P the collector posts into: P's one block, the same
    /// [`WriterLine::p_front_block`] names, for the thread's whole life.
    p_tail_block: AtomicPtr<BlockHeader>,
    /// Roots the collector takes from this mutator per batch, halved on a
    /// batch that met its budget and doubled back on a completed one
    /// (`crate::cycle::worker`); zero before the first batch, which reads
    /// it as the starting size.
    batch: AtomicUsize,
    /// The value of [`WriterLine::freeing_dispositions`] the collector last
    /// read, its own copy, so that each party writes its own line: a
    /// difference is a disposition that freed something since the last
    /// round ([`MutatorRecord::note_freeing_disposition`]).
    freeing_dispositions_seen: AtomicU32,
    /// Whether this mutator left the collector's last request unanswered
    /// past its wait: the collector's own mark, so that its next request
    /// stands without a wait (`crate::cycle::worker`, the standing
    /// requests). Cleared when a request is served, and with the line at a
    /// re-take.
    silent: AtomicBool,
    /// When the collector last served this mutator, in nanoseconds since the
    /// base `crate::cycle::worker` fixes at the process's first serve; zero
    /// before any serve. Restamped by every serve that found the mutator's
    /// clock moving, and by the serve that reached nothing with X elapsed
    /// and asked for a turnover (`crate::cycle::worker`, "The quiet
    /// thread"). The collector's own word, so relaxed.
    served_at: AtomicU64,
    /// The mutator's clock as the collector last stamped it, beside the
    /// instant: a clock that moved since is a thread whose stamps age on
    /// their own, and it is restamped rather than asked. The collector's own
    /// word, so relaxed.
    commits_seen: AtomicU64,
}

/// The line the mutator writes: where it registers into R, where it reads
/// verdicts from P, and whether it is collecting in line.
#[repr(C, align(64))]
struct WriterLine {
    /// The block of R the mutator registers into (`crate::ring::Slots`).
    r_tail_block: AtomicPtr<BlockHeader>,
    /// The block of P the mutator reads verdicts from: P's one block.
    p_front_block: AtomicPtr<BlockHeader>,
    /// Whether an in-line collection is running on the mutator, from before
    /// its take of the token to its close: the mutator's gate against a
    /// second collection on its own thread, written and read by the mutator
    /// alone. What keeps a collector out of R for the collection's whole
    /// length is the token itself, at `MUTATOR` from the take through the
    /// close (`crate::cycle::token`; `rfc/dev/design/trace-token-handshake.md`,
    /// E10).
    collecting: AtomicBool,
    /// Dispositions of P at this mutator's poll that freed something, counted
    /// up by the mutator and never cleared: the collector compares it with
    /// its own copy to shorten its fallback interval
    /// (`crate::cycle::worker`, "The thread, and the round over the
    /// records").
    freeing_dispositions: AtomicU32,
    /// This mutator's clock, which its maturation stamps are written and
    /// read against (`crate::cycle::epoch`): the commits it has closed,
    /// moved to the next turnover's first commit at its poll on the
    /// collector's request ([`MutatorRecord::turnover_requested`]). Written
    /// by the mutator alone, at its commit's close and at that poll, and
    /// read by the collector that traces this mutator's graph, which prunes
    /// against the owner's epoch and never against its own thread's. A record
    /// handed out again starts one turnover past where its last life left it
    /// (`crate::cycle::epoch::a_new_lifes_count`), so no stamp that life wrote
    /// reads fresh to this one.
    commits: AtomicU64,
}

/// The line the collector, the exit and the registry share.
#[repr(C, align(64))]
struct HoldLine {
    /// [`READING`] while a collector reads the rings' blocks before its
    /// claim; [`R_LEFT`] and [`P_LEFT`] while a ring's blocks an exit found
    /// held wait for the hand-back to return them; [`RETURNING`] from the
    /// exit's first return of a ring nobody held until the registry hands
    /// the record out again, so that no reading begins against a ring the
    /// exit is returning. Zero is a record whose blocks are the mutator's
    /// alone; the registry hands out a record with nothing but
    /// `RETURNING` set, and clears it. The collector's take is a
    /// compare-and-swap, the exit's leave one too; the hand-back and the
    /// registry store.
    reading: AtomicU8,
    /// The collector thread this mutator is named to, as a slot index of
    /// `crate::cycle::worker`'s: zero is the elder, and a fresh record's.
    /// Written by a collector at a handover and read by the mutator's poll,
    /// which wakes the collector it names, and by every collector's round,
    /// which serves the mutators named to it. On this line because it is the
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
            silent: AtomicBool::new(false),
            served_at: AtomicU64::new(0),
            commits_seen: AtomicU64::new(0),
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
        self.silent.store(false, Ordering::Relaxed);
        self.served_at.store(0, Ordering::Relaxed);
        self.commits_seen.store(0, Ordering::Relaxed);
    }
}

impl WriterLine {
    const fn empty() -> Self {
        Self {
            r_tail_block: AtomicPtr::new(std::ptr::null_mut()),
            p_front_block: AtomicPtr::new(std::ptr::null_mut()),
            collecting: AtomicBool::new(false),
            freeing_dispositions: AtomicU32::new(0),
            commits: AtomicU64::new(0),
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
        self.commits.store(
            crate::cycle::epoch::a_new_lifes_count(self.commits.load(Ordering::Relaxed)),
            Ordering::Relaxed,
        );
    }
}

// The free link is written under the registry's lock and every other field
// is atomic, which is what lets a record be reached from two threads.
unsafe impl Sync for MutatorRecord {}

const _: () = assert!(size_of::<MutatorRecord>() == 256);
const _: () = assert!(std::mem::offset_of!(MutatorRecord, reader) == 64);
const _: () = assert!(std::mem::offset_of!(MutatorRecord, writer) == 128);
const _: () = assert!(std::mem::offset_of!(MutatorRecord, hold) == 192);
const _: () = assert!(
    !std::mem::needs_drop::<MutatorRecord>(),
    "a record is written in place and never dropped"
);

/// Records one block's payload holds.
const RECORDS_PER_BLOCK: usize = BLOCK_PAYLOAD / size_of::<MutatorRecord>();

/// The process's records: the chain of blocks they stand in, how far the head
/// block is carved, and the records threads have finished with.
struct Registry {
    /// The block records are carved from, and behind it through
    /// [`BlockHeader::next`] every block carved before it.
    head: *mut BlockHeader,
    /// Records carved out of `head` so far.
    carved: usize,
    /// Records released by exited threads, each with its token held.
    free: *mut MutatorRecord,
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
    static MUTATOR_RECORD: Cell<*mut MutatorRecord> = const { Cell::new(std::ptr::null_mut()) };
}

impl MutatorRecord {
    /// A record in the state the registry hands out: token held by nobody in
    /// particular.
    const fn taken() -> Self {
        Self {
            token: TraceToken::new_held(),
            turnover_requested: AtomicU8::new(0),
            free_link: Cell::new(std::ptr::null_mut()),
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

    /// The collector this mutator is named to (`crate::cycle::worker`).
    #[inline]
    pub(crate) fn collector(&self) -> usize {
        usize::from(self.hold.collector.load(Ordering::Relaxed))
    }

    /// Name this mutator to `collector`, on a collector's thread; the mutator's
    /// next poll wakes the one named.
    #[inline]
    pub(crate) fn name_to_collector(&self, collector: usize) {
        self.hold.collector.store(
            u8::try_from(collector).expect("a collector slot index"),
            Ordering::Relaxed,
        );
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

    /// P's two words: the front block on the writer's line, since the mutator
    /// is P's reader, and the tail block on the reader's, the collector being
    /// its writer.
    #[inline]
    pub(crate) fn verdict_ring(&self) -> crate::ring::Slots<'_> {
        crate::ring::Slots {
            front_block: &self.writer.p_front_block,
            tail_block: &self.reader.p_tail_block,
        }
    }

    /// The collector's batch size for this mutator, its own word: zero before
    /// the first batch.
    #[inline]
    pub(crate) fn batch_size(&self) -> usize {
        self.reader.batch.load(Ordering::Relaxed)
    }

    #[inline]
    pub(crate) fn set_batch_size(&self, roots: usize) {
        self.reader.batch.store(roots, Ordering::Relaxed);
    }

    /// Whether the collector marked this mutator silent
    /// ([`ReaderLine::silent`]); the collector's own line, so relaxed.
    #[inline]
    pub(crate) fn is_silent(&self) -> bool {
        self.reader.silent.load(Ordering::Relaxed)
    }

    /// Mark or clear the silent note ([`ReaderLine::silent`]).
    #[inline]
    pub(crate) fn note_silent(&self, silent: bool) {
        self.reader.silent.store(silent, Ordering::Relaxed);
    }

    /// When the collector last served this mutator ([`ReaderLine::served_at`]).
    #[inline]
    pub(crate) fn served_at(&self) -> u64 {
        self.reader.served_at.load(Ordering::Relaxed)
    }

    /// Stamp the serve's instant ([`ReaderLine::served_at`]) and the clock as
    /// it stands ([`ReaderLine::commits_seen`]), on the collector's thread.
    #[inline]
    pub(crate) fn note_served_at(&self, nanos: u64) {
        self.reader.served_at.store(nanos, Ordering::Relaxed);
        self.reader
            .commits_seen
            .store(self.commits(), Ordering::Relaxed);
    }

    /// Whether the mutator's clock stands where the collector last stamped
    /// it ([`ReaderLine::commits_seen`]): no commit of its own since.
    #[inline]
    pub(crate) fn clock_stood_since_the_stamp(&self) -> bool {
        self.reader.commits_seen.load(Ordering::Relaxed) == self.commits()
    }

    /// Ask this mutator for a turnover of its epoch
    /// ([`MutatorRecord::turnover_requested`]), on the collector's thread.
    #[inline]
    pub(crate) fn request_a_turnover(&self) {
        self.turnover_requested.store(1, Ordering::Relaxed);
    }

    /// Whether the collector asked for a turnover, on the mutator's thread.
    #[inline]
    pub(crate) fn turnover_is_requested(&self) -> bool {
        self.turnover_requested.load(Ordering::Relaxed) != 0
    }

    /// Take the request down, on the mutator's thread: at the poll that acts
    /// on it, and at the fill of an empty deferred lane, where a request made
    /// against the last accumulation is stale.
    #[inline]
    pub(crate) fn clear_turnover_request(&self) {
        self.turnover_requested.store(0, Ordering::Relaxed);
    }

    /// Whether the mutator is collecting in line: the word is the mutator's
    /// own, so relaxed.
    #[inline]
    pub(crate) fn is_collecting(&self) -> bool {
        self.writer.collecting.load(Ordering::Relaxed)
    }

    /// Raise the collecting word, before the mutator takes its token.
    #[inline]
    pub(crate) fn set_collecting(&self) {
        self.writer.collecting.store(true, Ordering::Relaxed);
    }

    /// Clear the collecting word, at the close; the token's release after it
    /// is the close's last store.
    #[inline]
    pub(crate) fn clear_collecting(&self) {
        self.writer.collecting.store(false, Ordering::Relaxed);
    }

    /// Count one commit this mutator closed, on the mutator's own thread.
    #[inline]
    pub(crate) fn note_commit(&self) {
        let commits = self.writer.commits.load(Ordering::Relaxed);
        self.writer.commits.store(commits + 1, Ordering::Relaxed);
    }

    /// Move the clock to `commits`, on the mutator's own thread: the
    /// collector's turnover request answered
    /// (`crate::cycle::epoch::jump_to_the_next_turnover`).
    #[inline]
    pub(crate) fn jump_commits_to(&self, commits: u64) {
        self.writer.commits.store(commits, Ordering::Relaxed);
    }

    /// This mutator's clock: the commits it has closed, moved to the next
    /// turnover's first commit on the collector's request (the field's
    /// contract, `WriterLine::commits`). Read by the mutator itself and by
    /// the collector that traces for it, where it is the clock the stamps in
    /// that mutator's entities were written against.
    #[inline]
    pub(crate) fn commits(&self) -> u64 {
        self.writer.commits.load(Ordering::Relaxed)
    }

    /// Note, on the mutator's thread, that the collection its poll fired
    /// freed or retired something: the collector's next round reads it and
    /// shortens its fallback interval.
    #[inline]
    pub(crate) fn note_freeing_disposition(&self) {
        let count = self.writer.freeing_dispositions.load(Ordering::Relaxed);
        self.writer
            .freeing_dispositions
            .store(count.wrapping_add(1), Ordering::Relaxed);
    }

    /// Whether the mutator noted a freeing disposition since the collector
    /// last asked, on the collector's thread: the mutator's count against the
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
pub(crate) fn for_each_record(mut visit: impl FnMut(*mut MutatorRecord)) {
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
        let first = BlockHeader::payload_start(block) as *mut MutatorRecord;
        for index in 0..records {
            visit(unsafe { first.add(index) });
        }

        block = unsafe { (*block).next };
        records = RECORDS_PER_BLOCK;
    }
}

/// This thread's record, or null while it has none.
#[inline]
pub(crate) fn this_thread_record() -> *mut MutatorRecord {
    MUTATOR_RECORD.with(Cell::get)
}

/// Draw this thread's record from the registry, with P's block drawn and
/// installed and the token held as the mutator's own claim: the one draw of
/// it in a thread's life, made by `ll_thread_init` beside the base block,
/// on a thread that holds none.
///
/// `false` when the pool refuses the block a fresh record would stand in or
/// P's block, which the caller treats as it treats a refused base block; and
/// while the thread's own exit runs, which draws no record (module doc, "The
/// exit draws no record").
///
/// The hold is the mutator's own, `MUTATOR` on the byte, so the rest of the
/// initialisation runs under it and a rollback's returns go through the free
/// path, which withholds under a collector and not under the mutator. The
/// caller releases the hold ([`make_thread_record_claimable`]) or gives the
/// record back ([`release_thread_record`]).
pub(crate) fn draw_thread_record() -> bool {
    debug_assert!(
        this_thread_record().is_null(),
        "the record is drawn once per life of a thread"
    );
    if crate::memory::heap::thread_exit_running() {
        return false;
    }

    // P's block ahead of the record, so that no record is ever published
    // with a draw still to make behind it.
    let p_block = gc_metadata::acquire();
    if p_block.is_null() {
        return false;
    }

    let record = take_record();
    if record.is_null() {
        gc_metadata::release_to_critical(p_block);
        return false;
    }

    gc_metadata::charge(BLOCK_PAYLOAD);
    unsafe { (*record).verdict_ring().install_single_block(p_block) };
    MUTATOR_RECORD.with(|cell| cell.set(record));
    #[cfg(test)]
    RECORDS_TAKEN.with(|count| count.set(count.get() + 1));
    true
}

/// Make this thread's record claimable: the release of the hold
/// [`draw_thread_record`] took, made once, after the last thing a
/// collector's claim may not overtake.
///
/// # Safety
/// This thread's record was drawn by this thread's initialisation
/// ([`draw_thread_record`]) and nothing has released it since.
pub(crate) unsafe fn make_thread_record_claimable() {
    let record = this_thread_record();
    debug_assert!(
        !record.is_null()
            && crate::cycle::token::state(unsafe { (*record).token.read() })
                == crate::cycle::token::MUTATOR,
        "the initialisation's hold is what this releases"
    );
    unsafe { (*record).token.release() };
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
    let record = MUTATOR_RECORD.with(|cell| cell.replace(std::ptr::null_mut()));
    if record.is_null() {
        return;
    }

    // Under the held token, so no collector posts into the block as it
    // goes; a collector reading it before its claim keeps it until its
    // hand-back.
    if !unsafe { leave_to_holder_if_held(record, Ring::Verdicts) } {
        unsafe { give_back_verdict_ring(record) };
    }

    debug_assert_eq!(
        crate::cycle::token::state(unsafe { (*record).token.read() }),
        crate::cycle::token::MUTATOR,
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
unsafe fn give_back_verdict_ring(record: *mut MutatorRecord) {
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
pub(crate) unsafe fn take_for_reading(record: *mut MutatorRecord) -> bool {
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
pub(crate) unsafe fn hand_back_reading(record: *mut MutatorRecord) {
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
pub(crate) unsafe fn leave_to_holder_if_held(record: *mut MutatorRecord, ring: Ring) -> bool {
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
pub(crate) fn ring_left_to_a_holder(record: *mut MutatorRecord, ring: Ring) -> bool {
    unsafe { &(*record).hold.reading }.load(Ordering::Acquire) & ring.left_flag() != 0
}

/// Whether no reading holds `record`'s blocks and no exit left any: what
/// the registry requires of a record it hands out. [`RETURNING`] alone is
/// the state an exit leaves a record in, and the take clears it.
fn blocks_are_the_mutators(record: *mut MutatorRecord) -> bool {
    unsafe { &(*record).hold.reading }.load(Ordering::Acquire) & (READING | R_LEFT | P_LEFT) == 0
}

/// Take a record out of the registry: a released one first, then one carved
/// out of the head block, then one out of a block drawn for it. Null when
/// the pool refuses that draw.
fn take_record() -> *mut MutatorRecord {
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
            (*released).free_link.set(std::ptr::null_mut());
            (*released).reader.reset();
            (*released).writer.reset();
            (*released).hold.collector.store(0, Ordering::Relaxed);
            // The token line's request byte, which neither reset above
            // reaches: a request made against the last life's lane is stale.
            (*released).clear_turnover_request();
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
        (BlockHeader::payload_start(registry.head) as *mut MutatorRecord).add(registry.carved)
    };
    registry.carved += 1;
    #[cfg(test)]
    RECORDS_CARVED.with(|count| count.set(count.get() + 1));
    gc_metadata::charge(size_of::<MutatorRecord>());
    unsafe { record.write(MutatorRecord::taken()) };
    record
}

/// Unlink and answer the first record of the free list whose blocks are the
/// mutator's, or null: a record a collector is reading, or holds blocks of
/// that an exit left, stays on the list until its hand-back. In the test
/// build a case's pin and its named record narrow the walk further
/// (`skipped_by_a_case`).
fn first_free_record(registry: &mut Registry) -> *mut MutatorRecord {
    #[cfg(test)]
    let wanted = TAKE_THIS.with(|cell| cell.replace(std::ptr::null_mut()));
    let mut link: *mut *mut MutatorRecord = &raw mut registry.free;
    loop {
        let record = unsafe { *link };
        if record.is_null() {
            return record;
        }

        #[cfg(test)]
        let skipped = !blocks_are_the_mutators(record) || skipped_by_a_case(record, wanted);
        #[cfg(not(test))]
        let skipped = !blocks_are_the_mutators(record);
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
fn skipped_by_a_case(record: *mut MutatorRecord, wanted: *mut MutatorRecord) -> bool {
    if wanted.is_null() {
        unsafe { (*record).pinned.load(Ordering::Relaxed) }
    } else {
        record != wanted
    }
}

/// Make this thread's next take pop `record` off the free list, wherever it
/// stands and pinned or not; null when the list does not hold it.
#[cfg(test)]
pub(crate) fn take_this_record_for_test(record: *mut MutatorRecord) {
    TAKE_THIS.with(|cell| cell.set(record));
}

/// Keep `record` on the free list, or let it go again, for a case that
/// reads a record after its thread's exit.
#[cfg(test)]
pub(crate) fn pin_for_test(record: *mut MutatorRecord, pinned: bool) {
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
    static TAKE_THIS: Cell<*mut MutatorRecord> = const { Cell::new(std::ptr::null_mut()) };
    /// Whether this thread's draws answer null, for a case that reads what
    /// a refused record costs. Every draw while it stands, rather than the
    /// next one, so the case's own init is the draw that meets it.
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
/// too: set, it is the mutator's gate, and the exit would wait behind it.
#[cfg(test)]
pub(crate) fn scribble_lines_for_test(record: *mut MutatorRecord) {
    unsafe {
        (*record).reader.batch.store(7, Ordering::Relaxed);
        (*record).reader.served_at.store(7, Ordering::Relaxed);
        (*record).turnover_requested.store(1, Ordering::Relaxed);
    }
}

/// Store the turnover request into `record` from the harness thread, standing
/// in for the collector's round after X.
#[cfg(test)]
pub(crate) fn request_a_turnover_for_test(record: *mut MutatorRecord) {
    unsafe { (*record).request_a_turnover() };
}

/// Whether `record`'s lines hold what a fresh life starts with: R's words,
/// the batch size, the serve instant and the turnover request empty, the
/// collecting word clear, and P's two words naming one block.
#[cfg(test)]
pub(crate) fn lines_are_fresh(record: *mut MutatorRecord) -> bool {
    let reader = unsafe { &(*record).reader };
    let writer = unsafe { &(*record).writer };
    let p_block = reader.p_tail_block.load(Ordering::Relaxed);
    reader.r_front_block.load(Ordering::Relaxed).is_null()
        && reader.batch.load(Ordering::Relaxed) == 0
        && reader.served_at.load(Ordering::Relaxed) == 0
        && unsafe { !(*record).turnover_is_requested() }
        && writer.r_tail_block.load(Ordering::Relaxed).is_null()
        && !writer.collecting.load(Ordering::Relaxed)
        && !p_block.is_null()
        && writer.p_front_block.load(Ordering::Relaxed) == p_block
}

/// The block P stands in, or null for a record with none.
#[cfg(test)]
pub(crate) fn verdict_block(record: *mut MutatorRecord) -> *mut BlockHeader {
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
pub(crate) fn registry_lists_free(record: *mut MutatorRecord) -> bool {
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
