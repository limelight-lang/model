//! Growth moves the entries into another chunk and compaction slides
//! them inside the one they are in, which is why an element
//! reference is a `ReferenceBox` and not a pointer to the slot.

use super::*;

// ---- a reference into an element ---------------------------------

/// The box outlives the storage the element lived in. A slot pointer
/// would be dangling after the growth below; the box is not, which is
/// the whole reason an element reference is boxed
/// ([`box_element`]).
#[test]
fn a_reference_into_an_element_survives_growth() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;

    let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    unsafe {
        crate::array::testing::insert(a, Key::Int(1), Value::int(41));

        let r = box_element(a, arena_ptr, Key::Int(1));
        assert!(!r.is_null());
        assert_eq!((*r).value.as_int(), 41);

        // Enough inserts to reallocate the storage several times.
        for i in 2..5000i64 {
            crate::array::testing::insert(a, Key::Int(i), Value::int(i));
        }

        (*r).value = Value::int(99);
        assert_eq!((*r).value.as_int(), 99);

        // The element still holds the same box.
        let again = box_element(a, arena_ptr, Key::Int(1));
        assert_eq!(again, r, "asking twice must not build a second box");

        // Released to zero before the kill: `ll_free` asserts that
        // a slot reaching the free list carries a dead header.
        assert!(ll_release(a as *mut RcHeader));
        crate::object::ll_entity_die(a as *mut RcHeader);
    }
}

/// Compaction slides live entries down inside the same chunk, which
/// moves the element without moving the storage — the case a double
/// read of the storage pointer cannot see, and the box does not care
/// about either.
#[test]
fn a_reference_into_an_element_survives_compaction() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;

    let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    unsafe {
        for i in 0..200i64 {
            crate::array::testing::insert(a, Key::Int(i), Value::int(i));
        }

        let r = box_element(a, arena_ptr, Key::Int(150));
        assert!(!r.is_null());
        for i in 0..150i64 {
            let _ = crate::array::testing::remove(a, Key::Int(i));
        }

        crate::array::testing::compact(a);

        assert_eq!(
            box_element(a, arena_ptr, Key::Int(150)),
            r,
            "compaction moved the element, not the box"
        );
        (*r).value = Value::int(-1);
        assert_eq!(
            crate::array::testing::get(a, Key::Int(150)).unwrap().tag(),
            Tag::Reference,
            "the element holds the box, not the value"
        );

        assert!(ll_release(a as *mut RcHeader));
        crate::object::ll_entity_die(a as *mut RcHeader);
    }
}
