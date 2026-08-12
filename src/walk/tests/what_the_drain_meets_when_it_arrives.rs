//! A destructor that stores `$this` somewhere lasting gives its
//! member `RC > IN` beyond the guard, so the re-verify acquits the
//! whole component and the destructor is never run again. A child
//! held from outside survives the ring and loses exactly the ring's
//! one reference, through the deferred drop that runs after the
//! members are freed. A member reading refcount 0 died ordinarily
//! since the verdict was posted, so the message is dropped whole
//! before any field is traced or guard written.

use super::*;

/// A destructor that stores `$this` somewhere lasting gives the
/// member `RC > IN` beyond the guard: the re-verify acquits the whole
/// component, survivors keep true counts, and the destructor is
/// never run again — the ring dies silently once the resurrection
/// reference is dropped.
#[test]
fn a_resurrecting_destructor_acquits_and_never_reruns() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let lazarus_cls = ClassBuilder::new("WalkLazarus")
        .prop("child", true)
        .destructor(resurrecting_destructor as *const ())
        .build();
    let plain_cls = ClassBuilder::new("WalkLazarusPeer")
        .prop("child", true)
        .destructor(counting_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let a = unsafe { new_constructed(&mut ctx, lazarus_cls, MemoryCategory::GcHeap) };
    let b = unsafe { new_constructed(&mut ctx, plain_cls, MemoryCategory::GcHeap) };
    unsafe {
        tie(a, 16, b);
        tie(b, 16, a);
    }

    let stats = unsafe { collect_cycles() };
    assert_eq!(stats.collected, 0, "resurrection must acquit the component");
    assert!(stats.acquitted >= 1);
    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        2,
        "both destructors did run"
    );
    assert_eq!(RESURRECTED.load(Ordering::Relaxed), a as usize);
    let seen = walked_addresses();
    assert!(seen.contains(&(a as usize)) && seen.contains(&(b as usize)));
    assert_eq!(
        unsafe { (*a).rc.refcount },
        2,
        "slot + resurrection, guards off"
    );

    // The lasting reference goes away; the ring is garbage again, its
    // destructors already behind it.
    assert!(!unsafe { ll_release(a as *mut RcHeader) });
    unsafe { collect_cycles() };
    let seen = walked_addresses();
    assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        2,
        "__destruct exactly once per object"
    );
    arena.reset(|_| {});
}

/// A ring member holding a child that is ALSO externally held: the
/// child is live (a computed root keeps it marked), survives the
/// ring's death, and loses exactly the ring's one reference — through
/// the deferred external drop that runs only after the members are
/// freed.
#[test]
fn a_live_external_child_survives_the_ring_and_loses_one_reference() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("RingWithTenant")
        .prop("child", true)
        .prop("link", true)
        .destructor(counting_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let v = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe {
        tie(a, 16, b);
        tie(b, 16, a);
        crate::refcount::ll_retain(v as *mut RcHeader); // the live frame slot
        tie(a, 32, v); // the ring's reference
    }

    unsafe { collect_cycles() };
    let seen = walked_addresses();
    assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
    assert!(
        seen.contains(&(v as usize)),
        "externally held: must survive"
    );
    assert_eq!(
        unsafe { (*v).rc.refcount },
        1,
        "the ring's reference was dropped"
    );
    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        2,
        "only the ring destructed"
    );

    unsafe {
        assert!(ll_release(v as *mut RcHeader));
        crate::object::ll_object_die(v);
    }

    arena.reset(|_| {});
}

/// The corpse rule (eager-death amendment, 2026-07-27): the drain
/// header-scans before it trusts — any member reading `rc 0` died
/// ordinarily since the verdict was posted, and the message is
/// dropped whole before any field is traced or guard written. No
/// second teardown, no destructor re-run, and the live peer is
/// untouched.
#[cfg(feature = "rc-walk")]
#[test]
fn a_corpse_in_a_posted_component_drops_the_message_whole() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn counting(_o: *mut crate::object::Object) {
        DESTRUCTS.fetch_add(1, Ordering::Relaxed);
    }

    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("PostedCorpse")
        .destructor(counting as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let peer = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let ptr = obj as *mut RcHeader;

    // Epoch active: the ordinary death parks the slot instead of
    // recycling it — identity holds from walk to drain.
    crate::memory::deferred_free::begin_epoch();
    assert!(unsafe { ll_release(ptr) });
    unsafe { crate::object::ll_object_die(obj) };
    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        1,
        "died ordinarily, eagerly, once"
    );

    // The stale message arrives naming the corpse and a live peer.
    let outcome = unsafe { drain_confirmed(&[ptr, peer as *mut RcHeader]) };
    assert!(outcome.acquitted, "the corpse drops the message whole");
    assert_eq!(outcome.collected, 0, "nothing is torn by a drop");
    assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 1, "no destructor re-run");
    assert_eq!(
        unsafe { crate::refcount::header_refcount(peer as *mut RcHeader) },
        1,
        "the live peer is untouched — no guard was written"
    );

    crate::memory::deferred_free::end_epoch();
    assert!(unsafe { crate::memory::deferred_free::flush() } >= 1);
    unsafe {
        assert!(ll_release(peer as *mut RcHeader));
        crate::object::ll_object_die(peer);
    }

    arena.reset(|_| {});
}
