//! Teardown dispatches through the class's `dispose` pointer and
//! releases every counted slot, a Box run and a bare-pointer run
//! alike. A `__destruct` that publishes `$this` again aborts the
//! teardown and is never run twice, and one that merely borrows it
//! must not re-enter: under the guard a transient release reports no
//! death.

use super::*;

static TRANSIENT_DEATHS: AtomicUsize = AtomicUsize::new(0);

static RESURRECT_INTO: AtomicUsize = AtomicUsize::new(0);

static DISPOSE_DISPATCHED: AtomicUsize = AtomicUsize::new(0);

/// `$x = $this;` then `$x` leaves scope: a transient retain + release.
/// Under the destructor guard the release must NOT report death — a
/// reported death here re-enters teardown and double-frees `obj`.
unsafe extern "C" fn transient_this_destructor(obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
    unsafe { ll_retain(obj as *mut RcHeader) };
    if unsafe { ll_release(obj as *mut RcHeader) } {
        TRANSIENT_DEATHS.fetch_add(1, Ordering::Relaxed);
    }
}

unsafe extern "C" fn resurrecting_destructor(obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
    unsafe { ll_retain(obj as *mut RcHeader) };
    RESURRECT_INTO.store(obj as usize, Ordering::Relaxed);
}

/// A stand-in for a compiler-generated specialized `dispose`: it marks
/// that the descriptor's pointer was dispatched to, then delegates the
/// real teardown to the default so the effects are unchanged.
unsafe extern "C" fn counting_dispose(obj: *mut Object) -> bool {
    DISPOSE_DISPATCHED.fetch_add(1, Ordering::Relaxed);
    unsafe { ll_default_dispose(obj) }
}

#[test]
fn die_runs_three_phases_and_cascades_to_children() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);

    let child_cls = ClassBuilder::new("Child")
        .destructor(counting_destructor as *const ())
        .build();
    let parent_cls = ClassBuilder::new("Parent")
        .prop("child", true)
        .destructor(counting_destructor as *const ())
        .build();

    with_ctx(|ctx| {
        let child = unsafe { new_constructed(ctx, child_cls, MemoryCategory::GcHeap) };
        let parent = unsafe { new_constructed(ctx, parent_cls, MemoryCategory::GcHeap) };
        unsafe {
            Object::prop_at(parent, 16).write(Value::entity(Tag::Object, child as *mut RcHeader));
        }

        // The slot owns the child's initial reference: count stays 1.

        // Parent's last reference dies.
        assert!(unsafe { ll_release(parent as *mut RcHeader) });
        unsafe { ll_object_die(parent) };

        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            2,
            "parent and child pre-destructors both ran"
        );
    });
}

/// The same cascade, but through a **bare-pointer** slot (`prop_pointer`)
/// rather than a Box — this is what exercises `for_each_counted_child`'s
/// pointer-run branch (stride 8, skip `NULL`). Without it the child's
/// release never happens and its destructor does not run.
#[test]
fn teardown_cascades_through_a_bare_pointer_slot() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);

    let child_cls = ClassBuilder::new("PtrChild")
        .destructor(counting_destructor as *const ())
        .build();
    let parent_cls = ClassBuilder::new("PtrParent")
        .prop_pointer("child")
        .destructor(counting_destructor as *const ())
        .build();

    with_ctx(|ctx| {
        let child = unsafe { new_constructed(ctx, child_cls, MemoryCategory::GcHeap) };
        let parent = unsafe { new_constructed(ctx, parent_cls, MemoryCategory::GcHeap) };
        // Store a class-typed reference into the 8-byte pointer slot at
        // +16; the slot takes over the child's initial reference (count
        // stays 1), as the Box cascade above does. The store barrier's
        // pointer form is A4 — here the raw write models generated code.
        unsafe {
            let slot = (parent as *mut u8).add(16) as *mut *mut RcHeader;
            slot.write(child as *mut RcHeader);
        }

        assert!(unsafe { ll_release(parent as *mut RcHeader) });
        unsafe { ll_object_die(parent) };

        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            2,
            "parent and its pointer-slot child both destructed"
        );
    });
}

/// Teardown dispatches through the class's `dispose` pointer, not a
/// hardcoded path: a class carrying a custom `dispose` sees it invoked,
/// and the real teardown still runs (here via delegation). This is the
/// hook A3 opens for the compiler's specialized `dispose`.
#[test]
fn teardown_dispatches_through_the_class_dispose_pointer() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    DISPOSE_DISPATCHED.store(0, Ordering::Relaxed);

    let child_cls = ClassBuilder::new("DispChild")
        .destructor(counting_destructor as *const ())
        .build();
    let parent_cls = ClassBuilder::new("DispParent")
        .prop_pointer("child")
        .destructor(counting_destructor as *const ())
        .dispose(counting_dispose as *const ())
        .build();

    with_ctx(|ctx| {
        let child = unsafe { new_constructed(ctx, child_cls, MemoryCategory::GcHeap) };
        let parent = unsafe { new_constructed(ctx, parent_cls, MemoryCategory::GcHeap) };
        unsafe {
            let slot = (parent as *mut u8).add(16) as *mut *mut RcHeader;
            slot.write(child as *mut RcHeader);
        }

        assert!(unsafe { ll_release(parent as *mut RcHeader) });
        unsafe { ll_object_die(parent) };

        assert_eq!(
            DISPOSE_DISPATCHED.load(Ordering::Relaxed),
            1,
            "teardown went through the descriptor's dispose (the parent's only)"
        );
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            2,
            "parent + child still destructed via the custom dispose"
        );
    });
}

#[test]
fn resurrection_aborts_teardown_and_destructor_never_reruns() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);

    let cls = ClassBuilder::new("Lazarus")
        .destructor(resurrecting_destructor as *const ())
        .build();

    with_ctx(|ctx| {
        let obj = unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };

        assert!(unsafe { ll_release(obj as *mut RcHeader) });
        unsafe { ll_object_die(obj) };
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 1);
        assert_eq!(
            unsafe { crate::refcount::entity_refcount(obj) },
            1,
            "resurrected: the destructor's reference keeps it alive"
        );

        // The resurrection reference dies too. Phase 1 is skipped
        // (DESTRUCTOR_RAN bit), phases 2-3 proceed.
        assert!(unsafe { ll_release(obj as *mut RcHeader) });
        unsafe { ll_object_die(obj) };
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            1,
            "__destruct runs exactly once per object"
        );
    });
}

#[test]
fn transient_this_reference_in_destructor_does_not_reenter_teardown() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    TRANSIENT_DEATHS.store(0, Ordering::Relaxed);

    let cls = ClassBuilder::new("Fleeting")
        .destructor(transient_this_destructor as *const ())
        .build();

    with_ctx(|ctx| {
        let obj = unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };

        // Last reference dies; teardown runs the destructor, which takes
        // and drops a transient $this reference.
        assert!(unsafe { ll_release(obj as *mut RcHeader) });
        unsafe { ll_object_die(obj) };

        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 1, "destructor ran once");
        assert_eq!(
            TRANSIENT_DEATHS.load(Ordering::Relaxed),
            0,
            "a transient $this release must not report death: without the \
             guard it re-enters teardown and double-frees obj"
        );
    });
}
