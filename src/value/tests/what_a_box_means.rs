//! Which payload the tag admits, when truth may read the payload at
//! all, and which boxes carry a reference the runtime must count.

use super::*;

#[test]
fn scalar_round_trips() {
    assert_eq!(Value::int(-42).as_int(), -42);
    assert_eq!(Value::float(2.5).as_float(), 2.5);
    assert_eq!(Value::int(i64::MIN).as_int(), i64::MIN, "full 64-bit ints");
}

#[test]
fn truth_never_reads_payload_for_bool_tags() {
    assert_eq!(Value::null().is_truthy_tag(), Some(false));
    assert_eq!(Value::bool(false).is_truthy_tag(), Some(false));
    assert_eq!(Value::bool(true).is_truthy_tag(), Some(true));
    assert_eq!(Value::int(0).is_truthy_tag(), None, "ints decode payload");
    assert_eq!(
        Value::from_words([0, 0x0001]).is_truthy_tag(),
        Some(false),
        "a container's null is falsy"
    );
}

#[test]
fn null_is_the_initialized_null_tag_alone() {
    assert!(Value::null().is_null());
    assert!(
        !Value::undef().is_null(),
        "an undef slot is an error to read, not a null"
    );
    assert!(!Value::bool(false).is_null());
    assert!(!Value::int(0).is_null());
    assert!(
        Value::from_words([0, 0x0001]).is_null(),
        "a container's null is the second spelling every null test accepts"
    );
}

#[test]
fn the_one_word_tests_answer_by_tag() {
    assert!(Value::int(7).is_int());
    assert!(!Value::float(7.0).is_int());
    assert!(!Value::null().is_int());
    let mut e = RcHeader::new(MemoryCategory::GcHeap, 0);
    let v = Value::entity(Tag::Object, &mut e);
    assert!(v.is_pointer());
    assert!(!v.is_int());
    assert!(!Value::int(7).is_pointer());
    assert!(!Value::null().is_pointer());
}

#[test]
fn entity_boxes_count_scalars_do_not() {
    let _g = crate::memory::block_pool::test_guard();
    let mut e = RcHeader::new(MemoryCategory::GcHeap, 0);
    let v = Value::entity(Tag::Object, &mut e);
    assert!(v.is_pointer());
    unsafe { value_retain(&v) };
    assert_eq!(unsafe { crate::refcount::entity_refcount(&raw mut e) }, 2);
    assert!(!unsafe { value_release(&v) });
    assert_eq!(unsafe { crate::refcount::entity_refcount(&raw mut e) }, 1);
    // The release registered a header on this stack; the lane goes back
    // unread, because this thread's exit collects it.
    crate::cycle::queue::release_queue_segments();

    let i = Value::int(7);
    assert!(!i.is_pointer());
    assert!(!unsafe { value_release(&i) }, "scalars are no-ops");
}
