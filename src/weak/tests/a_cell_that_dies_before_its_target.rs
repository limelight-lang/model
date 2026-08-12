//! A cell dying while its target lives has to leave the table, or
//! the target's row maps to freed memory and the next `create` hands
//! that memory out. rc-trace frees its white set raw, so its kind-5
//! arm is a second site owing the same unregister.

use super::*;

/// The cell itself as cyclic garbage, its target alive outside: the
/// dying cell must unregister (row removed, bit 7 cleared), or the
/// live target's row keeps mapping to freed memory and the next
/// `create()` returns a freed cell.
#[test]
fn a_cell_dying_inside_cyclic_garbage_unregisters_from_its_live_target() {
    let _g = crate::memory::block_pool::test_guard();
    let target_cls = ClassBuilder::new("WeakCellTarget").build();
    // Two bare-pointer slots: the pointer run starts at +16, so
    // "next" is at 16 and "w" at 24 — raw writes below rely on it.
    let holder_cls = ClassBuilder::new("WeakCellHolder")
        .prop_pointer("next")
        .prop_pointer("w")
        .build();

    with_ctx(|ctx| {
        let c = unsafe { new_constructed(ctx, target_cls, MemoryCategory::GcHeap) };
        let a = unsafe { new_constructed(ctx, holder_cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(ctx, holder_cls, MemoryCategory::GcHeap) };
        // The ring, raw writes (slots take over the initial refs)...
        unsafe {
            ((a as *mut u8).add(16) as *mut *mut RcHeader).write(b as *mut RcHeader);
            ((b as *mut u8).add(16) as *mut *mut RcHeader).write(a as *mut RcHeader);
        }

        // ...and the cell, held ONLY by the ring: a's second slot
        // takes over the cell's initial reference.
        let w = unsafe { ll_weakref_create(ctx, c as *mut RcHeader) };
        unsafe {
            ((a as *mut u8).add(24) as *mut *mut RcHeader).write(w as *mut RcHeader);
        }

        let stats = unsafe { crate::walk::collect_cycles() };
        assert_eq!(stats.collected, 3, "ring + cell were collected");
        assert_eq!(
            unsafe { (*c).rc.flags } & HAS_WEAK_REFERENCES,
            0,
            "the dying cell unregistered from its live target"
        );
        // A fresh create must build a fresh, valid cell.
        let w2 = unsafe { ll_weakref_create(ctx, c as *mut RcHeader) };
        assert_eq!(unsafe { (*w2).target }, c as *mut RcHeader);
        assert_eq!(unsafe { (*w2).rc.refcount }, 1);

        unsafe {
            assert!(ll_release(w2 as *mut RcHeader));
            crate::object::ll_entity_die(w2 as *mut RcHeader);
            assert!(ll_release(c as *mut RcHeader));
            crate::object::ll_object_die(c);
        }
    });
}

/// The same shape through rc-trace, whose white-set free is a raw
/// `ll_free` and needs its own kind-5 arm (a bypassed unregister is
/// a use-after-free on the next `create()`).
#[cfg(not(feature = "rc-walk"))]
#[test]
fn rc_trace_frees_a_white_cell_through_its_unregister_arm() {
    let _g = crate::memory::block_pool::test_guard();
    let target_cls = ClassBuilder::new("TraceCellTarget").build();
    let holder_cls = ClassBuilder::new("TraceCellHolder")
        .prop_pointer("next")
        .prop_pointer("w")
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    unsafe {
        let c = new_constructed(&mut ctx, target_cls, MemoryCategory::GcHeap);
        let a = new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap);
        let b = new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap);
        // Raw writes: each slot takes over an initial reference, so
        // the ring plus the cell are garbage the moment the buffered
        // release below hands them to the collector.
        ((a as *mut u8).add(16) as *mut *mut RcHeader).write(b as *mut RcHeader);
        ((b as *mut u8).add(16) as *mut *mut RcHeader).write(a as *mut RcHeader);
        let w = ll_weakref_create(&mut ctx, c as *mut RcHeader);
        ((a as *mut u8).add(24) as *mut *mut RcHeader).write(w as *mut RcHeader);

        // A transient reference buffers `a` as a candidate root, as
        // any decrement-to-nonzero from generated code would.
        crate::refcount::ll_retain(a as *mut RcHeader);
        assert!(!ll_release(a as *mut RcHeader));
        assert_eq!(crate::gc::collect_cycles(), 3, "ring + cell collected");
        assert_eq!(
            (*c).rc.flags & HAS_WEAK_REFERENCES,
            0,
            "the white cell must unregister before its memory is freed"
        );
        let w2 = ll_weakref_create(&mut ctx, c as *mut RcHeader);
        assert_eq!((*w2).target, c as *mut RcHeader);
        assert_eq!((*w2).rc.refcount, 1, "a fresh cell, not the freed one");

        assert!(ll_release(w2 as *mut RcHeader));
        crate::object::ll_entity_die(w2 as *mut RcHeader);
        assert!(ll_release(c as *mut RcHeader));
        crate::object::ll_object_die(c);
    }

    arena.reset(|_| {});
}
