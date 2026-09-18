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
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
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
/// The handles of the threads spawned, for the join.
static HANDLES: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

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
    pub(crate) batches: usize,
    pub(crate) grants: usize,
    pub(crate) refusals: usize,
}

static TOKEN_HELD: AtomicUsize = AtomicUsize::new(0);
static POSTED: AtomicUsize = AtomicUsize::new(0);
static UNANSWERED: AtomicUsize = AtomicUsize::new(0);
static IDLE: AtomicUsize = AtomicUsize::new(0);
static MUTATORS_SERVED: AtomicUsize = AtomicUsize::new(0);
static GRANTS: AtomicUsize = AtomicUsize::new(0);
static REFUSALS: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn note_served(served: super::Served) {
    let count = match served {
        super::Served::TokenHeld => &TOKEN_HELD,
        super::Served::Posted => &POSTED,
        super::Served::Unanswered => &UNANSWERED,
        super::Served::Idle => &IDLE,
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

/// Every outcome since the last call, and zero the counts.
pub(crate) fn take_outcomes() -> Outcomes {
    Outcomes {
        token_held: TOKEN_HELD.swap(0, Ordering::Relaxed),
        posted: POSTED.swap(0, Ordering::Relaxed),
        unanswered: UNANSWERED.swap(0, Ordering::Relaxed),
        idle: IDLE.swap(0, Ordering::Relaxed),
        batches: MUTATORS_SERVED.swap(0, Ordering::Relaxed),
        grants: GRANTS.swap(0, Ordering::Relaxed),
        refusals: REFUSALS.swap(0, Ordering::Relaxed),
    }
}

/// The block budget the next batch traces under, for the case that reads
/// what a batch that meets it posts; `usize::MAX` for the module's own.
static NEXT_BATCH_BUDGET: AtomicUsize = AtomicUsize::new(usize::MAX);

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

/// What the next pre-claim reading runs between its take of the mutator's
/// blocks and its loads of them, for the case whose mutator exits in that
/// window; the closure runs on the collector's thread, once.
static AT_THE_NEXT_READING: Mutex<Option<Box<dyn FnOnce() + Send>>> = Mutex::new(None);

pub(crate) fn at_the_next_reading(act: Box<dyn FnOnce() + Send>) {
    *AT_THE_NEXT_READING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(act);
}

pub(crate) fn between_the_take_and_the_reading() {
    let act = AT_THE_NEXT_READING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(act) = act {
        act();
    }
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

/// Threads spawned since a case last asked.
static SPAWNS: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn keep_handle(handle: JoinHandle<()>) {
    SPAWNS.fetch_add(1, Ordering::Relaxed);
    HANDLES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(handle);
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
/// Closes the births again, lifts the confinement, restores the cap, the
/// threshold and the wait, forgets the last refused birth and zeroes the
/// rounds; a one-shot hook a case armed and never reached stays armed.
pub(crate) fn retire() {
    permit_births(false);
    RETIRING.store(true, Ordering::Relaxed);
    let handles = std::mem::take(
        &mut *HANDLES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    );
    // Every slot's word, under its mutex, before the notify: a thread between
    // its `retiring()` check and its wait would otherwise sleep out a wait a
    // case pinned long.
    for index in 0..super::MAX_COLLECTORS {
        let _ = super::wake(index);
    }
    for handle in handles {
        // A thread that panicked in a round is joined all the same: the case
        // that reads its word sees the panic there, and a panic raised inside
        // this drop during an unwind would end the whole binary.
        let _ = handle.join();
    }

    super::forget_refused_birth();

    RETIRING.store(false, Ordering::Relaxed);
    confine_rounds_to_records(&[]);
    serve_rounds_at(0);
    wait_between_rounds_for(None);
    super::set_collector_cap(super::DEFAULT_COLLECTOR_CAP);
    let _ = take_rounds();
    let _ = take_round_times();
    let _ = take_outcomes();
}

/// Take the elder's slot for the calling thread, so that a consent's wake
/// of slot [`ELDER`] reaches a stand-in collector: a stand-in that waits on
/// the slot's word until the grant ([`wait_for_the_elders_wake`]), rather
/// than spinning on the byte, makes progress under Miri's weak-memory
/// emulation, where a spinning reader can read the old value for a very
/// long time. The word is cleared as the thread's own birth clears it, so a
/// wake sent to the empty slot is not the stand-in's first wait ended.
/// Given back by [`stand_down_as_the_elder`]; a case that calls this has no
/// collector thread born.
pub(crate) fn stand_in_as_the_elder() {
    assert_eq!(
        thread_state(),
        ThreadState::Unborn,
        "the elder's slot is free"
    );
    super::forget_wakes(ELDER);
}

/// Leave the elder's slot as a thread's end leaves it: the word a late wake
/// set stays for the next birth's clear.
pub(crate) fn stand_down_as_the_elder() {}

/// Sleep on the elder slot's word until a wake or `timeout`, taking the
/// word, as the thread's own waits do.
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
    unsafe { super::serve(record, super::ELDER, 1, &mut standing) }
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
/// as a silent mutator in a case about something else. A case about the
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
/// to read as a silent mutator.
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
