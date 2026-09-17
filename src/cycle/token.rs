//! The trace token: the per-mutator word whose holder may trace that
//! mutator's graph — the arena, the block triples, the touched list — and
//! read its live candidate queue (`rfc/model/gc/rc-cycle.md`, "Concurrency").
//!
//! One token per mutator thread, taken by compare-and-swap and released by
//! one store. It stands in the thread's record, whose storage outlives the
//! thread so that a collector may reach it before holding anything
//! (`crate::cycle::mutator_record`). A thread meets its own token held only by
//! a collector that is tracing its graph, never by itself: mark and scan run
//! no user code, and the teardown that does runs after the release. So the
//! in-line collection takes the token before it detaches the lane and
//! releases it after its last row read — the scan's end on the path off the
//! poll, the harvest sweep on the path under pressure — and everything from
//! the exact validation on runs untokened. The one holder that keeps the
//! token past its last row read is the exit, whose final claim is never
//! released (`rfc/model/gc/rc-cycle.md`, "Concurrency", the exit paragraph).
//! What the release ends is the right to trace, not the life of the rows: a
//! collection off the poll keeps reading its rows through the teardown, and
//! whether a foreign holder may take the token over rows that teardown is
//! still reading is a ruling nobody has made (`rfc/model/gc/rc-cycle.md`,
//! "Concurrency", the readership paragraph). The collector thread's batch
//! is `crate::cycle::worker`'s, and the record's collecting word is what
//! keeps it off an in-line collection's rows
//! (`rfc/dev/DECISIONS.md`, "the candidate queue is read behind its writer,
//! and the collector's verdicts come back by a second ring").
//!
//! **A waiter blocks rather than spins.** The mutator that finds its token held
//! waits on a mutex and is woken by the release; a trace runs no user code and
//! takes no user lock, so the wait is bounded by one trace (Edmond,
//! 2026-08-29, `rfc/dev/DECISIONS.md`, "a trace stays inside the blocks of
//! the thread it claimed"). Eligibility is checked before the wait: a thread
//! the gate refuses — one already collecting, inside a teardown, or inside a
//! reset — opens no window (`crate::cycle::collect::may_collect`), and of
//! the three only the teardown refusal takes the token afterwards, for the
//! retirement pass that rewrites the ring
//! (`crate::cycle::collect::collect_under_pressure`); the exit's own
//! collection runs with the gate open and waits through the same take
//! (`crate::cycle::collect::collect_before_exit`).
//!
//! **Why per thread.** No thread names an entity in another thread's blocks —
//! `thread_move` and `thread_clone` require the graph arriving in a thread to
//! hold no reference to what stays behind — so two traces of two threads never
//! meet in a block, a triple or a row, and the exclusion narrows to the thread
//! (`rfc/dev/DECISIONS.md`, "a trace stays inside the blocks of the thread it
//! claimed").

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};

/// The token of one mutator thread.
///
/// Three fields: the flag the compare-and-swap takes, and the mutex and
/// condition variable a waiter blocks on. None of the three may carry drop
/// glue, because the token stands in a record that is written in place and
/// never dropped (`crate::cycle::mutator_record`); the assertion below holds
/// that on every target, and a target whose mutex is not futex-backed fails
/// there.
pub(crate) struct TraceToken {
    held: AtomicBool,
    wait: Mutex<()>,
    released: Condvar,
    /// How many times a taker has gone to wait on this token. A case reads
    /// it because a take that never waited is indistinguishable, from
    /// outside, from one whose wait is a no-op. Counted per wait on the
    /// condition variable rather than per take: a spurious wakeup re-tests
    /// the flag and waits again, so a case asserts that the count moved and
    /// never what it reached.
    #[cfg(test)]
    waits: std::sync::atomic::AtomicUsize,
}

const _: () = assert!(
    !std::mem::needs_drop::<TraceToken>(),
    "a record written in place and never dropped may carry no drop glue"
);

impl TraceToken {
    /// A token already held, the state a record leaves the registry in
    /// (`crate::cycle::mutator_record`): the taker releases it when its
    /// initialisation is complete, and no claim succeeds before that.
    pub(crate) const fn new_held() -> Self {
        Self {
            held: AtomicBool::new(true),
            wait: Mutex::new(()),
            released: Condvar::new(),
            #[cfg(test)]
            waits: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Take the token if it is free, and say whether it was.
    ///
    /// The form a collector worker uses: one that finds the token held skips
    /// this mutator until a later round rather than waiting for it.
    ///
    /// A take is followed by a `SeqCst` fence, paired with the one before
    /// the mutator's reading on its free path
    /// (`crate::cycle::mutator_record::MutatorRecord::held_by_another`): the
    /// pair is what makes the mutator's stores before that reading visible to
    /// the trace this take starts. Without it the taker may read the graph
    /// as it stood before the mutator's last stores — an array's storage head
    /// before its growth — and stride memory the mutator freed after reading
    /// the token free; the acquire on the swap alone orders nothing the
    /// mutator did before its load (`token/free_path_model.rs`, the loom
    /// model that exhibits the execution).
    #[must_use]
    pub(crate) fn try_take(&self) -> bool {
        let took = self
            .held
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok();
        if took {
            std::sync::atomic::fence(Ordering::SeqCst);
        }

        took
    }

    /// Take the token, waiting while a holder has it.
    ///
    /// The mutator's form. The wait is a block on the mutex, woken by
    /// [`release`](Self::release); the flag is re-tested under the mutex, so a
    /// release between the test and the wait is not lost.
    pub(crate) fn take(&self) {
        if self.try_take() {
            return;
        }

        let mut guard = self
            .wait
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while !self.try_take() {
            #[cfg(test)]
            self.waits.fetch_add(1, Ordering::SeqCst);
            guard = self
                .released
                .wait(guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// Release the token: one store, then the wake of whoever waits on it.
    ///
    /// The notify is made under the mutex so that a waiter which tested the
    /// flag before this store and is about to wait cannot miss it.
    pub(crate) fn release(&self) {
        debug_assert!(
            self.held.load(Ordering::Relaxed),
            "a release of a token nobody holds"
        );
        self.held.store(false, Ordering::Release);
        let _guard = self
            .wait
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.released.notify_all();
    }

    /// Whether some tracer holds the token now — a reading, not a claim, and
    /// stale by the time it is read unless the reader is the holder.
    ///
    /// The mutator reads it on its free path to decide whether a return waits
    /// for a foreign trace (`crate::cycle::deferred_slot_reuse`), and both
    /// stale directions are safe there: a holder that let go just after the
    /// read costs one return withheld until the mutator's next pop, and a
    /// taker that arrived just after it starts a trace that sees every store
    /// the mutator made before the read — the fence pair of
    /// [`try_take`](Self::try_take) is what makes that so — and so never
    /// holds the address the mutator is returning. The load is an acquire,
    /// paired with [`release`](Self::release)'s store: a return the mutator
    /// makes after reading the token free then happens after every load of
    /// the trace that held it, and the free-list link it writes into the
    /// dead entity does not race the trace's load of that word.
    pub(crate) fn is_held(&self) -> bool {
        self.held.load(Ordering::Acquire)
    }

    /// How many times a taker has gone to wait on this token so far.
    #[cfg(test)]
    pub(crate) fn waits(&self) -> usize {
        self.waits.load(Ordering::SeqCst)
    }
}

/// The token of the calling thread, as a pointer a case standing in for a
/// collector holds from another thread. The collector thread itself reaches a token through the
/// record a round hands it (`crate::cycle::worker`), and no production path
/// takes the pointer.
///
/// The pointee is a line of the mutator's record, and the record's storage
/// outlives the thread (`crate::cycle::mutator_record`), so the pointer stays
/// valid after this thread exits; what a holder finds there after the exit's
/// final claim is a token held until the record's next thread completes its
/// initialisation and releases it. Null for a thread with no record.
#[cfg(test)]
pub(crate) fn this_thread_token() -> *const TraceToken {
    let record = crate::cycle::mutator_record::this_thread_record();
    if record.is_null() {
        return std::ptr::null();
    }

    unsafe { &raw const (*record).token }
}

/// Whether a thread other than this one holds this thread's token now
/// ([`TraceToken::is_held`], less the mutator's own claim). False for a thread
/// with no record: no collector can reach a token that does not exist.
#[inline]
pub(crate) fn held_by_a_foreign_holder() -> bool {
    let record = crate::cycle::mutator_record::this_thread_record();
    !record.is_null() && unsafe { (*record).held_by_another() }
}

/// The token of the calling thread, held from the call to the guard's drop:
/// the mutator's own take around its trace.
///
/// **A take inside the mutator's own claim is nested and releases nothing**:
/// the exit claims its token once for good and runs its collection rounds
/// under that claim (`crate::cycle::collect::collect_before_exit`), and each
/// round's take must neither wait on the exit's own word nor let go of it.
///
/// **A thread with no record holds nothing.** Its collection is refused
/// before any window opens (`crate::cycle::collect::CollectingThread`), and
/// the one work it takes the guard around — the teardown refusal's
/// retirement pass — runs untokened, which excludes no one: a collector
/// reaches a thread through its record, and this thread has none.
///
/// The drop releases on the unwind as well as on the return, so a panic
/// inside a trace leaves no token held for a waiter to block on forever. Not
/// `Send`: the drop releases the word of the thread it runs on, and a guard
/// moved to another thread would free that thread's token instead.
#[must_use = "the token is released when this guard drops"]
pub(crate) struct HeldToken {
    /// The record whose token this guard released on drop, or null for a
    /// nested take and for a thread without a record.
    releases: *mut crate::cycle::mutator_record::MutatorRecord,
    thread_bound: std::marker::PhantomData<*const ()>,
}

impl HeldToken {
    /// Take this thread's token, waiting while a collector holds it.
    pub(crate) fn take() -> Self {
        let record = crate::cycle::mutator_record::this_thread_record();
        let releases = if record.is_null() {
            std::ptr::null_mut()
        } else if unsafe { crate::cycle::mutator_record::mutator_holds(record) } {
            std::ptr::null_mut()
        } else {
            unsafe { (*record).token.take() };
            unsafe { crate::cycle::mutator_record::note_mutator_holds(record, true) };
            record
        };

        Self {
            releases,
            thread_bound: std::marker::PhantomData,
        }
    }

    /// Keep the claim past the guard: the token stays held by this thread,
    /// and nothing releases it. The exit's final claim
    /// (`crate::cycle::mutator_record::release_thread_record`).
    pub(crate) fn keep(self) {
        std::mem::forget(self);
    }
}

#[cfg(test)]
thread_local! {
    /// Whether the traced mutator's token was held at the trace's last row
    /// read — the scan's end, and the harvest sweep under pressure — since a
    /// case last asked. The upper edge of what the token covers, which no
    /// destructor can observe.
    static HELD_AT_LAST_ROW_READ: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };

    /// The record of the mutator whose graph this thread is tracing as a
    /// collector, null while it traces as a mutator: the token the probe
    /// above reads is that mutator's rather than this thread's own.
    static TRACED_MUTATOR: std::cell::Cell<*mut crate::cycle::mutator_record::MutatorRecord> =
        const { std::cell::Cell::new(std::ptr::null_mut()) };
}

/// Name the mutator whose graph the calling collector thread traces under a
/// foreign claim, or null once its trace is over, and do nothing at all
/// without `cfg(test)`.
///
/// Called by `cycle::worker` around its trace, and by the verdict ring's
/// test collector (`cycle::queue::verdicts::testing`).
#[inline]
pub(crate) fn note_traced_mutator(record: *mut crate::cycle::mutator_record::MutatorRecord) {
    #[cfg(test)]
    TRACED_MUTATOR.with(|cell| cell.set(record));
    #[cfg(not(test))]
    let _ = record;
}

/// Record whether the token is held at the reading that ends a trace's row
/// reads, and do nothing at all without `cfg(test)`. The token is the traced
/// mutator's: this thread's own unless [`note_traced_mutator`] named another.
///
/// Called by `cycle::trace` at the scan's end and by the arena's harvest
/// sweep, and by nothing else.
#[inline]
pub(crate) fn note_last_row_read() {
    #[cfg(test)]
    {
        let mut record = TRACED_MUTATOR.with(std::cell::Cell::get);
        if record.is_null() {
            record = crate::cycle::mutator_record::this_thread_record();
        }

        let held = !record.is_null() && unsafe { (*record).token.is_held() };
        HELD_AT_LAST_ROW_READ.with(|cell| cell.set(Some(held)));
    }
}

/// The last reading [`note_last_row_read`] made, and clear it.
#[cfg(test)]
pub(crate) fn take_held_at_last_row_read() -> Option<bool> {
    HELD_AT_LAST_ROW_READ.with(|held| held.take())
}

impl Drop for HeldToken {
    fn drop(&mut self) {
        if self.releases.is_null() {
            return;
        }

        unsafe {
            crate::cycle::mutator_record::note_mutator_holds(self.releases, false);
            (*self.releases).token.release();
        }
    }
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests;

// The loom model of the free path's reading against a take is not run by
// the suite: it exists only under `--cfg loom`, where the dev-dependency
// exists too. How to run it, and what it demonstrated, are in the file.
#[cfg(loom)]
mod free_path_model;
