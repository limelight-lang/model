//! One cell per target while that cell lives, so `create` is
//! canonical and answers freshly once it has died. `get` hands the
//! target out retained, which is why a null answer is the only safe
//! report after the death.

use super::*;

#[test]
fn a_weak_cell_is_a_16_byte_kind_11_gc_heap_entity() {
    let _g = crate::memory::block_pool::test_guard();
    assert_eq!(size_of::<LLWeakRef>(), 16);
    assert_eq!(core::mem::offset_of!(LLWeakRef, rc), 0);
    assert_eq!(core::mem::offset_of!(LLWeakRef, target), 8);

    let cls = ClassBuilder::new("WeakTarget").build();
    with_ctx(|ctx| {
        let obj = unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
        let w = unsafe { ll_weakref_create(ctx, obj as *mut RcHeader) };
        assert_eq!(unsafe { crate::refcount::entity_refcount(w) }, 1);
        assert_eq!(
            unsafe { crate::refcount::entity_flags(w) } & ENTITY_KIND_MASK,
            EntityKind::WeakRef.to_flags()
        );
        assert_eq!(
            unsafe { crate::refcount::entity_category(w) },
            MemoryCategory::GcHeap,
            "the cell is GC-heap even when an arena is ambient"
        );
        assert_ne!(
            unsafe { crate::refcount::entity_flags(obj) } & HAS_WEAK_REFERENCES,
            0
        );

        unsafe {
            assert!(ll_release(obj as *mut RcHeader));
            crate::object::ll_object_die(obj);
            assert!(ll_release(w as *mut RcHeader));
            crate::object::ll_entity_die(w as *mut RcHeader);
        }
    });
}

#[test]
fn create_is_canonical_while_the_cell_lives_and_fresh_after_it_dies() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("Canonical").build();
    with_ctx(|ctx| {
        let obj = unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
        let w1 = unsafe { ll_weakref_create(ctx, obj as *mut RcHeader) };
        let w2 = unsafe { ll_weakref_create(ctx, obj as *mut RcHeader) };
        assert_eq!(w1, w2, "one canonical cell per live target");
        assert_eq!(unsafe { crate::refcount::entity_refcount(w1) }, 2);
        assert!(!unsafe { ll_release(w2 as *mut RcHeader) });

        // The last copy dies first: the target returns to the cheap
        // path and the next create() builds a fresh cell (PHP's
        // observable behaviour via spl_object_id).
        unsafe {
            assert!(ll_release(w1 as *mut RcHeader));
            crate::object::ll_entity_die(w1 as *mut RcHeader);
        }

        assert_eq!(
            unsafe { crate::refcount::entity_flags(obj) } & HAS_WEAK_REFERENCES,
            0
        );
        let w3 = unsafe { ll_weakref_create(ctx, obj as *mut RcHeader) };
        assert!(!w3.is_null());
        assert_eq!(unsafe { (*w3).target }, obj as *mut RcHeader);

        unsafe {
            assert!(ll_release(w3 as *mut RcHeader));
            crate::object::ll_entity_die(w3 as *mut RcHeader);
            assert!(ll_release(obj as *mut RcHeader));
            crate::object::ll_object_die(obj);
        }
    });
}

#[test]
fn get_returns_the_target_retained_until_death_nulls_it() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("GetTarget").build();
    with_ctx(|ctx| {
        let obj = unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
        let w = unsafe { ll_weakref_create(ctx, obj as *mut RcHeader) };

        let got = unsafe { ll_weakref_get(w) };
        assert_eq!(got, obj as *mut RcHeader);
        assert_eq!(
            unsafe { crate::refcount::entity_refcount(obj) },
            2,
            "get returned a strong reference"
        );
        assert!(!unsafe { ll_release(got) });

        // Death notifies: the cell reads null, the flag is gone.
        assert!(unsafe { ll_release(obj as *mut RcHeader) });
        unsafe { crate::object::ll_object_die(obj) };
        assert!(unsafe { ll_weakref_get(w) }.is_null());

        unsafe {
            assert!(ll_release(w as *mut RcHeader));
            crate::object::ll_entity_die(w as *mut RcHeader);
        }
    });
}

/// The composition a re-entrant creation produces: the call reads the gate bit
/// down, takes a cell, and finds the row taken by the time it inserts. It gives
/// its own cell back and hands out the canonical one retained.
///
/// **The state is made by hand and the reason it can arise is not.** Taking the
/// cell runs user destructors where the entity heap refuses
/// (`memory::heap::entity_alloc`), and one of them creating a weak reference to
/// this same target leaves the row standing under a `flags` word this call read
/// before it. Clearing the bit reproduces exactly what that call then sees: no
/// fast path at the top, a row at the insert.
#[test]
fn a_row_taken_while_the_cell_was_being_built_wins_and_the_cell_goes_back() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("WeakTargetReentrant").build();

    with_ctx(|ctx| {
        let target = unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) } as *mut RcHeader;
        let canonical = unsafe { ll_weakref_create(ctx, target) };
        assert!(!canonical.is_null(), "the first creation took the row");

        // The head of the cell class's free list, so the call below takes this
        // slot and giving it back puts it here again.
        let probe = unsafe { crate::memory::heap::entity_alloc(size_of::<LLWeakRef>()) };
        assert!(!probe.is_null(), "the heap serves a cell-sized slot");
        unsafe { crate::memory::stdapi::free_unpublished(probe) };

        unsafe { crate::refcount::update_header_flags(target, |f| f & !HAS_WEAK_REFERENCES) };
        let answer = unsafe { ll_weakref_create(ctx, target) };
        unsafe { crate::refcount::update_header_flags(target, |f| f | HAS_WEAK_REFERENCES) };

        assert_eq!(
            answer, canonical,
            "the row that stood is what the caller gets"
        );
        assert_eq!(
            unsafe { crate::refcount::entity_refcount(answer) },
            2,
            "handed out retained, so the caller owns a reference of its own"
        );

        let after = unsafe { crate::memory::heap::entity_alloc(size_of::<LLWeakRef>()) };
        assert_eq!(
            after, probe,
            "and the cell it built went back: the free list offers the same slot again"
        );
        unsafe { crate::memory::stdapi::free_unpublished(after) };

        unsafe {
            crate::refcount::ll_release(answer as *mut RcHeader);
            assert!(ll_release(target));
            crate::object::ll_entity_die(target);
        }
    });
}
