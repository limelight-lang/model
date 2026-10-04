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
    CPU_AT_THE_LAST_ROUND[index].store(thread_cpu_time().as_nanos() as u64, Ordering::Relaxed);
}

/// Each slot's standing life's CPU time at the end of its last round, in
/// nanoseconds, zero for no life: what [`collector_cpu_to_now`] adds to the
/// lives that ended.
static CPU_AT_THE_LAST_ROUND: [AtomicU64; super::MAX_COLLECTORS] =
    [const { AtomicU64::new(0) }; super::MAX_COLLECTORS];

/// The collectors' CPU time so far: every life that ended since the last
/// [`take_collector_lives`], and every standing life's to the end of its last
/// round. The rig reads it at the instant it stops its mutators, which the
/// lives' own figures run past.
pub(crate) fn collector_cpu_to_now() -> std::time::Duration {
    let standing: u64 = CPU_AT_THE_LAST_ROUND
        .iter()
        .map(|cpu| cpu.load(Ordering::Relaxed))
        .sum();
    lock(&COLLECTOR_LIVES).cpu + std::time::Duration::from_nanos(standing)
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
    /// Roots the batches carried, over all of them.
    pub(crate) roots_served: usize,
    pub(crate) grants: usize,
    pub(crate) refusals: usize,
}

static TOKEN_HELD: AtomicUsize = AtomicUsize::new(0);
static POSTED: AtomicUsize = AtomicUsize::new(0);
static UNANSWERED: AtomicUsize = AtomicUsize::new(0);
static IDLE: AtomicUsize = AtomicUsize::new(0);
static ASKED: AtomicUsize = AtomicUsize::new(0);
static MUTATORS_SERVED: AtomicUsize = AtomicUsize::new(0);
static ROOTS_SERVED: AtomicUsize = AtomicUsize::new(0);
static GRANTS: AtomicUsize = AtomicUsize::new(0);
static REFUSALS: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn note_served(served: super::Served) {
    let count = match served {
        super::Served::TokenHeld => &TOKEN_HELD,
        super::Served::Posted => &POSTED,
        super::Served::Unanswered => &UNANSWERED,
        super::Served::Idle => &IDLE,
        super::Served::Asked => &ASKED,
        super::Served::Batch { roots, .. } => {
            ROOTS_SERVED.fetch_add(roots, Ordering::Relaxed);
            &MUTATORS_SERVED
        }
    };
    count.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn note_grant() {
    GRANTS.fetch_add(1, Ordering::Relaxed);
    #[cfg(feature = "debug-journal")]
    AT_THE_NEXT_GRANT.run();
}

/// At the next grant, on the collector's thread, before its reading of the
/// recall: for the journal's case whose recall must stand after the consent
/// that clears an earlier one.
#[cfg(feature = "debug-journal")]
static AT_THE_NEXT_GRANT: OneShot = OneShot::new();

#[cfg(feature = "debug-journal")]
pub(crate) fn at_the_next_grant(act: Box<dyn FnOnce() + Send>) {
    AT_THE_NEXT_GRANT.install(act);
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
/// read after it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct TracedBatch {
    pub(crate) roots: usize,
    /// Whether the batch's roots were met and its trace opened, which a
    /// recall in the pass over the roots forestalls.
    pub(crate) traced: bool,
    pub(crate) complete: bool,
    pub(crate) blocks: usize,
    pub(crate) wall: std::time::Duration,
    pub(crate) positions_after_the_hook: Option<usize>,
    /// Edges the batch's marks pruned at a mature target
    /// (`crate::cycle::mark::take_edges_pruned`).
    pub(crate) edges_pruned: usize,
    /// Rows the batch's trace met.
    pub(crate) rows_met: usize,
    /// The mutator's record, as an address, which tells one mutator's
    /// batches from another's.
    pub(crate) mutator: usize,
    /// When the trace ended.
    pub(crate) ended: Instant,
    /// The batch's trace, where it completed.
    pub(crate) widest_part: PartReading,
    /// The positions the batch's trace inspected.
    pub(crate) positions: usize,
    /// Which exit ended the trace, one of the journal's `BATCH_END_*` codes.
    pub(crate) ending: u64,
    /// The mutator's epoch clock at the batch's end
    /// (`crate::cycle::mutator_record::MutatorRecord::turnovers`).
    pub(crate) turnovers: u64,
}

/// What one completed part read (`dev/BENCHMARKS.md`, "readings the S65 and S67
/// stage notes held, carried at the stages' close", run R1): the rows it met,
/// the trace arena's blocks at its end, its positions in the mark and in the
/// scan, the heap blocks whose rows it touched, and its wall.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct PartReading {
    pub(crate) rows: usize,
    pub(crate) blocks: usize,
    pub(crate) mark_positions: usize,
    pub(crate) scan_positions: usize,
    pub(crate) touched: usize,
    pub(crate) wall: std::time::Duration,
}

thread_local! {
    /// The widest part this thread completed since the last take.
    static WIDEST_PART: std::cell::Cell<PartReading> = const {
        std::cell::Cell::new(PartReading {
            rows: 0,
            blocks: 0,
            mark_positions: 0,
            scan_positions: 0,
            touched: 0,
            wall: std::time::Duration::ZERO,
        })
    };
    /// The arena's positions where this thread's last mark completed.
    static MARK_ENDED_AT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Note where a completed mark left the arena's positions, which splits the
/// part's positions between its mark and its scan.
pub(crate) fn note_the_mark_end(positions: usize) {
    MARK_ENDED_AT.with(|at| at.set(positions));
}

/// Read the part that just completed on `arena`, `rows` of them met, from
/// `from`, the arena's positions and the instant before its mark, and keep
/// it where it met more rows than the widest so far.
///
/// # Safety
/// The part's rows still stand.
pub(crate) unsafe fn note_the_part(
    arena: &crate::cycle::arena::TraceScratchArena,
    rows: usize,
    from: (usize, Instant),
) {
    if rows <= WIDEST_PART.with(|widest| widest.get().rows) {
        return;
    }

    let mark_end = MARK_ENDED_AT.with(|at| at.get());
    let mut touched = 0;
    let mut array = arena.touched_head();
    while !array.is_null() {
        touched += 1;
        array = unsafe { (*array).next };
    }
    let reading = PartReading {
        rows,
        blocks: arena.blocks_held(),
        mark_positions: mark_end - from.0,
        scan_positions: arena.positions_inspected() - mark_end,
        touched,
        wall: from.1.elapsed(),
    };
    WIDEST_PART.with(|widest| widest.set(reading));
}

pub(crate) fn take_widest_part() -> PartReading {
    WIDEST_PART.with(|widest| widest.replace(PartReading::default()))
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
        roots_served: ROOTS_SERVED.swap(0, Ordering::Relaxed),
        grants: GRANTS.swap(0, Ordering::Relaxed),
        refusals: REFUSALS.swap(0, Ordering::Relaxed),
    }
}

/// The wall a stubbed trace spins in place of a batch's parts, in
/// nanoseconds, zero for the real trace; process-wide, for the rig's cell
/// alone (`dev/BENCHMARKS.md`, "readings the S65 and S67 stage notes held,
/// carried at the stages' close", run R0).
static STUB_TRACE_NANOS: AtomicU64 = AtomicU64::new(0);

/// Replace every batch's parts with a spin of `wall` until the guard drops:
/// the roots the reading pass leaves without a verdict are posted read live,
/// as a trace that met the state whole posts them, so that R is read at the
/// cadence of the rounds alone.
pub(crate) fn stub_the_trace(wall: std::time::Duration) -> StubbedTrace {
    STUB_TRACE_NANOS.store(wall.as_nanos() as u64, Ordering::Relaxed);
    StubbedTrace
}

pub(crate) fn stubbed_trace() -> Option<std::time::Duration> {
    match STUB_TRACE_NANOS.load(Ordering::Relaxed) {
        0 => None,
        nanos => Some(std::time::Duration::from_nanos(nanos)),
    }
}

/// The stub [`stub_the_trace`] set, lifted at the drop.
pub(crate) struct StubbedTrace;

impl Drop for StubbedTrace {
    fn drop(&mut self) {
        STUB_TRACE_NANOS.store(0, Ordering::Relaxed);
    }
}

/// K for every batch at the threshold, zero for the size each mutator's
/// batches grow; process-wide, for the rig's cell alone.
static FIXED_BATCH_SIZE: AtomicUsize = AtomicUsize::new(0);

/// Clamp every batch at the threshold to `roots` until the guard drops.
pub(crate) fn fix_the_batch_size(roots: usize) -> FixedBatchSize {
    FIXED_BATCH_SIZE.store(roots, Ordering::Relaxed);
    FixedBatchSize
}

pub(crate) fn fixed_batch_size() -> Option<usize> {
    match FIXED_BATCH_SIZE.load(Ordering::Relaxed) {
        0 => None,
        roots => Some(roots),
    }
}

/// The size [`fix_the_batch_size`] set, lifted at the drop.
pub(crate) struct FixedBatchSize;

impl Drop for FixedBatchSize {
    fn drop(&mut self) {
        FIXED_BATCH_SIZE.store(0, Ordering::Relaxed);
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

/// The blocks the collector thread's critical reserve held when the last cut
/// mark ended, `usize::MAX` for none since a case took it: what the mark left
/// to the posts.
static RESERVE_AT_THE_CUT: AtomicUsize = AtomicUsize::new(usize::MAX);

pub(crate) fn note_the_reserve_at_the_cut(blocks: usize) {
    RESERVE_AT_THE_CUT.store(blocks, Ordering::Relaxed);
}

pub(crate) fn take_the_reserve_at_the_cut() -> Option<usize> {
    match RESERVE_AT_THE_CUT.swap(usize::MAX, Ordering::Relaxed) {
        usize::MAX => None,
        blocks => Some(blocks),
    }
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
pub(crate) unsafe fn note_rows_met(arena: &crate::cycle::arena::TraceScratchArena) -> usize {
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
    met
}

pub(crate) fn take_rows_met() -> usize {
    ROWS_MET.with(|rows| rows.replace(0))
}

/// The stride reading of the next collector's trace at which the traced
/// mutator's recall is raised, as its take or a withheld stack's mark would
/// raise it, and zero for none: the case that stops a trace at a position it
/// chooses, inside the mark's first regions or past them. One-shot; the case
/// clears the recall afterwards.
static RECALL_AT_THE_READING: AtomicUsize = AtomicUsize::new(0);
/// The level the hook above raises.
static LEVEL_AT_THE_READING: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(crate::cycle::token::RECALL_STOP);

/// Raise the stop at the `reading`th stride reading of the next collector's
/// trace, as a waiting take would.
pub(crate) fn recall_at_the_reading(reading: usize) {
    recall_at_the_reading_at_level(reading, crate::cycle::token::RECALL_STOP);
}

/// Raise `level` at the `reading`th stride reading of the next collector's
/// trace: [`crate::cycle::token::RECALL_WIND_DOWN`] stands in for a withheld
/// stack's crossing.
pub(crate) fn recall_at_the_reading_at_level(reading: usize, level: u8) {
    assert_ne!(reading, 0, "the first reading is the first");
    LEVEL_AT_THE_READING.store(level, Ordering::Relaxed);
    RECALL_AT_THE_READING.store(reading, Ordering::Relaxed);
}

/// The level [`recall_at_the_reading_at_level`] asked for.
pub(crate) fn level_at_the_reading() -> u8 {
    LEVEL_AT_THE_READING.load(Ordering::Relaxed)
}

/// Whether `reading`, a collector trace's count of its stride readings, is
/// the one a case named, taking the hook if so.
pub(crate) fn recalls_at_the_reading(reading: usize) -> bool {
    RECALL_AT_THE_READING
        .compare_exchange(reading, 0, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
}

/// A second reading of the same trace at which the stop is raised, as a take
/// would raise it over a wind-down; zero for none. One-shot.
static STOP_AT_THE_READING: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn then_stop_at_the_reading(reading: usize) {
    assert_ne!(reading, 0, "the first reading is the first");
    STOP_AT_THE_READING.store(reading, Ordering::Relaxed);
}

/// Whether `reading` is the one [`then_stop_at_the_reading`] named, taking
/// the hook if so.
pub(crate) fn stops_at_the_reading(reading: usize) -> bool {
    STOP_AT_THE_READING
        .compare_exchange(reading, 0, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
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

/// Turn the served record's epoch cell before the next batch's stamps, on the
/// collector's thread, as the round's re-naming of the record to the elder
/// may turn it inside a grant. One-shot.
static TURN_BEFORE_THE_NEXT_STAMPS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(crate) fn turn_the_epoch_before_the_next_stamps() {
    TURN_BEFORE_THE_NEXT_STAMPS.store(true, Ordering::Relaxed);
}

/// Run the hook before the stamps on `mutator`, where a case installed one.
pub(crate) fn before_the_stamps(mutator: &crate::cycle::mutator_record::MutatorRecord) {
    if TURN_BEFORE_THE_NEXT_STAMPS.swap(false, Ordering::Relaxed) {
        unsafe { crate::cycle::epoch::turn_the_cell_of(mutator) };
    }
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

/// Stamp the birth of slot `index`'s life, on the thread being born, and
/// open its instruction counter.
pub(crate) fn note_collector_born(index: usize) {
    lock(&COLLECTOR_BORN_AT)[index] = Some(Instant::now());
    lock(&COLLECTOR_INSTRUCTIONS).standing[index] = ThreadCycles::open();
}

/// The collector threads' user-mode instructions: each standing life's
/// counter, which the driver reads live from its own thread, the kernel
/// bringing an active counter up to date at the read, and the lives that
/// ended, each folded in under the same lock as its counter closes.
struct CollectorInstructions {
    standing: [Option<ThreadCycles>; super::MAX_COLLECTORS],
    ended: u64,
}

static COLLECTOR_INSTRUCTIONS: Mutex<CollectorInstructions> = Mutex::new(CollectorInstructions {
    standing: [const { None }; super::MAX_COLLECTORS],
    ended: 0,
});

/// The collector threads' user-mode instructions so far, the lives that
/// ended and the standing ones to this instant; zero where the kernel
/// refused the counters.
pub(crate) fn collector_instructions_to_now() -> u64 {
    let counters = lock(&COLLECTOR_INSTRUCTIONS);
    counters.ended
        + counters
            .standing
            .iter()
            .flatten()
            .map(|cycles| cycles.read().1)
            .sum::<u64>()
}

/// Members the commits of every thread reclaimed since the process
/// started (`crate::cycle::reclamation::reclaim_before_drops`).
static MEMBERS_RECLAIMED: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn note_members_reclaimed(members: usize) {
    MEMBERS_RECLAIMED.fetch_add(members, Ordering::Relaxed);
}

pub(crate) fn members_reclaimed() -> usize {
    MEMBERS_RECLAIMED.load(Ordering::Relaxed)
}

/// The epoch advances of every record since the process started, by the
/// journal's `TURNOVER_*` code (`MutatorRecord::advance_the_epoch`).
static TURNOVERS_BY_CAUSE: [AtomicUsize; 4] = [const { AtomicUsize::new(0) }; 4];

pub(crate) fn note_turnover(why: u64) {
    TURNOVERS_BY_CAUSE[(why as usize).min(3)].fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn turnovers_by_cause() -> [usize; 4] {
    std::array::from_fn(|why| TURNOVERS_BY_CAUSE[why].load(Ordering::Relaxed))
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
    {
        let mut counters = lock(&COLLECTOR_INSTRUCTIONS);
        if let Some(cycles) = counters.standing[index].take() {
            counters.ended += cycles.read().1;
        }
    }
    CPU_AT_THE_LAST_ROUND[index].store(0, Ordering::Relaxed);
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
/// take's first reading of the claim to the take; and the waits and their
/// time split by the [grant segment](SEGMENT_AROUND) the take's first reading
/// met.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct TokenWaits {
    pub(crate) waits: usize,
    pub(crate) total: std::time::Duration,
    pub(crate) longest: std::time::Duration,
    pub(crate) waits_by_segment: [usize; SEGMENTS],
    pub(crate) total_by_segment: [std::time::Duration; SEGMENTS],
}

static TOKEN_WAITS: Mutex<TokenWaits> = Mutex::new(TokenWaits {
    waits: 0,
    total: std::time::Duration::ZERO,
    longest: std::time::Duration::ZERO,
    waits_by_segment: [0; SEGMENTS],
    total_by_segment: [std::time::Duration::ZERO; SEGMENTS],
});

/// Count a take's wait of `waited`, whose first reading met the holder in
/// `segment`.
pub(crate) fn note_token_wait(waited: std::time::Duration, segment: u8) {
    let mut waits = lock(&TOKEN_WAITS);
    waits.waits += 1;
    waits.total += waited;
    waits.longest = waits.longest.max(waited);
    waits.waits_by_segment[usize::from(segment)] += 1;
    waits.total_by_segment[usize::from(segment)] += waited;
}

/// The parts of a grant the rig splits the mutator's costs by: around the
/// batch — the arena's open, the reset and the release — and the trace with
/// its posts. A slot's segment is [`SEGMENT_AROUND`] whenever its collector
/// is not inside a batch.
pub(crate) const SEGMENT_AROUND: u8 = 0;
pub(crate) const SEGMENT_TRACE: u8 = 1;
pub(crate) const SEGMENTS: usize = 2;

/// Each slot's segment, stored by its collector and read by the mutator
/// whose token the slot holds.
static GRANT_SEGMENTS: [std::sync::atomic::AtomicU8; MAX_COLLECTORS] =
    [const { std::sync::atomic::AtomicU8::new(SEGMENT_AROUND) }; MAX_COLLECTORS];

thread_local! {
    /// The slot this collector thread serves a grant as.
    static SERVING_SLOT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Name the slot this collector thread serves its next grant as.
pub(crate) fn note_serving_slot(slot: usize) {
    SERVING_SLOT.with(|serving| serving.set(slot));
}

/// The segment the collector named by `byte`, a token byte, is in;
/// [`SEGMENT_AROUND`] for a byte no collector holds.
pub(crate) fn segment_of_the_holder(byte: u8) -> u8 {
    if crate::cycle::token::state(byte) != crate::cycle::token::COLLECTOR {
        return SEGMENT_AROUND;
    }

    GRANT_SEGMENTS[crate::cycle::token::slot(byte)].load(Ordering::Relaxed)
}

/// The collector's time in each segment of the batches since a case last
/// asked, summed and at the longest in one batch, and the batches timed.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct SegmentTimes {
    pub(crate) batches: usize,
    pub(crate) total: [std::time::Duration; SEGMENTS],
    pub(crate) longest: [std::time::Duration; SEGMENTS],
}

static SEGMENT_TIMES: Mutex<SegmentTimes> = Mutex::new(SegmentTimes {
    batches: 0,
    total: [std::time::Duration::ZERO; SEGMENTS],
    longest: [std::time::Duration::ZERO; SEGMENTS],
});

/// The batches' segment times since the last call, and zero them.
pub(crate) fn take_segment_times() -> SegmentTimes {
    std::mem::take(&mut *lock(&SEGMENT_TIMES))
}

/// One batch's segments: [`enter`](Self::enter) stores the segment in the
/// serving slot's cell and closes the one before it; the drop closes the
/// last, adds the batch's times to [`take_segment_times`] and stores
/// [`SEGMENT_AROUND`].
pub(crate) struct BatchSegments {
    slot: usize,
    segment: u8,
    from: Instant,
    spent: [std::time::Duration; SEGMENTS],
}

impl BatchSegments {
    /// Open the batch's timing in `segment`, on the thread serving it.
    pub(crate) fn open(segment: u8) -> Self {
        let slot = SERVING_SLOT.with(std::cell::Cell::get);
        GRANT_SEGMENTS[slot].store(segment, Ordering::Relaxed);
        Self {
            slot,
            segment,
            from: Instant::now(),
            spent: [std::time::Duration::ZERO; SEGMENTS],
        }
    }

    pub(crate) fn enter(&mut self, segment: u8) {
        let now = Instant::now();
        self.spent[usize::from(self.segment)] += now - self.from;
        GRANT_SEGMENTS[self.slot].store(segment, Ordering::Relaxed);
        (self.segment, self.from) = (segment, now);
    }
}

impl Drop for BatchSegments {
    fn drop(&mut self) {
        self.enter(SEGMENT_AROUND);
        let mut times = lock(&SEGMENT_TIMES);
        times.batches += 1;
        for (segment, spent) in self.spent.iter().enumerate() {
            times.total[segment] += *spent;
            times.longest[segment] = times.longest[segment].max(*spent);
        }
    }
}

/// A mutator thread's returns withheld under a foreign holder, by the
/// segment the holder was in at the withholding: how many, and their time
/// withheld, from each withholding to the drain that gave every stack back.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct WithheldBySegment {
    pub(crate) returns: [usize; SEGMENTS],
    pub(crate) time: [std::time::Duration; SEGMENTS],
    /// Per segment, the returns not yet given back and the sum of their
    /// instants of withholding, in nanoseconds from [`SEGMENT_CLOCK`]'s
    /// origin: at the drain their time is `pending × now − instants`.
    pending: [u128; SEGMENTS],
    instants: [u128; SEGMENTS],
}

static SEGMENT_CLOCK: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn segment_clock_now() -> u128 {
    SEGMENT_CLOCK.get_or_init(Instant::now).elapsed().as_nanos()
}

impl WithheldBySegment {
    const EMPTY: Self = Self {
        returns: [0; SEGMENTS],
        time: [std::time::Duration::ZERO; SEGMENTS],
        pending: [0; SEGMENTS],
        instants: [0; SEGMENTS],
    };
}

thread_local! {
    static WITHHELD_BY_SEGMENT: std::cell::RefCell<WithheldBySegment> =
        const { std::cell::RefCell::new(WithheldBySegment::EMPTY) };
}

/// Whether the returns withheld are timed by segment: the rig's cells and the
/// case of the split turn it on, so that a probe of the free path prices the
/// path and one load rather than two readings of the clock a return.
static TIMES_THE_WITHHELD: AtomicBool = AtomicBool::new(false);

/// Time the returns withheld by segment from here on, or stop.
pub(crate) fn time_the_withheld_returns(on: bool) {
    TIMES_THE_WITHHELD.store(on, Ordering::Relaxed);
}

/// Count a return this thread withholds under a foreign holder of its token.
pub(crate) fn note_a_return_withheld() {
    if !TIMES_THE_WITHHELD.load(Ordering::Relaxed) {
        return;
    }

    let record = crate::cycle::mutator_record::this_thread_record();
    if record.is_null() {
        return;
    }

    let segment = usize::from(segment_of_the_holder(unsafe { (*record).token.read() }));
    let now = segment_clock_now();
    WITHHELD_BY_SEGMENT.with(|withheld| {
        let mut withheld = withheld.borrow_mut();
        withheld.returns[segment] += 1;
        withheld.pending[segment] += 1;
        withheld.instants[segment] += now;
    });
}

/// Close the time of every return this thread withheld: the drain gave
/// every stack back.
pub(crate) fn note_the_returns_given_back() {
    if !TIMES_THE_WITHHELD.load(Ordering::Relaxed) {
        return;
    }

    let now = segment_clock_now();
    WITHHELD_BY_SEGMENT.with(|withheld| {
        let mut withheld = withheld.borrow_mut();
        for segment in 0..SEGMENTS {
            let nanos = withheld.pending[segment] * now - withheld.instants[segment];
            withheld.time[segment] +=
                std::time::Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX));
            (withheld.pending[segment], withheld.instants[segment]) = (0, 0);
        }
    });
}

/// This thread's returns withheld by segment since its last call, and zero
/// the counts and times; returns still withheld keep their instants.
pub(crate) fn take_withheld_by_segment() -> WithheldBySegment {
    WITHHELD_BY_SEGMENT.with(|withheld| {
        let mut withheld = withheld.borrow_mut();
        let taken = *withheld;
        withheld.returns = [0; SEGMENTS];
        withheld.time = [std::time::Duration::ZERO; SEGMENTS];
        taken
    })
}

/// The takes' waits since the last call, and zero them.
pub(crate) fn take_token_waits() -> TokenWaits {
    std::mem::take(&mut *lock(&TOKEN_WAITS))
}

/// The mutators' collections over P since a case last asked: how many, how
/// long in all and at longest, and what they freed.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct VerdictCollections {
    pub(crate) collections: usize,
    pub(crate) total: std::time::Duration,
    pub(crate) longest: std::time::Duration,
    pub(crate) freed: usize,
    /// The positions the collections' own traces inspected, in all and at
    /// the most one collection did (`crate::cycle::mark::take_owner_positions`).
    pub(crate) positions: usize,
    pub(crate) positions_longest: usize,
    /// The collection's phases ([`COLLECTION_PHASES`]: the trace within the
    /// set as its mark and its scan, the membership's reading, and the commit's first reading,
    /// destructors, second reading with the teardown, and drops), in all and
    /// those of the longest collection; the close is what the total leaves.
    pub(crate) phases: [std::time::Duration; COLLECTION_PHASES],
    pub(crate) phases_of_the_longest: [std::time::Duration; COLLECTION_PHASES],
    /// The longest collection by the kind of set it read
    /// (`crate::cycle::posted_set::kind`), the last slot a collection that read
    /// none; all in the last slot without `recycler-over-counts`.
    pub(crate) longest_by_kind: [std::time::Duration; SET_KINDS + 1],
}

/// The kinds a posted set carries.
#[cfg(feature = "recycler-over-counts")]
pub(crate) const SET_KINDS: usize = crate::cycle::posted_set::kind::KINDS;
#[cfg(not(feature = "recycler-over-counts"))]
pub(crate) const SET_KINDS: usize = 0;

/// The phases of a collection over P the rig splits its pause into.
pub(crate) const COLLECTION_PHASES: usize = 7;

thread_local! {
    /// The commit's split as the commit running on this thread noted it:
    /// the first reading, the destructors, the second reading with the
    /// teardown, the drops (`crate::cycle::collect::commit`).
    static COMMIT_SPLIT: std::cell::Cell<[std::time::Duration; 4]> =
        const { std::cell::Cell::new([std::time::Duration::ZERO; 4]) };
}

/// Note one part of the commit running on this thread.
pub(crate) fn note_commit_part(part: usize, took: std::time::Duration) {
    COMMIT_SPLIT.with(|split| {
        let mut parts = split.get();
        parts[part] = took;
        split.set(parts);
    });
}

/// The commit's split this thread noted, and zero it.
pub(crate) fn take_commit_split() -> [std::time::Duration; 4] {
    COMMIT_SPLIT.with(|split| split.take())
}

thread_local! {
    /// The kind of set the collection over P running on this thread read,
    /// `SET_KINDS` for none.
    static PENDING_KIND: std::cell::Cell<usize> = const { std::cell::Cell::new(SET_KINDS) };
}

/// Note the kind of set the collection over P that is running read.
pub(crate) fn note_the_set_kind(kind: usize) {
    PENDING_KIND.with(|pending| pending.set(kind.min(SET_KINDS)));
}

thread_local! {
    /// The phases the collection over P running on this thread noted, read by
    /// the note of its whole time ([`note_verdict_collection`]), on the same
    /// thread: every mutator collects over its own P.
    static PENDING_PHASES: std::cell::Cell<[std::time::Duration; COLLECTION_PHASES]> =
        const { std::cell::Cell::new([std::time::Duration::ZERO; COLLECTION_PHASES]) };
}

/// Note the phases of the collection over P that is closing
/// (`crate::cycle::collect`).
pub(crate) fn note_collection_phases(phases: [std::time::Duration; COLLECTION_PHASES]) {
    PENDING_PHASES.with(|pending| pending.set(phases));
}

static VERDICT_COLLECTIONS: Mutex<VerdictCollections> = Mutex::new(VerdictCollections {
    collections: 0,
    total: std::time::Duration::ZERO,
    longest: std::time::Duration::ZERO,
    freed: 0,
    positions: 0,
    positions_longest: 0,
    phases: [std::time::Duration::ZERO; COLLECTION_PHASES],
    phases_of_the_longest: [std::time::Duration::ZERO; COLLECTION_PHASES],
    longest_by_kind: [std::time::Duration::ZERO; SET_KINDS + 1],
});

pub(crate) fn note_verdict_collection(took: std::time::Duration, freed: usize, positions: usize) {
    let phases = PENDING_PHASES.with(|pending| pending.take());
    let kind = PENDING_KIND.with(|pending| pending.replace(SET_KINDS));
    let mut collections = lock(&VERDICT_COLLECTIONS);
    collections.longest_by_kind[kind] = collections.longest_by_kind[kind].max(took);
    collections.collections += 1;
    collections.total += took;
    if took > collections.longest {
        collections.phases_of_the_longest = phases;
    }
    for (sum, phase) in collections.phases.iter_mut().zip(phases) {
        *sum += phase;
    }
    collections.longest = collections.longest.max(took);
    collections.freed += freed;
    collections.positions += positions;
    collections.positions_longest = collections.positions_longest.max(positions);
}

/// The collections over P since the last call, and zero them.
pub(crate) fn take_verdict_collections() -> VerdictCollections {
    std::mem::take(&mut *lock(&VERDICT_COLLECTIONS))
}

/// The mutators' dispositions of P with no trace window since a case last
/// asked, counted as the collections over P are; `freed` stays zero, a
/// disposition returning the slots of deaths and freeing no set.
static DISPOSALS: Mutex<VerdictCollections> = Mutex::new(VerdictCollections {
    collections: 0,
    total: std::time::Duration::ZERO,
    longest: std::time::Duration::ZERO,
    freed: 0,
    positions: 0,
    positions_longest: 0,
    phases: [std::time::Duration::ZERO; COLLECTION_PHASES],
    phases_of_the_longest: [std::time::Duration::ZERO; COLLECTION_PHASES],
    longest_by_kind: [std::time::Duration::ZERO; SET_KINDS + 1],
});

/// The returns a mutator withheld under a foreign holder, by stack — deaths,
/// chunks, blocks, in the unit each stack's mark counts — read at each
/// crossing of the mark and at each release's drain, where the count is the
/// grant's peak (`dev/BENCHMARKS.md`, "readings the S65 and S67 stage notes
/// held, carried at the stages' close", run R2).
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct WithheldReadings {
    pub(crate) crossings: [usize; 3],
    pub(crate) held_at_the_crossings: [usize; 3],
    pub(crate) most_at_a_crossing: [usize; 3],
    pub(crate) releases: [usize; 3],
    pub(crate) held_at_the_releases: [usize; 3],
    pub(crate) most_at_a_release: [usize; 3],
}

static WITHHELD_READINGS: Mutex<WithheldReadings> = Mutex::new(WithheldReadings {
    crossings: [0; 3],
    held_at_the_crossings: [0; 3],
    most_at_a_crossing: [0; 3],
    releases: [0; 3],
    held_at_the_releases: [0; 3],
    most_at_a_release: [0; 3],
});

/// Note stack `stack`'s count `held` just past its mark.
pub(crate) fn note_withheld_at_the_crossing(stack: usize, held: usize) {
    let mut readings = lock(&WITHHELD_READINGS);
    readings.crossings[stack] += 1;
    readings.held_at_the_crossings[stack] += held;
    readings.most_at_a_crossing[stack] = readings.most_at_a_crossing[stack].max(held);
}

/// Note the three stacks' counts as a release's drain begins, each one that
/// holds anything.
pub(crate) fn note_withheld_at_the_release(held: [usize; 3]) {
    // Every free's drain reaches here, almost always with nothing held.
    if held == [0; 3] {
        return;
    }

    let mut readings = lock(&WITHHELD_READINGS);
    for (stack, &held) in held.iter().enumerate().filter(|(_, held)| **held > 0) {
        readings.releases[stack] += 1;
        readings.held_at_the_releases[stack] += held;
        readings.most_at_a_release[stack] = readings.most_at_a_release[stack].max(held);
    }
}

/// The readings since the last call, and zero them.
pub(crate) fn take_withheld_readings() -> WithheldReadings {
    std::mem::take(&mut *lock(&WITHHELD_READINGS))
}

pub(crate) fn note_disposal(took: std::time::Duration) {
    let mut disposals = lock(&DISPOSALS);
    disposals.collections += 1;
    disposals.total += took;
    disposals.longest = disposals.longest.max(took);
}

/// What the schemes spend on the collector's stamps and the lane, in every
/// build, since a case last asked: stamps the collector wrote on a registered
/// member and on any other (`crate::cycle::collector_stamps`), the edges the collector's marks
/// pruned (the mutator's own collections not counted), and the roots the
/// deferred lane held at the turns, which a turn hands back into R whole
/// (pressure and the exit not counted) (`dev/BENCHMARKS.md`, "S65.36: G1, G3,
/// M1 and S1b each lose or tie against their scheme").
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct SchemeFigures {
    pub(crate) stamped_registered: usize,
    pub(crate) stamped_other: usize,
    pub(crate) collector_edges_pruned: usize,
    pub(crate) roots_reoffered: usize,
    /// The collector's marks' held stack: passes over held entries, entries
    /// held, and the most entries one pass read (`crate::cycle::mark`, "The
    /// held stack").
    pub(crate) collector_passes: usize,
    pub(crate) collector_held: usize,
    pub(crate) collector_widest_pass: usize,
}

static STAMPED_REGISTERED: AtomicUsize = AtomicUsize::new(0);
static STAMPED_OTHER: AtomicUsize = AtomicUsize::new(0);
static EDGES_PRUNED: AtomicUsize = AtomicUsize::new(0);
static ROOTS_REOFFERED: AtomicUsize = AtomicUsize::new(0);
static HELD_PASSES: AtomicUsize = AtomicUsize::new(0);
static HELD: AtomicUsize = AtomicUsize::new(0);
static WIDEST_PASS: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn note_stamped(registered: bool) {
    let counter = if registered {
        &STAMPED_REGISTERED
    } else {
        &STAMPED_OTHER
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn note_edges_pruned(edges: usize) {
    EDGES_PRUNED.fetch_add(edges, Ordering::Relaxed);
}

pub(crate) fn note_held_figures(figures: crate::cycle::mark::HeldFigures) {
    HELD_PASSES.fetch_add(figures.passes, Ordering::Relaxed);
    HELD.fetch_add(figures.held, Ordering::Relaxed);
    WIDEST_PASS.fetch_max(figures.widest_pass, Ordering::Relaxed);
}

pub(crate) fn note_reoffered(roots: usize) {
    ROOTS_REOFFERED.fetch_add(roots, Ordering::Relaxed);
}

/// The scheme figures since the last call, and zero them.
pub(crate) fn take_scheme_figures() -> SchemeFigures {
    SchemeFigures {
        stamped_registered: STAMPED_REGISTERED.swap(0, Ordering::Relaxed),
        stamped_other: STAMPED_OTHER.swap(0, Ordering::Relaxed),
        collector_edges_pruned: EDGES_PRUNED.swap(0, Ordering::Relaxed),
        roots_reoffered: ROOTS_REOFFERED.swap(0, Ordering::Relaxed),
        collector_passes: HELD_PASSES.swap(0, Ordering::Relaxed),
        collector_held: HELD.swap(0, Ordering::Relaxed),
        collector_widest_pass: WIDEST_PASS.swap(0, Ordering::Relaxed),
    }
}

/// The dispositions of P since the last call, and zero them.
pub(crate) fn take_disposals() -> VerdictCollections {
    std::mem::take(&mut *lock(&DISPOSALS))
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

/// Raises of the recall to the stop level by a withheld stack's second mark,
/// at a crossing or at a consent, since the last call.
static RECALLS_AT_THE_SECOND_MARK: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn note_second_mark_recall() {
    RECALLS_AT_THE_SECOND_MARK.fetch_add(1, Ordering::Relaxed);
}

/// Recalls at a stack's second mark since the last call, and zero the count.
pub(crate) fn take_second_mark_recalls() -> usize {
    RECALLS_AT_THE_SECOND_MARK.swap(0, Ordering::Relaxed)
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

/// How long a byte state stood before its end: a count, the total and the
/// longest (`dev/BENCHMARKS.md`, "readings the S65 and S67 stage notes held,
/// carried at the stages' close").
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct StandingTimes {
    pub(crate) count: usize,
    pub(crate) total: std::time::Duration,
    pub(crate) longest: std::time::Duration,
}

impl StandingTimes {
    const EMPTY: Self = Self {
        count: 0,
        total: std::time::Duration::ZERO,
        longest: std::time::Duration::ZERO,
    };

    fn note(&mut self, stood: std::time::Duration) {
        self.count += 1;
        self.total += stood;
        self.longest = self.longest.max(stood);
    }

    /// The sum of two readings: counts and totals add, the longest is the
    /// larger.
    pub(crate) fn merged(self, other: Self) -> Self {
        Self {
            count: self.count + other.count,
            total: self.total + other.total,
            longest: self.longest.max(other.longest),
        }
    }
}

/// A mutator's standing times: requests it consented to, requests its own
/// take went over, and `POSTED` or `ASKED` until its take consumed it.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct MutatorStandings {
    pub(crate) consented: StandingTimes,
    pub(crate) taken_over: StandingTimes,
    pub(crate) posted: StandingTimes,
}

impl MutatorStandings {
    const EMPTY: Self = Self {
        consented: StandingTimes::EMPTY,
        taken_over: StandingTimes::EMPTY,
        posted: StandingTimes::EMPTY,
    };

    /// The sum of two readings, figure by figure.
    pub(crate) fn merged(self, other: Self) -> Self {
        Self {
            consented: self.consented.merged(other.consented),
            taken_over: self.taken_over.merged(other.taken_over),
            posted: self.posted.merged(other.posted),
        }
    }
}

/// A state a token's byte enters, with the writer that enters it.
pub(crate) enum ByteState {
    /// `REQUESTED|slot`, entered by collector `slot`'s request.
    Requested(usize),
    /// `POSTED` or `NOTHING_PROPOSED`, entered by the grant holder's release.
    Posted,
    /// `ASKED`, entered by the elder's ask under a cap of zero.
    Asked,
}

impl ByteState {
    /// The second half of the state's key: the slot, or a value no slot
    /// takes.
    fn key_part(&self) -> usize {
        match *self {
            Self::Requested(slot) => slot,
            Self::Posted => usize::MAX,
            Self::Asked => usize::MAX - 1,
        }
    }
}

/// How a request stopped standing.
pub(crate) enum RequestEnd {
    /// The mutator's reading consented, on its own thread.
    Consented,
    /// The mutator's take went over it, on its own thread.
    TakenByTheMutator,
    /// The collector withdrew it.
    Withdrawn,
}

/// One state entered and not yet ended: its key, the token's address and the
/// state's [`ByteState::key_part`]; the instant; and the entry's own number.
struct Entry {
    key: (usize, usize),
    entered: Instant,
    number: u64,
}

/// Whether the entries below are recorded. Off until a case or the rig turns
/// it on: the table's lock sits on the handshake of both sides, collector and
/// mutator, which no production build has, so every other test and every rig
/// cell that does not read the figures runs the handshake without it.
static STANDINGS_RECORDED: AtomicBool = AtomicBool::new(false);
/// The states standing on the tokens' bytes, oldest first. An entry is pushed
/// before the swap or the store that enters its state, so that an end made at
/// once finds it; it leaves by its own swap's failure, which removes that
/// entry alone, or by the state's end, which takes the oldest entry under its
/// key. Each key has one writer at a time — a request's collector, the grant
/// holder releasing to `POSTED`, the elder asking — so one state stands under
/// a key and every younger entry is an attempt whose failure is still to be
/// noted: the oldest is the state that ended.
static STATES_STANDING: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static NEXT_ENTRY: AtomicU64 = AtomicU64::new(0);
/// The requests collectors withdrew, over every token, since the last
/// [`take_withdrawn_standings`].
static WITHDRAWN_STANDING: Mutex<StandingTimes> = Mutex::new(StandingTimes::EMPTY);

thread_local! {
    /// The standings this thread ended as a mutator since the last
    /// [`take_this_threads_standings`].
    static THIS_THREADS_STANDINGS: std::cell::RefCell<MutatorStandings> =
        const { std::cell::RefCell::new(MutatorStandings::EMPTY) };
}

/// Record the standing times from now on, or stop; the entries standing at
/// the switch keep their places.
pub(crate) fn record_standings(on: bool) {
    STANDINGS_RECORDED.store(on, Ordering::Relaxed);
}

/// `state` is about to be entered on the token at address `token`. The
/// answer goes to [`note_not_entered`] if the swap fails; a state entered
/// must be ended through [`note_request_ended`] or [`note_posted_taken`],
/// or its entry answers the key's next end. `None` while nothing is recorded.
pub(crate) fn note_entering(token: usize, state: ByteState) -> Option<u64> {
    if !STANDINGS_RECORDED.load(Ordering::Relaxed) {
        return None;
    }

    let number = NEXT_ENTRY.fetch_add(1, Ordering::Relaxed);
    lock(&STATES_STANDING).push(Entry {
        key: (token, state.key_part()),
        entered: Instant::now(),
        number,
    });
    Some(number)
}

/// The swap [`note_entering`] answered `entry` for failed, and its entry
/// goes.
pub(crate) fn note_not_entered(entry: Option<u64>) {
    if let Some(number) = entry {
        lock(&STATES_STANDING).retain(|entry| entry.number != number);
    }
}

/// How long the oldest state under `state`'s key on the token at address
/// `token` stood, its entry removed; `None` for a state no entry stands for
/// (a case's own write of the byte, or one entered while nothing was
/// recorded).
fn end_of(token: usize, state: ByteState) -> Option<std::time::Duration> {
    let key = (token, state.key_part());
    let mut standing = lock(&STATES_STANDING);
    let position = standing.iter().position(|entry| entry.key == key)?;
    Some(standing.remove(position).entered.elapsed())
}

/// Collector `slot`'s request over the token at address `token` ended `how`;
/// a consent or a take is noted on the mutator's thread and counts in its
/// figures.
pub(crate) fn note_request_ended(token: usize, slot: usize, how: RequestEnd) {
    let Some(stood) = end_of(token, ByteState::Requested(slot)) else {
        return;
    };
    match how {
        RequestEnd::Consented => {
            THIS_THREADS_STANDINGS.with(|s| s.borrow_mut().consented.note(stood))
        }
        RequestEnd::TakenByTheMutator => {
            THIS_THREADS_STANDINGS.with(|s| s.borrow_mut().taken_over.note(stood))
        }
        RequestEnd::Withdrawn => lock(&WITHDRAWN_STANDING).note(stood),
    }
}

/// The mutator's take consumed `ASKED` (`asked`) or `POSTED` over the token
/// at address `token`, on its own thread.
pub(crate) fn note_posted_taken(token: usize, asked: bool) {
    let state = if asked {
        ByteState::Asked
    } else {
        ByteState::Posted
    };
    if let Some(stood) = end_of(token, state) {
        THIS_THREADS_STANDINGS.with(|s| s.borrow_mut().posted.note(stood));
    }
}

/// How many states stand on the token at address `token`, entries of failed
/// attempts not yet noted included.
pub(crate) fn states_standing_on(token: usize) -> usize {
    lock(&STATES_STANDING)
        .iter()
        .filter(|entry| entry.key.0 == token)
        .count()
}

/// This thread's standing times since it last asked, and zero them.
pub(crate) fn take_this_threads_standings() -> MutatorStandings {
    THIS_THREADS_STANDINGS.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// The withdrawn requests' standing times since the last call, and zero them.
pub(crate) fn take_withdrawn_standings() -> StandingTimes {
    std::mem::take(&mut *lock(&WITHDRAWN_STANDING))
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

/// Two hardware counters of the calling thread, user mode only: the cycles
/// it ran and the instructions it retired since [`ThreadCycles::open`], which
/// answers `None` where the kernel refuses them (no PMU,
/// `perf_event_paranoid` above 2), off x86-64 Linux and under Miri.
pub(crate) struct ThreadCycles {
    /// The two descriptors, cycles first.
    descriptors: [i32; 2],
}

impl ThreadCycles {
    pub(crate) fn open() -> Option<ThreadCycles> {
        #[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
        {
            let cycles = open_counter(0)?;
            let Some(instructions) = open_counter(1) else {
                unsafe { close(cycles) };
                return None;
            };
            Some(ThreadCycles {
                descriptors: [cycles, instructions],
            })
        }

        #[cfg(not(all(target_os = "linux", target_arch = "x86_64", not(miri))))]
        None
    }

    /// Cycles and instructions since the open, each divided by the share of
    /// the interval its counter ran, and the lower share: below 1 only when
    /// the kernel multiplexed the counters, a reading not to be quoted.
    pub(crate) fn read(&self) -> (u64, u64, f64) {
        let mut values = [0u64; 2];
        let mut share = 1.0f64;
        for (value, &descriptor) in values.iter_mut().zip(&self.descriptors) {
            // `PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING`:
            // the count, the time enabled, the time running.
            let mut buffer = [0u64; 3];
            let read = unsafe { read(descriptor, buffer.as_mut_ptr().cast(), 24) };
            assert_eq!(read, 24, "the counter reads");
            let ran = if buffer[1] == 0 {
                1.0
            } else {
                buffer[2] as f64 / buffer[1] as f64
            };
            share = share.min(ran);
            *value = if ran > 0.0 {
                (buffer[0] as f64 / ran) as u64
            } else {
                0
            };
        }

        (values[0], values[1], share)
    }
}

impl Drop for ThreadCycles {
    fn drop(&mut self) {
        for &descriptor in &self.descriptors {
            unsafe { close(descriptor) };
        }
    }
}

unsafe extern "C" {
    fn syscall(number: i64, ...) -> i64;
    fn read(descriptor: i32, buffer: *mut u8, count: usize) -> isize;
    fn close(descriptor: i32) -> i32;
}

/// One `PERF_TYPE_HARDWARE` counter of `config` on the calling thread, any
/// CPU, user mode only, counting from the open.
#[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
fn open_counter(config: u64) -> Option<i32> {
    /// `perf_event_open` in x86-64 Linux's numbering.
    const PERF_EVENT_OPEN: i64 = 298;
    /// `PERF_ATTR_SIZE_VER7`: the kernel accepts every size it knows.
    const ATTRIBUTE_SIZE: usize = 128;
    /// `exclude_kernel | exclude_hv`, bits 5 and 6 of the flag word.
    const USER_ONLY: u64 = 1 << 5 | 1 << 6;

    let mut attribute = [0u64; ATTRIBUTE_SIZE / 8];
    // The type (`PERF_TYPE_HARDWARE`, zero) in the low half, the size in
    // the high.
    attribute[0] = (ATTRIBUTE_SIZE as u64) << 32;
    attribute[1] = config;
    // `read_format`: `PERF_FORMAT_TOTAL_TIME_ENABLED | ..._RUNNING`, the two
    // times that follow the count in `ThreadCycles::read`'s buffer.
    attribute[4] = 1 | 2;
    attribute[5] = USER_ONLY;
    let descriptor = unsafe {
        syscall(
            PERF_EVENT_OPEN,
            attribute.as_ptr(),
            0i32,
            -1i32,
            -1i32,
            0u64,
        )
    };
    (descriptor >= 0).then_some(descriptor as i32)
}

/// The calling thread's context switches since its creation, voluntary and
/// involuntary (`getrusage(RUSAGE_THREAD)`); zeros off Linux and under Miri.
pub(crate) fn thread_context_switches() -> (u64, u64) {
    let usage = thread_usage();
    (usage[16] as u64, usage[17] as u64)
}

/// The calling thread's minor page faults since its creation
/// (`getrusage(RUSAGE_THREAD)`); zero off Linux and under Miri.
pub(crate) fn thread_minor_faults() -> u64 {
    thread_usage()[8] as u64
}

/// `struct rusage` of the calling thread as sixteen longs after the two
/// `timeval`s' four: `ru_minflt` at 8, `ru_nvcsw` and `ru_nivcsw` at 16 and 17.
fn thread_usage() -> [i64; 18] {
    #[cfg(all(target_os = "linux", not(miri)))]
    {
        /// `RUSAGE_THREAD` in Linux's numbering.
        const THIS_THREAD: i32 = 1;

        unsafe extern "C" {
            fn getrusage(who: i32, usage: *mut [i64; 18]) -> i32;
        }

        let mut usage = [0i64; 18];
        let read = unsafe { getrusage(THIS_THREAD, &mut usage) };
        assert_eq!(read, 0, "the thread's usage reads");
        usage
    }

    #[cfg(not(all(target_os = "linux", not(miri))))]
    [0; 18]
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
    let served = unsafe {
        super::serve(
            record,
            super::ELDER,
            1,
            &mut standing,
            super::serve_clock_now(),
        )
    };
    #[cfg(feature = "debug-journal")]
    keep_the_serving_threads_counts();
    served
}

/// What the ring of the thread that last ran [`serve_alone`] counted, read
/// on that thread before it ends: a case's collector thread retires its ring
/// at its exit, and the suite's other threads can evict a retired ring
/// before the case reads it. The cases holding the pool's guard.
#[cfg(feature = "debug-journal")]
static SERVING_THREADS_COUNTS: Mutex<Option<crate::journal::Counts>> = Mutex::new(None);

#[cfg(feature = "debug-journal")]
fn keep_the_serving_threads_counts() {
    let counts = crate::journal::counts_of(&[crate::journal::this_thread_identity()]);
    *SERVING_THREADS_COUNTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(counts);
}

/// The counts [`serve_alone`] kept for its thread's ring, taken.
#[cfg(feature = "debug-journal")]
pub(crate) fn take_the_serving_threads_counts() -> crate::journal::Counts {
    SERVING_THREADS_COUNTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
        .expect("a serve kept its thread's counts")
}

/// Wait for `collector` as the mutator does: reading its byte — consenting
/// to a request, arming on `POSTED` — between yields, the way its poll and
/// its slot frees would, until the thread finishes; then join it.
pub(crate) fn consent_while<T>(collector: JoinHandle<T>) -> T {
    while !collector.is_finished() {
        crate::cycle::token::read_and_act_on_this_thread();
        // The harness thread stands at a safepoint between its reads: every
        // reference a case holds into the collected heap is counted, or names
        // an entity the case has made garbage on purpose.
        #[cfg(feature = "recycler-over-counts")]
        crate::cycle::token::reach_the_checkpoint_on_this_thread();
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

/// The figures a batch's trace starts from, taken after the hook a case
/// runs at its start: the instant and the arena's positions, the trace's
/// own counters emptied of what came before it.
pub(super) fn at_the_start_of_the_batchs_trace(
    arena: &crate::cycle::arena::TraceScratchArena,
) -> (std::time::Instant, usize) {
    at_the_start_of_the_trace();
    let _ = (
        crate::cycle::mark::take_edges_pruned(),
        crate::cycle::mark::take_held_figures(),
        take_rows_met(),
        take_widest_part(),
    );
    (std::time::Instant::now(), arena.positions_inspected())
}

/// Note what a batch's trace did, against the figures
/// [`at_the_start_of_the_batchs_trace`] took.
pub(super) fn note_the_batchs_trace(
    mutator: &MutatorRecord,
    arena: &crate::cycle::arena::TraceScratchArena,
    roots: usize,
    outcome: &super::BatchOutcome,
    traced_from: std::time::Instant,
    positions_from: usize,
) {
    let edges_pruned = crate::cycle::mark::take_edges_pruned();
    note_edges_pruned(edges_pruned);
    note_held_figures(crate::cycle::mark::take_held_figures());
    note_traced_batch(|| TracedBatch {
        roots,
        traced: outcome.traced,
        complete: outcome.complete,
        blocks: arena.blocks_held(),
        wall: traced_from.elapsed(),
        positions_after_the_hook: take_positions_after_the_hook(),
        edges_pruned,
        rows_met: take_rows_met(),
        mutator: std::ptr::from_ref(mutator) as usize,
        ended: std::time::Instant::now(),
        widest_part: take_widest_part(),
        positions: arena.positions_inspected() - positions_from,
        ending: outcome.ending,
        turnovers: mutator.turnovers(),
    });
}
