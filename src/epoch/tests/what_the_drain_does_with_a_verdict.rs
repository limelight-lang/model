//! A confirmed component is exact-tested, destructed, severed and
//! freed at the checkpoint, and the ack is what lets the epoch end.
//! A false post is dropped whole, and so is one whose members have
//! died on their own since the posting: acquittal has carried no
//! duties since eager death.

use super::*;

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
