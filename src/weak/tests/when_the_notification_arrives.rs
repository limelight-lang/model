//! The dying object's own `__destruct` still resolves it while every
//! later reader sees null, which places the notification at the
//! first act of phase 2. A cyclic death nulls the cell before any
//! destructor runs, and each collector reaches that site by a route
//! of its own; a resurrection never reaches the site at all, since
//! `dispose` reports the resurrection above it.

use super::*;

static SEEN_BY_OWN_DESTRUCTOR: AtomicUsize = AtomicUsize::new(0);

static SEEN_BY_CHILD_DESTRUCTOR: AtomicUsize = AtomicUsize::new(usize::MAX);

static RESURRECTED_INTO: AtomicUsize = AtomicUsize::new(0);

static PROBE_CELL: AtomicUsize = AtomicUsize::new(0);

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
        assert_ne!(
            unsafe { crate::refcount::entity_flags(obj) } & DESTRUCTOR_RAN,
            0
        );
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
