//! The mutator side of the rc-walk epoch protocol: the soft-handshake ack,
//! the confirmation message queue, and the checkpoint that serves both
//! (`rfc/model/gc/rc-walk.md`, Phases 3-4). The collector frees nothing
//! itself — every confirmed component ends in exactly one message drained
//! here, on the owning mutator thread. Acquittals post nothing since eager
//! death; they are dropped in the collector's private tables.
//!
//! **Checkpoints ride the death branch of `ll_release` rather than a
//! compiler-inserted poll.** The test lives in the `1 → 0` branch, already
//! cold and about to run teardown, and in `ll_gc_maybe_collect`, the poll
//! the reserve refills at. A compiler-emitted run of releases pays the
//! test once, split around the run: `ll_gc_checkpoint_ack` before it,
//! `ll_release_batch` per reference, a full `ll_gc_checkpoint` after. A
//! pre-run pickup would judge while the scope's transients are still
//! counted, and a loop whose only checkpoints are scope exits would then
//! phase-lock every pickup against the same held borrow. The raw
//! `ll_malloc` and `ll_free` paths and the factory carry no test, being
//! the hot paths: once a message is posted the epoch waits for the
//! thread's next death or poll, deliberately without a fallback.
//!
//! **The death branch acks only, and pickup rides teardown's exit.**
//! Between the committing zero store and dispose the dying entity is
//! committed-dead with a live weak cell, so no user code may run there. A
//! message picked up at that checkpoint runs drain destructors, and one
//! holding a `WeakRef` to the dying entity would `get()` a strong
//! reference to it: resurrection after commit, or a double teardown. So
//! [`checkpoint_ack`] acks the handshake and nothing else, and the full
//! [`checkpoint`] picks up messages only when no teardown is in flight on
//! this thread ([`TEARDOWN_DEPTH`]) and no synchronous collection runs
//! (`walk_active`, which holds guards a message may name). It runs at the
//! outermost dispose's exit, at the poll, and trailing a batched run.
//!
//! **The drain is not re-entrant.** A destructor run by the drain releases
//! references, and a release hitting zero checkpoints inside the drain.
//! One thread-local bit closes the recursion: the nested entry acks a
//! pending handshake and never picks up a message.
//!
//! The queue is a mutex rather than a lock-free structure, verdicts being a
//! cold per-epoch trickle (`dev/DECISIONS.md`, "cold concurrent structures
//! take a lock rather than a CAS loop").

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::refcount::RcHeader;
use crate::walk;

/// One posted confirmed component. Raw entity pointers cross from the
/// collector thread back to the mutator that owns them — the drain runs
/// where the entities live.
struct ConfirmationMessage {
    members: Vec<*mut RcHeader>,
}

// Safety: the pointers are only ever dereferenced by the owning mutator
// thread, in the drain; the collector treats them as opaque ids.
unsafe impl Send for ConfirmationMessage {}

/// Collector raised it; the next checkpoint acks and lowers it.
static HANDSHAKE_REQUESTED: AtomicBool = AtomicBool::new(false);
/// Monotonic ack count. The `AcqRel` bump is the release fence of the
/// handshake: every mutator write before the checkpoint is visible to a
/// collector that observes the new count with `Acquire`.
static HANDSHAKE_ACKS: AtomicU64 = AtomicU64::new(0);
/// Verdicts posted and not yet drained. The epoch cannot end while this
/// is non-zero — that ordering is what keeps ids naming one entity from
/// walk to drain, and at most one epoch's verdicts in flight, ever.
static OUTSTANDING_VERDICTS: AtomicUsize = AtomicUsize::new(0);
static QUEUE: Mutex<VecDeque<ConfirmationMessage>> = Mutex::new(VecDeque::new());

thread_local! {
    /// Set for the length of the pickup loop, which is what closes the
    /// drain recursion: a checkpoint reached from inside a drained
    /// destructor finds it set and returns.
    static MID_DRAIN: Cell<bool> = const { Cell::new(false) };
    /// Teardowns in flight on this thread. While non-zero, some entity
    /// on this thread is between its committing zero store and the end
    /// of its dispose — no message pickup (module doc).
    static TEARDOWN_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// The ack-only checkpoint: serve the handshake, nothing else. Two
/// callers: the death branch of `ll_release` — its before-teardown
/// position is what orders the epoch's activity bit before this
/// death's frees, and pickup would run user code against a
/// committed-dead entity (module doc) — and `ll_gc_checkpoint_ack`,
/// the ABI front of a batched-release run.
#[inline]
pub(crate) fn checkpoint_ack() {
    if HANDSHAKE_REQUESTED.load(Ordering::Relaxed) {
        ack_handshake();
    }
}

#[cold]
#[inline(never)]
fn ack_handshake() {
    if HANDSHAKE_REQUESTED.swap(false, Ordering::AcqRel) {
        HANDSHAKE_ACKS.fetch_add(1, Ordering::AcqRel);
    }
}

/// Bracket a teardown: no message pickup between a death's committing
/// store and the end of its dispose. Nests (child deaths inside a
/// parent's dispose); the exit of the **outermost** teardown runs the
/// full checkpoint — dead and disposed, or resurrected and live, the
/// entity is whole again there.
#[inline]
pub(crate) fn teardown_enter() {
    TEARDOWN_DEPTH.with(|d| d.set(d.get() + 1));
}

/// See [`teardown_enter`].
#[inline]
pub(crate) fn teardown_exit() {
    let depth = TEARDOWN_DEPTH.with(|d| {
        let depth = d.get() - 1;
        d.set(depth);
        depth
    });

    if depth == 0 {
        checkpoint();
    }
}

/// The full checkpoint test: cheap loads and a predicted branch, taken
/// only when the collector wants attention or a closed epoch left
/// parked memory to flush. Callers: the outermost teardown's exit, the
/// `ll_gc_maybe_collect` poll, and the explicit `ll_gc_checkpoint`
/// that trails a batched-release run (module doc). The death branch of
/// `ll_release` and the front of a batched run call [`checkpoint_ack`]
/// instead.
#[inline]
pub(crate) fn checkpoint() {
    if HANDSHAKE_REQUESTED.load(Ordering::Relaxed)
        || OUTSTANDING_VERDICTS.load(Ordering::Relaxed) != 0
        || crate::memory::deferred_free::flush_due()
    {
        checkpoint_attend();
    }
}

#[cold]
#[inline(never)]
fn checkpoint_attend() {
    ack_handshake();
    // Nested entry (an allocation inside a draining destructor, a poll
    // inside a running `__destruct`): memory served, handshake acked,
    // message left where it is. `TEARDOWN_DEPTH` covers the poll — a
    // destructor that allocates may checkpoint mid-dispose, and the
    // entity being disposed must not meet drain user code until its
    // teardown completes. The synchronous collection is drain-class
    // too (`walk_active`): it holds guards on members a message may
    // name, and a pickup inside one of its destructors would violate
    // the drain's no-other-guards contract (rc-walk.md, "When the
    // collector runs", step 4).
    if MID_DRAIN.with(|d| d.get()) || TEARDOWN_DEPTH.with(|d| d.get()) != 0 || walk::walk_active() {
        return;
    }

    MID_DRAIN.with(|d| d.set(true));
    loop {
        let message = QUEUE.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
        let Some(message) = message else { break };
        // Safety: the message names entities this thread owns; the
        // drain upholds its own contract.
        unsafe {
            walk::drain_confirmed(&message.members);
        }

        // The ack: released so a collector seeing zero (Acquire) also
        // sees every effect of the drain.
        OUTSTANDING_VERDICTS.fetch_sub(1, Ordering::Release);
    }

    MID_DRAIN.with(|d| d.set(false));
    // A closed epoch's parked memory returns at the owning thread's
    // next checkpoint — this one. Never mid-epoch (the queue's identity
    // job), and never mid-drain (the frees above parked while active).
    if crate::memory::deferred_free::flush_due() {
        unsafe { crate::memory::deferred_free::flush() };
    }
}

// --- Collector-side surface, driven by `crate::collector`; no
// production collector thread spawns it yet --------------------------------

/// Raise the handshake flag. The collector then waits for
/// [`handshake_acks`] to move past its snapshot.
pub(crate) fn request_handshake() {
    HANDSHAKE_REQUESTED.store(true, Ordering::Release);
}

/// Monotonic handshake ack count (`Acquire`: pairs with the ack's
/// release bump — mutator writes before the checkpoint are visible).
pub(crate) fn handshake_acks() -> u64 {
    HANDSHAKE_ACKS.load(Ordering::Acquire)
}

/// Post one confirmed component to the owning mutator. The outstanding
/// count moves **before** the message becomes visible, so a concurrent
/// drain can never drive the counter below zero. Acquittals post
/// nothing (eager-death amendment): the collector drops them in its
/// own tables.
pub(crate) fn post_confirmation(members: Vec<*mut RcHeader>) {
    OUTSTANDING_VERDICTS.fetch_add(1, Ordering::Relaxed);
    QUEUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_back(ConfirmationMessage { members });
}

/// Verdicts posted and not yet drained. Zero (with `Acquire`) is the
/// collector's licence to end the epoch: flush, retire blocks, walk
/// again.
pub(crate) fn outstanding_verdicts() -> usize {
    OUTSTANDING_VERDICTS.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests;
