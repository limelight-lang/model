//! A child a compiler-proven slot holds is destroyed with its holder, the
//! count unread: the default `dispose` reads the ownership mark on the child
//! and runs its death path instead of releasing it, while an unmarked child
//! at the same count survives its holder. The child's own `__destruct` still
//! runs under the resurrection guard, so one that stores `$this` keeps the
//! child alive, unmarked, with the reference it stored.

use super::*;
use crate::memory::barrier::store_box_owned;
use crate::refcount::{SlotState, entity_flags, entity_refcount, is_owned, slot_state};

static RESURRECTED: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn resurrecting_destructor(obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
    unsafe { ll_retain(obj as *mut RcHeader) };
    RESURRECTED.store(obj as usize, Ordering::Relaxed);
}

/// A holder with one Box property, and a child stored into it through the
/// owned store, so the child carries the mark. A case that raises the child's
/// count afterwards models a reference the proof says cannot stand at the
/// holder's death, which is what the rule does not read.
unsafe fn holder_and_owned_child(
    ctx: *mut LLContext,
    holder_cls: *const Class,
    child_cls: *const Class,
) -> (*mut Object, *mut Object) {
    let child = unsafe { new_constructed(ctx, child_cls, MemoryCategory::GcHeap) };
    let holder = unsafe { new_constructed(ctx, holder_cls, MemoryCategory::GcHeap) };
    let arena = unsafe { (*ctx).arena };
    let slot = unsafe { Object::prop_at(holder, crate::test_support::prop_offset(0)) };
    assert!(unsafe {
        store_box_owned(
            arena,
            MemoryCategory::GcHeap,
            slot,
            Value::entity(Tag::Object, child as *mut RcHeader),
        )
    });
    // The creation reference is spent: the slot is the child's one holder.
    assert!(!unsafe { ll_release(child as *mut RcHeader) });
    assert!(is_owned(unsafe { entity_flags(child) }));
    (holder, child)
}

#[test]
fn a_marked_child_is_destroyed_with_its_holder_and_its_count_is_not_read() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);

    let child_cls = ClassBuilder::new("OwnedChild")
        .destructor(counting_destructor as *const ())
        .build();
    let holder_cls = ClassBuilder::new("OwningHolder")
        .prop("child", true)
        .destructor(counting_destructor as *const ())
        .build();

    with_ctx(|ctx| {
        let (holder, child) = unsafe { holder_and_owned_child(ctx, holder_cls, child_cls) };
        unsafe { ll_retain(child as *mut RcHeader) };
        assert_eq!(unsafe { entity_refcount(child) }, 2);

        assert!(unsafe { ll_release(holder as *mut RcHeader) });
        unsafe { ll_object_die(holder) };

        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            2,
            "the holder's destructor and the child's: the child died at a count of two"
        );
        assert_eq!(
            unsafe { slot_state(child as *const RcHeader) },
            SlotState::DeadInPlace,
            "freed, not read as resurrected by the count it carried"
        );
    });
}

/// The contrast: the same graph with the plain store, and the child at two
/// outlives its holder with one reference left.
#[test]
fn an_unmarked_child_at_the_same_count_survives_its_holder() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);

    let child_cls = ClassBuilder::new("SharedChild")
        .destructor(counting_destructor as *const ())
        .build();
    let holder_cls = ClassBuilder::new("SharingHolder")
        .prop("child", true)
        .destructor(counting_destructor as *const ())
        .build();

    with_ctx(|ctx| {
        let child = unsafe { new_constructed(ctx, child_cls, MemoryCategory::GcHeap) };
        let holder = unsafe { new_constructed(ctx, holder_cls, MemoryCategory::GcHeap) };
        unsafe {
            crate::test_support::store_prop(
                (*ctx).arena,
                holder,
                crate::test_support::prop_offset(0),
                child,
            )
        };
        assert_eq!(unsafe { entity_refcount(child) }, 2, "creation + the slot");

        assert!(unsafe { ll_release(holder as *mut RcHeader) });
        unsafe { ll_object_die(holder) };

        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 1, "the holder's alone");
        assert_eq!(
            unsafe { entity_refcount(child) },
            1,
            "the creation reference stands"
        );

        assert!(unsafe { ll_release(child as *mut RcHeader) });
        unsafe { ll_object_die(child) };
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
    });
}

/// The child's `__destruct` runs from a count of zero under the guard, as any
/// entity's does, so storing `$this` keeps it: one reference, and no mark,
/// since the slot that held it is gone with the holder.
#[test]
fn a_marked_child_resurrected_by_its_own_destructor_lives_on_unmarked() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    RESURRECTED.store(0, Ordering::Relaxed);

    let child_cls = ClassBuilder::new("OwnedLazarus")
        .destructor(resurrecting_destructor as *const ())
        .build();
    let holder_cls = ClassBuilder::new("LazarusHolder")
        .prop("child", true)
        .destructor(counting_destructor as *const ())
        .build();

    with_ctx(|ctx| {
        let (holder, child) = unsafe { holder_and_owned_child(ctx, holder_cls, child_cls) };

        assert!(unsafe { ll_release(holder as *mut RcHeader) });
        unsafe { ll_object_die(holder) };

        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        assert_eq!(RESURRECTED.load(Ordering::Relaxed), child as usize);
        assert_eq!(
            unsafe { entity_refcount(child) },
            1,
            "the reference the destructor stored, and nothing else"
        );
        assert!(
            !is_owned(unsafe { entity_flags(child) }),
            "no proven slot holds it now"
        );

        // Phase 1 is skipped the second time; phases 2 and 3 free it.
        assert!(unsafe { ll_release(child as *mut RcHeader) });
        unsafe { ll_object_die(child) };
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2, "__destruct ran once");
        assert_eq!(
            unsafe { slot_state(child as *const RcHeader) },
            SlotState::DeadInPlace
        );
    });
}
