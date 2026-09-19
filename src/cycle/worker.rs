//! The collector thread, and what it does for one mutator: request the
//! mutator's token and wait for its consent, take a batch of the mutator's
//! candidates from behind its writer, trace them on a copy through
//! `cells::AtomicCells` under a block budget, post one verdict per root into
//! the mutator's verdict ring P in R's order, advance R past them, and
//! release — to `POSTED`, which tells the mutator to collect over P
//! (`rfc/dev/design/trace-token-handshake.md`;
//! `rfc/model/gc/rc-cycle.md`, "Worker-to-owner handoff";
//! `rfc/dev/DECISIONS.md`, "the candidate queue is read behind its writer,
//! and the collector's verdicts come back by a second ring", "The
//! collector's batch"). What the mutator does with the verdicts is
//! `crate::cycle::queue::verdicts`.
//!
//! # The batch
//!
//! Before any claim the collector reads whether the mutator has work — R at
//! the threshold, off the front block's words alone, and P's room: a
//! mutator with nothing to take pays no foreign-holder window, under which
//! every one of its deaths is withheld. Then it requests
//! the token by one swap from `FREE`, which fails on a mutator collecting in
//! line — the mutator holds `MUTATOR` through its close — on one that has
//! not disposed of the last batch (`POSTED`) and on every other holder, a
//! failure being a skip; and it waits for the mutator's consent, which the
//! mutator gives at its next slot free or poll ([`serve`] says how long,
//! and what a mutator that never answers costs). It peeks up to K entries
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
//! consumed without a verdict and none twice. The arena is opened under
//! the grant, one per batch, and is reset before the token goes — on the
//! unwind as on the return, since its rows stand over the mutator's blocks
//! (`rfc/dev/design/trace-token-handshake.md`, E2); a workspace the pool
//! refuses is a grant released with no batch.
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
//! startup: at the first wake a mutator's poll would send it — a block of R
//! filled (`crate::cycle::queue::signal_the_collector_if_due`) — and at each
//! ending of a pressure collection
//! (`crate::cycle::collect::collect_under_pressure`), so that a process
//! that never fills a block and never runs short holds no thread. It starts
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
//! fallback timer, and four things end the wait: a mutator's poll, a
//! registration having filled a block of its R since its last signal
//! ([`wake`], through `crate::cycle::queue`); a mutator's consent to a
//! request this collector left standing; a pressure collection, at every
//! ending; and the timer. The timer is what serves a mutator whose signal
//! bought no batch — its token held or it collecting in line at the round,
//! P without room, the workspace refused — and a mutator at the threshold
//! that reaches no poll; a mutator below the threshold is served by no
//! round. The interval adapts between [`FALLBACK_INTERVAL_MIN`] and
//! [`FALLBACK_INTERVAL_MAX`]: the minimum after a round that made a batch,
//! so that a backlog above the threshold drains at a batch per minimum,
//! and after one that read a mutator's note that the collection its poll
//! fired freed or retired something
//! ([`MutatorRecord::take_freeing_disposition_note`]); held after a round
//! that read a mutator at the threshold and could not serve it; doubled
//! after a round that read no mutator at the threshold — a mutator at
//! `POSTED` or one whose request stands unanswered is neither a batch nor
//! work, and its note is the way back — so that a process with nothing to
//! screen costs a wake a second. What a wake with no thread to receive it
//! costs is [`wake`]'s; one sent during a round ends the wait that follows
//! it, and a wait inside a round that consumed a wake is paid for by
//! skipping the sleep after that round once.
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

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::cells::AtomicCells;
use crate::cycle::arena::TraceScratchArena;
use crate::cycle::mark::{MarkResult, mark};
use crate::cycle::mutator_record::{self, MutatorRecord};
use crate::cycle::queue::verdicts::{Verdict, VerdictWriter};
use crate::cycle::row::{EdgeTarget, resolve_edge_target};
use crate::cycle::scan::{ScanResult, scan};
use crate::cycle::shadow::{self, Color};
use crate::cycle::token::{COLLECTOR, POSTED, REQUESTED, Withdrawn, state, word};
use crate::refcount::RcHeader;
use crate::ring::{BLOCK_ENTRIES, Reader};

/// What one round over one mutator did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Served {
    /// The byte was not free: the mutator collects in line, another
    /// collector holds or asks for it, or the mutator refused the request.
    TokenHeld,
    /// The mutator has not disposed of the last batch's verdicts: the byte
    /// reads `POSTED`. Neither a batch nor work — nothing the collector can
    /// do serves it — and the mutator's note of its disposition is what
    /// brings the timer back.
    Posted,
    /// The request stands unanswered: the mutator reached no slot free and
    /// no poll inside the wait, or was silent already. Neither a batch nor
    /// work; the request is served at a checkpoint when the mutator
    /// answers, or, past the standing array's capacity, was withdrawn for
    /// this round.
    Unanswered,
    /// Nothing was taken: before any claim — R below the threshold, P
    /// without room, the record under another collector's reading — under
    /// the claim, when the workspace was refused or the peek came up empty,
    /// or at the withdrawal, when the record had moved on.
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

/// How long a collector waits for a mutator that answered its last request
/// to consent to this one, before it withdraws. Not a measured figure: it
/// lands above the tail of the interval between two polls or slot frees of
/// a running mutator on the corpus, which bench measures
/// (`rfc/dev/design/trace-token-handshake.md`, "Cost"); a mutator blocked
/// past it is marked silent and its next request stands with no wait.
const REQUEST_WAIT: Duration = Duration::from_millis(2);

/// Requests to silent mutators a collector keeps standing at once, on its
/// own frame; a silent mutator past the capacity is skipped that round. Not
/// a measured figure.
const STANDING_CAPACITY: usize = BACKLOGGED_REMEMBERED;

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

/// One collector slot: where its thread stands, the word a wake reaches it
/// through, and the count the elder reads of a sibling.
struct Collector {
    /// [`UNBORN`], [`STARTING`] from the spawn until its `ll_thread_init`
    /// answered, [`ALIVE`] from a started init until the thread ends, and
    /// [`ENDING`] from the elder's word to end a sibling until it does.
    state: AtomicU8,
    /// The wake word: set under the mutex by [`wake`], whoever the sender,
    /// and taken by the thread's waits, so a wake sent before the wait ends
    /// it at once. Cleared when the thread announces itself alive, so a
    /// wake sent to an empty slot is lost rather than handed to the next
    /// birth as a round nobody asked for.
    wake_pending: Mutex<bool>,
    /// What the waits sleep on, notified with every set of the wake word.
    wake_signal: Condvar,
    /// Rounds in a row this collector made no batch and read no mutator at
    /// the threshold, its own count, read by the elder to end an idle
    /// sibling.
    idle_rounds: AtomicUsize,
}

impl Collector {
    const fn unborn() -> Self {
        Self {
            state: AtomicU8::new(UNBORN),
            wake_pending: Mutex::new(false),
            wake_signal: Condvar::new(),
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
/// The birth is a thread the OS entry creates on a stack the slot keeps
/// ([`birth`]), so the path meets no allocation whose refusal is an abort.
/// **A call that births waits for the slot's last thread first**: the stack
/// is the slot's, so the birth joins what stood there, which returns once
/// that thread's teardown is done. A call that births nothing — the common
/// one, the thread standing — takes no lock and waits for nothing.
pub(crate) fn ensure_thread() {
    ensure_collector(ELDER);
}

/// Start the collector of slot `index` unless it stands or is starting, or a
/// birth was refused inside the interval; true when this call spawned it. A
/// call that spawns waits for the slot's last thread, as [`ensure_thread`]
/// says.
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
    if !birth::spawn(index) {
        note_refused_birth();
        collector.state.store(UNBORN, Ordering::Release);
        return false;
    }

    #[cfg(test)]
    testing::note_spawn();
    true
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
/// birth, between its spawn and its init, while it ends, and after its
/// end. A lost wake
/// costs nothing but the round it did not start: the poll leaves its flag
/// standing and sends again at its next poll
/// (`crate::cycle::queue::signal_the_collector_if_due`), and a sibling's
/// first round runs at its birth. The one wake that answers true and starts
/// no round of its own is the one sent between [`forget_wakes`] and the
/// `ALIVE` store of [`begin_the_thread`]: it is cleared there, and the
/// thread's first round, which runs before its first wait, stands in for
/// it.
pub(crate) fn wake(index: usize) -> bool {
    let collector = &COLLECTORS[index];
    *collector
        .wake_pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
    collector.wake_signal.notify_all();
    is_alive(index)
}

/// Sleep on slot `index`'s wake word until a wake or `timeout`, and take
/// the word either way: a wake that lands as the timeout runs out is spent
/// on the round that follows rather than kept for the next wait. The thread
/// blocks on the condvar and never re-reads the word in a loop of its own,
/// which is what lets it make progress under Miri's weak-memory emulation
/// (`dev/WORKFLOW.md`, Miri, "A test thread waits, it does not spin").
fn wait_for_a_wake(index: usize, timeout: Duration) {
    let collector = &COLLECTORS[index];
    let pending = collector
        .wake_pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut pending, _) = collector
        .wake_signal
        .wait_timeout_while(pending, timeout, |pending| !*pending)
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *pending = false;
}

/// Clear slot `index`'s wake word, so that a wake sent while no thread
/// stood is not the next thread's first wait ended.
fn forget_wakes(index: usize) {
    *COLLECTORS[index]
        .wake_pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = false;
}

/// Whether slot `index` holds a thread that has started and not ended.
fn is_alive(index: usize) -> bool {
    COLLECTORS[index].state.load(Ordering::Acquire) == ALIVE
}

/// Whether slot `index` holds no thread of the crate's: none born, one
/// ended, or one whose birth was refused. A slot between its spawn and its
/// init is neither. An ended thread's OS thread may still stand — the word
/// goes unborn on the thread itself, before glibc's teardown and while the
/// slot still holds it to be joined ([`birth`]) — so this answers who reads
/// the mutators of the slot and never whether a stack is free.
fn has_no_thread(index: usize) -> bool {
    COLLECTORS[index].state.load(Ordering::Acquire) == UNBORN
}

/// The collector thread's life: its registration, its rounds, its exit.
/// Run by [`birth::run_the_life`], which stores the word back to unborn
/// however this returns — a refused base block, a test's retire, the
/// elder's end, or a panic in a round that unwinds out of here — after the
/// runtime exit, so that a later birth can happen rather than read a thread
/// that no longer exists.
fn thread_body(index: usize) {
    if !begin_the_thread(index) {
        return;
    }

    let collector = &COLLECTORS[index];
    let mut interval = FALLBACK_INTERVAL_MIN;
    let mut backlog_rounds = 0;
    // The requests this collector left standing on silent mutators, on this
    // frame for the thread's life; the drop withdraws them.
    let mut standing = Standing::new(index);
    while !retiring() && collector.state.load(Ordering::Relaxed) == ALIVE {
        #[cfg(test)]
        testing::note_round_start(index);
        let outcome = round(index, threshold_for_rounds(), &mut standing);
        interval = next_interval(interval, &outcome);

        backlog_rounds = if outcome.backlogged.len() >= 2 {
            backlog_rounds + 1
        } else {
            0
        };
        if backlog_rounds >= BACKLOG_ROUNDS_TO_BIRTH {
            backlog_rounds = 0;
            grow_the_siblings(index, &outcome.backlogged);
        }

        note_idleness(index, &outcome);

        #[cfg(test)]
        testing::note_round(index, interval);
        #[cfg(test)]
        let interval = testing::interval_for_this_wait().unwrap_or(interval);
        // A wait inside a round that ended early with nothing served may
        // have consumed a round-start wake, so the sleep after that round is
        // skipped once (`rfc/dev/design/trace-token-handshake.md`, "The two
        // sides", the collector).
        if !standing.take_consumed_a_wake() {
            wait_for_a_wake(index, interval);
        }
    }

    crate::memory::heap::ll_thread_exit();
}

/// Draw this collector thread's base block and announce it alive, or answer
/// false for a thread that never started.
///
/// A refused base block is a birth that did not happen
/// (`rfc/dev/DECISIONS.md`, "the baseline overflow segment is
/// allocator-issued"), and a call after the interval births again.
fn begin_the_thread(index: usize) -> bool {
    let started = {
        #[cfg(test)]
        let _budget = testing::base_block_budget_for_this_birth();
        crate::memory::heap::ll_thread_init()
    };
    if !started {
        note_refused_birth();
        return false;
    }

    // Before the state, so that the first wake this thread's wait can take
    // is one sent after the state that wake answers on.
    forget_wakes(index);
    COLLECTORS[index].state.store(ALIVE, Ordering::Release);
    true
}

/// The wait before the next round, from what this one did: a batch or a
/// freeing disposition is the shortest interval, work seen without a batch
/// holds it where it stands, and an idle round doubles it up to
/// [`FALLBACK_INTERVAL_MAX`].
fn next_interval(interval: Duration, outcome: &Round) -> Duration {
    if outcome.made_a_batch || outcome.read_a_freeing_disposition {
        FALLBACK_INTERVAL_MIN
    } else if outcome.saw_work {
        interval
    } else {
        (interval * 2).min(FALLBACK_INTERVAL_MAX)
    }
}

/// Birth one sibling for the backlog this round left and hand it half of
/// the backlogged mutators; a cap that refuses the birth leaves the backlog
/// where it is.
fn grow_the_siblings(index: usize, backlogged: &Backlogged) {
    if let Some(sibling) = birth_a_sibling(index) {
        hand_over_half(backlogged, sibling);
    }
}

/// Count this round against collector `index`'s idle rounds, and let the
/// elder end the siblings that have run out of work.
///
/// A mutator at the threshold this round could not serve is work and not
/// idleness, so a sibling whose mutator collects in line for a while is not
/// ended for it.
fn note_idleness(index: usize, outcome: &Round) {
    let collector = &COLLECTORS[index];
    let idle = if outcome.made_a_batch || outcome.saw_work {
        0
    } else {
        collector.idle_rounds.load(Ordering::Relaxed) + 1
    };
    collector.idle_rounds.store(idle, Ordering::Relaxed);
    if index == ELDER && outcome.backlogged.len() < 2 {
        end_idle_siblings();
    }
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
fn round(index: usize, threshold: usize, standing: &mut Standing) -> Round {
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

        unsafe { read_one_record(record, index, threshold, standing, &mut outcome) };
    });
    outcome
}

/// One record of a round: read its note for the timer, serve it once, and
/// fold what the serve answered into `outcome`.
///
/// The note is read whatever the serve answers — a free-list record has a
/// count equal to the copy, and a record between threads answers one
/// spurious shortening at most. A batch served at a checkpoint inside the
/// serve counts as this round's too, which is what the standing array's
/// count carries out.
///
/// # Safety
/// `record` is a record of the registry's that this collector reclaims, and
/// the calling thread is not its mutator.
unsafe fn read_one_record(
    record: *mut MutatorRecord,
    index: usize,
    threshold: usize,
    standing: &mut Standing,
    outcome: &mut Round,
) {
    if unsafe { &*record }.take_freeing_disposition_note() {
        outcome.read_a_freeing_disposition = true;
    }

    let served = unsafe { serve(record, index, threshold, standing) };
    outcome.made_a_batch |= standing.take_batches_served() > 0;
    match served {
        Served::Batch { backlog: true, .. } => {
            outcome.made_a_batch = true;
            outcome.backlogged.push(record);
        }
        Served::Batch { .. } => outcome.made_a_batch = true,
        Served::TokenHeld => outcome.saw_work = true,
        Served::Idle | Served::Posted | Served::Unanswered => {}
    }

    #[cfg(test)]
    testing::note_served(served);
}

/// Serve `record`'s mutator once: request its token, wait for the mutator's
/// consent, make one batch (module doc) under the grant and release —
/// to `POSTED` when the batch posted verdicts, which is what tells the
/// mutator to collect, and to `FREE` when it posted nothing. `threshold`
/// is the count of R, read before any request, below which the mutator is
/// idle to this serve; the round passes [`SOFT_THRESHOLD`]. `slot` is the
/// calling collector's, the name its request writes into the byte.
///
/// **The request is one swap `FREE → REQUESTED|slot`**, and every other
/// value is a skip: `MUTATOR` a mutator collecting in line, `POSTED` one
/// that has not disposed of the last batch, `REQUESTED` or `COLLECTOR`
/// another collector's. A mutator that answered its last request is waited
/// for up to [`REQUEST_WAIT`], in a loop on the byte alone — the wait's
/// return is not an answer — and past the deadline the request is withdrawn, the
/// withdrawal's read-back deciding what stood there meanwhile
/// ([`crate::cycle::token::Withdrawn`]). A mutator that did not answer its
/// last request is silent ([`MutatorRecord::is_silent`]): its request is
/// made and left standing on the collector's frame, with no wait, and is
/// served at a checkpoint ([`Standing::checkpoint`]) once the mutator
/// answers — at its first slot free or poll after waking, at most one
/// stranger's batch away — or withdrawn when the thread ends. The
/// checkpoints are the two places the collector commits time: before every
/// request, here, and after every return of the wait
/// (`rfc/dev/design/trace-token-handshake.md`, "The two sides", the
/// collector, and the second and third rounds).
///
/// Runs on a collector thread, which holds a base block of its own for the
/// workspace the batch's trace opens (`crate::memory::heap::ll_thread_init`).
///
/// # Safety
/// `record` is a record of the registry's, and the calling thread is not its
/// mutator.
pub(crate) unsafe fn serve(
    record: *mut MutatorRecord,
    slot: usize,
    threshold: usize,
    standing: &mut Standing,
) -> Served {
    let mutator = unsafe { &*record };
    // The checkpoint before the request: a silent mutator that consented
    // since the last one is served ahead of any stranger.
    standing.checkpoint(threshold);

    // Work first, and the collector's own memory, before any request — by
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

    if let Err(seen) = mutator.token.request(slot) {
        return unsafe {
            answer_a_refused_request(mutator, seen, record, slot, threshold, standing)
        };
    }

    if mutator.is_silent() {
        return if standing.push(record) {
            Served::Unanswered
        } else {
            // Past the array's capacity the request is withdrawn at once,
            // and the mutator skipped this round.
            unsafe { answer_the_withdrawal(mutator, slot, threshold) }
        };
    }

    unsafe { wait_for_consent(mutator, slot, threshold, standing) }
}

/// The four-way reading of a request the token refused, as [`serve`]'s
/// answer.
///
/// The refusal's value names who holds the byte: a batch of this mutator's
/// own that nothing has disposed of, this collector's request still standing
/// on a silent mutator, this collector's grant — consented to between the
/// checkpoint and the request, so the array forgets the standing entry and
/// the grant is served here — or any other holder, which is a skip.
///
/// # Safety
/// As [`serve`], and `seen` is the value that refusal read back.
unsafe fn answer_a_refused_request(
    mutator: &MutatorRecord,
    seen: u8,
    record: *mut MutatorRecord,
    slot: usize,
    threshold: usize,
    standing: &mut Standing,
) -> Served {
    if state(seen) == POSTED {
        return Served::Posted;
    }

    if seen == word(REQUESTED, slot) {
        // This collector's own request, still standing on a silent
        // mutator: neither a batch nor work, round after round.
        return Served::Unanswered;
    }

    if seen == word(COLLECTOR, slot) {
        // This collector's own grant: a standing request consented to
        // between [`serve`]'s checkpoint and its request, served here
        // and forgotten by the array.
        standing.forget(record);
        return unsafe { serve_the_grant(mutator, slot, threshold) };
    }

    Served::TokenHeld
}

/// Wait out [`REQUEST_WAIT`] for the mutator's consent to the request
/// [`serve`] just made, and answer: the grant is served, and anything else
/// — a refusal, a life ended, or the deadline — is the withdrawal's
/// read-back ([`answer_the_withdrawal`]).
///
/// The wait is on the byte alone, its own return being no answer; a return
/// before the deadline that is not the grant runs the second checkpoint and
/// remembers a wake it may have consumed.
///
/// # Safety
/// As [`serve`], and this collector's request stands on `mutator`.
unsafe fn wait_for_consent(
    mutator: &MutatorRecord,
    slot: usize,
    threshold: usize,
    standing: &mut Standing,
) -> Served {
    // Withdrawn on the unwind between the request and the grant: a request
    // left standing by a collector that is gone would be consented to by a
    // mutator that then withholds forever.
    let mut request = WithdrawOnDrop {
        token: &mutator.token,
        slot,
        standing: true,
    };
    let granted = word(COLLECTOR, slot);
    let requested = word(REQUESTED, slot);
    #[cfg(not(test))]
    let wait = REQUEST_WAIT;
    #[cfg(test)]
    let wait = testing::request_wait();
    let deadline = Instant::now() + wait;
    loop {
        let seen = mutator.token.read();
        if seen == granted {
            request.standing = false;
            break;
        }

        // Anything but the standing request is an answer: the grant above,
        // or a refusal or a life ended, which the withdrawal's read-back
        // names without waiting out the bound.
        let now = Instant::now();
        if seen != requested || now >= deadline {
            request.standing = false;
            return unsafe { answer_the_withdrawal(mutator, slot, threshold) };
        }

        wait_for_a_wake(slot, deadline - now);
        // A return before the deadline that was not the grant: the
        // checkpoint, and if it served nothing the wake this wait may have
        // consumed is remembered.
        if Instant::now() < deadline
            && mutator.token.read() != granted
            && standing.checkpoint(threshold) == 0
        {
            standing.consumed_a_wake = true;
        }
    }

    unsafe { serve_the_grant(mutator, slot, threshold) }
}

/// Withdraw collector `slot`'s request from `mutator` and answer by the
/// read-back: a grant is served, a withdrawal that landed marks the mutator
/// silent, a take by the mutator is its refusal, and a record moved on —
/// `FREE`, or another slot's value — is idle to this serve, the collector
/// holding nothing of it.
///
/// # Safety
/// The calling collector made the request `REQUESTED|slot` on `mutator`.
unsafe fn answer_the_withdrawal(mutator: &MutatorRecord, slot: usize, threshold: usize) -> Served {
    match mutator.token.withdraw(slot) {
        Withdrawn::Granted => unsafe { serve_the_grant(mutator, slot, threshold) },
        Withdrawn::Withdrawn => {
            mutator.note_silent(true);
            Served::Unanswered
        }
        Withdrawn::TakenByTheMutator => {
            #[cfg(test)]
            testing::note_refusal();
            Served::TokenHeld
        }
        Withdrawn::MovedOn => Served::Idle,
    }
}

/// Serve a grant this collector holds: the arena opened, the batch under
/// `COLLECTOR|slot`, the arena's reset, the release — to `POSTED` when the
/// batch posted, and to `FREE` at once when the pool refuses the workspace,
/// which is `Idle`.
///
/// # Safety
/// The calling collector holds `mutator`'s token as `COLLECTOR|slot`.
unsafe fn serve_the_grant(mutator: &MutatorRecord, slot: usize, threshold: usize) -> Served {
    // Released on the unwind too: a collector that panicked under the claim
    // would otherwise leave the mutator's wait forever; the posted fact is
    // set before the first post, so the unwind's release says what the
    // return's would.
    struct ReleaseOnDrop<'a> {
        token: &'a crate::cycle::token::TraceToken,
        slot: usize,
        posted: std::cell::Cell<bool>,
    }
    impl Drop for ReleaseOnDrop<'_> {
        fn drop(&mut self) {
            crate::cycle::token::note_traced_mutator(std::ptr::null_mut());
            self.token.release_claim(self.slot, self.posted.get());
        }
    }
    let held = ReleaseOnDrop {
        token: &mutator.token,
        slot,
        posted: std::cell::Cell::new(false),
    };
    crate::cycle::token::note_traced_mutator(std::ptr::from_ref(mutator).cast_mut());
    mutator.note_silent(false);
    #[cfg(test)]
    testing::note_grant();

    // Declared after the release guard, so that its drop — the reset of
    // the rows, which stand over the mutator's blocks — runs before the
    // release on the unwind as on the return.
    let Some(mut arena) = TraceScratchArena::open() else {
        return Served::Idle;
    };
    unsafe { batch(mutator, &mut arena, threshold, &held.posted) }
}

/// A request between its swap and its grant, withdrawn on the unwind.
struct WithdrawOnDrop<'a> {
    token: &'a crate::cycle::token::TraceToken,
    slot: usize,
    standing: bool,
}

impl Drop for WithdrawOnDrop<'_> {
    fn drop(&mut self) {
        if !self.standing {
            return;
        }

        if self.token.withdraw(self.slot) == Withdrawn::Granted {
            // A grant read back on the unwind is released with no batch.
            self.token.release_claim(self.slot, false);
        }
    }
}

/// The requests a collector left standing on silent mutators: a fixed array
/// on the collector thread's frame, read at the checkpoints and withdrawn
/// when the thread ends. A standing request costs no wait; the consent
/// wake cannot be lost, since the slot's wake word makes a wake sent
/// mid-round end the next wait at once.
pub(crate) struct Standing {
    slot: usize,
    entries: [*mut MutatorRecord; STANDING_CAPACITY],
    len: usize,
    /// Batches the checkpoints made since the round last asked.
    batches_served: usize,
    /// Whether a wait inside a round returned early and served nothing, so
    /// that the sleep after the round is skipped once.
    consumed_a_wake: bool,
}

impl Standing {
    pub(crate) fn new(slot: usize) -> Self {
        Self {
            slot,
            entries: [std::ptr::null_mut(); STANDING_CAPACITY],
            len: 0,
            batches_served: 0,
            consumed_a_wake: false,
        }
    }

    /// Keep `record`'s request standing; false past the capacity.
    fn push(&mut self, record: *mut MutatorRecord) -> bool {
        if self.len == STANDING_CAPACITY {
            return false;
        }

        self.entries[self.len] = record;
        self.len += 1;
        true
    }

    /// Read every standing request once, one acquire load each, and serve
    /// the ones consented to: `COLLECTOR|slot` is served now, `REQUESTED|slot`
    /// left standing, and anything else — `MUTATOR`, `FREE`, another slot's
    /// value — is a record moved on, whose entry is dropped. Answers how
    /// many batches it made.
    fn checkpoint(&mut self, threshold: usize) -> usize {
        let requested = word(REQUESTED, self.slot);
        let granted = word(COLLECTOR, self.slot);
        let mut served = 0;
        let mut index = 0;
        while index < self.len {
            let record = self.entries[index];
            let mutator = unsafe { &*record };
            let seen = mutator.token.read();
            if seen == requested {
                index += 1;
                continue;
            }

            self.remove(index);
            if seen == granted {
                let outcome = unsafe { serve_the_grant(mutator, self.slot, threshold) };
                #[cfg(test)]
                testing::note_served(outcome);
                if let Served::Batch { .. } = outcome {
                    served += 1;
                }
            }
        }

        self.batches_served += served;
        served
    }

    /// Drop `record`'s entry, if it stands: the request was served by the
    /// walk itself.
    fn forget(&mut self, record: *mut MutatorRecord) {
        if let Some(index) = self.entries[..self.len]
            .iter()
            .position(|&entry| entry == record)
        {
            self.remove(index);
        }
    }

    fn remove(&mut self, index: usize) {
        self.len -= 1;
        self.entries[index] = self.entries[self.len];
        self.entries[self.len] = std::ptr::null_mut();
    }

    fn take_batches_served(&mut self) -> usize {
        std::mem::replace(&mut self.batches_served, 0)
    }

    /// Batches the checkpoints made since the round last asked, left as
    /// they are.
    #[cfg(test)]
    pub(crate) fn batches_served_for_test(&self) -> usize {
        self.batches_served
    }

    fn take_consumed_a_wake(&mut self) -> bool {
        std::mem::replace(&mut self.consumed_a_wake, false)
    }
}

impl Drop for Standing {
    /// The withdrawal of every standing request, at the thread's end: a
    /// grant read back is released without a batch.
    fn drop(&mut self) {
        for &record in &self.entries[..self.len] {
            let token = unsafe { &(*record).token };
            if token.withdraw(self.slot) == Withdrawn::Granted {
                token.release_claim(self.slot, false);
            }
        }
    }
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
    posted: &std::cell::Cell<bool>,
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
    posted.set(true);
    unsafe { post_the_verdicts(&verdicts, roots, complete) };

    // Every verdict is posted: from here the advance is owed, and the guard
    // makes it from the unwind as well.
    let advance = AdvanceOnDrop(&reader, peeked);
    #[cfg(test)]
    testing::between_the_post_and_the_advance();
    drop(advance);
    let backlog = reader.has_at_least(threshold);

    let met_budget = arena.met_its_budget();
    arena.reset();
    size_the_next_batch(mutator, size, complete, met_budget);

    Served::Batch {
        roots: roots.len(),
        complete,
        backlog,
    }
}

/// The advance a posted batch owes R, made on the unwind as well: past the
/// last verdict the entries are the collector's answer, and leaving them
/// unread would hand the same roots to the next batch.
struct AdvanceOnDrop<'a>(&'a Reader<'a>, crate::ring::Peeked);

impl Drop for AdvanceOnDrop<'_> {
    fn drop(&mut self) {
        self.0.commit(self.1);
    }
}

/// Post one verdict per root of `roots`, in the order the batch read them.
///
/// A trace that ran to its end gives each root the colour its rows carry;
/// an abandoned one posts [`Verdict::Unwalked`] for every root, the batch
/// having no reading to offer (`rfc/model/gc/rc-cycle.md`, "Speculative
/// tracing and exact validation").
///
/// # Safety
/// `roots` are the entries this batch copied out of R under the token, and
/// `verdicts` is the writer opened over the same mutator.
unsafe fn post_the_verdicts(verdicts: &VerdictWriter, roots: &[usize], complete: bool) {
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
}

/// Size the mutator's next batch from what this one of `size` roots did: a
/// trace that finished doubles it up to [`BATCH_BOUND`], and one that met
/// the workspace budget halves it. A trace abandoned for anything else
/// leaves the size where it stands, the refusal saying nothing about how
/// much of the heap the batch would have reached.
fn size_the_next_batch(mutator: &MutatorRecord, size: usize, complete: bool, met_budget: bool) {
    if complete {
        mutator.set_batch_size((size * 2).min(BATCH_BOUND));
    } else if met_budget {
        mutator.set_batch_size((size / 2).max(1));
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

mod birth;

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests;
