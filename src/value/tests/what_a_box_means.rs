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
}

#[test]
fn entity_boxes_count_scalars_do_not() {
    let mut e = RcHeader::new(MemoryCategory::GcHeap, 0);
    let v = Value::entity(Tag::Object, &mut e);
    assert!(v.is_refcounted());
    unsafe { value_retain(&v) };
    assert_eq!(unsafe { crate::refcount::entity_refcount(&raw mut e) }, 2);
    assert!(!unsafe { value_release(&v) });
    assert_eq!(unsafe { crate::refcount::entity_refcount(&raw mut e) }, 1);

    let i = Value::int(7);
    assert!(!i.is_refcounted());
    assert!(!unsafe { value_release(&i) }, "scalars are no-ops");
}
