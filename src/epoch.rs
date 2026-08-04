//! The mutator side of the rc-walk epoch protocol: the soft-handshake
//! ack, the confirmation message queue, and the checkpoint that serves
//! both (`rfc/model/gc/rc-walk.md`, Phases 3–4). The collector never
//! frees anything itself — every confirmed component ends in exactly
//! one message drained here, on the owning mutator thread, race-free.
//! Acquittals post nothing since the eager-death amendment
//! (2026-07-27): they are dropped in the collector's private tables.
//!
//! **Checkpoints ride the death branch of `ll_release`, not a
//! compiler-inserted poll** (decision 2026-07-27; formerly the entity
//! factory): the test lives in the `1 → 0` branch — already cold and
//! about to run teardown — and in `ll_gc_maybe_collect` (the poll the
//! reserve already refills at). Compiler-emitted runs of releases pay
//! the test once, split around the run (amendment 2026-07-28): one
//! `ll_gc_checkpoint_ack` before it, `ll_release_batch` per reference,
//! one full `ll_gc_checkpoint` after it — a pre-run pickup would judge
//! while the scope's transients are still counted, and a loop whose
//! only checkpoints are scope exits would then phase-lock every pickup
//! against the same held borrow. The raw
//! `ll_malloc`/`ll_free` C-ABI paths and the factory carry no test —
//! they are the hot paths, and a workload with no entity deaths
//! starves the epoch no worse than the accepted limit (finding F2,
//! reshaped: once a message is posted, the epoch waits for the
//! thread's next death or poll; deliberately no fallback).
//!
//! **The death branch acks only; pickup rides teardown's exit**
//! (review finding, 2026-07-27, `rfc/model/gc/rc-walk.md`). Between
//! the committing zero store and dispose, the dying entity is
//! committed-dead with a live weak cell — no user code may run there.
//! A message picked up at that checkpoint runs drain destructors, and
//! one holding a `WeakRef` to the dying entity would `get()` a strong
//! reference to it: resurrection after commit, or a double teardown.
//! So [`checkpoint_ack`] (the death branch) acks the handshake and
//! nothing else, and the full [`checkpoint`] picks up messages only
//! when no teardown is in flight on this thread ([`TEARDOWN_DEPTH`])
//! and no synchronous collection runs (`walk_active` — drain-class,
//! it holds guards a message may name) — it runs at the outermost
//! dispose's exit, at the poll, and trailing a batched-release run.
//!
//! **The drain is not re-entrant** (finding F8): a destructor run by
//! the drain releases references, and a release hitting zero
//! checkpoints inside the drain. One thread-local bit closes the
//! recursion — the nested entry acks a pending handshake, but never
//! picks up a message.
//!
//! The queue is a mutex, not a lock-free structure: verdicts are a
//! cold, per-epoch trickle, and cold concurrent structures take a lock
//! here (`dev/DECISIONS.md`, 2026-07-20).

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
    /// The one bit of allocator state that closes the drain recursion.
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

// --- Collector-side surface (the collector thread of commit 4; tests
// drive it directly until then) ---------------------------------------------

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
mod tests {
    use super::*;
    use crate::class::ClassBuilder;
    use crate::memory::arena::Arena;
    use crate::memory::context::LLContext;
    use crate::memory::heap::for_each_entity_slot;
    use crate::object::{Object, new_constructed};
    use crate::refcount::{MemoryCategory, ll_release, ll_retain};
    use crate::value::{Tag, Value};
    use std::sync::atomic::AtomicUsize;

    fn walked_addresses() -> Vec<usize> {
        let mut seen = Vec::new();
        unsafe { for_each_entity_slot(|e| seen.push(e as usize)) };
        seen
    }

    /// `a.child = b` as generated code leaves it: the slot owns one ref.
    unsafe fn tie(a: *mut Object, offset: u32, b: *mut Object) {
        unsafe {
            Object::prop_at(a, offset).write(Value::entity(Tag::Object, b as *mut RcHeader));
        }
    }

    // No condemn helper: condemnation is collector-private since the
    // eager-death amendment — posting the confirmation IS the
    // collector's whole footprint on these tests.

    static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
        DESTRUCTS.fetch_add(1, Ordering::Relaxed);
    }

    /// The checkpoint rides the death branch of `ll_release` (decision
    /// 2026-07-27): a non-final release carries no test, the `1 → 0`
    /// release acks a pending handshake.
    #[test]
    fn a_release_hitting_zero_is_a_checkpoint_and_a_non_final_one_is_not() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("DeathCheckpoint").build();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe { ll_retain(obj as *mut RcHeader) }; // rc 2

        let before = handshake_acks();
        request_handshake();
        assert!(!unsafe { ll_release(obj as *mut RcHeader) }); // rc 2 → 1
        assert_eq!(handshake_acks(), before, "non-final release: no checkpoint");

        assert!(unsafe { ll_release(obj as *mut RcHeader) }); // rc 1 → 0
        assert_eq!(handshake_acks(), before + 1, "the death branch acks");
        unsafe { crate::object::ll_object_die(obj) };
    }

    /// The batched contract splits the checkpoint around the run
    /// (`rfc/model/gc/rc-walk.md`, "Batched releases", amendment
    /// 2026-07-28): `ll_gc_checkpoint_ack` fronts the run — ack only,
    /// never a pickup — `ll_release_batch` carries no test, and the
    /// trailing `ll_gc_checkpoint` picks up. Pinned on a death-free
    /// run: those are exactly the runs where a pre-run pickup would
    /// judge against transients the run is about to return
    /// (the phase-lock shape).
    #[test]
    fn a_batched_run_acks_at_entry_and_picks_up_after_it() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let ring_cls = ClassBuilder::new("BatchedRunRing")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();
        let cls = ClassBuilder::new("BatchedRelease").build();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, ring_cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, ring_cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
        }
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe { ll_retain(obj as *mut RcHeader) }; // rc 2: the run's release is non-final

        post_confirmation(vec![a as *mut RcHeader, b as *mut RcHeader]);
        let before = handshake_acks();
        request_handshake();

        unsafe { crate::gc::ll_gc_checkpoint_ack() };
        assert_eq!(handshake_acks(), before + 1, "the front acks");
        assert_eq!(
            outstanding_verdicts(),
            1,
            "ack only: no pickup before the run"
        );

        assert!(!unsafe { crate::refcount::ll_release_batch(obj as *mut RcHeader) });
        assert_eq!(outstanding_verdicts(), 1, "the run itself never picks up");

        unsafe { crate::gc::ll_gc_checkpoint() };
        assert_eq!(outstanding_verdicts(), 0, "the trailing call picks up");
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            2,
            "the posted ring drained"
        );

        unsafe {
            assert!(ll_release(obj as *mut RcHeader));
            crate::object::ll_object_die(obj);
        }
        arena.reset(|_| {});
    }

    /// `ll_release_vector` acks once at entry — before any death, one
    /// ack for the whole batch — and runs the destructors in vector
    /// order (`rfc/model/memory/bulk-operations.md`). The entry
    /// position is pinned from inside the first destructor: it runs
    /// before any teardown-exit checkpoint could ack in entry's stead.
    #[test]
    fn a_vector_release_acks_once_and_dies_in_order() {
        use std::sync::Mutex;
        static ORDER: Mutex<Vec<usize>> = Mutex::new(Vec::new());
        static ACKS_AT_FIRST_DEATH: AtomicUsize = AtomicUsize::new(usize::MAX);
        unsafe extern "C" fn recording(obj: *mut Object) {
            if ORDER.lock().unwrap().is_empty() {
                ACKS_AT_FIRST_DEATH.store(handshake_acks() as usize, Ordering::Relaxed);
            }
            ORDER.lock().unwrap().push(obj as usize);
        }

        let _g = crate::memory::block_pool::test_guard();
        ORDER.lock().unwrap().clear();
        ACKS_AT_FIRST_DEATH.store(usize::MAX, Ordering::Relaxed);
        let cls = ClassBuilder::new("VectorRelease")
            .destructor(recording as *const ())
            .build();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let objects: Vec<*mut RcHeader> = (0..3)
            .map(|_| unsafe {
                new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) as *mut RcHeader
            })
            .collect();

        let before = handshake_acks();
        request_handshake();
        unsafe { crate::object::ll_release_vector(objects.as_ptr(), objects.len()) };
        assert_eq!(handshake_acks(), before + 1, "one ack for the whole vector");
        assert_eq!(
            ACKS_AT_FIRST_DEATH.load(Ordering::Relaxed),
            (before + 1) as usize,
            "the ack preceded the first death"
        );

        let order = ORDER.lock().unwrap();
        let expected: Vec<usize> = objects.iter().map(|&p| p as usize).collect();
        assert_eq!(*order, expected, "destructors in vector order");
    }

    /// The vector pickup trails the run (amendment 2026-07-28). The
    /// phase-lock shape: a component is posted while the vector still
    /// holds the reference keeping it alive. A pre-run pickup judges
    /// against that transient — exact-test mismatch, message dropped,
    /// garbage survives; and a loop whose only checkpoints are scope
    /// exits presents *every* pickup with the same held borrow. The
    /// trailing pickup judges after the release and collects.
    #[test]
    fn a_vector_release_picks_up_after_the_run_not_before() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("VectorPhaseLock")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
            ll_retain(a as *mut RcHeader); // the vector's transient
        }

        post_confirmation(vec![a as *mut RcHeader, b as *mut RcHeader]);
        let transients = [a as *mut RcHeader];
        unsafe { crate::object::ll_release_vector(transients.as_ptr(), transients.len()) };

        assert_eq!(
            outstanding_verdicts(),
            0,
            "the trailing pickup served the message"
        );
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            2,
            "judged after the release: collected"
        );
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
        arena.reset(|_| {});
    }

    /// The pickup gate refuses messages while a synchronous collection
    /// runs (`rfc/model/gc/rc-walk.md`, "When the collector runs",
    /// step 4): the collection is drain-class — it holds guards on
    /// members an epoch message may name. Checkpoints fire inside
    /// `walk::collect_cycles` (a destructor reaching the poll, every
    /// teardown exit on the sever path); each must leave the message
    /// pending.
    #[test]
    fn a_synchronous_collection_refuses_message_pickup() {
        let _g = crate::memory::block_pool::test_guard();
        static SEEN_OUTSTANDING_IN_WALK: AtomicUsize = AtomicUsize::new(usize::MAX);

        unsafe extern "C" fn checkpointing_probe(_obj: *mut Object) {
            // A checkpoint fires inside the walk — the shape of a
            // destructor reaching the compiler's poll. The pending
            // message must still be pending afterwards.
            checkpoint();
            SEEN_OUTSTANDING_IN_WALK.store(outstanding_verdicts(), Ordering::Relaxed);
        }

        DESTRUCTS.store(0, Ordering::Relaxed);
        SEEN_OUTSTANDING_IN_WALK.store(usize::MAX, Ordering::Relaxed);
        let live_cls = ClassBuilder::new("WalkGateLiveRing")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();
        let dying_cls = ClassBuilder::new("WalkGateDyingRing")
            .prop("child", true)
            .destructor(checkpointing_probe as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        // The posted component stays live through the walk (external
        // reference): only pickup timing is under test, not a verdict.
        let c1 = unsafe { new_constructed(&mut ctx, live_cls, MemoryCategory::GcHeap) };
        let c2 = unsafe { new_constructed(&mut ctx, live_cls, MemoryCategory::GcHeap) };
        let d1 = unsafe { new_constructed(&mut ctx, dying_cls, MemoryCategory::GcHeap) };
        let d2 = unsafe { new_constructed(&mut ctx, dying_cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(c1, 16, c2);
            tie(c2, 16, c1);
            ll_retain(c1 as *mut RcHeader); // keeps the posted ring live
            tie(d1, 16, d2);
            tie(d2, 16, d1);
        }

        post_confirmation(vec![c1 as *mut RcHeader, c2 as *mut RcHeader]);
        // The d-ring is garbage: the walk collects it, running the
        // probing destructors while the walk-active bit is set.
        unsafe { crate::walk::collect_cycles() };

        assert_eq!(
            SEEN_OUTSTANDING_IN_WALK.load(Ordering::Relaxed),
            1,
            "the checkpoint inside the walk left the message pending"
        );
        assert_eq!(outstanding_verdicts(), 1, "still pending after the walk");
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            0,
            "the live ring untouched"
        );

        // Outside the walk the pickup proceeds: the live ring fails the
        // exact test and the message is dropped whole.
        checkpoint();
        assert_eq!(outstanding_verdicts(), 0);
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 0, "dropped, not torn");
        let seen = walked_addresses();
        assert!(seen.contains(&(c1 as usize)) && seen.contains(&(c2 as usize)));

        // Cleanup: drop the external reference, the ring is garbage.
        assert!(!unsafe { ll_release(c1 as *mut RcHeader) });
        unsafe { crate::walk::collect_cycles() };
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        arena.reset(|_| {});
    }

    #[test]
    fn a_requested_handshake_is_acked_at_the_next_checkpoint() {
        let _g = crate::memory::block_pool::test_guard();
        let before = handshake_acks();
        checkpoint(); // nothing requested: no ack
        assert_eq!(handshake_acks(), before);

        request_handshake();
        checkpoint();
        assert_eq!(handshake_acks(), before + 1, "one callback, one ack");
        checkpoint(); // flag was consumed: no second ack
        assert_eq!(handshake_acks(), before + 1);
    }

    /// The full confirm path by message: a condemned garbage ring is
    /// exact-tested, destructed, severed and freed at the checkpoint —
    /// and the ack (outstanding → 0) is what lets the epoch end.
    #[test]
    fn a_confirmed_ring_is_freed_by_the_message_drain() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("EpochRing")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
        }

        post_confirmation(vec![a as *mut RcHeader, b as *mut RcHeader]);
        assert_eq!(outstanding_verdicts(), 1);
        checkpoint();
        assert_eq!(outstanding_verdicts(), 0, "the drain acked the message");
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
        arena.reset(|_| {});
    }

    /// A false post dies at the exact test: message dropped whole, no
    /// destructor, ring intact. A drop leaves nothing behind to clean —
    /// acquittal carries no duties since the eager-death amendment.
    #[test]
    fn a_false_post_is_dropped_and_the_live_ring_survives() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("EpochLiveRing")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
            ll_retain(a as *mut RcHeader); // the frame reference
        }

        post_confirmation(vec![a as *mut RcHeader, b as *mut RcHeader]);
        checkpoint();
        assert_eq!(outstanding_verdicts(), 0);
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            0,
            "no destructor on a live ring"
        );
        let seen = walked_addresses();
        assert!(seen.contains(&(a as usize)) && seen.contains(&(b as usize)));

        // The ring is genuinely garbage once the frame lets go.
        assert!(!unsafe { ll_release(a as *mut RcHeader) });
        unsafe { crate::walk::collect_cycles() };
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
        arena.reset(|_| {});
    }

    /// DC0 under eager death (`rfc/model/gc/rc-walk-danger-cases.md`,
    /// fix of 2026-07-27): a member that dies after its component was
    /// posted dies **whole, at the natural point** — destructor
    /// included — and the drain drops the message on the corpse
    /// without touching it. Exactly-once is probed through the free
    /// list after the flush: a twice-enqueued slot would be handed to
    /// two allocations.
    #[test]
    fn dc0_a_death_since_posting_drops_the_message_untouched() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("Dc0Corpse")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let x = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let x_addr = x as usize;

        // The epoch is in flight: deaths park, identity holds from
        // walk to drain — the corpse rule's precondition.
        crate::memory::deferred_free::begin_epoch();
        post_confirmation(vec![x as *mut RcHeader]);

        // The stale hypothesis is overtaken by an ordinary death:
        // eager — destructor NOW, free parked.
        unsafe {
            assert!(
                ll_release(x as *mut RcHeader),
                "eager: the death is reported"
            );
            crate::object::ll_object_die(x);
        }
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            1,
            "destructor at the natural point"
        );

        // The drain meets the corpse: message dropped whole, nothing
        // touched — the destructor count must not move.
        checkpoint();
        assert_eq!(outstanding_verdicts(), 0, "dropped counts as drained");
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            1,
            "the corpse is not torn again"
        );

        // Mid-epoch the slot stays out of circulation (parked).
        let y = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        assert_ne!(y as usize, x_addr, "parked: no reuse before the flush");

        crate::memory::deferred_free::end_epoch();
        checkpoint(); // flushes the parked slot

        // The free-list probe: one enqueue → the slot serves one
        // allocation, and the next one gets different memory.
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        assert_eq!(
            a as usize, x_addr,
            "LIFO: the flushed slot is back in circulation"
        );
        assert_ne!(b as usize, x_addr, "and was enqueued exactly once");
        unsafe {
            for &e in &[y, a, b] {
                assert!(ll_release(e as *mut RcHeader));
                crate::object::ll_object_die(e);
            }
        }
        arena.reset(|_| {});
    }

    /// Review finding (2026-07-27): between a death's committing zero
    /// store and its dispose the entity is committed-dead with a live
    /// weak cell. A message picked up at the death-branch checkpoint
    /// runs drain destructors — user code — and one holding a
    /// `WeakRef` to the dying entity would `get()` a strong reference
    /// to it (resurrection after commit, or a double teardown). The
    /// death branch acks only; pickup rides the outermost dispose's
    /// exit, by which point the cell reads null.
    #[test]
    fn the_drain_never_sees_an_entity_between_commit_and_dispose() {
        let _g = crate::memory::block_pool::test_guard();
        static RING_DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
        static GOT: AtomicUsize = AtomicUsize::new(usize::MAX);
        static CELL: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn probing_destructor(_obj: *mut Object) {
            RING_DESTRUCTS.fetch_add(1, Ordering::Relaxed);
            let cell = CELL.load(Ordering::Relaxed) as *mut crate::weak::LLWeakRef;
            if GOT.load(Ordering::Relaxed) == usize::MAX {
                let got = unsafe { crate::weak::ll_weakref_get(cell) };
                GOT.store(got as usize, Ordering::Relaxed);
                if !got.is_null() {
                    // Drop the strong reference `get` handed out; on
                    // the broken interleaving this very release is the
                    // second teardown the fix removes.
                    unsafe {
                        if ll_release(got) {
                            crate::object::ll_entity_die(got);
                        }
                    }
                }
            }
        }

        RING_DESTRUCTS.store(0, Ordering::Relaxed);
        GOT.store(usize::MAX, Ordering::Relaxed);
        DESTRUCTS.store(0, Ordering::Relaxed);
        let ring_cls = ClassBuilder::new("CommitWindowRing")
            .prop("child", true)
            .destructor(probing_destructor as *const ())
            .build();
        let target_cls = ClassBuilder::new("CommitWindowTarget")
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let r1 = unsafe { new_constructed(&mut ctx, ring_cls, MemoryCategory::GcHeap) };
        let r2 = unsafe { new_constructed(&mut ctx, ring_cls, MemoryCategory::GcHeap) };
        let x = unsafe { new_constructed(&mut ctx, target_cls, MemoryCategory::GcHeap) };
        let x_addr = x as usize;
        unsafe {
            tie(r1, 16, r2);
            tie(r2, 16, r1);
        }
        let cell = unsafe { crate::weak::ll_weakref_create(&mut ctx, x as *mut RcHeader) };
        CELL.store(cell as usize, Ordering::Relaxed);

        // Epoch in flight (identity holds), the ring posted confirmed.
        crate::memory::deferred_free::begin_epoch();
        post_confirmation(vec![r1 as *mut RcHeader, r2 as *mut RcHeader]);

        // X's final release: the death branch must NOT pick the ring
        // up here — X is committed-dead, its cell still live.
        unsafe {
            assert!(ll_release(x as *mut RcHeader));
            crate::object::ll_object_die(x);
            // The dispose's exit picked up and drained the ring.
        }
        assert_eq!(
            outstanding_verdicts(),
            0,
            "the ring drained at the dispose's exit"
        );
        assert_eq!(RING_DESTRUCTS.load(Ordering::Relaxed), 2);
        assert_eq!(
            GOT.load(Ordering::Relaxed),
            0,
            "the drain destructor's get() read a nulled cell, never the corpse"
        );
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 1, "X torn exactly once");

        crate::memory::deferred_free::end_epoch();
        checkpoint(); // flush

        // Exactly-once through the free list, as in DC0.
        let a = unsafe { new_constructed(&mut ctx, target_cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, target_cls, MemoryCategory::GcHeap) };
        assert_eq!(a as usize, x_addr, "LIFO: X's slot back in circulation");
        assert_ne!(b as usize, x_addr, "and was enqueued exactly once");
        unsafe {
            for &e in &[a, b] {
                assert!(ll_release(e as *mut RcHeader));
                crate::object::ll_object_die(e);
            }
            assert!(ll_release(cell as *mut RcHeader));
            crate::object::ll_entity_die(cell as *mut RcHeader);
        }
        arena.reset(|_| {});
    }

    /// Finding F8: an allocation inside a draining destructor is a
    /// checkpoint inside the drain — it must serve memory and never pick
    /// up the next message. The second message drains only after the
    /// first completes, in the outer loop.
    #[test]
    fn the_drain_is_not_reentrant() {
        let _g = crate::memory::block_pool::test_guard();
        static SEEN_OUTSTANDING_INSIDE: AtomicUsize = AtomicUsize::new(usize::MAX);
        static PEER_DESTRUCTS_INSIDE: AtomicUsize = AtomicUsize::new(usize::MAX);

        unsafe extern "C" fn allocating_destructor(_obj: *mut Object) {
            DESTRUCTS.fetch_add(1, Ordering::Relaxed);
            // Memory is served mid-drain (the factory itself carries
            // no checkpoint since the death-branch move, 2026-07-27),
            // and a checkpoint firing here — the compiler's poll — must
            // not pick up the pending second message.
            let p = unsafe { crate::memory::heap::entity_alloc(24) };
            assert!(!p.is_null(), "the nested entry still serves memory");
            unsafe { crate::memory::stdapi::ll_free(p) };
            checkpoint();
            SEEN_OUTSTANDING_INSIDE.store(outstanding_verdicts(), Ordering::Relaxed);
            PEER_DESTRUCTS_INSIDE.store(DESTRUCTS.load(Ordering::Relaxed), Ordering::Relaxed);
        }

        DESTRUCTS.store(0, Ordering::Relaxed);
        let alloc_cls = ClassBuilder::new("EpochNestedAllocator")
            .prop("child", true)
            .destructor(allocating_destructor as *const ())
            .build();
        let plain_cls = ClassBuilder::new("EpochNestedPeer")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let mk =
            |ctx: &mut LLContext, cls| unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
        // Two independent condemned rings, one message each.
        let (a1, a2) = (mk(&mut ctx, alloc_cls), mk(&mut ctx, alloc_cls));
        let (b1, b2) = (mk(&mut ctx, plain_cls), mk(&mut ctx, plain_cls));
        unsafe {
            tie(a1, 16, a2);
            tie(a2, 16, a1);
            tie(b1, 16, b2);
            tie(b2, 16, b1);
        }

        post_confirmation(vec![a1 as *mut RcHeader, a2 as *mut RcHeader]);
        post_confirmation(vec![b1 as *mut RcHeader, b2 as *mut RcHeader]);
        checkpoint();

        // Both messages still count as outstanding inside the first
        // drain: a message acks only when its drain completes, so the
        // nested checkpoint saw 2 — its own, undecremented, plus the
        // second it must not touch.
        assert_eq!(SEEN_OUTSTANDING_INSIDE.load(Ordering::Relaxed), 2);
        assert_eq!(
            PEER_DESTRUCTS_INSIDE.load(Ordering::Relaxed),
            2,
            "only the first ring's destructors had run at that point"
        );
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            4,
            "outer loop drained the rest"
        );
        assert_eq!(outstanding_verdicts(), 0);
        let seen = walked_addresses();
        for &e in &[a1, a2, b1, b2] {
            assert!(!seen.contains(&(e as usize)));
        }
        arena.reset(|_| {});
    }
}
