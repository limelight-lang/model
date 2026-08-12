//! The dying object's own `__destruct` still resolves it while every
//! later reader sees null, which places the notification at the
//! first act of phase 2. A cyclic death nulls the cell before any
//! destructor runs, and each collector reaches that site by a route
//! of its own; a resurrection never reaches the site at all, since
//! `dispose` reports the resurrection above it.

use super::*;

unsafe extern "C" fn resurrecting_destructor(obj: *mut Object) {
    unsafe { crate::refcount::ll_retain(obj as *mut RcHeader) };
    RESURRECTED_INTO.store(obj as usize, Ordering::Relaxed);
}

/// The dying object's own `__destruct`: `get()` must still produce it.
unsafe extern "C" fn probing_own_destructor(_obj: *mut Object) {
    let cell = PROBE_CELL.load(Ordering::Relaxed) as *mut LLWeakRef;
    let got = unsafe { ll_weakref_get(cell) };
    SEEN_BY_OWN_DESTRUCTOR.store(got as usize, Ordering::Relaxed);
    if !got.is_null() {
        assert!(
            !unsafe { ll_release(got) },
            "the object is alive mid-destructor"
        );
    }
}

/// A child's `__destruct`, running inside the parent's phase 2:
/// `get()` on the parent must already read null (the wrong order is
/// a use-after-free — `rfc/runtime/object-lifecycle.md`, phase 2).
unsafe extern "C" fn probing_child_destructor(_obj: *mut Object) {
    let cell = PROBE_CELL.load(Ordering::Relaxed) as *mut LLWeakRef;
    SEEN_BY_CHILD_DESTRUCTOR.store(unsafe { ll_weakref_get(cell) } as usize, Ordering::Relaxed);
}

unsafe extern "C" fn cycle_probing_destructor(_obj: *mut Object) {
    let cell = PROBE_CELL.load(Ordering::Relaxed) as *mut LLWeakRef;
    CYCLE_DESTRUCTOR_SAW.store(unsafe { ll_weakref_get(cell) } as usize, Ordering::Relaxed);
}

static SEEN_BY_OWN_DESTRUCTOR: AtomicUsize = AtomicUsize::new(0);

static SEEN_BY_CHILD_DESTRUCTOR: AtomicUsize = AtomicUsize::new(usize::MAX);

static RESURRECTED_INTO: AtomicUsize = AtomicUsize::new(0);

static PROBE_CELL: AtomicUsize = AtomicUsize::new(0);

static CYCLE_DESTRUCTOR_SAW: AtomicUsize = AtomicUsize::new(usize::MAX);

#[test]
fn own_destructor_still_sees_the_object_but_a_child_destructor_sees_null() {
    let _g = crate::memory::block_pool::test_guard();
    let child_cls = ClassBuilder::new("WeakProbeChild")
        .destructor(probing_child_destructor as *const ())
        .build();
    let parent_cls = ClassBuilder::new("WeakProbeParent")
        .prop("child", true)
        .destructor(probing_own_destructor as *const ())
        .build();

    with_ctx(|ctx| {
        let child = unsafe { new_constructed(ctx, child_cls, MemoryCategory::GcHeap) };
        let parent = unsafe { new_constructed(ctx, parent_cls, MemoryCategory::GcHeap) };
        unsafe {
            Object::prop_at(parent, 16).write(Value::entity(Tag::Object, child as *mut RcHeader));
        }

        let w = unsafe { ll_weakref_create(ctx, parent as *mut RcHeader) };
        PROBE_CELL.store(w as usize, Ordering::Relaxed);
        SEEN_BY_OWN_DESTRUCTOR.store(0, Ordering::Relaxed);
        SEEN_BY_CHILD_DESTRUCTOR.store(usize::MAX, Ordering::Relaxed);

        assert!(unsafe { ll_release(parent as *mut RcHeader) });
        unsafe { crate::object::ll_object_die(parent) };

        assert_eq!(
            SEEN_BY_OWN_DESTRUCTOR.load(Ordering::Relaxed),
            parent as usize,
            "phase 1: the object's own __destruct still resolves itself"
        );
        assert_eq!(
            SEEN_BY_CHILD_DESTRUCTOR.load(Ordering::Relaxed),
            0,
            "phase 2: a cascading child __destruct must read null — \
             anything else is a strong reference to a freed object"
        );

        unsafe {
            assert!(ll_release(w as *mut RcHeader));
            crate::object::ll_entity_die(w as *mut RcHeader);
        }
    });
}

#[test]
fn cyclic_death_nulls_the_cell_before_any_destructor_runs() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("WeakRing")
        .prop("next", true)
        .destructor(cycle_probing_destructor as *const ())
        .build();

    with_ctx(|ctx| {
        let a = unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
        // Raw slot writes: each slot takes over the object's initial
        // reference, so the ring is pure garbage held only by itself.
        unsafe {
            Object::prop_at(a, 16).write(Value::entity(Tag::Object, b as *mut RcHeader));
            Object::prop_at(b, 16).write(Value::entity(Tag::Object, a as *mut RcHeader));
        }

        let w = unsafe { ll_weakref_create(ctx, a as *mut RcHeader) };
        PROBE_CELL.store(w as usize, Ordering::Relaxed);
        CYCLE_DESTRUCTOR_SAW.store(usize::MAX, Ordering::Relaxed);

        let stats = unsafe { crate::walk::collect_cycles() };
        assert_eq!(stats.collected, 2, "the ring was collected");
        assert_eq!(
            CYCLE_DESTRUCTOR_SAW.load(Ordering::Relaxed),
            0,
            "a member's __destruct must not be able to fish a condemned \
             sibling out of a weak cell (the PEP 442 obligation)"
        );
        assert!(unsafe { ll_weakref_get(w) }.is_null());

        unsafe {
            assert!(ll_release(w as *mut RcHeader));
            crate::object::ll_entity_die(w as *mut RcHeader);
        }
    });
}

/// rc-trace frees its white set raw (no dispose), so its weak pass is
/// a separate site from the walk drain and needs its own proof.
#[cfg(not(feature = "rc-walk"))]
#[test]
fn rc_trace_cycle_collection_nulls_the_cell() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("WeakTraceRing")
        .prop("next", true)
        .build();

    /// Real store through the barrier: retain + whole-value write.
    unsafe fn link(arena: *mut Arena, from: *mut Object, to: *mut Object) {
        unsafe {
            assert!(
                crate::memory::barrier::ref_store(
                    arena,
                    from as *mut RcHeader,
                    Object::prop_at(from, 16),
                    std::ptr::null_mut(),
                    Value::entity(Tag::Object, to as *mut RcHeader),
                ),
                "the barrier refused the link this test is built on"
            );
        }
    }

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    // Barrier stores (retain + slot write), then the variable
    // references die: the decrement-to-nonzero buffers the
    // candidates, exactly as generated code would.
    unsafe {
        let a = new_constructed(&mut ctx, cls, MemoryCategory::GcHeap);
        let b = new_constructed(&mut ctx, cls, MemoryCategory::GcHeap);
        link(&mut arena, a, b);
        link(&mut arena, b, a);
        let w = ll_weakref_create(&mut ctx, a as *mut RcHeader);

        assert!(!ll_release(a as *mut RcHeader));
        assert!(!ll_release(b as *mut RcHeader));
        assert_eq!(crate::gc::collect_cycles(), 2, "the ring was collected");
        assert!(ll_weakref_get(w).is_null());

        assert!(ll_release(w as *mut RcHeader));
        crate::object::ll_entity_die(w as *mut RcHeader);
    }

    arena.reset(|_| {});
}

#[test]
fn a_resurrected_object_keeps_its_weak_state() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("WeakLazarus")
        .destructor(resurrecting_destructor as *const ())
        .build();
    with_ctx(|ctx| {
        let obj = unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
        let w = unsafe { ll_weakref_create(ctx, obj as *mut RcHeader) };

        assert!(unsafe { ll_release(obj as *mut RcHeader) });
        unsafe { crate::object::ll_object_die(obj) };
        assert_ne!(unsafe { (*obj).rc.flags } & DESTRUCTOR_RAN, 0);
        assert_eq!(
            unsafe { ll_weakref_get(w) },
            obj as *mut RcHeader,
            "invalidation only after teardown commits: a resurrected \
             object keeps resolving"
        );
        assert!(
            !unsafe { ll_release(obj as *mut RcHeader) },
            "the get retained"
        );

        // The second, final death nulls it.
        assert!(unsafe { ll_release(obj as *mut RcHeader) });
        unsafe { crate::object::ll_object_die(obj) };
        assert!(unsafe { ll_weakref_get(w) }.is_null());

        unsafe {
            assert!(ll_release(w as *mut RcHeader));
            crate::object::ll_entity_die(w as *mut RcHeader);
        }
    });
}
