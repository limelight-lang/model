//! Cyclic garbage runs `__destruct` before it is freed, so user code
//! runs over a half-deleted graph: a destructor nulling its own edge
//! releases a sibling the guard must hold to its own un-guard, and
//! one storing `$this` into a live holder resurrects the object
//! together with the child that survives only through it, without
//! the destructor running a second time.

use super::*;

/// Cyclic garbage must run `__destruct` before it is freed — the gap
/// this closes. A two-node cycle of objects each with a destructor,
/// unreferenced from outside, is collected; both destructors must fire.
#[test]
fn cyclic_garbage_runs_its_destructor() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static CYCLE_DTORS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn counting(_o: *mut Object) {
        CYCLE_DTORS.fetch_add(1, Ordering::Relaxed);
    }

    let _g = crate::memory::block_pool::test_guard();
    CYCLE_DTORS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("DtorNode")
        .prop("next", true)
        .destructor(counting as *const ())
        .build();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    unsafe {
        let a = new_constructed(&mut ctx, cls, MemoryCategory::GcHeap);
        let b = new_constructed(&mut ctx, cls, MemoryCategory::GcHeap);
        link(&mut arena, a, 16, b);
        link(&mut arena, b, 16, a);
        assert!(!ll_release(a as *mut RcHeader));
        assert!(!ll_release(b as *mut RcHeader));

        assert_eq!(collect_cycles(), 2, "the cycle is garbage");
        assert_eq!(
            CYCLE_DTORS.load(Ordering::Relaxed),
            2,
            "both cyclic objects ran __destruct before being freed"
        );
    }

    arena.reset(|_| {});
}

/// A destructor that nulls its own edge (`$this->next = null`) releases
/// a sibling mid-teardown. The guard must hold that sibling to its own
/// un-guard; nothing may be freed twice (Miri is the real check).
#[test]
fn a_destructor_unsetting_its_own_edge_does_not_double_free() {
    use crate::memory::context::{resolve_arena, set_current_context};
    use std::sync::atomic::{AtomicUsize, Ordering};
    static DTORS: AtomicUsize = AtomicUsize::new(0);

    // An `assert!` inside a destructor aborts rather than failing the
    // test: this is `extern "C"` and a panic may not unwind out of it.
    // Legitimate here because `ref_store` can only refuse a COW entity
    // leaving the arena, and both stores below write null or a heap
    // object — but the idiom does not travel to a path where a refusal
    // is reachable.
    unsafe extern "C" fn unset_next(obj: *mut Object) {
        DTORS.fetch_add(1, Ordering::Relaxed);
        unsafe {
            let arena = resolve_arena(std::ptr::null_mut());
            let slot = Object::prop_at(obj, 16);
            let v = slot.read();
            let old = if v.is_refcounted() {
                v.entity_ptr()
            } else {
                std::ptr::null_mut()
            };

            assert!(
                ref_store(arena, obj as *mut RcHeader, slot, old, Value::null()),
                "the barrier refused the unset this destructor performs"
            );
        }
    }

    let _g = crate::memory::block_pool::test_guard();
    DTORS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("Unsetter")
        .prop("next", true)
        .destructor(unset_next as *const ())
        .build();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = LLContext { arena: arena_ptr };
    let ctx_ptr: *mut LLContext = &mut ctx;
    set_current_context(ctx_ptr);

    unsafe {
        let a = new_constructed(ctx_ptr, cls, MemoryCategory::GcHeap);
        let b = new_constructed(ctx_ptr, cls, MemoryCategory::GcHeap);
        link(arena_ptr, a, 16, b);
        link(arena_ptr, b, 16, a);
        assert!(!ll_release(a as *mut RcHeader));
        assert!(!ll_release(b as *mut RcHeader));
        collect_cycles();
        assert_eq!(
            DTORS.load(Ordering::Relaxed),
            2,
            "both ran once, no double free"
        );
    }

    set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}

/// A destructor stores `$this` into a live holder, resurrecting the
/// cycle. The re-trace must keep the resurrected object *and* its child
/// (which gained no direct external reference — it survives only because
/// its parent does), and `__destruct` must not run a second time.
#[test]
fn a_destructor_resurrecting_the_cycle_keeps_it_and_its_child() {
    use crate::memory::context::{resolve_arena, set_current_context};
    use std::sync::atomic::{AtomicUsize, Ordering};
    static DTORS: AtomicUsize = AtomicUsize::new(0);
    static LIVE: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn resurrect(obj: *mut Object) {
        DTORS.fetch_add(1, Ordering::Relaxed);
        unsafe {
            let arena = resolve_arena(std::ptr::null_mut());
            let l = LIVE.load(Ordering::Relaxed) as *mut Object;
            let slot = Object::prop_at(l, 16);
            assert!(
                ref_store(
                    arena,
                    l as *mut RcHeader,
                    slot,
                    std::ptr::null_mut(),
                    Value::entity(Tag::Object, obj as *mut RcHeader),
                ),
                "the barrier refused the resurrection this destructor stages"
            );
        }
    }

    let _g = crate::memory::block_pool::test_guard();
    DTORS.store(0, Ordering::Relaxed);
    let a_cls = ClassBuilder::new("Resur")
        .prop("next", true)
        .destructor(resurrect as *const ())
        .build();
    let l_cls = ClassBuilder::new("LiveHolder").prop("keep", true).build();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = LLContext { arena: arena_ptr };
    let ctx_ptr: *mut LLContext = &mut ctx;
    set_current_context(ctx_ptr);

    unsafe {
        let l = new_constructed(ctx_ptr, l_cls, MemoryCategory::GcHeap);
        let a = new_constructed(ctx_ptr, a_cls, MemoryCategory::GcHeap);
        let b = new_constructed(ctx_ptr, node_class(), MemoryCategory::GcHeap);
        LIVE.store(l as usize, Ordering::Relaxed);
        link(arena_ptr, a, 16, b);
        link(arena_ptr, b, 16, a);
        assert!(!ll_release(a as *mut RcHeader));
        assert!(!ll_release(b as *mut RcHeader));

        let freed = collect_cycles();
        assert_eq!(DTORS.load(Ordering::Relaxed), 1);
        assert_eq!(freed, 0, "resurrected: nothing freed");
        assert_eq!(
            Object::prop_at(l, 16).read().entity_ptr(),
            a as *mut RcHeader,
            "L keeps A"
        );
        assert_eq!(
            Object::prop_at(a, 16).read().entity_ptr(),
            b as *mut RcHeader,
            "A->B intact"
        );
        assert_eq!((*a).rc.refcount, 2, "A: B->A + L->A");
        assert_eq!((*b).rc.refcount, 1, "B: A->B");

        // Drop the holder; the cycle is garbage again but __destruct
        // already ran, so it must not fire twice.
        assert!(
            ref_store(
                arena_ptr,
                l as *mut RcHeader,
                Object::prop_at(l, 16),
                a as *mut RcHeader,
                Value::null(),
            ),
            "the barrier refused the drop of the holder's slot"
        );
        assert!(ll_release(l as *mut RcHeader));
        crate::object::ll_object_die(l);
        assert_eq!(collect_cycles(), 2, "the un-held cycle is reclaimed");
        assert_eq!(DTORS.load(Ordering::Relaxed), 1, "no second __destruct");
    }

    set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}
