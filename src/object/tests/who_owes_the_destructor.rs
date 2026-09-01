//! The completed user constructor registers the record, not the
//! factory: an object that never got past the factory is in no
//! destructor log and runs no `__destruct` at the reset. A refused
//! record puts the object in that same state by the code's other
//! door, and fails the construction saying so.

use super::*;

#[test]
fn arena_object_with_destructor_is_tracked_and_reset_delivers_it() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("WithDtor")
        .destructor(counting_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };
    assert_ne!(
        unsafe { crate::refcount::entity_flags(obj) } & DESTRUCTOR_PENDING,
        0
    );

    let mut delivered = Vec::new();
    arena.reset(|o| delivered.push(o));
    assert_eq!(delivered, vec![obj as *mut RcHeader]);
}

/// The factory does not owe a `__destruct`; the completed user
/// constructor does. An object that never got past the factory —
/// because `__construct` threw, or because registering the record was
/// refused — must not appear in the arena's destructor log and must
/// not run its `__destruct` on teardown.
#[test]
fn an_unconstructed_object_owes_no_destructor() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("ThrewInCtor")
        .destructor(counting_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    // The factory alone: no `object_constructed` call, as for a
    // constructor that raised.
    let obj = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::RequestArena) };
    assert_eq!(
        unsafe { crate::refcount::entity_flags(obj) } & DESTRUCTOR_PENDING,
        0
    );

    let mut delivered = Vec::new();
    arena.reset(|o| delivered.push(o));
    assert!(delivered.is_empty(), "nothing was registered");
    assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 0, "and nothing ran");

    // Same rule on the refcounted path, where teardown dispatches on
    // the header rather than on a log: a heap object that never
    // completed construction dies without its `__destruct`.
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let heap_obj = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::GcHeap) };
    // Through the count first, as generated code would: `ll_release`
    // reports the death, and the caller performs the teardown.
    assert!(unsafe { crate::refcount::ll_release(heap_obj as *mut RcHeader) });
    unsafe { ll_object_die(heap_obj) };
    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        0,
        "teardown must dispatch on the object's own flag, not on the class"
    );
    arena.reset(|_| {});
}

/// The other door into that state: the arena's destructor log could
/// not take the record, so `object_constructed` reports false and the
/// object owes nothing. The caller raises memory-exhausted at the
/// creation site and the outcome is a constructor that threw, which
/// is why the flag must be clear and the reset must stay silent —
/// a `__destruct` on an object whose construction failed would run
/// over half-initialised properties.
///
/// **Which allocation is refused matters here**, `FORCE_OOM` naming
/// one allocator and not one call: the object is allocated before the
/// forcing starts, the arena's block is then spent to its last byte,
/// and the reserve behind it is emptied — so the only allocation left
/// inside the call is the log segment's.
///
/// Three conditions are held at once by then, and a refusal keyed on
/// the wrong one of the three looks exactly like this. So the same
/// call is made a second time with the block still spent and the
/// reserve still empty, and only the pool's answer changed: it must
/// succeed. A `track_destructor` that refused on the arena's own
/// remaining bytes — which would fail every destructor-carrying
/// construction in the last kilobytes of each block — passes the first
/// half of this test and fails the second.
#[test]
fn a_refused_destructor_record_fails_the_construction() {
    let _g = crate::memory::block_pool::test_guard();
    use crate::memory::block_pool::force_oom;
    let cls = ClassBuilder::new("RecordRefused")
        .destructor(counting_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let obj = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::RequestArena) };
    assert!(
        !obj.is_null(),
        "the object itself was served, before forcing"
    );

    // Spend the rest of the arena's block, so the log's growth has to
    // ask the pool rather than the bump it is sitting on.
    let rest = unsafe { (*ctx.arena).remaining() };
    assert!(!unsafe { (*ctx.arena).alloc(rest) }.is_null());
    assert_eq!(unsafe { (*ctx.arena).remaining() }, 0);
    crate::memory::reserve::drain_for_test();

    let oom = force_oom();
    let registered = unsafe { object_constructed(&mut ctx, obj) };
    drop(oom);

    assert!(!registered, "the record could not be written");
    assert_eq!(
        unsafe { crate::refcount::entity_flags(obj) } & DESTRUCTOR_PENDING,
        0,
        "so the object owes no destructor"
    );

    // The same call, the same spent block, the same empty reserve —
    // the pool serving again is the only difference.
    assert!(
        unsafe { object_constructed(&mut ctx, obj) },
        "the refusal was the pool's, not the arena's remaining bytes"
    );
    assert_ne!(
        unsafe { crate::refcount::entity_flags(obj) } & DESTRUCTOR_PENDING,
        0,
        "and now the object owes one"
    );

    // Exactly one record, which says both halves: the refused call
    // wrote nothing, and the call that succeeded wrote once.
    let mut delivered = Vec::new();
    arena.reset(|o| delivered.push(o));
    assert_eq!(delivered, vec![obj as *mut RcHeader]);
    assert!(crate::memory::reserve::replenish());
}
