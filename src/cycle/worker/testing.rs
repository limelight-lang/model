//! The switches a case sets on the collector thread: whether
//! [`super::ensure_thread`] may birth one, which record its rounds visit,
//! the threshold its rounds serve at, how long it waits between rounds, whether its base
//! block is refused, and how it is ended; and the probes that count its
//! rounds and what they served.
//!
//! Every switch is process-wide, so a case that sets one holds the memory
//! tests' guard, and births are forbidden again before the case ends.
//! The rounds are confined because the harness runs other cases' threads
//! beside this one: a claim on a stranger's record is a foreign holder its
//! free path withholds returns under, which a case counting its own returns
//! reads as a wrong count.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use super::{ALIVE, COLLECTORS, ELDER, ENDING, MAX_COLLECTORS, STARTING, UNBORN};
use crate::cycle::mutator_record::MutatorRecord;

/// Whether [`super::ensure_thread`] may spawn.
static BIRTHS_PERMITTED: AtomicBool = AtomicBool::new(false);
/// The records a round serves and asks, null slots unused; all null is every
/// record.
static CONFINED: [AtomicPtr<MutatorRecord>; 4] =
    [const { AtomicPtr::new(std::ptr::null_mut()) }; 4];
/// Records the rounds have reached since a case last asked, confined or not.
static RECORDS_VISITED: AtomicUsize = AtomicUsize::new(0);
/// Whether the next birth's `ll_thread_init` runs under a zero block budget.
static REFUSE_NEXT_BASE_BLOCK: AtomicBool = AtomicBool::new(false);
/// Whether the thread was asked to end.
static RETIRING: AtomicBool = AtomicBool::new(false);

/// Where the thread stands, for a case that waits on its birth.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ThreadState {
    Unborn,
    Starting,
    Alive,
    Ending,
}

/// Where the elder stands.
pub(crate) fn thread_state() -> ThreadState {
    collector_state(ELDER)
}

/// Where the collector of slot `index` stands.
pub(crate) fn collector_state(index: usize) -> ThreadState {
    match COLLECTORS[index].state.load(Ordering::Acquire) {
        UNBORN => ThreadState::Unborn,
        STARTING => ThreadState::Starting,
        ALIVE => ThreadState::Alive,
        ENDING => ThreadState::Ending,
        other => unreachable!("the thread word holds {other}"),
    }
}

/// Let [`super::ensure_thread`] birth the thread, or forbid it again.
pub(crate) fn permit_births(permitted: bool) {
    BIRTHS_PERMITTED.store(permitted, Ordering::Relaxed);
}

pub(crate) fn births_permitted() -> bool {
    BIRTHS_PERMITTED.load(Ordering::Relaxed)
}

/// Confine the rounds to `record`; null lifts the confinement.
pub(crate) fn confine_rounds_to(record: *mut MutatorRecord) {
    confine_rounds_to_records(&[record]);
}

/// Confine the rounds to `records`, up to four; an empty list lifts the
/// confinement.
pub(crate) fn confine_rounds_to_records(records: &[*mut MutatorRecord]) {
    assert!(records.len() <= CONFINED.len());
    for (slot, cell) in CONFINED.iter().enumerate() {
        cell.store(
            records.get(slot).copied().unwrap_or(std::ptr::null_mut()),
            Ordering::Relaxed,
        );
    }
}

/// Whether the next visit of a round panics, for the case that reads what a
/// panicking round leaves behind.
static PANIC_AT_NEXT_VISIT: AtomicBool = AtomicBool::new(false);

pub(crate) fn panic_at_the_next_visit() {
    PANIC_AT_NEXT_VISIT.store(true, Ordering::Relaxed);
}

/// Whether a round serves and asks `record`, counting the visit either way.
pub(crate) fn in_round(record: *mut MutatorRecord) -> bool {
    RECORDS_VISITED.fetch_add(1, Ordering::Relaxed);
    if PANIC_AT_NEXT_VISIT.swap(false, Ordering::Relaxed) {
        panic!("a round panicked at a visit, by the case's request");
    }

    let confined: Vec<*mut MutatorRecord> = CONFINED
        .iter()
        .map(|cell| cell.load(Ordering::Relaxed))
        .filter(|record| !record.is_null())
        .collect();
    confined.is_empty() || confined.contains(&record)
}

/// Records the rounds reached since the last call, and zero the count.
pub(crate) fn take_records_visited() -> usize {
    RECORDS_VISITED.swap(0, Ordering::Relaxed)
}

/// The threshold the thread's rounds serve at, or zero for the module's
/// own: a case whose ring holds a few entries serves them at one.
static ROUNDS_THRESHOLD: AtomicUsize = AtomicUsize::new(0);

/// Serve the thread's rounds at `entries`, or at the module's own for zero.
pub(crate) fn serve_rounds_at(entries: usize) {
    ROUNDS_THRESHOLD.store(entries, Ordering::Relaxed);
}

pub(crate) fn threshold_for_rounds() -> Option<usize> {
    match ROUNDS_THRESHOLD.load(Ordering::Relaxed) {
        0 => None,
        entries => Some(entries),
    }
}

/// The epoch interval the thread's rounds advance a mutator's epoch at, in
/// nanoseconds, or zero for the module's own: a case that reads the advance
/// sets it below its own wait.
static EPOCH_NANOS: AtomicU64 = AtomicU64::new(0);

/// Advance a mutator's epoch after `interval` of the collector's clock, or
/// after the module's own for `None`.
pub(crate) fn advance_epochs_after(interval: Option<std::time::Duration>) {
    EPOCH_NANOS.store(
        interval.map_or(0, |interval| interval.as_nanos() as u64),
        Ordering::Relaxed,
    );
}

pub(crate) fn epoch_interval() -> Option<std::time::Duration> {
    match EPOCH_NANOS.load(Ordering::Relaxed) {
        0 => None,
        nanos => Some(std::time::Duration::from_nanos(nanos)),
    }
}

/// The interval the thread's rounds take a standing sub-threshold ring
/// after, in nanoseconds, or zero for the module's own: a case that reads
/// the take sets it below its own wait.
static STANDING_NANOS: AtomicU64 = AtomicU64::new(0);

/// Take a standing sub-threshold ring after `interval`, or after the
/// module's own for `None`. A zero interval is stored as one nanosecond,
/// so that a case asking for the take at the next visit reads as an
/// override and not as the module's own figure.
pub(crate) fn take_standing_after(interval: Option<std::time::Duration>) {
    STANDING_NANOS.store(
        interval.map_or(0, |interval| (interval.as_nanos() as u64).max(1)),
        Ordering::Relaxed,
    );
}

pub(crate) fn standing_interval() -> Option<std::time::Duration> {
    match STANDING_NANOS.load(Ordering::Relaxed) {
        0 => None,
        nanos => Some(std::time::Duration::from_nanos(nanos)),
    }
}

/// The wait after every round, in milliseconds, or zero for the timer's
/// own: a case that reads what a wake does sets it above its own wait.
static WAIT_MILLIS: AtomicUsize = AtomicUsize::new(0);

/// Wait `interval` after every round, or the timer's own for `None`.
pub(crate) fn wait_between_rounds_for(interval: Option<std::time::Duration>) {
    WAIT_MILLIS.store(
        interval.map_or(0, |interval| interval.as_millis() as usize),
        Ordering::Relaxed,
    );
}

pub(crate) fn interval_for_this_wait() -> Option<std::time::Duration> {
    match WAIT_MILLIS.load(Ordering::Relaxed) {
        0 => None,
        millis => Some(std::time::Duration::from_millis(millis as u64)),
    }
}

/// Rounds each collector made since a case last asked, and the interval the
/// elder's timer holds after the last of its rounds, in milliseconds.
static ROUNDS: [AtomicUsize; MAX_COLLECTORS] = [const { AtomicUsize::new(0) }; MAX_COLLECTORS];
static TIMER_MILLIS: AtomicUsize = AtomicUsize::new(0);
/// When each round of the elder began and ended, since a case last asked:
/// the stress probe reads the gap between two rounds against the timer.
static ROUND_TIMES: Mutex<Vec<(Instant, Option<Instant>)>> = Mutex::new(Vec::new());

fn round_times() -> std::sync::MutexGuard<'static, Vec<(Instant, Option<Instant>)>> {
    ROUND_TIMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn note_round_start(index: usize) {
    if index == ELDER {
        round_times().push((Instant::now(), None));
    }
}

pub(crate) fn note_round(index: usize, interval: std::time::Duration) {
    if index == ELDER {
        TIMER_MILLIS.store(interval.as_millis() as usize, Ordering::Relaxed);
        if let Some(open) = round_times().last_mut() {
            open.1 = Some(Instant::now());
        }
    }

    ROUNDS[index].fetch_add(1, Ordering::Release);
}

/// The elder's rounds since the last call as (start, end) pairs, the last
/// end `None` for a round still running, and forget them.
pub(crate) fn take_round_times() -> Vec<(Instant, Option<Instant>)> {
    std::mem::take(&mut *round_times())
}

/// Rounds every collector made since the last call, and zero the counts.
pub(crate) fn take_rounds() -> usize {
    ROUNDS
        .iter()
        .map(|rounds| rounds.swap(0, Ordering::Acquire))
        .sum()
}

/// Rounds the collector of slot `index` made since the last call, and zero
/// its count.
pub(crate) fn take_rounds_of(index: usize) -> usize {
    ROUNDS[index].swap(0, Ordering::Acquire)
}

/// The fallback interval as the elder's timer holds it after its last round.
pub(crate) fn timer_interval() -> std::time::Duration {
    std::time::Duration::from_millis(TIMER_MILLIS.load(Ordering::Relaxed) as u64)
}

/// What the serves answered since a case last asked, one count per
/// [`super::Served`] variant, and the two counts a variant does not carry:
/// grants served, whether the batch under one took anything, and requests
/// the mutator refused by a take of its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Outcomes {
    pub(crate) token_held: usize,
    pub(crate) posted: usize,
    pub(crate) unanswered: usize,
    pub(crate) idle: usize,
    pub(crate) asked: usize,
    pub(crate) batches: usize,
    pub(crate) grants: usize,
    pub(crate) refusals: usize,
}

static TOKEN_HELD: AtomicUsize = AtomicUsize::new(0);
static POSTED: AtomicUsize = AtomicUsize::new(0);
static UNANSWERED: AtomicUsize = AtomicUsize::new(0);
static IDLE: AtomicUsize = AtomicUsize::new(0);
static ASKED: AtomicUsize = AtomicUsize::new(0);
static MUTATORS_SERVED: AtomicUsize = AtomicUsize::new(0);
static GRANTS: AtomicUsize = AtomicUsize::new(0);
static REFUSALS: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn note_served(served: super::Served) {
    let count = match served {
        super::Served::TokenHeld => &TOKEN_HELD,
        super::Served::Posted => &POSTED,
        super::Served::Unanswered => &UNANSWERED,
        super::Served::Idle => &IDLE,
        super::Served::Asked => &ASKED,
        super::Served::Batch { .. } => &MUTATORS_SERVED,
    };
    count.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn note_grant() {
    GRANTS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn note_refusal() {
    REFUSALS.fetch_add(1, Ordering::Relaxed);
}

/// What one batch's trace did, for the probe that reads a take's cost by
/// the shape of its roots (`dev/BENCHMARKS.md`, "S64.5 what a take costs by
/// the shape of its roots"): the roots the trace walked, the parts it opened,
/// whether it ran to its end, the blocks its arena drew above the workspace,
/// and the wall of the two phases; where a case's hook ran between the
/// phases ([`between_the_next_phases`]), the positions of storage the scan
/// read after it; and the visits of the lookup of met roots.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct TracedBatch {
    pub(crate) roots: usize,
    pub(crate) parts: usize,
    pub(crate) complete: bool,
    pub(crate) blocks: usize,
    pub(crate) wall: std::time::Duration,
    pub(crate) positions_after_the_hook: Option<usize>,
    /// Roots and rows the lookup of each part's met roots visited.
    pub(crate) lookup_visits: usize,
    /// Edges the batch's marks pruned at a mature target
    /// (`crate::cycle::mark::take_edges_pruned`).
    pub(crate) edges_pruned: usize,
    /// Rows the batch's parts met, summed over the parts.
    pub(crate) rows_met: usize,
    /// Parts that met B, the retry of one under `B_max` not counted.
    pub(crate) parts_met_budget: usize,
    /// Whether a part was retried under `B_max`.
    pub(crate) retried: bool,
    /// Parts whose met roots were deferred read live, past B with the retry
    /// spent or past `B_max`.
    pub(crate) deferred_parts: usize,
}

/// Whether a case is reading the batches: off by the module's own, so that
/// a suite's batches pay one relaxed load and allocate nothing.
static READING_BATCHES: AtomicBool = AtomicBool::new(false);

/// The batches traced since the last take, in the order the collectors
/// traced them.
static TRACED_BATCHES: Mutex<Vec<TracedBatch>> = Mutex::new(Vec::new());

/// Read every batch's trace from here on, or stop reading; either way what
/// stood is cleared, the reading being one case's.
pub(crate) fn read_traced_batches(reading: bool) {
    READING_BATCHES.store(reading, Ordering::Relaxed);
    batches().clear();
}

/// Record what a batch's trace did, and do nothing at all while no case is
/// reading: `reading` is called under the flag alone, since the blocks it
/// asks the arena for are a walk of the arena's chain.
pub(crate) fn note_traced_batch(reading: impl FnOnce() -> TracedBatch) {
    if !READING_BATCHES.load(Ordering::Relaxed) {
        return;
    }

    let batch = reading();
    batches().push(batch);
}

/// Every batch traced since the last call, in the collectors' own order.
pub(crate) fn take_traced_batches() -> Vec<TracedBatch> {
    std::mem::take(&mut batches())
}

fn batches() -> std::sync::MutexGuard<'static, Vec<TracedBatch>> {
    TRACED_BATCHES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The grants served idle and the batches traced so far, read without taking
/// them: what a case's hook waits on while its test loop takes the counts.
pub(crate) fn idle_and_traced_so_far() -> (usize, usize) {
    (IDLE.load(Ordering::Relaxed), batches().len())
}

/// On the mutator's thread, between its consent's swap and the reading of its
/// withheld stacks' marks, for the case that holds the mutator there until
/// the collector has made its choice over the grant.
static AFTER_THE_CONSENT: OneShot = OneShot::new();

pub(crate) fn after_the_next_consents_swap(act: Box<dyn FnOnce() + Send>) {
    AFTER_THE_CONSENT.install(act);
}

pub(crate) fn after_the_consents_swap() {
    AFTER_THE_CONSENT.run();
}

/// Every outcome since the last call, and zero the counts.
pub(crate) fn take_outcomes() -> Outcomes {
    Outcomes {
        token_held: TOKEN_HELD.swap(0, Ordering::Relaxed),
        posted: POSTED.swap(0, Ordering::Relaxed),
        unanswered: UNANSWERED.swap(0, Ordering::Relaxed),
        idle: IDLE.swap(0, Ordering::Relaxed),
        asked: ASKED.swap(0, Ordering::Relaxed),
        batches: MUTATORS_SERVED.swap(0, Ordering::Relaxed),
        grants: GRANTS.swap(0, Ordering::Relaxed),
        refusals: REFUSALS.swap(0, Ordering::Relaxed),
    }
}

/// The block budget the next batch traces under, for the case that reads
/// what a batch that meets it posts; `usize::MAX` for the module's own.
static NEXT_BATCH_BUDGET: AtomicUsize = AtomicUsize::new(usize::MAX);

/// The budget a part that met B is retried under, `usize::MAX` for the
/// module's own `B_max`; process-wide, the cases holding the pool's guard.
static RETRY_BUDGET: AtomicUsize = AtomicUsize::new(usize::MAX);

/// Retry parts under `blocks` rather than `B_max` until the guard drops.
pub(crate) fn retry_parts_under(blocks: usize) -> RetryBudget {
    RETRY_BUDGET.store(blocks, Ordering::Relaxed);
    RetryBudget
}

pub(crate) fn retry_budget() -> Option<usize> {
    match RETRY_BUDGET.load(Ordering::Relaxed) {
        usize::MAX => None,
        blocks => Some(blocks),
    }
}

/// The budget [`retry_parts_under`] set, lifted at the drop.
pub(crate) struct RetryBudget;

impl Drop for RetryBudget {
    fn drop(&mut self) {
        RETRY_BUDGET.store(usize::MAX, Ordering::Relaxed);
    }
}

pub(crate) fn budget_the_next_batch(blocks: usize) {
    NEXT_BATCH_BUDGET.store(blocks, Ordering::Relaxed);
}

pub(crate) fn budget_for_this_batch() -> Option<usize> {
    match NEXT_BATCH_BUDGET.swap(usize::MAX, Ordering::Relaxed) {
        usize::MAX => None,
        blocks => Some(blocks),
    }
}

/// Whether the next batch panics between its last post and its advance, for
/// the case that reads what the advance's guard does from the unwind.
static PANIC_BEFORE_THE_ADVANCE: AtomicBool = AtomicBool::new(false);

pub(crate) fn panic_before_the_next_advance() {
    PANIC_BEFORE_THE_ADVANCE.store(true, Ordering::Relaxed);
}

/// A channel the next batch waits on between its last post and its
/// advance, for the case that registers while a batch stands peeked in R
/// and posted in P; the case sends to let it go.
static WAIT_BEFORE_THE_ADVANCE: Mutex<Option<std::sync::mpsc::Receiver<()>>> = Mutex::new(None);

pub(crate) fn make_the_next_batch_wait_before_its_advance(until: std::sync::mpsc::Receiver<()>) {
    *WAIT_BEFORE_THE_ADVANCE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(until);
}

pub(crate) fn between_the_post_and_the_advance() {
    if PANIC_BEFORE_THE_ADVANCE.swap(false, Ordering::Relaxed) {
        panic!("a batch panicked between its post and its advance, by the case's request");
    }

    let waiting = WAIT_BEFORE_THE_ADVANCE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(until) = waiting {
        let _ = until.recv();
    }
}

/// A closure a case installs at one point of the serve, run there once on
/// the collector's thread, so that the case acts inside a window the serve
/// otherwise closes in nanoseconds.
struct OneShot(Mutex<Option<Box<dyn FnOnce() + Send>>>);

impl OneShot {
    const fn new() -> Self {
        Self(Mutex::new(None))
    }

    fn install(&self, act: Box<dyn FnOnce() + Send>) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(act);
    }

    /// Run the closure installed, if one is, and say whether one ran.
    fn run(&self) -> bool {
        let act = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let installed = act.is_some();
        if let Some(act) = act {
            act();
        }

        installed
    }
}

/// At the start of the next batch's trace, on the collector's thread, for
/// the probe whose mutator asks for its token while the trace runs.
static AT_THE_NEXT_TRACE: OneShot = OneShot::new();

pub(crate) fn at_the_start_of_the_next_trace(act: Box<dyn FnOnce() + Send>) {
    AT_THE_NEXT_TRACE.install(act);
}

pub(crate) fn at_the_start_of_the_trace() {
    AT_THE_NEXT_TRACE.run();
}

/// At the start of the next retry under `B_max`, on the collector's thread,
/// for the case whose mutator recalls its token while the retry runs.
static AT_THE_NEXT_RETRY: OneShot = OneShot::new();

pub(crate) fn at_the_start_of_the_next_retry(act: Box<dyn FnOnce() + Send>) {
    AT_THE_NEXT_RETRY.install(act);
}

pub(crate) fn at_the_start_of_the_retry() {
    AT_THE_NEXT_RETRY.run();
}

/// Before the trace of one part of the next batches, numbered from one, on
/// the collector's thread: for the case that recalls inside a part after a
/// deferral.
static BEFORE_A_PART: Mutex<Option<(usize, Box<dyn FnOnce() + Send>)>> = Mutex::new(None);

pub(crate) fn before_the_trace_of(part: usize, act: Box<dyn FnOnce() + Send>) {
    *BEFORE_A_PART
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((part, act));
}

/// Run the hook installed before `part`, once.
pub(crate) fn before_the_trace_of_part(part: usize) {
    let mut installed = BEFORE_A_PART
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if installed.as_ref().is_some_and(|&(at, _)| at == part) {
        let (_, act) = installed.take().expect("read above");
        drop(installed);
        act();
    }
}

/// After the trace of one part of the next batches, numbered from one, on the
/// collector's thread, the part's rows standing and none of its verdicts
/// posted: for the cases that unwind or recall inside a later part.
static AFTER_A_PART: Mutex<Option<(usize, Box<dyn FnOnce() + Send>)>> = Mutex::new(None);

pub(crate) fn after_the_trace_of(part: usize, act: Box<dyn FnOnce() + Send>) {
    *AFTER_A_PART
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((part, act));
}

/// Run the hook installed for `part`, once.
pub(crate) fn after_the_trace_of_part(part: usize) {
    let mut installed = AFTER_A_PART
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if installed.as_ref().is_some_and(|&(at, _)| at == part) {
        let (_, act) = installed.take().expect("read above");
        drop(installed);
        act();
    }
}

thread_local! {
    /// Roots and rows the lookup of met roots visited on this thread since the
    /// last take: the instrument of its bound.
    static LOOKUP_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(crate) fn note_a_lookup_visit() {
    LOOKUP_VISITS.with(|visits| visits.set(visits.get() + 1));
}

pub(crate) fn take_lookup_visits() -> usize {
    LOOKUP_VISITS.with(|visits| visits.replace(0))
}

thread_local! {
    /// Rows the parts on this thread met since the last take.
    static ROWS_MET: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Count the rows a completed part met, off its touched list before the
/// reset: every row of a block's array the trace did not leave untouched,
/// and a large entity's one row where its colour says it was met.
///
/// # Safety
/// The part's rows still stand.
pub(crate) unsafe fn note_rows_met(arena: &crate::cycle::arena::TraceScratchArena) {
    use crate::cycle::row::Population;
    use crate::cycle::shadow::{self, Color};

    let mut met = 0;
    let mut array = arena.touched_head();
    while !array.is_null() {
        let (block, population) = unsafe { ((*array).block, (*array).population) };
        if population == Population::SingleEntity {
            let row = unsafe { *crate::memory::large_entity::shadow_row(block) };
            met += usize::from(shadow::color(row) != Color::Untouched);
        } else {
            let _ = unsafe {
                shadow::for_each_met_row(array, |_| {
                    met += 1;
                    std::ops::ControlFlow::Continue(())
                })
            };
        }

        array = unsafe { (*array).next };
    }
    ROWS_MET.with(|rows| rows.set(rows.get() + met));
}

pub(crate) fn take_rows_met() -> usize {
    ROWS_MET.with(|rows| rows.replace(0))
}

/// Between the next batch's mark and its scan, on the collector's thread,
/// for the case whose mutator asks for its token after the mark: what the
/// scan reads from there on is what the mutator waits through.
static BETWEEN_THE_NEXT_PHASES: OneShot = OneShot::new();

pub(crate) fn between_the_next_phases(act: Box<dyn FnOnce() + Send>) {
    BETWEEN_THE_NEXT_PHASES.install(act);
}

/// Run the hook between the phases, and say whether a case installed one.
pub(crate) fn between_the_phases() -> bool {
    BETWEEN_THE_NEXT_PHASES.run()
}

/// The positions the scan read after the hook between the phases, left by
/// the hooked batch's trace for its own record, and `usize::MAX` for none:
/// only a batch that ran the hook writes it, and the hook is one case's.
static POSITIONS_AFTER_THE_HOOK: AtomicUsize = AtomicUsize::new(usize::MAX);

pub(crate) fn note_positions_after_the_hook(positions: usize) {
    POSITIONS_AFTER_THE_HOOK.store(positions, Ordering::Relaxed);
}

pub(crate) fn take_positions_after_the_hook() -> Option<usize> {
    match POSITIONS_AFTER_THE_HOOK.swap(usize::MAX, Ordering::Relaxed) {
        usize::MAX => None,
        positions => Some(positions),
    }
}

/// The instant just before the first release of a grant since a case began
/// reading the batches: the end of what a mutator standing in the token's wait
/// waits for, the wake aside.
static RELEASED_AT: Mutex<Option<Instant>> = Mutex::new(None);

pub(crate) fn note_release() {
    if !READING_BATCHES.load(Ordering::Relaxed) {
        return;
    }

    // The first release a case reads, which is its batch's: a round after
    // it may serve the ring again before the case stops reading.
    RELEASED_AT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get_or_insert_with(Instant::now);
}

pub(crate) fn take_released_at() -> Option<Instant> {
    RELEASED_AT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
}

/// Between the pre-claim reading's take of the mutator's blocks and its
/// loads of them, for the case whose mutator exits in that window.
static AT_THE_NEXT_READING: OneShot = OneShot::new();

pub(crate) fn at_the_next_reading(act: Box<dyn FnOnce() + Send>) {
    AT_THE_NEXT_READING.install(act);
}

pub(crate) fn between_the_take_and_the_reading() {
    AT_THE_NEXT_READING.run();
}

/// Between the reading's loads and the request, for the case whose mutator
/// takes its token in that window.
static BEFORE_THE_NEXT_REQUEST: OneShot = OneShot::new();

pub(crate) fn before_the_next_request(act: Box<dyn FnOnce() + Send>) {
    BEFORE_THE_NEXT_REQUEST.install(act);
}

pub(crate) fn between_the_reading_and_the_request() {
    BEFORE_THE_NEXT_REQUEST.run();
}

/// At the next refused request, before the serve acts on the refusal, for
/// the case that reads the record's hold at that instant.
static AT_THE_NEXT_REFUSAL: OneShot = OneShot::new();

pub(crate) fn at_the_next_refusal(act: Box<dyn FnOnce() + Send>) {
    AT_THE_NEXT_REFUSAL.install(act);
}

pub(crate) fn at_a_refused_request() {
    REFUSED_REQUESTS.fetch_add(1, Ordering::Relaxed);
    AT_THE_NEXT_REFUSAL.run();
}

/// Requests the token refused since the last call, and zero the count: the
/// figure a case reads to tell a serve that made its swap from one that
/// answered without it.
static REFUSED_REQUESTS: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn take_refused_requests() -> usize {
    REFUSED_REQUESTS.swap(0, Ordering::Relaxed)
}

/// The bound on a walk's expired consent waits for the probes, or zero for
/// the module's own: the null arm of the cap's measurement sets it to
/// `usize::MAX`, which is the round before the cap.
static EXPIRED_WAITS_CAP: AtomicUsize = AtomicUsize::new(0);

/// Bound a walk's expired waits at `waits`, or at the module's own for
/// `None`.
pub(crate) fn cap_expired_waits_at(waits: Option<usize>) {
    EXPIRED_WAITS_CAP.store(waits.unwrap_or(0), Ordering::Relaxed);
}

pub(crate) fn expired_waits_cap() -> Option<usize> {
    match EXPIRED_WAITS_CAP.load(Ordering::Relaxed) {
        0 => None,
        waits => Some(waits),
    }
}

/// The serve clock a round reads once per record, for a case outside this
/// module that serves a record itself ([`super::serve`]'s `now`).
pub(crate) fn serve_clock_now() -> u64 {
    super::serve_clock_now()
}

/// Mutators the rounds claimed since the last call, and zero the count.
pub(crate) fn take_mutators_served() -> usize {
    MUTATORS_SERVED.swap(0, Ordering::Relaxed)
}

/// Refuse the base block of the next birth: its `ll_thread_init` runs under a
/// block budget of zero, so the base block draw is the refusal.
pub(crate) fn refuse_the_next_births_base_block() {
    REFUSE_NEXT_BASE_BLOCK.store(true, Ordering::Relaxed);
}

/// The budget a birth's init runs under, taken on the thread being born.
pub(crate) fn base_block_budget_for_this_birth() -> Option<crate::memory::block_pool::BlockBudget> {
    REFUSE_NEXT_BASE_BLOCK
        .swap(false, Ordering::Relaxed)
        .then(|| crate::memory::block_pool::budget_blocks(0))
}

/// The CPU each slot's collector pins itself to at its birth, `usize::MAX`
/// for none: the rig's placement of the collectors
/// (`worker::tests::the_rig`). Atomics rather than a list, because the birth
/// that reads them asks the global allocator for nothing.
static COLLECTOR_CPUS: [AtomicUsize; super::MAX_COLLECTORS] =
    [const { AtomicUsize::new(usize::MAX) }; super::MAX_COLLECTORS];
/// Births that pinned their thread, and births whose pin the kernel refused,
/// since a case last asked.
static COLLECTORS_PINNED: AtomicUsize = AtomicUsize::new(0);
static COLLECTOR_PINS_REFUSED: AtomicUsize = AtomicUsize::new(0);

/// Pin the collector of slot `index` to `cpus[index % cpus.len()]` at its
/// next birth; an empty list leaves every birth where the scheduler puts it.
/// A thread standing already stays where it is.
pub(crate) fn pin_collectors_to(cpus: &[usize]) {
    for (index, slot) in COLLECTOR_CPUS.iter().enumerate() {
        let cpu = match cpus {
            [] => usize::MAX,
            cpus => cpus[index % cpus.len()],
        };
        slot.store(cpu, Ordering::Relaxed);
    }
}

/// Pin the calling collector thread of slot `index` where
/// [`pin_collectors_to`] said, counting the outcome for
/// [`take_collectors_pinned`]; a refusal leaves the thread unpinned rather
/// than failing its birth.
pub(crate) fn pin_this_collector(index: usize) {
    let cpu = COLLECTOR_CPUS[index].load(Ordering::Relaxed);
    if cpu == usize::MAX {
        return;
    }

    match pin_this_thread_to(cpu) {
        Ok(()) => COLLECTORS_PINNED.fetch_add(1, Ordering::Relaxed),
        Err(_) => COLLECTOR_PINS_REFUSED.fetch_add(1, Ordering::Relaxed),
    };
}

/// Collector births pinned, and births whose pin was refused, since the last
/// call; both counts go back to zero.
pub(crate) fn take_collectors_pinned() -> (usize, usize) {
    (
        COLLECTORS_PINNED.swap(0, Ordering::Relaxed),
        COLLECTOR_PINS_REFUSED.swap(0, Ordering::Relaxed),
    )
}

/// Pin the calling thread to logical CPU `cpu`, numbered as
/// `/sys/devices/system/cpu` numbers them; the error is the kernel's. Linux
/// only, and never under Miri, which models no affinity: elsewhere every
/// call is refused as unsupported.
pub(crate) fn pin_this_thread_to(cpu: usize) -> std::io::Result<()> {
    #[cfg(all(target_os = "linux", not(miri)))]
    {
        /// `cpu_set_t` as glibc and musl declare it: 1,024 bits.
        type CpuSet = [u64; 16];

        unsafe extern "C" {
            fn sched_setaffinity(pid: i32, set_size: usize, set: *const CpuSet) -> i32;
        }

        let mut set: CpuSet = [0; 16];
        let word = set
            .get_mut(cpu / 64)
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        *word = 1 << (cpu % 64);
        // Pid zero is the calling thread, not the whole process.
        match unsafe { sched_setaffinity(0, size_of::<CpuSet>(), &set) } {
            0 => Ok(()),
            _ => Err(std::io::Error::last_os_error()),
        }
    }

    #[cfg(not(all(target_os = "linux", not(miri))))]
    {
        let _ = cpu;
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }
}

/// What the collector lives that ended since a case last asked cost, summed:
/// the rig's reading of the collectors (`worker::tests::the_rig`).
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct CollectorLives {
    pub(crate) lives: usize,
    /// CPU time of the lives' threads, from their creation to their end.
    pub(crate) cpu: std::time::Duration,
    /// Wall time from each life's birth to its end.
    pub(crate) wall: std::time::Duration,
    pub(crate) voluntary_switches: u64,
    pub(crate) involuntary_switches: u64,
}

static COLLECTOR_LIVES: Mutex<CollectorLives> = Mutex::new(CollectorLives {
    lives: 0,
    cpu: std::time::Duration::ZERO,
    wall: std::time::Duration::ZERO,
    voluntary_switches: 0,
    involuntary_switches: 0,
});
/// When each slot's standing life was born, `None` for no life.
static COLLECTOR_BORN_AT: Mutex<[Option<Instant>; super::MAX_COLLECTORS]> =
    Mutex::new([None; super::MAX_COLLECTORS]);

/// Stamp the birth of slot `index`'s life, on the thread being born.
pub(crate) fn note_collector_born(index: usize) {
    lock(&COLLECTOR_BORN_AT)[index] = Some(Instant::now());
}

/// Add slot `index`'s life to [`take_collector_lives`], on its own thread at
/// the life's end; a life whose birth was refused before its stamp adds
/// nothing.
pub(crate) fn note_collector_life_end(index: usize) {
    let Some(born) = lock(&COLLECTOR_BORN_AT)[index].take() else {
        return;
    };

    let wall = born.elapsed();
    let cpu = thread_cpu_time();
    let (voluntary, involuntary) = thread_context_switches();
    let mut lives = lock(&COLLECTOR_LIVES);
    lives.lives += 1;
    lives.cpu += cpu;
    lives.wall += wall;
    lives.voluntary_switches += voluntary;
    lives.involuntary_switches += involuntary;
}

/// The collector lives that ended since the last call, and zero them.
pub(crate) fn take_collector_lives() -> CollectorLives {
    std::mem::take(&mut *lock(&COLLECTOR_LIVES))
}

/// The mutators' takes that waited out a collector's claim since a case last
/// asked: how many, how long in all, and the longest, each timed from the
/// take's first reading of the claim to the take.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct TokenWaits {
    pub(crate) waits: usize,
    pub(crate) total: std::time::Duration,
    pub(crate) longest: std::time::Duration,
}

static TOKEN_WAITS: Mutex<TokenWaits> = Mutex::new(TokenWaits {
    waits: 0,
    total: std::time::Duration::ZERO,
    longest: std::time::Duration::ZERO,
});

pub(crate) fn note_token_wait(waited: std::time::Duration) {
    let mut waits = lock(&TOKEN_WAITS);
    waits.waits += 1;
    waits.total += waited;
    waits.longest = waits.longest.max(waited);
}

/// The takes' waits since the last call, and zero them.
pub(crate) fn take_token_waits() -> TokenWaits {
    std::mem::take(&mut *lock(&TOKEN_WAITS))
}

/// Grants recalled by a stack of withheld returns at its mark M, and by the
/// mutator's take, since a case last asked; a grant is counted once, by
/// whichever recalled it first.
static RECALLS_BY_THE_MARK: AtomicUsize = AtomicUsize::new(0);
static RECALLS_BY_A_TAKE: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn note_recall(by_the_mark: bool) {
    match by_the_mark {
        true => &RECALLS_BY_THE_MARK,
        false => &RECALLS_BY_A_TAKE,
    }
    .fetch_add(1, Ordering::Relaxed);
}

/// Recalls by the mark and by a take since the last call, and zero both.
pub(crate) fn take_recalls() -> (usize, usize) {
    (
        RECALLS_BY_THE_MARK.swap(0, Ordering::Relaxed),
        RECALLS_BY_A_TAKE.swap(0, Ordering::Relaxed),
    )
}

/// Roots a collection over P wrote back into R untraced since a case last
/// asked: one round P → R → P each (`crate::cycle::queue::compaction`,
/// `dispose_verdicts`).
static WRITTEN_BACK: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn note_written_back() {
    WRITTEN_BACK.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn take_written_back() -> usize {
    WRITTEN_BACK.swap(0, Ordering::Relaxed)
}

/// Parts whose met roots a batch deferred read live since a case last asked:
/// past B with the grant's retry spent, or past `B_max`.
static PARTS_DEFERRED: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn note_part_deferred() {
    PARTS_DEFERRED.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn take_parts_deferred() -> usize {
    PARTS_DEFERRED.swap(0, Ordering::Relaxed)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The calling thread's CPU time since its creation
/// (`CLOCK_THREAD_CPUTIME_ID`); zero off Linux and under Miri.
pub(crate) fn thread_cpu_time() -> std::time::Duration {
    #[cfg(all(target_os = "linux", not(miri)))]
    {
        /// `CLOCK_THREAD_CPUTIME_ID` in Linux's numbering.
        const THREAD_CPU_CLOCK: i32 = 3;

        unsafe extern "C" {
            /// `struct timespec` on a 64-bit Linux: seconds, nanoseconds.
            fn clock_gettime(clock: i32, time: *mut [i64; 2]) -> i32;
        }

        let mut time = [0i64; 2];
        let read = unsafe { clock_gettime(THREAD_CPU_CLOCK, &mut time) };
        assert_eq!(read, 0, "the thread's CPU clock reads");
        std::time::Duration::new(time[0] as u64, time[1] as u32)
    }

    #[cfg(not(all(target_os = "linux", not(miri))))]
    std::time::Duration::ZERO
}

/// The calling thread's context switches since its creation, voluntary and
/// involuntary (`getrusage(RUSAGE_THREAD)`); zeros off Linux and under Miri.
pub(crate) fn thread_context_switches() -> (u64, u64) {
    #[cfg(all(target_os = "linux", not(miri)))]
    {
        /// `RUSAGE_THREAD` in Linux's numbering.
        const THIS_THREAD: i32 = 1;

        unsafe extern "C" {
            /// `struct rusage` on a 64-bit Linux: two `timeval`s, then
            /// fourteen longs, of which `ru_nvcsw` and `ru_nivcsw` are the
            /// last two.
            fn getrusage(who: i32, usage: *mut [i64; 18]) -> i32;
        }

        let mut usage = [0i64; 18];
        let read = unsafe { getrusage(THIS_THREAD, &mut usage) };
        assert_eq!(read, 0, "the thread's usage reads");
        (usage[16] as u64, usage[17] as u64)
    }

    #[cfg(not(all(target_os = "linux", not(miri))))]
    (0, 0)
}

/// Slot `index`'s byte-event sequence number as it stands.
pub(crate) fn byte_wakes_of(index: usize) -> usize {
    super::COLLECTORS[index].byte_wakes.load(Ordering::Acquire)
}

/// Passes the checkpoints made over a standing list — walks, not
/// checkpoints — since a case last asked.
static PASSES: AtomicUsize = AtomicUsize::new(0);
/// Grants the passes released without a batch since a case last asked.
static RELEASED_UNSERVED: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn note_pass() {
    PASSES.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn note_release_unserved() {
    RELEASED_UNSERVED.fetch_add(1, Ordering::Relaxed);
}

/// Passes since the last call, and zero the count.
pub(crate) fn take_passes() -> usize {
    PASSES.swap(0, Ordering::Relaxed)
}

/// Grants released without a batch since the last call, and zero the count.
pub(crate) fn take_releases_unserved() -> usize {
    RELEASED_UNSERVED.swap(0, Ordering::Relaxed)
}

/// Threads spawned since a case last asked.
static SPAWNS: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn note_spawn() {
    SPAWNS.fetch_add(1, Ordering::Relaxed);
}

/// Backlog rounds that reached the birth and got no sibling — no slot under
/// the cap, or a refused spawn — since a case last asked: what tells a
/// negative case that the cap was reached rather than the backlog never
/// read.
static BACKLOG_ROUNDS_WITHOUT_A_BIRTH: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn note_backlog_round_without_a_birth() {
    BACKLOG_ROUNDS_WITHOUT_A_BIRTH.fetch_add(1, Ordering::Relaxed);
}

/// Backlog rounds without a birth since the last call, and zero the count.
pub(crate) fn take_backlog_rounds_without_a_birth() -> usize {
    BACKLOG_ROUNDS_WITHOUT_A_BIRTH.swap(0, Ordering::Relaxed)
}

/// The exit sequences the collector thread had run at the instant it stored
/// its word unborn, as recorded by the thread itself just before the store:
/// zero is a word stored before the runtime exit.
static EXITS_BEFORE_THE_WORD: AtomicUsize = AtomicUsize::new(usize::MAX);

pub(crate) fn note_exits_before_the_word(exits: usize) {
    EXITS_BEFORE_THE_WORD.store(exits, Ordering::Release);
}

/// What the last collector thread to end recorded at its word, or
/// `usize::MAX` for none since the last call.
pub(crate) fn take_exits_before_the_word() -> usize {
    EXITS_BEFORE_THE_WORD.swap(usize::MAX, Ordering::AcqRel)
}

/// Threads spawned since the last call, and zero the count.
pub(crate) fn take_spawns() -> usize {
    SPAWNS.swap(0, Ordering::Relaxed)
}

pub(crate) fn retiring() -> bool {
    RETIRING.load(Ordering::Relaxed)
}

/// End every collector thread and wait for it: the flag, a wake out of its
/// wait, the join. A thread whose birth was refused is joined the same way.
/// Closes the births again, lifts the confinement and the collectors' pins,
/// restores the cap, the threshold and the wait, forgets the last refused
/// birth and zeroes the rounds; a one-shot hook a case armed and never
/// reached stays armed.
pub(crate) fn retire() {
    permit_births(false);
    RETIRING.store(true, Ordering::Relaxed);
    // Every slot's word, under its mutex, before the notify: a thread between
    // its `retiring()` check and its wait would otherwise sleep out a wait a
    // case pinned long.
    for index in 0..super::MAX_COLLECTORS {
        let _ = super::wake(index);
    }
    // A thread that panicked in a round is joined all the same: the panic
    // was caught on the thread, and the case that raised it reads its word.
    super::birth::join_every_slot();

    super::forget_refused_birth();

    RETIRING.store(false, Ordering::Relaxed);
    confine_rounds_to_records(&[]);
    pin_collectors_to(&[]);
    serve_rounds_at(0);
    wait_between_rounds_for(None);
    advance_epochs_after(None);
    take_standing_after(None);
    cap_expired_waits_at(None);
    super::set_collector_cap(super::DEFAULT_COLLECTOR_CAP);
    super::set_epoch_interval(std::time::Duration::ZERO);
    super::set_standing_interval(std::time::Duration::ZERO);
    let _ = take_rounds();
    let _ = take_round_times();
    let _ = take_outcomes();
    let _ = take_backlog_rounds_without_a_birth();
    let _ = take_passes();
    let _ = take_releases_unserved();
    let _ = take_refused_requests();
    read_traced_batches(false);
}

/// Take the elder's slot for the calling thread, so that a consent's wake
/// of slot [`ELDER`] reaches a stand-in collector: a stand-in that waits on
/// the slot's wake word until the grant ([`wait_for_the_elders_wake`]),
/// rather than spinning on the byte, makes progress under Miri's weak-memory
/// emulation, where a spinning reader can read the old value for a very
/// long time. The word is cleared here as the thread's own birth clears it.
/// The caller has no collector thread born, and gives the slot back by
/// returning: what a late wake left is the next birth's to clear.
pub(crate) fn stand_in_as_the_elder() {
    assert_eq!(
        thread_state(),
        ThreadState::Unborn,
        "the elder's slot is free"
    );
    super::forget_wakes(ELDER);
}

/// Sleep on the elder slot's wake word until a wake or `timeout`, taking
/// the word, as the thread's own waits do.
pub(crate) fn wait_for_the_elders_wake(timeout: std::time::Duration) {
    super::wait_for_a_wake(ELDER, timeout);
}

/// One [`super::serve`] of `record` on the calling thread with standing
/// requests of its own, for a case whose collector thread serves once.
///
/// # Safety
/// As [`super::serve`].
pub(crate) unsafe fn serve_alone(record: *mut MutatorRecord) -> super::Served {
    let mut standing = super::Standing::new(super::ELDER);
    unsafe {
        super::serve(
            record,
            super::ELDER,
            1,
            &mut standing,
            super::serve_clock_now(),
        )
    }
}

/// Wait for `collector` as the mutator does: reading its byte — consenting
/// to a request, arming on `POSTED` — between yields, the way its poll and
/// its slot frees would, until the thread finishes; then join it.
pub(crate) fn consent_while<T>(collector: JoinHandle<T>) -> T {
    while !collector.is_finished() {
        crate::cycle::token::read_and_act_on_this_thread();
        std::thread::yield_now();
    }

    collector.join().expect("the collector finished")
}

/// The request wait the serves use under the harness: the crate's own
/// bound is a placeholder sized for a running mutator, and a harness thread
/// consenting between yields on a loaded box misses it, which would read
/// as a sleeping mutator in a case about something else. A case about the
/// bound itself holds its own ([`HeldRequestWait`]).
pub(crate) const HARNESS_REQUEST_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

static REQUEST_WAIT_MILLIS: AtomicUsize =
    AtomicUsize::new(HARNESS_REQUEST_WAIT.as_millis() as usize);

fn request_wait_for_tests(wait: std::time::Duration) {
    REQUEST_WAIT_MILLIS.store(wait.as_millis() as usize, Ordering::Relaxed);
}

/// A request wait held for one case — the crate's own, or one the case is
/// about — and the harness's put back when the guard drops, on the unwind
/// too, so that a failed case leaves no short wait for the cases after it
/// to read as a sleeping mutator.
pub(crate) struct HeldRequestWait;

impl HeldRequestWait {
    pub(crate) fn crate_own() -> Self {
        Self::of(super::REQUEST_WAIT)
    }

    pub(crate) fn of(wait: std::time::Duration) -> Self {
        request_wait_for_tests(wait);
        Self
    }
}

impl Drop for HeldRequestWait {
    fn drop(&mut self) {
        request_wait_for_tests(HARNESS_REQUEST_WAIT);
    }
}

pub(crate) fn request_wait() -> std::time::Duration {
    std::time::Duration::from_millis(REQUEST_WAIT_MILLIS.load(Ordering::Relaxed) as u64)
}
