use super::*;
use crate::array::entity::{
    LLArray, as_vector, as_vector_mut, new_vector_array, sever_counted_children, storage_head,
};
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
    let a = unsafe { new_vector_array(category) };
    assert!(!a.is_null(), "allocation refused in a test");
    a
}

/// Give an array whose elements are integers back: the last reference
/// goes, then the teardown frees the storage and the slot. A test holding
/// entities releases them itself instead — the drain would take them a
/// second time.
fn discard(a: *mut LLArray) {
    unsafe {
        assert!(
            ll_release(a as *mut RcHeader),
            "the test was the only holder"
        );
        crate::object::ll_entity_die(a as *mut RcHeader);
    }
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
///
/// Every test here works through an array rather than a bare `Vector`,
/// and not for the reason the entity tests below do: since the head
/// became the entity's (`array::head`), a `Vector` on its own has no
/// version, no chunk and no count, so there is nothing about it to
/// measure without the array in front of it.
mod the_dense_range {
    use super::*;

    #[test]
    fn a_fresh_vector_is_strategy_two_and_holds_nothing() {
        let _g = crate::memory::block_pool::test_guard();
        let a = vector_array(MemoryCategory::GcHeap);
        let (v, head) = unsafe { as_vector(a) };
        assert_eq!(head.tag(), StorageTag::Vector);
        assert_eq!(head.used(), 0);
        assert_eq!(
            Vector::append_key(head),
            Some(0),
            "the first append takes key 0"
        );
        assert!(v.get(head, 0).is_none());
        discard(a);
    }

    #[test]
    fn pushing_keeps_order_and_the_length_is_the_next_key() {
        let _g = crate::memory::block_pool::test_guard();
        let a = vector_array(MemoryCategory::GcHeap);
        let (v, head) = unsafe { as_vector_mut(a) };
        for i in 0..5i64 {
            assert_eq!(Vector::append_key(head), Some(i));
            assert!(v.push(head, MemoryCategory::GcHeap, Value::int(i * 10)));
        }

        assert_eq!(head.used(), 5);
        for i in 0..5usize {
            assert_eq!(v.get(head, i).unwrap().as_int(), i as i64 * 10);
        }

        assert!(
            v.get(head, 5).is_none(),
            "past the end is absent, not a hole"
        );
        discard(a);
    }

    /// Growth moves every element into a fresh chunk, so it opens the
    /// head's window: a walker that strode the old chunk against the new
    /// count would read past its end. The version is the instrument —
    /// it moves across the growth and the elements survive it in order.
    #[test]
    fn growth_moves_the_elements_inside_the_window() {
        let _g = crate::memory::block_pool::test_guard();
        let a = vector_array(MemoryCategory::GcHeap);
        let (v, head) = unsafe { as_vector_mut(a) };
        for i in 0..FIRST_CAP {
            assert!(v.push(head, MemoryCategory::GcHeap, Value::int(i as i64)));
        }

        let before = head.version();
        let chunk = head.storage();
        assert!(v.push(head, MemoryCategory::GcHeap, Value::int(FIRST_CAP as i64)));
        assert_ne!(head.storage(), chunk, "the ninth element needs a new chunk");
        assert_eq!(
            head.version(),
            before + 2,
            "one window opened and closed around the move"
        );
        assert_eq!(head.version() % 2, 0, "and it is closed");

        for i in 0..=FIRST_CAP {
            assert_eq!(
                v.get(head, i).unwrap().as_int(),
                i as i64,
                "order survived the move"
            );
        }

        discard(a);
    }

    /// Releasing the storage publishes the same words growth does, so it
    /// takes the same window: a collector whose snapshot holds the slot
    /// of a dying array must not be handed the chunk with the counts of
    /// the empty state (`PLAN.md` S13.2). The mixture this
    /// representation could offer is narrower than the ordered hash's —
    /// it has no index region to stride — and the bracket is what says
    /// so rather than the order of two stores.
    #[test]
    fn the_release_of_the_storage_is_one_window() {
        let _g = crate::memory::block_pool::test_guard();
        let a = vector_array(MemoryCategory::GcHeap);
        let (v, head) = unsafe { as_vector_mut(a) };
        for i in 0..4i64 {
            assert!(v.push(head, MemoryCategory::GcHeap, Value::int(i)));
        }

        let before = head.version();
        v.dispose(head, MemoryCategory::GcHeap);
        assert_eq!(
            head.version(),
            before + 2,
            "one window opened and closed around the release"
        );
        assert!(head.storage().is_null() && head.used() == 0);
        discard(a);
    }

    /// `set` overwrites a published element and hands the displaced value
    /// back for the caller to release; past the end it refuses, because
    /// what a key outside the range means is a migration and that is not
    /// this type's answer to give.
    #[test]
    fn set_hands_the_displaced_value_back_and_refuses_past_the_end() {
        let _g = crate::memory::block_pool::test_guard();
        let a = vector_array(MemoryCategory::GcHeap);
        let (v, head) = unsafe { as_vector_mut(a) };
        assert!(v.push(head, MemoryCategory::GcHeap, Value::int(1)));
        assert_eq!(v.set(head, 0, Value::int(2)).unwrap().as_int(), 1);
        assert_eq!(v.get(head, 0).unwrap().as_int(), 2);
        assert!(
            v.set(head, 1, Value::int(3)).is_none(),
            "past the end refuses"
        );
        assert_eq!(head.used(), 1, "and the refusal appended nothing");
        discard(a);
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
            assert!(crate::array::testing::push(
                a,
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

    /// The carry out of a dying arena reaches the vector's own chunk.
    ///
    /// This is the door the tag has to be read at, and the one that had no
    /// reader for it: the carry used to name the ordered hash outright, so
    /// a surviving vector array had its `cap` read as a granted byte size
    /// and its uninitialised tail read as a table (Critic, S7.1 round 2).
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
}
