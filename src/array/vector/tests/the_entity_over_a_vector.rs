//! An array whose storage is a vector is walked, severed and torn down
//! through the same entry points the ordered hash uses: one tracing stride
//! dispatching on the tag, one sever, one teardown. The children are
//! elements alone — a vector keys on the position, so there is no string
//! key beside them to count.

use super::*;

/// A child array, handed to the caller at the +1 its factory returns.
fn child() -> *mut LLArray {
    let c = unsafe { crate::array::entity::ll_array_new(MemoryCategory::GcHeap) };
    assert!(!c.is_null());
    c
}

#[test]
fn every_element_is_a_counted_child_and_nothing_else_is() {
    let _g = crate::memory::block_pool::test_guard();
    let a = vector_array(MemoryCategory::GcHeap);
    let first = child();
    let second = child();
    unsafe {
        let (v, head) = as_vector_mut(a);
        assert!(v.push(
            head,
            MemoryCategory::GcHeap,
            Value::entity(Tag::Array, first as *mut RcHeader)
        ));
        assert!(v.push(head, MemoryCategory::GcHeap, Value::int(7)));
        assert!(v.push(
            head,
            MemoryCategory::GcHeap,
            Value::entity(Tag::Array, second as *mut RcHeader)
        ));
    }

    let mut seen = Vec::new();
    unsafe { crate::array::entity::for_each_counted_child(a, |c| seen.push(c)) };
    assert_eq!(
        seen,
        vec![first as *mut RcHeader, second as *mut RcHeader],
        "the two entities, in order, and the integer is no child"
    );

    unsafe {
        assert!(ll_release(a as *mut RcHeader));
        crate::object::ll_entity_die(a as *mut RcHeader);
    }
}

/// The sever hands every counted child out without releasing one and
/// leaves the vector empty — the caller owes one drop per child, the
/// same contract the hash's sever has.
#[test]
fn the_sever_empties_the_vector_and_hands_the_children_out() {
    let _g = crate::memory::block_pool::test_guard();
    let a = vector_array(MemoryCategory::GcHeap);
    let held = child();
    unsafe {
        assert!(crate::array::testing::push(
            a,
            Value::entity(Tag::Array, held as *mut RcHeader)
        ));
    }

    let mut displaced = Vec::new();
    unsafe { sever_counted_children(a, &mut displaced) };
    assert_eq!(displaced, vec![held as *mut RcHeader]);
    assert!(
        unsafe { (*storage_head(a)).used() } == 0,
        "severed, so nothing is left to release twice"
    );
    assert_eq!(
        unsafe { crate::refcount::entity_refcount(held) },
        1,
        "the sever released nothing: that reference is the caller's now"
    );

    unsafe {
        assert!(ll_release(held as *mut RcHeader));
        crate::object::ll_entity_die(held as *mut RcHeader);
        assert!(ll_release(a as *mut RcHeader));
        crate::object::ll_entity_die(a as *mut RcHeader);
    }
}

/// Teardown releases the elements and frees the storage, so a child
/// the array was the last holder of dies with it.
#[test]
fn teardown_releases_the_elements_and_frees_the_storage() {
    let _g = crate::memory::block_pool::test_guard();
    let before = unsafe { crate::cells::heap_census() };
    let a = vector_array(MemoryCategory::GcHeap);
    let only = child();
    unsafe {
        assert!(crate::array::testing::push(
            a,
            Value::entity(Tag::Array, only as *mut RcHeader)
        ));
    }

    let k = crate::refcount::EntityKind::Array as usize;
    let with_both = unsafe { crate::cells::heap_census() };
    assert_eq!(with_both.by_kind[k], before.by_kind[k] + 2);

    unsafe {
        assert!(ll_release(a as *mut RcHeader));
        crate::object::ll_entity_die(a as *mut RcHeader);
    }

    let after = unsafe { crate::cells::heap_census() };
    assert_eq!(
        after.by_kind[k], before.by_kind[k],
        "the array and the child it was the last holder of are both gone"
    );
}

/// The carry out of a dying arena reaches the vector's own chunk.
///
/// This is the entry point the tag has to be read at, and the one that had no
/// reader for it: the carry used to name the ordered hash outright, so
/// a surviving vector array had its `cap` read as a granted byte size
/// and its uninitialised tail read as a table.
/// What the elements are is beside the point — a chunk is bytes — so
/// this asserts what the operation owes: the storage leaves the arena
/// for a buffer chunk, keeps its contents, and the head names the new
/// address.
#[test]
fn a_surviving_vector_carries_its_chunk_out_of_the_arena() {
    use crate::memory::block_pool::{BLOCK_KIND_BUFFER, BLOCK_MASK};
    use crate::memory::context::{LLContext, set_current_context};
    let _g = crate::memory::block_pool::test_guard();

    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let a = vector_array(MemoryCategory::RequestArena);
    for i in 0..5i64 {
        assert!(unsafe { crate::array::testing::push(a, Value::int(i)) });
    }

    let inside = unsafe { (*storage_head(a)).storage() };
    assert!(unsafe { crate::array::entity::carry_storage_out_of(arena_ptr, a) });
    let outside = unsafe { (*storage_head(a)).storage() };
    assert_ne!(inside, outside, "the chunk stayed in the dying arena");
    assert_eq!(
        unsafe { *(((outside as usize) & !BLOCK_MASK) as *const u32) },
        BLOCK_KIND_BUFFER,
        "the carried chunk came from somewhere other than the buffer arena"
    );

    let (v, head) = unsafe { as_vector(a) };
    for i in 0..5usize {
        assert_eq!(
            v.get(head, i).unwrap().as_int(),
            i as i64,
            "the copy lost an element"
        );
    }

    // The array itself is arena memory and dies with the reset; its
    // storage is not, and nothing has promoted the header, so the free
    // goes by hand at the category the carry moved the chunk to.
    let (v, head) = unsafe { as_vector_mut(a) };
    v.dispose(head, MemoryCategory::GcHeap);
    set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}
