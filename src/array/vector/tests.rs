use super::*;
use crate::array::entity::{LLArray, Storage, new_with_storage, sever_counted_children};
use crate::refcount::ll_release;
use crate::value::Tag;

/// An array whose storage is a vector, at count one, empty.
///
/// Through `new_with_storage` rather than `ll_array_new`, which stamps
/// the ordered hash until the element layer reads the tag (`PLAN.md`
/// S7.2). That is what makes these tests the vector's only producer for
/// one step, and it is why they build the entity rather than the bare
/// representation: what the criterion asks about is an array.
fn vector_array(category: MemoryCategory) -> *mut LLArray {
    let a = unsafe { new_with_storage(category, Storage::vector()) };
    assert!(!a.is_null(), "allocation refused in a test");
    a
}

/// A child array, handed to the caller at the +1 its factory returns.
fn child() -> *mut LLArray {
    let c = unsafe { crate::array::entity::ll_array_new(MemoryCategory::GcHeap) };
    assert!(!c.is_null());
    c
}

/// The key is the position, so a vector stores no key and its length is
/// both the element count and the next append key. Growth doubles and
/// moves the elements, which is why it runs inside the head's window.
mod the_dense_range {
    use super::*;

    #[test]
    fn a_fresh_vector_is_strategy_two_and_holds_nothing() {
        let v = Vector::empty();
        assert_eq!(v.head.tag(), StorageTag::Vector);
        assert!(v.is_empty());
        assert_eq!(v.append_key(), Some(0), "the first append takes key 0");
        assert!(v.get(0).is_none());
    }

    #[test]
    fn pushing_keeps_order_and_the_length_is_the_next_key() {
        let _g = crate::memory::block_pool::test_guard();
        let mut v = Vector::empty();
        for i in 0..5i64 {
            assert_eq!(v.append_key(), Some(i));
            assert!(v.push(MemoryCategory::GcHeap, Value::int(i * 10)));
        }

        assert_eq!(v.len(), 5);
        for i in 0..5usize {
            assert_eq!(v.get(i).unwrap().as_int(), i as i64 * 10);
        }

        assert!(v.get(5).is_none(), "past the end is absent, not a hole");
        v.dispose(MemoryCategory::GcHeap);
    }

    /// Growth moves every element into a fresh chunk, so it opens the
    /// head's window: a walker that strode the old chunk against the new
    /// count would read past its end. The version is the instrument —
    /// it moves across the growth and the elements survive it in order.
    #[test]
    fn growth_moves_the_elements_inside_the_window() {
        let _g = crate::memory::block_pool::test_guard();
        let mut v = Vector::empty();
        for i in 0..FIRST_CAP {
            assert!(v.push(MemoryCategory::GcHeap, Value::int(i as i64)));
        }

        let before = v.head.version();
        let chunk = v.head.storage();
        assert!(v.push(MemoryCategory::GcHeap, Value::int(FIRST_CAP as i64)));
        assert_ne!(
            v.head.storage(),
            chunk,
            "the ninth element needs a new chunk"
        );
        assert_eq!(
            v.head.version(),
            before + 2,
            "one window opened and closed around the move"
        );
        assert_eq!(v.head.version() % 2, 0, "and it is closed");

        for i in 0..=FIRST_CAP {
            assert_eq!(
                v.get(i).unwrap().as_int(),
                i as i64,
                "order survived the move"
            );
        }

        v.dispose(MemoryCategory::GcHeap);
    }

    /// `set` overwrites a published element and hands the displaced value
    /// back for the caller to release; past the end it refuses, because
    /// what a key outside the range means is a migration and that is not
    /// this type's answer to give.
    #[test]
    fn set_hands_the_displaced_value_back_and_refuses_past_the_end() {
        let _g = crate::memory::block_pool::test_guard();
        let mut v = Vector::empty();
        assert!(v.push(MemoryCategory::GcHeap, Value::int(1)));
        assert_eq!(v.set(0, Value::int(2)).unwrap().as_int(), 1);
        assert_eq!(v.get(0).unwrap().as_int(), 2);
        assert!(v.set(1, Value::int(3)).is_none(), "past the end refuses");
        assert_eq!(v.len(), 1, "and the refusal appended nothing");
        v.dispose(MemoryCategory::GcHeap);
    }
}

/// An array whose storage is a vector is walked, severed and torn down
/// through the same doors the ordered hash uses: one tracing stride
/// dispatching on the tag, one sever, one teardown. The children are
/// elements alone — a vector keys on the position, so there is no string
/// key beside them to count.
mod the_entity_over_a_vector {
    use super::*;

    #[test]
    fn every_element_is_a_counted_child_and_nothing_else_is() {
        let _g = crate::memory::block_pool::test_guard();
        let a = vector_array(MemoryCategory::GcHeap);
        let first = child();
        let second = child();
        unsafe {
            let v = (*a).storage.as_vector_mut();
            assert!(v.push(
                MemoryCategory::GcHeap,
                Value::entity(Tag::Array, first as *mut RcHeader)
            ));
            assert!(v.push(MemoryCategory::GcHeap, Value::int(7)));
            assert!(v.push(
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
            assert!((*a).storage.as_vector_mut().push(
                MemoryCategory::GcHeap,
                Value::entity(Tag::Array, held as *mut RcHeader)
            ));
        }

        let mut displaced = Vec::new();
        unsafe { sever_counted_children(a, &mut displaced) };
        assert_eq!(displaced, vec![held as *mut RcHeader]);
        assert!(
            unsafe { (*a).storage.as_vector().is_empty() },
            "severed, so nothing is left to release twice"
        );
        assert_eq!(
            unsafe { (*held).rc.refcount },
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
        let before = unsafe { crate::walk::heap_census() };
        let a = vector_array(MemoryCategory::GcHeap);
        let only = child();
        unsafe {
            assert!((*a).storage.as_vector_mut().push(
                MemoryCategory::GcHeap,
                Value::entity(Tag::Array, only as *mut RcHeader)
            ));
        }

        let k = crate::refcount::EntityKind::Array as usize;
        let with_both = unsafe { crate::walk::heap_census() };
        assert_eq!(with_both.by_kind[k], before.by_kind[k] + 2);

        unsafe {
            assert!(ll_release(a as *mut RcHeader));
            crate::object::ll_entity_die(a as *mut RcHeader);
        }

        let after = unsafe { crate::walk::heap_census() };
        assert_eq!(
            after.by_kind[k], before.by_kind[k],
            "the array and the child it was the last holder of are both gone"
        );
    }
}
