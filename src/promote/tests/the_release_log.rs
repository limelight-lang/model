//! A heap entity stored into an arena container is released by the
//! reset, so a survivor holding one has to compensate that log
//! entry, while a holder that dies takes its child down with it at
//! teardown. Overwriting the last reference to a heap object tears
//! it down at the store rather than leaking it.

use super::*;

#[test]
fn survivor_holding_heap_entity_compensates_the_release_log() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("Keeper").prop("cfg", true).build();
    let holder_cls = ClassBuilder::new("Slot").prop("v", true).build();
    let cfg_cls = ClassBuilder::new("Config").build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
    let cfg = unsafe { new_constructed(&mut ctx, cfg_cls, MemoryCategory::GcHeap) };
    let keeper = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };

    unsafe {
        // Heap entity into an arena container: retain + release log.
        store_prop(&mut arena, keeper, 16, cfg);
        assert_eq!((*cfg).rc.refcount, 2);
        // The keeper escapes.
        store_prop(&mut arena, holder, 16, keeper);
        arena_reset_full(&mut arena);
    }

    // Log's -1 and the survivor compensation +1 cancel out: the
    // keeper legitimately holds cfg.
    assert_eq!(unsafe { (*cfg).rc.refcount }, 2);

    // Keeper dies for real, and it dies **through its holder**: the
    // `Slot` object's property is the reference keeping it alive, so
    // releasing behind the holder's back leaves a live object naming
    // freed memory. Only block reuse makes that visible — a freed
    // slot nobody reissues still reads refcount 0, which is what
    // makes the dangling property look harmless.
    unsafe {
        let slot = Object::prop_at(holder, 16);
        assert!(crate::memory::barrier::ref_store(
            &mut arena,
            holder as *mut RcHeader,
            slot,
            keeper as *mut RcHeader,
            Value::null(),
        ));
    }

    assert_eq!(
        unsafe { (*cfg).rc.refcount },
        1,
        "exactly one release at real death"
    );
    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
}

#[test]
fn heap_entity_of_a_dying_holder_dies_with_teardown() {
    let _g = crate::memory::block_pool::test_guard();
    static CFG_DTORS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn cfg_dtor(_o: *mut Object) {
        CFG_DTORS.fetch_add(1, Ordering::Relaxed);
    }

    let cfg_cls = ClassBuilder::new("DoomedCfg")
        .destructor(cfg_dtor as *const ())
        .build();
    let tmp_cls = ClassBuilder::new("Tmp").prop("cfg", true).build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let cfg = unsafe { new_constructed(&mut ctx, cfg_cls, MemoryCategory::GcHeap) };
    let tmp = unsafe { new_constructed(&mut ctx, tmp_cls, MemoryCategory::RequestArena) };

    unsafe {
        store_prop(&mut arena, tmp, 16, cfg);
        // The test's own reference goes away: the arena holds the last one.
        assert!(!crate::refcount::ll_release(cfg as *mut RcHeader));
        arena_reset_full(&mut arena);
    }

    assert_eq!(
        CFG_DTORS.load(Ordering::Relaxed),
        1,
        "release log's last release must run real teardown"
    );
}

/// Overwriting a slot that held the last reference to a heap object
/// tears that object down (destructor + children + free), rather than
/// leaking it — the store barrier's displaced-value path.
#[test]
fn overwriting_the_last_reference_tears_down_the_displaced_object() {
    let _g = crate::memory::block_pool::test_guard();
    static DTORS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn dtor(_o: *mut Object) {
        DTORS.fetch_add(1, Ordering::Relaxed);
    }

    let val_cls = ClassBuilder::new("Val")
        .destructor(dtor as *const ())
        .build();
    let holder_cls = ClassBuilder::new("Holder").prop("x", true).build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let owner = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
    let a = unsafe { new_constructed(&mut ctx, val_cls, MemoryCategory::GcHeap) };
    let b = unsafe { new_constructed(&mut ctx, val_cls, MemoryCategory::GcHeap) };

    unsafe {
        store_prop(&mut arena, owner, 16, a); // owner->x = a (a.rc 2)
        assert!(!crate::refcount::ll_release(a as *mut RcHeader)); // a.rc 1 (the slot)

        // Overwrite: A's last reference (the slot) goes away → A dies and
        // its destructor runs. The old code released A but never tore it
        // down.
        store_prop(&mut arena, owner, 16, b);
        assert_eq!(
            DTORS.load(Ordering::Relaxed),
            1,
            "displaced A was torn down"
        );

        // cleanup: owner death releases b's slot reference (b.rc 2 → 1),
        // then drop b's creator reference.
        assert!(crate::refcount::ll_release(owner as *mut RcHeader));
        ll_object_die(owner);
        assert!(crate::refcount::ll_release(b as *mut RcHeader));
        ll_object_die(b);
    }
}
