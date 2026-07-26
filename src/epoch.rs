//! The mutator side of the rc-walk epoch protocol: the soft-handshake
//! ack, the verdict message queue, and the checkpoint that serves both
//! (`rfc/model/gc/rc-walk.md`, Phases 3–4). The collector never frees
//! anything itself — every verdict, confirmation or acquittal, ends in
//! exactly one message drained here, on the owning mutator thread,
//! race-free.
//!
//! **Checkpoints ride the memory manager, not a compiler-inserted
//! poll**: the test lives at the end of `entity_alloc` (the factory
//! allocation path) and in `ll_gc_maybe_collect` (the existing poll the
//! reserve already refills at). The raw `ll_malloc` C-ABI path is
//! deliberately not a checkpoint — it is the benchmarked hot path, and
//! a buffer-only workload starves the epoch no worse than the
//! accepted no-allocation limit (finding F2: once a message is posted,
//! the epoch waits for the thread's next checkpoint; deliberately no
//! fallback).
//!
//! **The drain is not re-entrant** (finding F8): a destructor run by
//! the drain may allocate, and that allocation is a checkpoint inside
//! the drain. One thread-local bit closes the recursion — the nested
//! entry serves memory and acks a pending handshake, but never picks up
//! a message.
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

/// What the collector concluded about one component.
// Constructed by the collector thread (commit 4); tests drive it until then.
#[cfg_attr(not(test), expect(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Phase 3 confirmed: drain verifies (exact test) and frees.
    Confirm,
    /// Phase 3 acquitted: drain clears the bytes and tears the deaths
    /// that were deferred while condemned.
    Acquit,
}

/// One posted component. Raw entity pointers cross from the collector
/// thread back to the mutator that owns them — the drain runs where the
/// entities live.
struct VerdictMessage {
    verdict: Verdict,
    members: Vec<*mut RcHeader>,
}
// Safety: the pointers are only ever dereferenced by the owning mutator
// thread, in the drain; the collector treats them as opaque ids.
unsafe impl Send for VerdictMessage {}

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
static QUEUE: Mutex<VecDeque<VerdictMessage>> = Mutex::new(VecDeque::new());

thread_local! {
    /// The one bit of allocator state that closes the drain recursion.
    static MID_DRAIN: Cell<bool> = const { Cell::new(false) };
}

/// The checkpoint test: two relaxed loads and a predicted branch, taken
/// only when the collector wants attention. Callers are the allocation
/// paths named in the module doc.
#[inline]
pub(crate) fn checkpoint() {
    if HANDSHAKE_REQUESTED.load(Ordering::Relaxed)
        || OUTSTANDING_VERDICTS.load(Ordering::Relaxed) != 0
    {
        checkpoint_attend();
    }
}

#[cold]
#[inline(never)]
fn checkpoint_attend() {
    if HANDSHAKE_REQUESTED.swap(false, Ordering::AcqRel) {
        HANDSHAKE_ACKS.fetch_add(1, Ordering::AcqRel);
    }
    // Nested entry (an allocation inside a draining destructor): memory
    // served, handshake acked, message left where it is.
    if MID_DRAIN.with(|d| d.get()) {
        return;
    }
    MID_DRAIN.with(|d| d.set(true));
    loop {
        let message = QUEUE.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
        let Some(message) = message else { break };
        match message.verdict {
            // Safety: the message names entities this thread owns; the
            // drain functions uphold their own contracts.
            Verdict::Confirm => unsafe {
                walk::drain_confirmed(&message.members);
            },
            Verdict::Acquit => unsafe {
                walk::acquit_condemned(&message.members);
            },
        }
        // The ack: released so a collector seeing zero (Acquire) also
        // sees every effect of the drain.
        OUTSTANDING_VERDICTS.fetch_sub(1, Ordering::Release);
    }
    MID_DRAIN.with(|d| d.set(false));
}

// --- Collector-side surface (the collector thread of commit 4; tests
// drive it directly until then) ---------------------------------------------

/// Raise the handshake flag. The collector then waits for
/// [`handshake_acks`] to move past its snapshot.
#[cfg_attr(not(test), expect(dead_code))]
pub(crate) fn request_handshake() {
    HANDSHAKE_REQUESTED.store(true, Ordering::Release);
}

/// Monotonic handshake ack count (`Acquire`: pairs with the ack's
/// release bump — mutator writes before the checkpoint are visible).
#[cfg_attr(not(test), expect(dead_code))]
pub(crate) fn handshake_acks() -> u64 {
    HANDSHAKE_ACKS.load(Ordering::Acquire)
}

/// Post one component's verdict to the owning mutator. The outstanding
/// count moves **before** the message becomes visible, so a concurrent
/// drain can never drive the counter below zero.
#[cfg_attr(not(test), expect(dead_code))]
pub(crate) fn post_verdict(verdict: Verdict, members: Vec<*mut RcHeader>) {
    OUTSTANDING_VERDICTS.fetch_add(1, Ordering::Relaxed);
    QUEUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_back(VerdictMessage { verdict, members });
}

/// Verdicts posted and not yet drained. Zero (with `Acquire`) is the
/// collector's licence to end the epoch: flush, retire blocks, walk
/// again.
#[cfg_attr(not(test), expect(dead_code))]
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
    use crate::refcount::{CONDEMNED_BYTE_SHIFT, MemoryCategory, ll_release, ll_retain};
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

    unsafe fn condemn(e: *mut RcHeader) {
        unsafe { (*e).flags |= 1 << CONDEMNED_BYTE_SHIFT };
    }

    static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
        DESTRUCTS.fetch_add(1, Ordering::Relaxed);
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
            condemn(a as *mut RcHeader);
            condemn(b as *mut RcHeader);
        }

        post_verdict(Verdict::Confirm, vec![a as *mut RcHeader, b as *mut RcHeader]);
        assert_eq!(outstanding_verdicts(), 1);
        checkpoint();
        assert_eq!(outstanding_verdicts(), 0, "the drain acked the message");
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
        arena.reset(|_| {});
    }

    /// A false post dies at the exact test: message dropped whole, no
    /// destructor, bytes cleared (the acquittal duty), ring intact.
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
            condemn(a as *mut RcHeader);
            condemn(b as *mut RcHeader);
        }

        post_verdict(Verdict::Confirm, vec![a as *mut RcHeader, b as *mut RcHeader]);
        checkpoint();
        assert_eq!(outstanding_verdicts(), 0);
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 0, "no destructor on a live ring");
        let seen = walked_addresses();
        assert!(seen.contains(&(a as usize)) && seen.contains(&(b as usize)));
        unsafe {
            use crate::refcount::CONDEMNED_BYTE_MASK;
            assert_eq!((*a).rc.flags & CONDEMNED_BYTE_MASK, 0, "byte cleared on the drop");
            assert_eq!((*b).rc.flags & CONDEMNED_BYTE_MASK, 0);
        }

        // The ring is genuinely garbage once the frame lets go.
        assert!(!unsafe { ll_release(a as *mut RcHeader) });
        unsafe { crate::walk::collect_cycles() };
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
        arena.reset(|_| {});
    }

    /// DC0's rule side (`rfc/model/gc/rc-walk-danger-cases.md`): a
    /// release reaching zero on a condemned entity defers its death, and
    /// the acquittal message tears it down exactly once — while a live
    /// condemned peer just gets its byte cleared.
    #[test]
    fn an_acquittal_tears_deferred_deaths_and_clears_the_rest() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("EpochAcquitted")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let dead = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let live = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let dead_addr = dead as usize;
        unsafe {
            condemn(dead as *mut RcHeader);
            condemn(live as *mut RcHeader);
            // The last reference goes away while condemned: death is
            // deferred — reported as no-death, count zero, destructor
            // still owed (the F5 rule, pinned in refcount.rs tests too).
            assert!(!ll_release(dead as *mut RcHeader), "condemned: no ordinary death");
            assert_eq!((*dead).rc.refcount, 0);
        }
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 0, "destructor deferred with the death");

        post_verdict(
            Verdict::Acquit,
            vec![dead as *mut RcHeader, live as *mut RcHeader],
        );
        checkpoint();
        assert_eq!(outstanding_verdicts(), 0);
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 1, "the deferred death ran exactly once");
        let seen = walked_addresses();
        assert!(!seen.contains(&dead_addr), "torn down and freed");
        assert!(seen.contains(&(live as usize)), "the live peer survives");
        unsafe {
            use crate::refcount::CONDEMNED_BYTE_MASK;
            assert_eq!((*live).rc.flags & CONDEMNED_BYTE_MASK, 0, "duty: byte cleared");
            assert!(ll_release(live as *mut RcHeader), "byte clear: ordinary death again");
            crate::object::ll_object_die(live);
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
            // This allocation is a checkpoint inside the drain (the
            // entity_alloc tail calls it). The pending second message
            // must still be pending afterwards.
            let p = unsafe { crate::memory::heap::entity_alloc(24) };
            assert!(!p.is_null(), "the nested entry still serves memory");
            unsafe { crate::memory::stdapi::ll_free(p) };
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
        let mk = |ctx: &mut LLContext, cls| unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
        // Two independent condemned rings, one message each.
        let (a1, a2) = (mk(&mut ctx, alloc_cls), mk(&mut ctx, alloc_cls));
        let (b1, b2) = (mk(&mut ctx, plain_cls), mk(&mut ctx, plain_cls));
        unsafe {
            tie(a1, 16, a2);
            tie(a2, 16, a1);
            tie(b1, 16, b2);
            tie(b2, 16, b1);
            for &e in &[a1, a2, b1, b2] {
                condemn(e as *mut RcHeader);
            }
        }

        post_verdict(Verdict::Confirm, vec![a1 as *mut RcHeader, a2 as *mut RcHeader]);
        post_verdict(Verdict::Confirm, vec![b1 as *mut RcHeader, b2 as *mut RcHeader]);
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
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 4, "outer loop drained the rest");
        assert_eq!(outstanding_verdicts(), 0);
        let seen = walked_addresses();
        for &e in &[a1, a2, b1, b2] {
            assert!(!seen.contains(&(e as usize)));
        }
        arena.reset(|_| {});
    }
}
