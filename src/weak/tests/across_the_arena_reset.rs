//! The reset is a death for everything it does not promote, so a
//! cell naming an unpromoted target reads null afterwards, while a
//! survivor keeps its weak state and its address: promotion rewrites
//! the category in place, which is why the cell goes on resolving
//! through the same pointer.

use super::*;

#[test]
fn arena_reset_nulls_cells_of_dying_arena_targets() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("WeakArenaTarget").build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };
    let w = unsafe { ll_weakref_create(&mut ctx, obj as *mut RcHeader) };
    assert_eq!(unsafe { ll_weakref_get(w) }, obj as *mut RcHeader);
    assert!(
        !unsafe { ll_release(obj as *mut RcHeader) },
        "arena objects are not counted"
    );

    arena.reset(|_| {});
    assert!(
        unsafe { ll_weakref_get(w) }.is_null(),
        "the pages are gone; a stale cell would be a dangling read"
    );

    unsafe {
        assert!(ll_release(w as *mut RcHeader));
        crate::object::ll_entity_die(w as *mut RcHeader);
    }
}

#[test]
fn a_promoted_survivor_keeps_its_weak_state_across_reset() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("WeakEscapee").build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };
    let w = unsafe { ll_weakref_create(&mut ctx, obj as *mut RcHeader) };

    // A longer-lived holder takes the object: hold-count 1, logged —
    // the store barrier's escape protocol, done by hand here.
    unsafe {
        crate::refcount::update_header_flags(obj as *mut RcHeader, |f| {
            f | crate::refcount::IS_ESCAPEE
        });
        crate::refcount::set_header_refcount(obj as *mut RcHeader, 1);
        arena.log_escapee(obj as *mut RcHeader);
    }

    unsafe { crate::promote::arena_reset_full(&mut arena) };

    assert_eq!(
        unsafe { crate::refcount::entity_category(obj) },
        MemoryCategory::GcHeap,
        "the escapee was promoted in place"
    );
    assert_eq!(
        unsafe { ll_weakref_get(w) },
        obj as *mut RcHeader,
        "a promoted survivor is alive — reset must not null its cell"
    );
    assert!(
        !unsafe { ll_release(obj as *mut RcHeader) },
        "the get retained"
    );

    // The promoted object now dies the ordinary counted death.
    assert!(unsafe { ll_release(obj as *mut RcHeader) });
    unsafe { crate::object::ll_object_die(obj) };
    assert!(unsafe { ll_weakref_get(w) }.is_null());

    unsafe {
        assert!(ll_release(w as *mut RcHeader));
        crate::object::ll_entity_die(w as *mut RcHeader);
    }
}
