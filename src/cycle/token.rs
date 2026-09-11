//! The trace token: the per-mutator word whose holder may trace that
//! mutator's graph — the arena, the block triples, the touched list — and
//! read its live candidate queue (`rfc/model/gc/rc-cycle.md`, "Concurrency").
//!
//! One token per mutator thread, taken by compare-and-swap and released by
//! one store. A thread meets its own token held only by a collector that is
//! tracing its graph, never by itself: mark and scan run no user code, and the
//! teardown that does runs after the release. So the in-line collection takes
//! the token before it detaches the lane and releases it after its last row
//! read — the scan's end on the path off the poll, the harvest sweep on the
//! path under pressure — and everything from the exact validation on runs
//! untokened. What the release ends is the right to trace, not the life of
//! the rows: a collection off the poll keeps reading its rows through the
//! teardown, and whether a foreign holder may take the token over rows that
//! teardown is still reading is a ruling nobody has made
//! (`rfc/model/gc/rc-cycle.md`, "Concurrency", the readership paragraph);
//! until one is, the only tracer is the owner, and the question has no
//! second party.
//!
//! **A waiter blocks rather than spins.** The owner that finds its token held
//! waits on a mutex and is woken by the release; a trace runs no user code and
//! takes no user lock, so the wait is bounded by one trace (Edmond,
//! 2026-08-29, `rfc/dev/DECISIONS.md`, "a trace stays inside the blocks of
//! the thread it claimed"). Eligibility is checked before the wait: a thread
//! today's gate refuses — one already collecting, or inside a reset — never
//! reaches the token; the teardown-depth clause of that gate is `PLAN.md`
//! S38.4's (`crate::cycle::collect`).
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
/// glue, because the token stands in a `thread_local!` that thread exit
/// reaches (`crate::memory::heap::ll_thread_exit`); the assertion below holds
/// that on every target, and a target whose mutex is not futex-backed fails
/// there.
pub(crate) struct TraceToken {
    held: AtomicBool,
    wait: Mutex<()>,
    released: Condvar,
    /// How many times a taker has gone to wait on this token. A case reads
    /// it because a take that never waited is indistinguishable, from
    /// outside, from one whose wait is a no-op.
    #[cfg(test)]
    waits: std::sync::atomic::AtomicUsize,
}

const _: () = assert!(
    !std::mem::needs_drop::<TraceToken>(),
    "a thread-local the exit path reaches may carry no drop glue"
);

impl TraceToken {
    const fn new() -> Self {
        Self {
            held: AtomicBool::new(false),
            wait: Mutex::new(()),
            released: Condvar::new(),
            #[cfg(test)]
            waits: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Take the token if it is free, and say whether it was.
    ///
    /// The form a collector worker uses: one that finds the token held skips
    /// this owner until a later round rather than waiting for it.
    #[must_use]
    pub(crate) fn try_take(&self) -> bool {
        self.held
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    /// Take the token, waiting while a holder has it.
    ///
    /// The owner's form. The wait is a block on the mutex, woken by
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
    #[cfg(test)]
    pub(crate) fn is_held(&self) -> bool {
        self.held.load(Ordering::Relaxed)
    }

    /// How many times a taker has gone to wait on this token so far.
    #[cfg(test)]
    pub(crate) fn waits(&self) -> usize {
        self.waits.load(Ordering::SeqCst)
    }
}

thread_local! {
    /// This thread's token. `const`-initialised and without drop glue, as the
    /// exit path requires.
    static TOKEN: TraceToken = const { TraceToken::new() };
}

/// The token of the calling thread, as a pointer a collector can hold from
/// another thread.
///
/// The pointee lives until this thread exits, and nothing yet keeps a holder
/// from outliving it: the exit that waits for a trace over this thread's
/// blocks is `PLAN.md` S39.1's, the collector that would trace another
/// thread's graph is S38.0's, and how it finds an owner's token no step names
/// yet. Today the pointer is taken by the case that stands in for a
/// collector, and every such case joins the holder before it returns.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "a collector tracing another thread's graph is S38.0's; \
                  until it lands only a test takes the pointer"
    )
)]
pub(crate) fn this_thread_token() -> *const TraceToken {
    TOKEN.with(|token| token as *const TraceToken)
}

/// The token of the calling thread, held from the call to the guard's drop:
/// the owner's own take around its trace.
///
/// The drop releases on the unwind as well as on the return, so a panic
/// inside a trace leaves no token held for a waiter to block on forever. Not
/// `Send`: the drop releases the word of the thread it runs on, and a guard
/// moved to another thread would free that thread's token instead.
#[must_use = "the token is released when this guard drops"]
pub(crate) struct HeldToken {
    thread_bound: std::marker::PhantomData<*const ()>,
}

impl HeldToken {
    /// Take this thread's token, waiting while a collector holds it.
    pub(crate) fn take() -> Self {
        TOKEN.with(TraceToken::take);
        Self {
            thread_bound: std::marker::PhantomData,
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Whether this thread's token was held at the trace's last row read —
    /// the scan's end, and the harvest sweep under pressure — since a case
    /// last asked. The upper edge of what the token covers, which no
    /// destructor can observe.
    static HELD_AT_LAST_ROW_READ: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Record whether the token is held at the reading that ends a trace's row
/// reads, and do nothing at all without `cfg(test)`.
///
/// Called by `cycle::trace` at the scan's end and by the arena's harvest
/// sweep, and by nothing else.
#[inline]
pub(crate) fn note_last_row_read() {
    #[cfg(test)]
    HELD_AT_LAST_ROW_READ.with(|held| held.set(Some(TOKEN.with(TraceToken::is_held))));
}

/// The last reading [`note_last_row_read`] made, and clear it.
#[cfg(test)]
pub(crate) fn take_held_at_last_row_read() -> Option<bool> {
    HELD_AT_LAST_ROW_READ.with(|held| held.take())
}

impl Drop for HeldToken {
    fn drop(&mut self) {
        TOKEN.with(TraceToken::release);
    }
}

#[cfg(test)]
mod tests;
