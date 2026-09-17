//! The collector thread, and what it does for one mutator: claim the mutator's
//! token by compare-and-swap, take a batch of the mutator's candidates from
//! behind its writer, trace them on a copy through `cells::AtomicCells`
//! under a block budget, post one verdict per root into the mutator's verdict
//! ring P in R's order, advance R past them, and release
//! (`rfc/model/gc/rc-cycle.md`, "Worker-to-owner handoff";
//! `rfc/dev/DECISIONS.md`, "the candidate queue is read behind its writer,
//! and the collector's verdicts come back by a second ring", "The
//! collector's batch"). What the mutator does with the verdicts is
//! `crate::cycle::queue::verdicts`.
//!
//! # The batch
//!
//! Before any claim the collector reads whether the mutator has work — R at
//! the threshold, off the front block's words alone, and P's room — and
//! opens its own workspace: a mutator with nothing to take pays no
//! foreign-holder window, under which every one of its deaths is withheld.
//! Then it claims
//! the token and reads the mutator's collecting word with acquire: set, the
//! mutator is collecting in line and the collector releases and skips — the
//! two orders both resolve, a claim made first being waited out by the
//! mutator's take, one made second seeing the word. It peeks up to K entries
//! from R's front through the reader's pair without consuming them, K
//! clamped to P's room and to what R holds, and copies them into its
//! workspace. It marks and scans each root through `cells::AtomicCells` on
//! an arena bounded to [`TRACE_BLOCK_BUDGET`] blocks; a root at count zero
//! is marked by nothing. Then it posts one verdict per entry, in R's order
//! — *proposed*, *read live*, *zero-count* ([`verdict_for`]) — or *unwalked*
//! for every root of a batch whose trace met the budget or a refused
//! allocation: no color of such a trace is a verdict, so the whole batch is
//! handed to the mutator's exact trace rather than a prefix of it
//! (`rfc/model/gc/rc-cycle.md`, "Worker-to-owner handoff", amended
//! 2026-09-16 to the whole batch). R's
//! front advances past the batch only after every verdict is posted, by
//! one guard that runs from the unwind as well, so that no entry is
//! consumed without a verdict and none twice. The arena is reset before the
//! token goes, since its rows stand over the mutator's blocks.
//!
//! K starts at [`INITIAL_BATCH`], halves after a batch that met the budget
//! — the budget alone, a pool refusal saying nothing about the batch's size
//! — and doubles back after a completed one, up to [`BATCH_BOUND`]: under a
//! block's capacity, so a batch spans at most two blocks of R, and small
//! enough that the copy leaves the workspace to the rows. What a mutator
//! waits for when it needs its token is one batch's trace, bounded by the
//! blocks rather than by the roots.
//!
//! # The thread, and the round over the records
//!
//! The elder collector thread is born by [`ensure_thread`] and never at
//! startup (`dev/DECISIONS.md`, "the collector thread is born at the first
//! pressure collection"): the pressure path is the one fire point the
//! runtime owns, and a thread born there costs nothing to a process that
//! never runs short. The pressure collection asks for it at each of its
//! endings (`crate::cycle::collect::collect_under_pressure`). It starts
//! as any registered thread does, through `ll_thread_init`, whose base block
//! draw can be refused; a refused base block is a thread that never started,
//! and a call [`BIRTH_RETRY_INTERVAL`] or more after the refusal births
//! again. A round walks every record the registry has carved ([`round`])
//! and serves each mutator whose R holds [`SOFT_THRESHOLD`] entries or more,
//! by the collector's own reading off the front block; nothing but a test
//! ends the thread.
//!
//! **A wake starts a round and decides nothing else** (`rfc/dev/DECISIONS.md`,
//! "the collector traces on the count it reads itself"; `rfc/model/gc/
//! rc-cycle.md`, "Signals"). Between two rounds the thread waits on its
//! fallback timer, and three things end the wait: a mutator's poll, a
//! registration having filled a block of its R since its last signal
//! ([`wake`], through `crate::cycle::queue`); a pressure collection,
//! at every ending; and the timer. The timer is what serves a mutator whose
//! signal bought no batch — its token held or it collecting in line at the
//! round, P without room, the workspace refused — and a mutator at the
//! threshold that reaches no poll; a mutator below the threshold is served
//! by no round, its ring being its own collections'. The interval adapts
//! between [`FALLBACK_INTERVAL_MIN`] and [`FALLBACK_INTERVAL_MAX`]: the
//! minimum after a round that made a batch, so that a backlog above the
//! threshold drains at a batch per minimum, and after one that read a
//! mutator's note that its disposition of P freed something
//! ([`MutatorRecord::take_freeing_disposition_note`]); held after a round
//! that read a mutator at the threshold and could not serve it; doubled
//! after a round that read no mutator at the threshold, so that a process
//! with nothing to screen costs a wake a second. What a wake with no
//! thread to receive it costs is [`wake`]'s; one sent during a round ends
//! the wait that follows it.
//!
//! # Siblings
//!
//! Several collectors divide the mutators, each mutator named to one collector
//! by a word in its record ([`MutatorRecord::collector`]); two collectors
//! never read one mutator's ring. The elder, slot [`ELDER`], is the one the
//! pressure path births and every fresh record is named to. A collector
//! that served a backlog — two or more mutators still at the threshold after
//! their batches, read off the front block under the token, since one
//! mutator is read by one collector at a time and a backlog of one is nothing
//! a sibling relieves — for [`BACKLOG_ROUNDS_TO_BIRTH`] rounds in a row
//! births a sibling into the first empty slot under the embedder's cap
//! ([`set_collector_cap`]), the elder's slot included, through the elder's
//! own birth path with its retry interval, and names every second of those
//! backlogged mutators to it; the sibling's first round runs at its birth. A
//! mutator's poll wakes the collector its word names. The elder ends a
//! sibling that made no batch and saw no work for [`IDLE_ROUNDS_TO_END`]
//! rounds in a row, and one above a lowered cap, by its state word, which
//! the sibling reads before its next round, and only in a round of its own
//! with no backlog, so that it does not end what it is about to birth back;
//! the mutators of a slot with no thread — ended, refused at its birth, or
//! unwound — are named back to the elder by its next round, and a signal
//! sent to that slot meanwhile is lost with its count standing. The word
//! says whose a mutator is between rounds; that
//! one collector reads a ring at any instant is the token's, and a reclaim
//! landing beside a slot's rebirth resolves at the token like any two
//! claims.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::thread::Thread;
use std::time::{Duration, Instant};

use crate::cells::AtomicCells;
use crate::cycle::arena::TraceScratchArena;
use crate::cycle::mark::{MarkResult, mark};
use crate::cycle::mutator_record::{self, MutatorRecord};
use crate::cycle::queue::verdicts::{Verdict, VerdictWriter};
use crate::cycle::row::{EdgeTarget, resolve_edge_target};
use crate::cycle::scan::{ScanResult, scan};
use crate::cycle::shadow::{self, Color};
use crate::refcount::RcHeader;
use crate::ring::{BLOCK_ENTRIES, Reader};

/// What one round over one mutator did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Served {
    /// The mutator, or another collector, holds the token.
    TokenHeld,
    /// The token was claimed and released at once: the mutator is collecting
    /// in line.
    MutatorCollecting,
    /// Nothing was taken: before any claim — R below the threshold, P
    /// without room, the workspace refused, the record under another
    /// collector's reading — or under the claim, when the peek came up
    /// empty.
    Idle,
    /// A batch was made: this many roots taken from R, each with a verdict
    /// posted into P, whether their trace completed, and whether R still
    /// read at the threshold behind it, off the front block under the token.
    Batch {
        roots: usize,
        complete: bool,
        backlog: bool,
    },
}

/// Roots a batch takes from a mutator the collector has not served before.
/// Not a measured figure: the rfc names no start, and the size adapts from
/// here by the batch's outcome.
const INITIAL_BATCH: usize = 64;

/// The most roots a batch takes: under a block's capacity, so that the peek
/// spans at most two blocks of R, and a copy of at most a quarter of the
/// workspace's bump, so that the rows of the trace do not start by growing.
const BATCH_BOUND: usize = 1024;

const _: () = assert!(BATCH_BOUND < BLOCK_ENTRIES);
const _: () =
    assert!(BATCH_BOUND * size_of::<usize>() * 4 <= crate::cycle::arena::WORKSPACE_BUMP_BYTES);

/// Blocks a batch's trace may draw above the collector's workspace before it
/// ends with its roots unwalked. Not a measured figure: what it bounds is
/// the mutator's wait for its token, and the rfc names the bound and not its
/// size.
const TRACE_BLOCK_BUDGET: usize = 8;

/// Entries a mutator's R holds at or above which a round takes a batch from
/// it. Not a measured figure: the rfc names the threshold as the runtime's
/// own and not its size, and this one is [`INITIAL_BATCH`], so that a
/// mutator at the threshold gets a full first batch. The poll's signal is
/// sent on a block filled, a coarser unit, and the timer's rounds read this
/// one.
pub(crate) const SOFT_THRESHOLD: usize = INITIAL_BATCH;

const _: () = assert!(SOFT_THRESHOLD <= BATCH_BOUND);

/// The fallback timer's minimum: the wait after a round that made a batch or
/// read a freeing disposition. Not a measured figure.
const FALLBACK_INTERVAL_MIN: Duration = Duration::from_millis(10);

/// The fallback timer's maximum, reached by doubling after empty rounds.
/// Not a measured figure: what it bounds is how long a mutator at the
/// threshold that reaches no poll waits for a round nobody signalled.
const FALLBACK_INTERVAL_MAX: Duration = Duration::from_secs(1);

/// How long after a refused birth the pressure path waits before it spawns
/// again: a process that stays short of memory collects at every refused
/// allocation, and without the wait it would spawn a thread per refusal.
/// A sibling's birth waits the same interval after a refused one.
const BIRTH_RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// When the last birth was refused — a spawn the operating system refused,
/// or a base block the pool refused — or `None`. Shared by every slot.
static REFUSED_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// Collector threads the process can hold at once; the embedder's cap is at
/// most this. The slot index is what a mutator's record names its collector
/// by ([`MutatorRecord::collector`]).
pub(crate) const MAX_COLLECTORS: usize = 8;

/// The elder's slot: the collector the pressure path births, that every
/// fresh record is named to, and that ends idle siblings.
pub(crate) const ELDER: usize = 0;

/// Collectors the process may hold until the embedder sets its own cap
/// ([`set_collector_cap`]). Not a measured figure.
const DEFAULT_COLLECTOR_CAP: usize = 4;

/// The embedder's cap on collector threads, one to [`MAX_COLLECTORS`].
static COLLECTOR_CAP: AtomicUsize = AtomicUsize::new(DEFAULT_COLLECTOR_CAP);

/// Rounds in a row a collector serves a backlog — two or more mutators of its
/// own reading at the threshold after their batches, since one mutator is
/// read by one collector at a time and a backlog of one is no reason to
/// birth — before it births a sibling. Not a measured figure; two is the
/// rfc's.
const BACKLOG_ROUNDS_TO_BIRTH: usize = 2;

/// Mutators a round remembers as backlogged, for the handover: the first this
/// many, every second of which goes to the sibling.
const BACKLOGGED_REMEMBERED: usize = 16;

/// Rounds in a row a sibling makes no batch and reads no mutator at the
/// threshold before the elder ends it. Not a measured figure: the rfc says
/// several.
const IDLE_ROUNDS_TO_END: usize = 8;

/// The names the collector threads are spawned under, by slot.
const THREAD_NAMES: [&str; MAX_COLLECTORS] = [
    "ll-collector",
    "ll-collector-1",
    "ll-collector-2",
    "ll-collector-3",
    "ll-collector-4",
    "ll-collector-5",
    "ll-collector-6",
    "ll-collector-7",
];

/// One collector slot: where its thread stands, the handle a wake reaches it
/// through, and the count the elder reads of a sibling.
struct Collector {
    /// [`UNBORN`], [`STARTING`] from the spawn until its `ll_thread_init`
    /// answered, [`ALIVE`] from a started init until the thread ends, and
    /// [`ENDING`] from the elder's word to end a sibling until it does.
    state: AtomicU8,
    /// The handle a wake ends the thread's wait through, published by the
    /// thread once its init is through and cleared as it ends; `None` is a
    /// wake lost.
    handle: Mutex<Option<Thread>>,
    /// Rounds in a row this collector made no batch and read no mutator at
    /// the threshold, its own count, read by the elder to end an idle
    /// sibling.
    idle_rounds: AtomicUsize,
}

impl Collector {
    const fn unborn() -> Self {
        Self {
            state: AtomicU8::new(UNBORN),
            handle: Mutex::new(None),
            idle_rounds: AtomicUsize::new(0),
        }
    }
}

static COLLECTORS: [Collector; MAX_COLLECTORS] = [const { Collector::unborn() }; MAX_COLLECTORS];

const UNBORN: u8 = 0;
const STARTING: u8 = 1;
const ALIVE: u8 = 2;
const ENDING: u8 = 3;

/// Set the embedder's cap on collector threads: `cap` clamped to one and
/// [`MAX_COLLECTORS`]. Siblings above a lowered cap end as idle ones do, at
/// the elder's hand; a cap of one is a process with the elder alone.
pub(crate) fn set_collector_cap(cap: usize) {
    COLLECTOR_CAP.store(cap.clamp(1, MAX_COLLECTORS), Ordering::Relaxed);
}

fn collector_cap() -> usize {
    COLLECTOR_CAP.load(Ordering::Relaxed)
}

/// Start the elder collector thread unless the process has one already, one
/// is starting, or a birth was refused less than [`BIRTH_RETRY_INTERVAL`]
/// ago. A spawn the operating system refuses, and a base block the pool
/// refuses, each leave the process without a thread until a call after the
/// interval. The pressure path calls it after its collection, so that the
/// thread's draws compete with no rows of the caller's own.
///
/// The spawn allocates through the global allocator — the thread's name, the
/// handle's shared state — which the ruling that no runtime path may abort
/// on an allocation forbids; the debt is `PLAN.md`, backlog, "The collector
/// thread's spawn allocates through the global allocator".
pub(crate) fn ensure_thread() {
    ensure_collector(ELDER);
}

/// Start the collector of slot `index` unless it stands or is starting, or a
/// birth was refused inside the interval; true when this call spawned it.
fn ensure_collector(index: usize) -> bool {
    #[cfg(test)]
    if !testing::births_permitted() {
        return false;
    }

    let collector = &COLLECTORS[index];
    // The load before the exchange keeps every pressure collection after the
    // birth off a read-modify-write of the word.
    if collector.state.load(Ordering::Relaxed) != UNBORN
        || birth_refused_recently()
        || collector
            .state
            .compare_exchange(UNBORN, STARTING, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
    {
        return false;
    }

    collector.idle_rounds.store(0, Ordering::Relaxed);
    match std::thread::Builder::new()
        .name(THREAD_NAMES[index].into())
        .spawn(move || thread_body(index))
    {
        Ok(handle) => {
            #[cfg(test)]
            testing::keep_handle(handle);
            #[cfg(not(test))]
            drop(handle);
            true
        }
        Err(_) => {
            note_refused_birth();
            collector.state.store(UNBORN, Ordering::Release);
            false
        }
    }
}

/// Whether a birth was refused less than [`BIRTH_RETRY_INTERVAL`] ago.
fn birth_refused_recently() -> bool {
    let refused_at = REFUSED_AT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    refused_at.is_some_and(|at| at.elapsed() < BIRTH_RETRY_INTERVAL)
}

fn note_refused_birth() {
    let mut refused_at = REFUSED_AT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *refused_at = Some(Instant::now());
}

/// Wake the collector of slot `index` out of its wait, and answer whether
/// the process had one to wake: a wake is a soft signal that starts a round,
/// and a round reads every mutator's count itself (module doc). Callable from
/// any thread; a mutator's poll makes it at [`SOFT_THRESHOLD`] registrations,
/// to the collector its record names, and a pressure collection at each of
/// its endings, to the elder. False is a wake lost: before the thread's
/// birth, between its spawn and its init, and after its end. A lost wake
/// costs nothing but the round it did not start: the poll leaves its flag
/// standing and sends again at its next poll
/// (`crate::cycle::queue::signal_the_collector_if_due`), and a sibling's
/// first round runs at its birth.
pub(crate) fn wake(index: usize) -> bool {
    let collector = COLLECTORS[index]
        .handle
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match collector.as_ref() {
        Some(thread) => {
            thread.unpark();
            true
        }
        None => false,
    }
}

/// Whether slot `index` holds a thread that has started and not ended.
fn is_alive(index: usize) -> bool {
    COLLECTORS[index].state.load(Ordering::Acquire) == ALIVE
}

/// Whether slot `index` holds no thread at all: none born, one ended, or
/// one whose birth was refused. A slot between its spawn and its init is
/// neither.
fn has_no_thread(index: usize) -> bool {
    COLLECTORS[index].state.load(Ordering::Acquire) == UNBORN
}

/// The collector thread's life: its registration, its rounds, its exit.
fn thread_body(index: usize) {
    // The word goes back to unborn however this thread ends — a refused
    // base block, a test's retire, the elder's end, or a panic in a round
    // that unwinds out of here — so that a later birth can happen rather
    // than read a thread that no longer exists; the wake handle goes with
    // it, so that a wake after the end is lost rather than sent to a thread
    // that is not there.
    struct UnbornOnDrop(usize);
    impl Drop for UnbornOnDrop {
        fn drop(&mut self) {
            let collector = &COLLECTORS[self.0];
            *collector
                .handle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
            collector.state.store(UNBORN, Ordering::Release);
        }
    }
    let _unborn = UnbornOnDrop(index);

    let started = {
        #[cfg(test)]
        let _budget = testing::base_block_budget_for_this_birth();
        crate::memory::heap::ll_thread_init()
    };
    if !started {
        // The base block was refused, so this is a thread that never started
        // (`rfc/dev/DECISIONS.md`, "the baseline overflow segment is
        // allocator-issued"); a call after the interval births again.
        note_refused_birth();
        return;
    }

    let collector = &COLLECTORS[index];
    *collector
        .handle
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(std::thread::current());
    collector.state.store(ALIVE, Ordering::Release);
    let mut interval = FALLBACK_INTERVAL_MIN;
    let mut backlog_rounds = 0;
    while !retiring() && collector.state.load(Ordering::Relaxed) == ALIVE {
        let Round {
            made_a_batch,
            saw_work,
            read_a_freeing_disposition,
            backlogged,
        } = round(index, threshold_for_rounds());
        interval = if made_a_batch || read_a_freeing_disposition {
            FALLBACK_INTERVAL_MIN
        } else if saw_work {
            interval
        } else {
            (interval * 2).min(FALLBACK_INTERVAL_MAX)
        };

        backlog_rounds = if backlogged.len() >= 2 {
            backlog_rounds + 1
        } else {
            0
        };
        if backlog_rounds >= BACKLOG_ROUNDS_TO_BIRTH {
            backlog_rounds = 0;
            if let Some(sibling) = birth_a_sibling(index) {
                hand_over_half(&backlogged, sibling);
            }
        }

        // A mutator at the threshold this round could not serve is work and
        // not idleness, so a sibling whose mutator collects in line for a
        // while is not ended for it.
        let idle = if made_a_batch || saw_work {
            0
        } else {
            collector.idle_rounds.load(Ordering::Relaxed) + 1
        };
        collector.idle_rounds.store(idle, Ordering::Relaxed);
        if index == ELDER && backlogged.len() < 2 {
            end_idle_siblings();
        }

        #[cfg(test)]
        testing::note_round(index, interval);
        #[cfg(test)]
        let interval = testing::interval_for_this_wait().unwrap_or(interval);
        std::thread::park_timeout(interval);
    }

    crate::memory::heap::ll_thread_exit();
}

/// Birth a sibling in the first empty slot under the cap other than `from`,
/// the elder's slot included — an elder that unwound is reborn by the
/// first backlogged sibling rather than by the next memory shortage —
/// through the same path as the elder's birth, retry interval included;
/// the slot it took, or `None` for no slot or a refused spawn. The
/// sibling's first round runs at its birth, so no wake is owed.
fn birth_a_sibling(from: usize) -> Option<usize> {
    (0..collector_cap())
        .filter(|&slot| slot != from)
        .find(|&slot| ensure_collector(slot))
}

/// Name every second of `backlogged` to `to`: the handover the sibling's
/// first rounds read, made over the mutators the round read at the threshold
/// after their batches and no other, so that what moves is work.
fn hand_over_half(backlogged: &Backlogged, to: usize) {
    for record in backlogged.iter().skip(1).step_by(2) {
        unsafe { &**record }.name_to_collector(to);
    }
}

/// End every sibling that made no batch and saw no work for
/// [`IDLE_ROUNDS_TO_END`] rounds, and every one above the cap: the elder's,
/// once per round in which it read no backlog of its own — a sibling is not
/// ended while the elder would birth one back. The mutators of an ended
/// sibling are the elder's again at its next round ([`reclaims`]).
fn end_idle_siblings() {
    let cap = collector_cap();
    for (slot, collector) in COLLECTORS.iter().enumerate().skip(1) {
        if !is_alive(slot) {
            continue;
        }

        if slot >= cap || collector.idle_rounds.load(Ordering::Relaxed) >= IDLE_ROUNDS_TO_END {
            // From alive alone: a slot that ended or unwound meanwhile is
            // left as it is, for a birth to take.
            let _ = collector.state.compare_exchange(
                ALIVE,
                ENDING,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
            let _ = wake(slot);
        }
    }
}

/// Whether the round of `index` serves `record`: a mutator named to it, and
/// for the elder also a mutator named to a slot with no living thread — a
/// sibling ended, refused at its birth, or unwound — which it takes back by
/// rewriting the word.
fn reclaims(index: usize, record: &MutatorRecord) -> bool {
    let named = record.collector();
    if named == index {
        return true;
    }

    if index == ELDER && has_no_thread(named) {
        record.name_to_collector(ELDER);
        return true;
    }

    false
}

/// The threshold the rounds serve at: the module's own, or a case's.
fn threshold_for_rounds() -> usize {
    #[cfg(test)]
    if let Some(threshold) = testing::threshold_for_rounds() {
        return threshold;
    }

    SOFT_THRESHOLD
}

/// The block budget the next batch traces under: the module's own, or a
/// case's.
fn budget_for_this_batch() -> usize {
    #[cfg(test)]
    if let Some(budget) = testing::budget_for_this_batch() {
        return budget;
    }

    TRACE_BLOCK_BUDGET
}

/// Clear the last refusal, so that a case's birth is not held by the
/// refusal of the case before it.
#[cfg(test)]
fn forget_refused_birth() {
    let mut refused_at = REFUSED_AT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *refused_at = None;
}

/// Whether a test asked the thread to end; false in every other build.
#[cfg(not(test))]
fn retiring() -> bool {
    false
}

#[cfg(test)]
use testing::retiring;

/// The mutators a round read at the threshold after their batches, the first
/// [`BACKLOGGED_REMEMBERED`] of them, in a fixed array on the round's frame.
#[derive(Debug)]
struct Backlogged {
    records: [*mut MutatorRecord; BACKLOGGED_REMEMBERED],
    len: usize,
}

impl Default for Backlogged {
    fn default() -> Self {
        Self {
            records: [std::ptr::null_mut(); BACKLOGGED_REMEMBERED],
            len: 0,
        }
    }
}

impl Backlogged {
    /// Remember `record`, or drop it once the array is full.
    fn push(&mut self, record: *mut MutatorRecord) {
        if self.len < BACKLOGGED_REMEMBERED {
            self.records[self.len] = record;
            self.len += 1;
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn iter(&self) -> impl Iterator<Item = &*mut MutatorRecord> {
        self.records[..self.len].iter()
    }
}

/// What a round read across the records, for the timer and the siblings.
#[derive(Debug, Default)]
struct Round {
    /// Some mutator was served a batch.
    made_a_batch: bool,
    /// Some mutator read at the threshold was not served: its token held, or
    /// it collecting in line.
    saw_work: bool,
    /// Some mutator's poll noted a disposition that freed something since the
    /// last round.
    read_a_freeing_disposition: bool,
    /// The mutators still at the threshold after their batches.
    backlogged: Backlogged,
}

/// One round of the collector of slot `index` over the records: visit every
/// record named to it but the thread's own, which polls nothing, read each
/// mutator's note for the timer, and serve each whose R holds `threshold`
/// entries or more. What a serve does is [`serve`]'s.
fn round(index: usize, threshold: usize) -> Round {
    let own = mutator_record::this_thread_record();
    let mut outcome = Round::default();
    mutator_record::for_each_record(|record| {
        if record == own || !reclaims(index, unsafe { &*record }) {
            return;
        }

        #[cfg(test)]
        if !testing::in_round(record) {
            return;
        }

        // The note is read whatever the serve answers: a free-list record
        // has a count equal to the copy, and a record between threads
        // answers one spurious shortening at most.
        if unsafe { &*record }.take_freeing_disposition_note() {
            outcome.read_a_freeing_disposition = true;
        }

        let served = unsafe { serve(record, threshold) };
        match served {
            Served::Batch { backlog, .. } => {
                outcome.made_a_batch = true;
                if backlog {
                    outcome.backlogged.push(record);
                }
            }
            Served::TokenHeld | Served::MutatorCollecting => outcome.saw_work = true,
            Served::Idle => {}
        }

        #[cfg(test)]
        testing::note_served(served);
    });
    outcome
}

/// Serve `record`'s mutator once: claim its token by compare-and-swap, held
/// being a skip; skip a mutator collecting in line; otherwise make one batch
/// (module doc) and release. `threshold` is the count of R, read before
/// any claim, below which the mutator is idle to this serve; the round passes
/// [`SOFT_THRESHOLD`].
///
/// Runs on a collector thread, which holds a base block of its own for the
/// workspace the batch's trace opens (`crate::memory::heap::ll_thread_init`).
///
/// # Safety
/// `record` is a record of the registry's, and the calling thread is not its
/// mutator.
pub(crate) unsafe fn serve(record: *mut MutatorRecord, threshold: usize) -> Served {
    let mutator = unsafe { &*record };
    // Work first, and the collector's own memory, before any claim — by
    // loads alone, since nothing of the mutator's may be written under no
    // claim, and off the front block alone (`Reader::has_at_least` says
    // why): whether R holds the threshold, P's room off its index words,
    // and the workspace this thread's. The figures are an idle test and not
    // the clamp: the clamp is re-read under the token. The blocks read are
    // held for the reading, since a mutator exiting meanwhile returns them
    // (`crate::cycle::mutator_record`, "The blocks a collector reads before
    // its claim are held"); a record another reading holds is idle to this
    // round.
    if !unsafe { mutator_record::take_for_reading(record) } {
        return Served::Idle;
    }

    // Handed back on the unwind too: a hold left standing keeps the record
    // off the registry's free list and its blocks out of the pool for good.
    struct HandBackOnDrop(*mut MutatorRecord);
    impl Drop for HandBackOnDrop {
        fn drop(&mut self) {
            unsafe { mutator_record::hand_back_reading(self.0) };
        }
    }
    let (has_work, room) = {
        let _reading = HandBackOnDrop(record);
        #[cfg(test)]
        testing::between_the_take_and_the_reading();
        let has_work = unsafe { Reader::new(mutator.candidate_ring()) }.has_at_least(threshold);
        let room = unsafe { VerdictWriter::open(mutator) }.room_by_loads();
        (has_work, room)
    };
    if !has_work || room == 0 {
        return Served::Idle;
    }

    let Some(mut arena) = TraceScratchArena::open() else {
        return Served::Idle;
    };

    if !mutator.token.try_take() {
        return Served::TokenHeld;
    }

    // Released on the unwind too: a collector that panicked under the claim
    // would otherwise leave the mutator's exit waiting forever.
    struct ReleaseOnDrop<'a>(&'a crate::cycle::token::TraceToken);
    impl Drop for ReleaseOnDrop<'_> {
        fn drop(&mut self) {
            crate::cycle::token::note_traced_mutator(std::ptr::null_mut());
            self.0.release();
        }
    }
    let _held = ReleaseOnDrop(&mutator.token);
    crate::cycle::token::note_traced_mutator(record);
    if mutator.is_collecting_as_collector() {
        return Served::MutatorCollecting;
    }

    unsafe { batch(mutator, &mut arena, threshold) }
}

/// One batch over `mutator`, under its token, on `arena` — the collector's own
/// memory, reset before the token goes (module doc); `threshold` is what
/// the batch's backlog reading is against.
///
/// # Safety
/// The calling thread holds `mutator`'s token and `mutator` is not collecting
/// in line.
unsafe fn batch(
    mutator: &MutatorRecord,
    arena: &mut TraceScratchArena,
    threshold: usize,
) -> Served {
    let verdicts = unsafe { VerdictWriter::open(mutator) };
    // The clamp, under the token: P's room cannot move under it, R's count
    // can only grow.
    let size = match mutator.batch_size() {
        0 => INITIAL_BATCH,
        size => size,
    };
    let take = verdicts.room().min(size);
    if take == 0 {
        return Served::Idle;
    }
    arena.budget_blocks(budget_for_this_batch());
    let copy = arena.alloc(take * size_of::<usize>()) as *mut usize;
    assert!(
        !copy.is_null(),
        "the copy fits the workspace by the bound on K"
    );

    // The entries copied out of R, which stay in R until the advance.
    let out = unsafe { std::slice::from_raw_parts_mut(copy, take) };
    let reader = unsafe { Reader::new(mutator.candidate_ring()) };
    let peeked = reader.peek(out);
    let roots = &out[..peeked.len()];
    if roots.is_empty() {
        return Served::Idle;
    }

    let complete = unsafe { trace(arena, roots) };
    for &entry in roots {
        let root = crate::cycle::queue::entry_root(entry);
        let verdict = if complete {
            unsafe { verdict_for(root) }
        } else {
            Verdict::Unwalked
        };
        verdicts
            .post(root, verdict)
            .expect("the batch was clamped to P's room");
    }

    // Every verdict is posted: from here the advance is owed, and the guard
    // makes it from the unwind as well.
    struct AdvanceOnDrop<'a>(&'a Reader<'a>, crate::ring::Peeked);
    impl Drop for AdvanceOnDrop<'_> {
        fn drop(&mut self) {
            self.0.commit(self.1);
        }
    }
    let advance = AdvanceOnDrop(&reader, peeked);
    #[cfg(test)]
    testing::between_the_post_and_the_advance();
    drop(advance);
    let backlog = reader.has_at_least(threshold);

    let met_budget = arena.met_its_budget();
    arena.reset();
    if complete {
        mutator.set_batch_size((size * 2).min(BATCH_BOUND));
    } else if met_budget {
        mutator.set_batch_size((size / 2).max(1));
    }

    Served::Batch {
        roots: roots.len(),
        complete,
        backlog,
    }
}

/// Mark every root of `roots`, then scan every one: true when both phases
/// completed, false when either met the budget or a refused allocation — at
/// which point no color is a verdict.
///
/// # Safety
/// As [`mark`] through `AtomicCells`: the calling thread holds the mutator's
/// token, and every root is an entry of the mutator's R.
unsafe fn trace(arena: &mut TraceScratchArena, roots: &[usize]) -> bool {
    for &entry in roots {
        let root = crate::cycle::queue::entry_root(entry);
        if unsafe { mark::<AtomicCells>(arena, root) } != MarkResult::Complete {
            return false;
        }
    }

    for &entry in roots {
        let root = crate::cycle::queue::entry_root(entry);
        if unsafe { scan::<AtomicCells>(arena, root) } != ScanResult::Complete {
            return false;
        }
    }

    true
}

/// The verdict a completed trace supports for `root`: a count read zero is
/// [`Verdict::ZeroCount`] before any row is read, a row read potentially
/// unreachable is [`Verdict::Proposed`], and a row read live is
/// [`Verdict::ReadLive`] — as is a live root with no met row, one the trace
/// could not place, which the trace's own rule reads as an external live
/// reference and which the mutator's trace would place no better; an
/// *unwalked* verdict would send it round P and R at every poll.
///
/// # Safety
/// The trace over `root` completed on this thread and its rows still stand.
unsafe fn verdict_for(root: *mut RcHeader) -> Verdict {
    if unsafe { crate::refcount::slot_state(root) } != crate::refcount::SlotState::Live {
        return Verdict::ZeroCount;
    }

    let EdgeTarget::Tracked(key) = (unsafe { resolve_edge_target(root) }) else {
        return Verdict::ReadLive;
    };

    match unsafe { crate::cycle::arena::find_initialized_row(key) } {
        Some(row) => match shadow::color(unsafe { *row }) {
            Color::PotentiallyUnreachable => Verdict::Proposed,
            _ => Verdict::ReadLive,
        },
        None => Verdict::ReadLive,
    }
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests;
