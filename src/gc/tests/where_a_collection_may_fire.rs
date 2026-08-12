//! Crossing the threshold arms a collection and never runs one
//! inline, and each of the two crossing sites has a hazard of its
//! own. Inside `ll_release` the mutation is half-done, so a
//! collection there judges a graph nobody has finished writing.
//! Inside a teardown's child releases the dying object sits at
//! refcount zero and is still a buffered root, so a collection there
//! computes it garbage and frees it under its own teardown — which
//! is why a fire point reached from inside a destructor collects
//! nothing and leaves the work for the next clean one.

use super::*;

/// The candidate buffer crossing its threshold *arms* a collection but
/// never runs it inline. Here the arming happens inside `ll_object_die`'s
/// phase 2 (a child release), the worst possible moment: on the old
/// fire-inline code that collection ran mid-teardown and freed the
/// dying object a second time. Now it only sets the pending flag, and
/// the live child survives.
#[test]
fn threshold_crossing_during_teardown_only_arms() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = node_class();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    let p = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let c = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };

    unsafe {
        // p.next = c  → c held by p's slot (rc 2) and by us (the creator
        // reference, which must keep c alive past p's death).
        link(&mut arena, p, 16, c);
        assert_eq!((*c).rc.refcount, 2);

        // Buffer p as a cycle-root candidate (a non-zero decrement),
        // still under the default threshold so nothing arms yet.
        crate::refcount::ll_retain(p as *mut RcHeader); // rc 2
        assert!(!ll_release(p as *mut RcHeader)); // rc 1, buffered
        assert!(!COLLECT_PENDING.with(|f| f.get()), "not armed yet");

        // From now the next buffered candidate crosses the threshold.
        set_test_threshold(1);

        // p's last reference dies; teardown releases c during phase 2,
        // which buffers c and crosses the threshold *mid-teardown*.
        assert!(ll_release(p as *mut RcHeader)); // rc 0 → death
        crate::object::ll_object_die(p);
        set_test_threshold(CANDIDATE_THRESHOLD);

        // The collection was armed, not fired: nothing ran inside the
        // teardown, so the still-referenced child is untouched and p was
        // freed exactly once (no crash). On the fire-inline code
        // COLLECT_PENDING is instead false here (a collection ran).
        assert!(COLLECT_PENDING.with(|f| f.get()), "armed, not fired");
        assert_eq!((*c).rc.refcount, 1, "the live child must survive");

        // Firing at a clean point reclaims nothing (c is externally held).
        assert_eq!(ll_gc_maybe_collect(), 0);
        assert!(!COLLECT_PENDING.with(|f| f.get()), "pending cleared");

        assert!(ll_release(c as *mut RcHeader));
        crate::object::ll_object_die(c);
    }

    arena.reset(|_| {});
}

/// An armed collection is deferred to a clean fire point: crossing the
/// threshold from inside `ll_release` must not collect there (that is
/// the mid-mutation hazard), only arm. The cyclic garbage stays live
/// until `ll_gc_maybe_collect` runs it at a safe point.
#[test]
fn armed_cycle_is_deferred_to_maybe_collect() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = node_class();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe {
        link(&mut arena, a, 16, a); // self-cycle: a.rc = 2
        set_test_threshold(1); // the buffering release will cross it

        // External reference dies: a non-zero decrement (a is still held
        // by its own self-edge), so it buffers a and crosses the
        // threshold from *inside* ll_release. Arm-and-defer must not
        // collect here.
        assert!(!ll_release(a as *mut RcHeader)); // a.rc 1, buffered
        set_test_threshold(CANDIDATE_THRESHOLD);

        assert!(COLLECT_PENDING.with(|f| f.get()), "armed");
        assert_eq!(
            (*a).rc.refcount,
            1,
            "cyclic garbage still live, not collected inline"
        );

        // Fire at a clean point: now the cycle is reclaimed.
        assert_eq!(ll_gc_maybe_collect(), 1);
        assert!(
            !COLLECT_PENDING.with(|f| f.get()),
            "pending cleared after fire"
        );
    }

    arena.reset(|_| {});
}

/// A fire point reached from inside a destructor collects nothing and
/// leaves the work for the next clean point. Edmond's ruling of
/// 2026-08-07 is that `ll_gc_maybe_collect` may stand inside a
/// destructor body and must return there, so the runtime enforces it
/// rather than trusting the compiler not to emit one.
///
/// What it prevents: the object under teardown is at refcount zero
/// and still a buffered root while its `dispose` releases children,
/// so a collection running there computes it garbage and frees it,
/// and the teardown that was interrupted frees it again. Seen
/// failing at the returned count, which was 2 without the guard —
/// the two objects being freed were the ones already dying.
#[test]
fn a_collection_fired_from_a_destructor_does_nothing_and_defers() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    /// `usize::MAX` until the destructor has run.
    static FIRED: AtomicUsize = AtomicUsize::new(usize::MAX);

    unsafe extern "C" fn fire_a_collection(_o: *mut Object) {
        FIRED.store(unsafe { collect_cycles() }, Ordering::Relaxed);
    }

    let _g = crate::memory::block_pool::test_guard();
    FIRED.store(usize::MAX, Ordering::Relaxed);
    let node = node_class();
    let firer = ClassBuilder::new("FiringNode")
        .prop("next", true)
        .destructor(fire_a_collection as *const ())
        .build();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = LLContext { arena: arena_ptr };

    unsafe {
        // Garbage waiting for a collection: a two-node cycle whose
        // creation references are gone. It is what the deferred
        // collection must still find afterwards.
        let d = new_constructed(&mut ctx, node, MemoryCategory::GcHeap);
        let e = new_constructed(&mut ctx, node, MemoryCategory::GcHeap);
        link(arena_ptr, d, 16, e);
        link(arena_ptr, e, 16, d);
        assert!(!ll_release(d as *mut RcHeader));
        assert!(!ll_release(e as *mut RcHeader));

        // The object that dies with a fire point inside its teardown:
        // `a` holds `c`, so `a`'s dispose drops `c` and `c`'s
        // destructor collects while `a` is a refcount-zero root.
        let a = new_constructed(&mut ctx, node, MemoryCategory::GcHeap);
        let c = new_constructed(&mut ctx, firer, MemoryCategory::GcHeap);
        link(arena_ptr, a, 16, c);
        assert!(!ll_release(c as *mut RcHeader), "a holds it");
        // A non-zero decrement, so `a` is a candidate root when it
        // dies a moment later.
        crate::refcount::ll_retain(a as *mut RcHeader);
        assert!(!ll_release(a as *mut RcHeader));
        assert!(ll_release(a as *mut RcHeader), "the last reference");
        crate::object::ll_entity_die(a as *mut RcHeader);
    }

    assert_eq!(
        FIRED.load(Ordering::Relaxed),
        0,
        "a collection fired from inside teardown must reclaim nothing"
    );
    assert_eq!(
        unsafe { collect_cycles() },
        2,
        "the refused collection deferred the work rather than losing it"
    );
    arena.reset(|_| {});
}
