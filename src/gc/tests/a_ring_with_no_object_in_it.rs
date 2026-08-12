//! The candidate gate decides what this strategy can ever see, and
//! both shapes reach it through a kind that is not an object: two
//! arrays holding each other, and `$a[0] = &$a`, where the last
//! external release lands on the box. While the gate was a mask over
//! the kind codes neither produced a candidate, so the configuration
//! whose whole purpose is cycles was green with a systematic leak.

use super::*;

/// A ring with no object anywhere in it: two arrays holding each
/// other. The last external release of either is a non-zero
/// decrement and bought nothing while the gate masked all three kind
/// bits — neither array ever became a candidate, the collector never
/// got a root, and the ring leaked in the configuration whose whole
/// purpose is cycles. Both configurations are required legs of the
/// gate, so rc-trace was green with a systematic leak in it; the
/// rc-walk twin is `walk::tests::a_ring_of_two_arrays_and_no_object_
/// is_collected`.
///
/// Seen failing on the candidacy assertion below.
#[test]
fn a_ring_of_two_arrays_and_no_object_is_collected() {
    use crate::array::entity::ll_array_new;
    use crate::array::table::Key;
    use crate::refcount::ll_retain;
    let _g = crate::memory::block_pool::test_guard();

    let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let b = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    unsafe {
        // The reference is taken before the entry is published, which
        // is `Table::insert`'s contract: an entry a walker can reach
        // must already be backed by a count.
        ll_retain(b as *mut RcHeader);
        crate::array::testing::insert(
            a,
            Key::Int(0),
            Value::entity(Tag::Array, b as *mut RcHeader),
        );
        ll_retain(a as *mut RcHeader);
        crate::array::testing::insert(
            b,
            Key::Int(0),
            Value::entity(Tag::Array, a as *mut RcHeader),
        );
        // Drop the creation references: each array is held by the
        // other and by nothing else, which is the ring.
        assert!(!ll_release(a as *mut RcHeader), "a is still held by b");
        assert!(!ll_release(b as *mut RcHeader), "b is still held by a");
    }

    assert!(
        unsafe { (*candidate_buffer()).contains(&(a as *mut RcHeader)) },
        "an array that took a non-zero decrement is a candidate root"
    );
    // At least two, not exactly two: the buffer is this thread's and
    // an earlier test on it may have left roots of its own, so an
    // exact count would be a claim about them rather than about this
    // ring.
    assert!(
        unsafe { collect_cycles() } >= 2,
        "the ring was judged and then not freed"
    );
}

/// A ring whose last external release lands on the **ReferenceBox**:
/// `$a[0] = &$a`, where `&$a` makes the variable a box, the box
/// holds the array and the array's element holds the box. An integer
/// key, because the key's own kind is not what this measures — a
/// string key would add one counted child and no edge through the
/// box. Nothing
/// outside ever decrements the array, so the only entity that can
/// become a candidate is the box — and unless the gate admits its
/// kind, the ring produces no candidate at all, so no collection ever
/// judges it and it lives to process exit.
///
/// The rc-walk twin is
/// `walk::tests::a_ring_through_a_reference_box_and_an_array_is_collected`,
/// which needs no candidate at all.
#[test]
fn a_ring_whose_last_release_lands_on_a_reference_box_is_collected() {
    use crate::array::entity::ll_array_new;
    use crate::array::table::Key;
    use crate::refcount::ll_retain;
    let _g = crate::memory::block_pool::test_guard();

    let array = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let boxed = crate::reference::ll_reference_new();
    unsafe {
        // `&$a` moves the variable's hold onto the box rather than
        // adding one, so the box takes the array's creation reference.
        (*boxed).value = Value::entity(Tag::Array, array as *mut RcHeader);
        // Retained before the entry is published, per `Table::insert`.
        ll_retain(boxed as *mut RcHeader);
        crate::array::testing::insert(
            array,
            Key::Int(0),
            Value::entity(Tag::Reference, boxed as *mut RcHeader),
        );
        // The frame's reference dies. It is the ring's only external
        // hold, and it lands on the box rather than on the array.
        assert!(
            !ll_release(boxed as *mut RcHeader),
            "the box is still held by the array's element"
        );
    }

    assert!(
        unsafe { (*candidate_buffer()).contains(&(boxed as *mut RcHeader)) },
        "a reference box that took a non-zero decrement is a candidate root"
    );
    assert!(
        unsafe { collect_cycles() } >= 2,
        "the ring was judged and then not freed"
    );
}
