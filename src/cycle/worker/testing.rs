//! The switches a case sets on the collector thread: whether the pressure
//! path may birth one, which record its rounds visit, whether its base block
//! is refused, and how it is ended.
//!
//! Every switch is process-wide, so a case that sets one holds the memory
//! tests' guard, and births are forbidden again before the case ends.
//! The rounds are confined because the harness runs other cases' threads
//! beside this one: a request set on a stranger's record makes its next poll
//! offer, which a case asserting on its own lane reads as a wrong count.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::thread::JoinHandle;

use super::{ALIVE, STARTING, THREAD, UNBORN};
use crate::cycle::owner_record::OwnerRecord;

/// Whether [`super::ensure_thread`] may spawn.
static BIRTHS_PERMITTED: AtomicBool = AtomicBool::new(false);
/// The one record a round serves and asks, or null for every record.
static CONFINED: AtomicPtr<OwnerRecord> = AtomicPtr::new(std::ptr::null_mut());
/// Records the rounds have reached since a case last asked, confined or not.
static RECORDS_VISITED: AtomicUsize = AtomicUsize::new(0);
/// Whether the next birth's `ll_thread_init` runs under a zero block budget.
static REFUSE_NEXT_BASE_BLOCK: AtomicBool = AtomicBool::new(false);
/// Whether the thread was asked to end.
static RETIRING: AtomicBool = AtomicBool::new(false);
/// The handle of the last thread spawned, for the join.
static HANDLE: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

/// Where the thread stands, for a case that waits on its birth.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ThreadState {
    Unborn,
    Starting,
    Alive,
}

pub(crate) fn thread_state() -> ThreadState {
    match THREAD.load(Ordering::Acquire) {
        UNBORN => ThreadState::Unborn,
        STARTING => ThreadState::Starting,
        ALIVE => ThreadState::Alive,
        other => unreachable!("the thread word holds {other}"),
    }
}

/// Let the pressure path birth the thread, or forbid it again.
pub(crate) fn permit_births(permitted: bool) {
    BIRTHS_PERMITTED.store(permitted, Ordering::Relaxed);
}

pub(crate) fn births_permitted() -> bool {
    BIRTHS_PERMITTED.load(Ordering::Relaxed)
}

/// Confine the rounds to `record`; null lifts the confinement.
pub(crate) fn confine_rounds_to(record: *mut OwnerRecord) {
    CONFINED.store(record, Ordering::Relaxed);
}

/// Whether the next visit of a round panics, for the case that reads what a
/// panicking round leaves behind.
static PANIC_AT_NEXT_VISIT: AtomicBool = AtomicBool::new(false);

pub(crate) fn panic_at_the_next_visit() {
    PANIC_AT_NEXT_VISIT.store(true, Ordering::Relaxed);
}

/// Whether a round serves and asks `record`, counting the visit either way.
pub(crate) fn in_round(record: *mut OwnerRecord) -> bool {
    RECORDS_VISITED.fetch_add(1, Ordering::Relaxed);
    if PANIC_AT_NEXT_VISIT.swap(false, Ordering::Relaxed) {
        panic!("a round panicked at a visit, by the case's request");
    }

    let confined = CONFINED.load(Ordering::Relaxed);
    confined.is_null() || confined == record
}

/// Records the rounds reached since the last call, and zero the count.
pub(crate) fn take_records_visited() -> usize {
    RECORDS_VISITED.swap(0, Ordering::Relaxed)
}

/// Chains the rounds posted, traced or not, since a case last asked.
static CHAINS_POSTED: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn note_served(served: super::Served) {
    if matches!(
        served,
        super::Served::Posted { .. } | super::Served::PostedUntraced
    ) {
        CHAINS_POSTED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Chains the rounds posted since the last call, and zero the count.
pub(crate) fn take_chains_posted() -> usize {
    CHAINS_POSTED.swap(0, Ordering::Relaxed)
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
    let mut kept = HANDLE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *kept = Some(handle);
}

/// Threads spawned since the last call, and zero the count.
pub(crate) fn take_spawns() -> usize {
    SPAWNS.swap(0, Ordering::Relaxed)
}

pub(crate) fn retiring() -> bool {
    RETIRING.load(Ordering::Relaxed)
}

/// End the thread and wait for it: the flag, a wake out of its pause, the
/// join. A thread whose birth was refused is joined the same way. Closes the
/// births again and lifts the confinement as well, so the next case starts
/// from the state the binary started in.
pub(crate) fn retire() {
    permit_births(false);
    RETIRING.store(true, Ordering::Relaxed);
    let handle = HANDLE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(handle) = handle {
        handle.thread().unpark();
        // A thread that panicked in a round is joined all the same: the case
        // that reads its word sees the panic there, and a panic raised inside
        // this drop during an unwind would end the whole binary.
        let _ = handle.join();
    }

    super::forget_refused_birth();

    RETIRING.store(false, Ordering::Relaxed);
    confine_rounds_to(std::ptr::null_mut());
}
