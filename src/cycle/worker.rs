//! The collector thread, and what it does for one owner: claim the owner's
//! token by compare-and-swap, and release it. The batch it will make under
//! that claim — the entries taken from the owner's ring behind its writer,
//! traced through `cells::AtomicCells`, the verdicts posted to the owner's
//! verdict ring — is `PLAN.md` S49's, built over the ring S49.3 lays down
//! (`rfc/model/gc/rc-cycle.md`, "Worker-to-owner handoff"; `rfc/dev/DECISIONS.md`,
//! "the candidate queue is read behind its writer, and the collector's
//! verdicts come back by a second ring"). Until then a round over the records
//! serves nothing.
//!
//! # The thread, and the round over the records
//!
//! One collector thread per process, born by [`ensure_thread`] and never at
//! startup (`dev/DECISIONS.md`, "the collector thread is born at the first
//! pressure collection"): the pressure path is the one fire point the
//! runtime owns, and a thread born there costs nothing to a process that
//! never runs short. No production path births it until S49.7 wires the
//! pressure path back to [`ensure_thread`] with the wake channel. It starts
//! as any registered thread does, through `ll_thread_init`, whose base block
//! draw can be refused; a refused base block is a thread that never started,
//! and a call [`BIRTH_RETRY_INTERVAL`] or more after the refusal births
//! again. A round walks every record the registry has carved ([`round`]).
//! Between two rounds the thread sleeps for [`ROUND_INTERVAL`]. Nothing
//! wakes the thread early, and nothing but a test ends it.

// Dead in a build without tests until S49.7 gives the birth its caller; the
// tests are the module's only driver until then.
#![cfg_attr(not(test), allow(dead_code))]

use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use crate::cycle::owner_record::{self, OwnerRecord};

/// What one round over one owner did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Served {
    /// The owner, or another collector, holds the token.
    TokenHeld,
    /// The token was claimed and released with nothing served under it.
    Idle,
}

/// The pause between two rounds over the records.
const ROUND_INTERVAL: Duration = Duration::from_millis(10);

/// How long after a refused birth the pressure path waits before it spawns
/// again: a process that stays short of memory collects at every refused
/// allocation, and without the wait it would spawn a thread per refusal.
const BIRTH_RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// When the last birth was refused — a spawn the operating system refused,
/// or a base block the pool refused — or `None`.
static REFUSED_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// The name the collector thread is spawned under.
const THREAD_NAME: &str = "ll-collector";

/// Where the process's collector thread stands: [`UNBORN`], [`STARTING`]
/// from the spawn until its `ll_thread_init` answered, [`ALIVE`] from a
/// started init until the thread ends.
static THREAD: AtomicU8 = AtomicU8::new(UNBORN);
const UNBORN: u8 = 0;
const STARTING: u8 = 1;
const ALIVE: u8 = 2;

/// Start the collector thread unless the process has one already, one is
/// starting, or a birth was refused less than [`BIRTH_RETRY_INTERVAL`] ago.
/// A spawn the operating system refuses, and a base block the pool refuses,
/// each leave the process without a thread until a call after the interval.
/// No production path calls it yet (S49.7 makes the pressure path its
/// caller, after the collection so that the thread's draws compete with no
/// rows of the caller's own).
///
/// The spawn allocates through the global allocator — the thread's name and
/// the handle's shared state — on a path the ruling that no runtime path may
/// end the process on an allocation the manager could have refused forbids
/// it; `PLAN.md`'s backlog carries the debt.
pub(crate) fn ensure_thread() {
    #[cfg(test)]
    if !testing::births_permitted() {
        return;
    }

    // The load before the exchange keeps every pressure collection after the
    // birth off a read-modify-write of the word.
    if THREAD.load(Ordering::Relaxed) != UNBORN
        || birth_refused_recently()
        || THREAD
            .compare_exchange(UNBORN, STARTING, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
    {
        return;
    }

    match std::thread::Builder::new()
        .name(THREAD_NAME.into())
        .spawn(thread_body)
    {
        Ok(handle) => {
            #[cfg(test)]
            testing::keep_handle(handle);
            #[cfg(not(test))]
            drop(handle);
        }
        Err(_) => {
            note_refused_birth();
            THREAD.store(UNBORN, Ordering::Release);
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

/// The collector thread's life: its registration, its rounds, its exit.
fn thread_body() {
    // The word goes back to unborn however this thread ends — a refused
    // base block, a test's retire, or a panic in a round that unwinds out of
    // here — so that a later birth can happen rather than read a thread
    // that no longer exists.
    struct UnbornOnDrop;
    impl Drop for UnbornOnDrop {
        fn drop(&mut self) {
            THREAD.store(UNBORN, Ordering::Release);
        }
    }
    let _unborn = UnbornOnDrop;

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

    THREAD.store(ALIVE, Ordering::Release);
    while !retiring() {
        round();
        std::thread::park_timeout(ROUND_INTERVAL);
    }

    crate::memory::heap::ll_thread_exit();
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

/// One round over the records: visit every record but the thread's own,
/// which polls nothing, and serve each. What a serve does is [`serve`]'s.
fn round() {
    let own = owner_record::this_thread_record();
    owner_record::for_each_record(|record| {
        if record == own {
            return;
        }

        #[cfg(test)]
        if !testing::in_round(record) {
            return;
        }

        let served = unsafe { serve(record) };
        #[cfg(test)]
        testing::note_served(served);
        #[cfg(not(test))]
        let _ = served;
    });
}

/// Serve `record`'s owner once: claim its token by compare-and-swap, held
/// being a skip, and release it. Nothing is read or written of the owner's
/// under the claim until S49.5 builds the batch.
///
/// Runs on a collector thread, which holds a base block of its own for the
/// workspace the batch's trace opens (`crate::memory::heap::ll_thread_init`).
///
/// # Safety
/// `record` is a record of the registry's, and the calling thread is not its
/// owner.
pub(crate) unsafe fn serve(record: *mut OwnerRecord) -> Served {
    let token = unsafe { &(*record).token };
    if !token.try_take() {
        return Served::TokenHeld;
    }

    // Released on the unwind too: a collector that panicked under the claim
    // would otherwise leave the owner's exit waiting forever.
    struct ReleaseOnDrop<'a>(&'a crate::cycle::token::TraceToken);
    impl Drop for ReleaseOnDrop<'_> {
        fn drop(&mut self) {
            crate::cycle::token::note_traced_owner(std::ptr::null_mut());
            self.0.release();
        }
    }
    let _held = ReleaseOnDrop(token);
    crate::cycle::token::note_traced_owner(record);
    Served::Idle
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests;
