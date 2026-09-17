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

pub(crate) fn note_round(index: usize, interval: std::time::Duration) {
    if index == ELDER {
        TIMER_MILLIS.store(interval.as_millis() as usize, Ordering::Relaxed);
    }

    ROUNDS[index].fetch_add(1, Ordering::Release);
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

/// Mutators the rounds claimed and released since a case last asked.
static MUTATORS_SERVED: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn note_served(served: super::Served) {
    if matches!(served, super::Served::Batch { .. }) {
        MUTATORS_SERVED.fetch_add(1, Ordering::Relaxed);
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
    for handle in handles {
        handle.thread().unpark();
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
}
