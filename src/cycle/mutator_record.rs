//! The mutator's record: the words of one mutator thread that a collector
//! thread reaches, in storage that outlives the thread
//! (`rfc/model/gc/rc-cycle.md`, "Worker-to-owner handoff"; `dev/DECISIONS.md`,
//! "the token stands in a record the process keeps, and the exit's claim on
//! it is never released"). Four lines: the token line, the trace token's
//! byte and its wait; the reader's line, the collector's words of the
//! two rings; the writer's line, the mutator's words of them and its count
//! of freeing dispositions; and the hold line, the word under
//! which a collector reads the rings' blocks before its claim, the word
//! that names the mutator's collector, and the mutator's epoch clock, which
//! that collector keeps (`crate::cycle::epoch`; `rfc/dev/DECISIONS.md`, "the candidate
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
//! The same gate reads the standing list's link ([`first_free_record`]): a
//! record a collector's request stands on past the wait is linked into
//! that collector's list and is handed out only once its pass has dropped
//! it, so that one collector's list is never threaded through a record's
//! next life ([`ReaderLine::standing_next`]).
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
    /// The low eight bits of this mutator's epoch cell
    /// ([`HoldLine::turnovers`]), stored by the collector at every advance
    /// and by nobody else. The poll that finds the deferred lane occupied
    /// compares it with the lane's mirror and re-offers the lane when the two
    /// differ (`crate::gc`, the poll; `crate::cycle::queue`, the deferred
    /// lane). On this line because the poll reads the token beside it.
    /// Relaxed on both sides: nothing is published beside it, and a byte read
    /// late delays the re-offer by one poll, never a wrong free. Eight bits
    /// wrap at 256 turnovers, which at X is over half an hour of a thread
    /// that never polls; the price of the alias is one more X.
    turnover: AtomicU8,
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
/// read of the front block on a full tail block), and its link word by the
/// registry's gate ([`first_free_record`]).
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
    /// Whether the collector's checkpoint released this mutator's grant
    /// without a batch — one grant is served per pass, the rest let go —
    /// so that the collector's next request to it is pushed onto the
    /// standing list with no wait: the mutator is asleep again by the time
    /// the walk reaches it (`dev/design/the-standing-request-lives-on-the-record.md`,
    /// "The collector").
    /// Set and cleared by the collector alone; cleared with the line at a
    /// re-take.
    released_unserved: AtomicBool,
    /// The link pair of the collector's standing list: the records whose
    /// request stands on the byte past the wait, a doubly linked list
    /// threaded through the records with its two end pointers on the
    /// collector's frame (`crate::cycle::worker::Standing`;
    /// `dev/design/the-standing-request-lives-on-the-record.md`, "The
    /// words"). The ends are self-terminated — the last record's `next` and
    /// the first's `prev` name the record itself — so both words are null
    /// exactly when the record is in no list, a non-null `next` reads as
    /// linked, and no pointer names anything but a record. Written by the
    /// collector the record names and by no mutator, in one order: `next`
    /// is the first word a link writes and the last an unlink clears, both
    /// with release, `prev` and the neighbours' words strictly between, so
    /// that the registry's one acquire load of `next` ([`first_free_record`])
    /// sees nothing of the link or all of it and a record is renamed only
    /// while unlinked. What publishes the link to the gate is the request
    /// that follows it, whose release the exit's take reads, and where the
    /// request fails the reading's hold ([`HoldLine::reading`]), which
    /// spans the link and the request and is handed back after the unlink:
    /// the gate reads the hold word before the link word. Not reset with
    /// the line: a reset that nulled a link would cut the list behind it.
    standing_next: AtomicPtr<MutatorRecord>,
    standing_prev: AtomicPtr<MutatorRecord>,
}

const _: () = assert!(size_of::<ReaderLine>() == 64);

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
    /// Merges of the deferred lane into R at a turnover, counted up by the
    /// mutator after each such splice and never cleared within a life,
    /// wrapping: the collector
    /// compares it with [`HoldLine::merges_seen`] and takes a ring below the
    /// threshold at the round after a merge rather than an interval later,
    /// whether or not the owner packed the merged blocks into one since
    /// (`crate::cycle::worker`, "The thread, and the round over the
    /// records"). Stored with release after the splice's own stores, so that
    /// a round whose acquire load reads a merge reads the spliced ring
    /// after it. On this line because the round loads [`WriterLine::r_tail_block`]
    /// beside it.
    merges: AtomicU32,
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
    /// The serve clock's reading at the round that first read this
    /// mutator's candidate ring non-empty and below the round's threshold,
    /// in nanoseconds since the base `crate::cycle::worker` fixes at the
    /// process's first serve; zero for a ring the round counts no interval
    /// for — one it read empty or at the threshold, and one whose record
    /// the registry has just handed out. The round takes such a ring as an
    /// ordinary batch at the first visit the standing interval of
    /// `crate::cycle::worker` or more after that reading, and stamps the
    /// word again at the end of every batch
    /// (`dev/design/a-standing-r-is-taken-after-n-rounds.md`, "The round").
    /// On this line with the collector's other words a round writes. Read and
    /// written under the reading hold or under this collector's grant,
    /// which is where the round's reading and the batch's end stand, and
    /// nowhere else: outside both, a record between the hand-back and the
    /// registry's clear belongs to whichever thread takes it next. Written
    /// by the collector the record names and read by no mutator, so
    /// relaxed; cleared with the line at a re-take.
    standing_since: AtomicU64,
    /// The collector whose standing list this record is in, as its slot
    /// index plus one, and zero for a record in no list. Written by that
    /// collector alone, beside the link pair and in the same operations
    /// ([`crate::cycle::worker::Standing`]), and read by its own debug
    /// assertions: a list's splice reads the record's words and the
    /// frame's ends, so a record renamed to another collector while linked
    /// would be spliced out of a list that is not the splicer's, silently
    /// in a release build (`rfc/dev/design/trace-token-handshake.md`,
    /// "(a)"). Relaxed both ways: every store and every read is that one
    /// collector's own thread.
    standing_slot: AtomicU8,
    /// The collector thread this mutator is named to, as a slot index of
    /// `crate::cycle::worker`'s: zero is the elder, and a fresh record's.
    /// Written by a collector at a handover and read by the mutator's poll,
    /// which wakes the collector it names, and by every collector's round,
    /// which serves the mutators named to it. On this line because it is the
    /// one word a collector writes into a record it does not read for.
    collector: AtomicU8,
    /// This word and the three after it, the epoch's, are written by the
    /// advance outside the reading hold and the grant, unlike
    /// [`HoldLine::standing_since`]: the collector's visit advances before its
    /// serve requests the token, and a record between two lives is visited
    /// like any other. Every interleaving with the
    /// registry's re-take ends in an extra advance or a restamp, and either
    /// costs recall only (`crate::cycle::epoch`, "A reading that missed an
    /// advance is conservative").
    ///
    /// Whether the registry handed this record out again since the
    /// collector's last visit: that visit advances the epoch once and clears
    /// the byte, so that no stamp the last life wrote reads fresh against
    /// the new one's clock (`crate::cycle::epoch`, "A record's next life").
    /// Set by the registry, cleared by the collector; relaxed, since a flag
    /// read late costs recall until the next advance and nothing else.
    new_life: AtomicU8,
    /// Batches the collector made for this mutator since the last advance,
    /// saturating: the advance comes at the first of
    /// `crate::cycle::epoch::BATCHES_PER_EPOCH` of them or X of the
    /// collector's clock. Written by the collector the record is named to
    /// alone, so relaxed; cleared at every advance and at a re-take.
    batches_since: AtomicU8,
    /// This mutator's epoch clock: the turnovers of its epoch, full width and
    /// monotone across the record's lives. The epoch a maturation stamp
    /// carries is its low two bits (`crate::cycle::epoch`). Written by the
    /// collector the record is named to and by nobody else, at each advance
    /// (`crate::cycle::worker`, "The epoch clock"); read once per collection
    /// by whoever traces this mutator's graph, at the arena's open, so that
    /// the owner's collections and the collector's batches prune against
    /// the same clock. Relaxed: a reading that missed an advance prunes
    /// against an epoch that has ended, which costs a descent and never a
    /// wrong free (`crate::cycle::mark`, "The epoch is the arena's").
    turnovers: AtomicU64,
    /// The collector's clock at the last advance, in nanoseconds since the
    /// base `crate::cycle::worker` fixes at the process's first serve; zero
    /// for a life no visit has read yet, which the first visit stamps
    /// without advancing. The collector's own word, so relaxed; cleared at a
    /// re-take.
    advanced_at: AtomicU64,
    /// The value of [`WriterLine::merges`] every entry of whose merges the
    /// collector has accounted for: stored at the end of every grant, with
    /// the count read under the token before the batch's peek, and at a
    /// round that read the ring empty, with the count read before the ring.
    /// A round that reads the ring below the threshold
    /// and the two counts apart takes it. Read and written where
    /// [`HoldLine::standing_since`] is, and for the same reason; relaxed,
    /// the collector's own word; cleared at a re-take.
    merges_seen: AtomicU32,
    /// The first block of the live list the last grant's batch wrote, for the
    /// mutator to stamp from or to drop, and null for none
    /// (`crate::cycle::live_list`). Written by the collector under its grant,
    /// before the release that stores `POSTED`, with a release of its own; taken
    /// back to null by one swap, by the mutator after its acquire reading of
    /// `POSTED`, or by the collector the record is named to once the epoch has
    /// advanced past the list, with an acquire swap
    /// (`crate::cycle::live_list::give_back_a_stale_list`). Either ordering makes
    /// the chain's blocks visible to the side that wins the swap. `FREE`
    /// promises a null word as it promises an empty P. Not cleared at a
    /// re-take: the exit's take consumed it, and the registry asserts so.
    live_list: AtomicPtr<BlockHeader>,
    /// The serve clock's reading when [`HoldLine::live_list`] was published,
    /// in nanoseconds since the base `crate::cycle::worker` fixes: a list
    /// published before the last advance of the epoch is the collector's to
    /// give back. Written before the word, which its release publishes.
    live_list_published_at: AtomicU64,
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
            released_unserved: AtomicBool::new(false),
            standing_next: AtomicPtr::new(std::ptr::null_mut()),
            standing_prev: AtomicPtr::new(std::ptr::null_mut()),
        }
    }

    /// Empty the line in place, for a record taken off the free list. The
    /// link pair is left alone: the registry hands out no linked record, so
    /// both read null here.
    fn reset(&self) {
        debug_assert!(
            self.standing_next.load(Ordering::Relaxed).is_null()
                && self.standing_prev.load(Ordering::Relaxed).is_null(),
            "a linked record was handed out"
        );
        self.r_front_block
            .store(std::ptr::null_mut(), Ordering::Relaxed);
        self.p_tail_block
            .store(std::ptr::null_mut(), Ordering::Relaxed);
        self.batch.store(0, Ordering::Relaxed);
        self.freeing_dispositions_seen.store(0, Ordering::Relaxed);
        self.released_unserved.store(false, Ordering::Relaxed);
    }
}

impl WriterLine {
    const fn empty() -> Self {
        Self {
            r_tail_block: AtomicPtr::new(std::ptr::null_mut()),
            p_front_block: AtomicPtr::new(std::ptr::null_mut()),
            collecting: AtomicBool::new(false),
            freeing_dispositions: AtomicU32::new(0),
            merges: AtomicU32::new(0),
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
        self.merges.store(0, Ordering::Relaxed);
    }
}

// The free link is written under the registry's lock and every other field
// is atomic, which is what lets a record be reached from two threads.
unsafe impl Sync for MutatorRecord {}

const _: () = assert!(size_of::<HoldLine>() == 64);
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
            turnover: AtomicU8::new(0),
            free_link: Cell::new(std::ptr::null_mut()),
            #[cfg(test)]
            pinned: AtomicBool::new(false),
            reader: ReaderLine::empty(),
            writer: WriterLine::empty(),
            hold: HoldLine {
                reading: AtomicU8::new(0),
                standing_since: AtomicU64::new(0),
                standing_slot: AtomicU8::new(0),
                collector: AtomicU8::new(0),
                new_life: AtomicU8::new(0),
                batches_since: AtomicU8::new(0),
                turnovers: AtomicU64::new(0),
                advanced_at: AtomicU64::new(0),
                merges_seen: AtomicU32::new(0),
                live_list: AtomicPtr::new(std::ptr::null_mut()),
                live_list_published_at: AtomicU64::new(0),
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

    /// The instant a round first read this mutator's candidate ring standing
    /// non-empty below the round's threshold ([`HoldLine::standing_since`]),
    /// or zero for a ring no interval is counted for. Read under the reading
    /// hold or under this collector's grant: outside both, the record may
    /// be the registry's or another thread's between the load and its use.
    /// A case standing in for the collector reads it on its own thread,
    /// where the record's holder is the case.
    #[inline]
    pub(crate) fn standing_since(&self) -> u64 {
        self.hold.standing_since.load(Ordering::Relaxed)
    }

    /// Stamp the instant this mutator's ring has stood since, or zero for a
    /// ring no interval is counted for ([`HoldLine::standing_since`]).
    /// Written under the reading hold or under this collector's grant, and
    /// nowhere else: outside both, the record may be the registry's or
    /// another thread's before the store lands.
    #[inline]
    pub(crate) fn note_standing_since(&self, nanos: u64) {
        self.hold.standing_since.store(nanos, Ordering::Relaxed);
    }

    /// Count one merge of the deferred lane into R ([`WriterLine::merges`]),
    /// on the mutator's thread, after the splice.
    #[inline]
    pub(crate) fn note_a_merge(&self) {
        let merges = self.writer.merges.load(Ordering::Relaxed);
        self.writer
            .merges
            .store(merges.wrapping_add(1), Ordering::Release);
    }

    /// The mutator's count of merges ([`WriterLine::merges`]), on the
    /// collector's thread, loaded before the ring reading it goes with.
    #[inline]
    pub(crate) fn merges(&self) -> u32 {
        self.writer.merges.load(Ordering::Acquire)
    }

    /// The merges the collector has accounted for ([`HoldLine::merges_seen`]).
    /// Read under the reading hold or under this collector's grant, as
    /// [`MutatorRecord::standing_since`] is.
    #[inline]
    pub(crate) fn merges_seen(&self) -> u32 {
        self.hold.merges_seen.load(Ordering::Relaxed)
    }

    /// Record `merges` as accounted for ([`HoldLine::merges_seen`]). Written
    /// under the reading hold or under this collector's grant, as
    /// [`MutatorRecord::note_standing_since`] is.
    #[inline]
    pub(crate) fn note_merges_seen(&self, merges: u32) {
        self.hold.merges_seen.store(merges, Ordering::Relaxed);
    }

    /// Publish `head`, the first block of the live list this grant's batch
    /// wrote ([`HoldLine::live_list`]), on the collector's thread under its
    /// grant and before the release, whose store of `POSTED` is what the
    /// mutator reads it behind. The word reads null: the request that opened
    /// the grant landed on `FREE`.
    #[inline]
    pub(crate) fn publish_live_list(&self, head: *mut BlockHeader, published_at: u64) {
        debug_assert!(
            self.hold.live_list.load(Ordering::Relaxed).is_null(),
            "a grant opened over a list nobody consumed"
        );
        self.hold
            .live_list_published_at
            .store(published_at, Ordering::Relaxed);
        self.hold.live_list.store(head, Ordering::Release);
    }

    /// When the standing live list was published
    /// ([`HoldLine::live_list_published_at`]).
    #[inline]
    pub(crate) fn live_list_published_at(&self) -> u64 {
        self.hold.live_list_published_at.load(Ordering::Relaxed)
    }

    /// The live list standing on this record, or null
    /// ([`HoldLine::live_list`]): the mutator's filter before it reads the
    /// byte, which decides whether the list is its own yet, and the
    /// collector's before it reads the instant of the publication, which the
    /// acquire orders after the word.
    #[inline]
    pub(crate) fn live_list(&self) -> *mut BlockHeader {
        self.hold.live_list.load(Ordering::Acquire)
    }

    /// Write `FREE` over `POSTED`, giving back the live list the grant left
    /// under the mutator's claim in between, so that `FREE` keeps its promise
    /// of a null list word: a case that batches again without a collection
    /// between, standing in for the disposition it makes by hand afterwards
    /// (`discard_standing_verdicts`). Nothing else writes `FREE` over
    /// `POSTED`. On the thread whose record this is.
    #[cfg(test)]
    pub(crate) fn clear_posted_for_test(&self) {
        if self.token.take_posted_for_test() {
            unsafe { crate::cycle::live_list::drop_from(self) };
            self.token.release();
        }
    }

    /// Take the live list off this record, leaving null: the one swap that
    /// decides whether the mutator or the collector gives it back
    /// ([`HoldLine::live_list`]). Acquire, for the collector, which read no
    /// `POSTED` before it.
    #[inline]
    pub(crate) fn take_live_list(&self) -> *mut BlockHeader {
        self.hold
            .live_list
            .swap(std::ptr::null_mut(), Ordering::Acquire)
    }

    /// The collector whose list this record stands in, as a slot index plus
    /// one, or zero for a record in no list ([`HoldLine::standing_slot`]).
    #[inline]
    pub(crate) fn standing_slot(&self) -> u8 {
        self.hold.standing_slot.load(Ordering::Relaxed)
    }

    /// Stamp the collector whose list this record now stands in, or zero it
    /// as the unlink does ([`HoldLine::standing_slot`]).
    #[inline]
    pub(crate) fn note_standing_slot(&self, stamp: u8) {
        self.hold.standing_slot.store(stamp, Ordering::Relaxed);
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

    /// Whether a checkpoint released this mutator's grant without a batch
    /// ([`ReaderLine::released_unserved`]); the collector's own line, so
    /// relaxed.
    #[inline]
    pub(crate) fn was_released_unserved(&self) -> bool {
        self.reader.released_unserved.load(Ordering::Relaxed)
    }

    /// Note or clear the release without a batch
    /// ([`ReaderLine::released_unserved`]).
    #[inline]
    pub(crate) fn note_released_unserved(&self, released: bool) {
        self.reader
            .released_unserved
            .store(released, Ordering::Relaxed);
    }

    /// Whether this record stands in a collector's standing list
    /// ([`ReaderLine::standing_next`]): one acquire load, paired with the
    /// release of the unlink that nulls it.
    #[inline]
    pub(crate) fn is_standing(&self) -> bool {
        !self.reader.standing_next.load(Ordering::Acquire).is_null()
    }

    /// The list's forward link, for the collector's list alone
    /// (`crate::cycle::worker::Standing`).
    #[inline]
    pub(crate) fn standing_next(&self) -> &AtomicPtr<MutatorRecord> {
        &self.reader.standing_next
    }

    /// The list's backward link, for the collector's list alone
    /// (`crate::cycle::worker::Standing`).
    #[inline]
    pub(crate) fn standing_prev(&self) -> &AtomicPtr<MutatorRecord> {
        &self.reader.standing_prev
    }

    /// This mutator's epoch clock ([`HoldLine::turnovers`]): read by the
    /// arena of every collection over this mutator's graph, once, at its
    /// open.
    #[inline]
    pub(crate) fn turnovers(&self) -> u64 {
        self.hold.turnovers.load(Ordering::Relaxed)
    }

    /// The low eight bits of the clock as the collector last stored them
    /// ([`MutatorRecord::turnover`]), on the mutator's thread: the poll's
    /// reading against the deferred lane's mirror.
    #[inline]
    pub(crate) fn turnover_byte(&self) -> u8 {
        self.turnover.load(Ordering::Relaxed)
    }

    /// Advance the clock by one turnover, on the collector's thread: the
    /// cell, then the byte the poll reads, then the instant and the batch
    /// count the next advance is measured from. The cell is moved by one
    /// read-modify-write, so two visits of one record — a round and a
    /// handover's, or a round over a record between two lives — each count
    /// and neither moves the cell back; the byte they store last may be the
    /// earlier of the two, which delays a re-offer by one advance and frees
    /// nothing wrongly.
    #[inline]
    pub(crate) fn advance_the_epoch(&self, now: u64) {
        let turnovers = self
            .hold
            .turnovers
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        self.turnover.store(turnovers as u8, Ordering::Relaxed);
        self.hold.advanced_at.store(now, Ordering::Relaxed);
        self.hold.batches_since.store(0, Ordering::Relaxed);
    }

    /// The collector's clock at the last advance, or zero for a life no
    /// visit has read yet ([`HoldLine::advanced_at`]).
    #[inline]
    pub(crate) fn advanced_at(&self) -> u64 {
        self.hold.advanced_at.load(Ordering::Relaxed)
    }

    /// Stamp the instant the next advance is measured from without
    /// advancing: the first visit of a life.
    #[inline]
    pub(crate) fn note_advanced_at(&self, now: u64) {
        self.hold.advanced_at.store(now, Ordering::Relaxed);
    }

    /// Batches made for this mutator since the last advance
    /// ([`HoldLine::batches_since`]).
    #[inline]
    pub(crate) fn batches_since_the_advance(&self) -> u8 {
        self.hold.batches_since.load(Ordering::Relaxed)
    }

    /// Count one batch made for this mutator, on the collector's thread,
    /// saturating.
    #[inline]
    pub(crate) fn note_batch(&self) {
        let batches = self.hold.batches_since.load(Ordering::Relaxed);
        self.hold
            .batches_since
            .store(batches.saturating_add(1), Ordering::Relaxed);
    }

    /// Whether the registry handed this record out again since the last
    /// visit, clearing the note ([`HoldLine::new_life`]).
    #[inline]
    pub(crate) fn take_new_life(&self) -> bool {
        self.hold.new_life.swap(0, Ordering::Relaxed) != 0
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
    debug_assert!(
        unsafe { (*record).live_list() }.is_null(),
        "the exit's take consumes the live list a life left"
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
            (*released).hold.standing_since.store(0, Ordering::Relaxed);
            // The stamp of the list the record last stood in: cleared by the
            // unlink already, and cleared again here because a record whose
            // life ended under a standing request is unlinked by its
            // collector's pass and not by this path.
            (*released).hold.standing_slot.store(0, Ordering::Relaxed);
            // The clock itself is left where the last life moved it: the
            // collector's next visit advances it once, so that no stamp that
            // life wrote reads fresh against this one's
            // (`crate::cycle::epoch`, "A record's next life"). The instant and
            // the batch count restart with the life.
            (*released).hold.batches_since.store(0, Ordering::Relaxed);
            (*released).hold.advanced_at.store(0, Ordering::Relaxed);
            (*released).hold.merges_seen.store(0, Ordering::Relaxed);
            debug_assert!(
                (*released).hold.live_list.load(Ordering::Relaxed).is_null(),
                "the exit's take consumes the live list a life left"
            );
            (*released).hold.new_life.store(1, Ordering::Relaxed);
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
/// mutator's and which stands in no collector's list, or null: a record a
/// collector is reading, or holds blocks of that an exit left, stays on the
/// list until its hand-back, and one a collector's request stands on until
/// that collector's pass drops it. In the test build a case's pin and its
/// named record narrow the walk further (`skipped_by_a_case`).
fn first_free_record(registry: &mut Registry) -> *mut MutatorRecord {
    #[cfg(test)]
    let wanted = TAKE_THIS.with(|cell| cell.replace(std::ptr::null_mut()));
    let mut link: *mut *mut MutatorRecord = &raw mut registry.free;
    loop {
        let record = unsafe { *link };
        if record.is_null() {
            return record;
        }

        // A record in a collector's standing list is skipped, so that a
        // record is renamed only while unlinked and one collector's list is
        // never threaded through another's: a thread that exited under a
        // standing request leaves its record linked until that collector's
        // next pass drops it, one round at most, since the exit's take of
        // the request is a refusal that wakes the collector
        // (`crate::cycle::token::TraceToken::take_unless`).
        let skipped = !blocks_are_the_mutators(record) || unsafe { &*record }.is_standing();
        #[cfg(test)]
        let skipped = skipped || skipped_by_a_case(record, wanted);
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

/// Link or unlink `record` by hand, standing in for the list of collector
/// `slot`: a list of one, whose ends name the record itself. The stores are
/// in the list's order ([`ReaderLine::standing_next`]): `next` first on the
/// link and last on the unlink, with the slot's stamp beside them as a
/// collector's own link makes it.
#[cfg(test)]
pub(crate) fn link_for_test(record: *mut MutatorRecord, slot: usize, linked: bool) {
    let mutator = unsafe { &*record };
    let (next, prev) = (mutator.standing_next(), mutator.standing_prev());
    if linked {
        mutator.note_standing_slot(slot as u8 + 1);
        next.store(record, Ordering::Release);
        prev.store(record, Ordering::Release);
    } else {
        prev.store(std::ptr::null_mut(), Ordering::Release);
        next.store(std::ptr::null_mut(), Ordering::Release);
        mutator.note_standing_slot(0);
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

/// A thread that asks the registry for `record` by name, and answers whether
/// it got it; a refused name falls through to a carve, so the thread starts
/// either way.
#[cfg(test)]
pub(crate) fn a_thread_asking_for(record: *mut MutatorRecord) -> bool {
    let sent = crate::cycle::testing::Sent(record);
    std::thread::spawn(move || {
        let wanted = sent.into_inner();
        take_this_record_for_test(wanted);
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the asking thread"
        );
        let got = this_thread_record() == wanted;
        crate::memory::heap::ll_thread_exit();
        got
    })
    .join()
    .expect("the asking thread finished")
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

/// Write into `record`'s lines, for a case that reads whether a re-take
/// empties them: the batch size, which no exit reads, the merge count, and
/// on the hold line the standing instant, the list's stamp, the advance's
/// instant, its batch count and the merges seen. The four block words are left alone, because the
/// exit reads both rings through them and a scribbled pointer would be
/// followed; they are nulled by the rings' dismantle before the record goes
/// back, which is what the reset repeats. The collecting word is left alone
/// too: set, it is the mutator's gate, and the exit would wait behind it.
/// The hold line's reading word is left alone for the same reason: it is
/// what the registry's gate reads to tell a held record from a free one.
#[cfg(test)]
pub(crate) fn scribble_lines_for_test(record: *mut MutatorRecord) {
    unsafe {
        (*record).reader.batch.store(7, Ordering::Relaxed);
        (*record).hold.standing_since.store(7, Ordering::Relaxed);
        (*record).hold.standing_slot.store(7, Ordering::Relaxed);
        (*record).hold.advanced_at.store(7, Ordering::Relaxed);
        (*record).hold.batches_since.store(7, Ordering::Relaxed);
        (*record).hold.merges_seen.store(7, Ordering::Relaxed);
        (*record).writer.merges.store(7, Ordering::Relaxed);
    }
}

/// Note a new life on `record` as the registry's re-take does, for a case
/// that reads what the collector's next visit makes of it.
#[cfg(test)]
pub(crate) fn note_new_life_for_test(record: *mut MutatorRecord) {
    unsafe { (*record).hold.new_life.store(1, Ordering::Relaxed) };
}

/// Whether `record`'s lines hold what a fresh life starts with: R's words,
/// the batch size, the standing instant, the standing list's stamp, the
/// advance's instant, its batch count, the merge count and the merges seen
/// empty, the collecting word clear, and P's two words naming one block.
#[cfg(test)]
pub(crate) fn lines_are_fresh(record: *mut MutatorRecord) -> bool {
    let reader = unsafe { &(*record).reader };
    let writer = unsafe { &(*record).writer };
    let p_block = reader.p_tail_block.load(Ordering::Relaxed);
    reader.r_front_block.load(Ordering::Relaxed).is_null()
        && reader.batch.load(Ordering::Relaxed) == 0
        && unsafe { (*record).hold.standing_since.load(Ordering::Relaxed) == 0 }
        && unsafe { (*record).hold.standing_slot.load(Ordering::Relaxed) == 0 }
        && unsafe { (*record).hold.advanced_at.load(Ordering::Relaxed) == 0 }
        && unsafe { (*record).hold.batches_since.load(Ordering::Relaxed) == 0 }
        && unsafe { (*record).hold.merges_seen.load(Ordering::Relaxed) == 0 }
        && writer.merges.load(Ordering::Relaxed) == 0
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
