//! A ring that closes through a string-keyed array dies like any other:
//! the tracer reports the array's element and its string key as true
//! addresses while the recheck compares the key cell against the raw
//! word it recorded. Both halves are load-bearing — a child reported
//! under a wrong address maps to no walked row, the dropped in-edge
//! makes the target a computed root, and the ring is never collected;
//! a recorded word that differs from what a re-read returns acquits the
//! component on every epoch, with the same symptom.

use super::*;
use crate::array::table::Key;
use crate::string::ll_string_new;

#[test]
fn a_ring_closed_through_a_string_key_is_collected() {
    let _g = crate::memory::block_pool::test_guard();
    let holder_cls = ClassBuilder::new("StringKeyRingHolder")
        .prop("table", true)
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let a = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
    let t = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
    let k = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"back") };

    unsafe {
        // The ring: a.table -> t, t["back"] -> a. The barrier retains t;
        // the raw table insert retains nothing, so the table's two owed
        // references — the key and the element — are taken by hand.
        assert!(crate::memory::barrier::ref_store(
            &mut arena,
            a as *mut RcHeader,
            Object::prop_at(a, 16),
            std::ptr::null_mut(),
            Value::entity(Tag::Array, t as *mut RcHeader),
        ));
        ll_retain(k as *mut RcHeader);
        ll_retain(a as *mut RcHeader);
        crate::array::testing::insert(
            t,
            Key::Str(k),
            Value::entity(Tag::Object, a as *mut RcHeader),
        );

        // Drop the frame's references; the ring floats with no external
        // root, every count owed to an edge inside it.
        assert!(!ll_release(a as *mut RcHeader));
        assert!(!ll_release(t as *mut RcHeader));
        assert!(!ll_release(k as *mut RcHeader));
    }

    let first = stepped_epoch();
    assert!(first.stamped_new >= 3, "the ring is new to the collector");
    let second = stepped_epoch();
    assert_eq!(second.confirmed, 1, "the ring is one confirmed component");

    let seen = walked_addresses();
    assert!(!seen.contains(&(a as usize)), "the holder died");
    assert!(!seen.contains(&(t as usize)), "the array died");
    assert!(!seen.contains(&(k as usize)), "the key died with its table");
    arena.reset(|_| {});
}
