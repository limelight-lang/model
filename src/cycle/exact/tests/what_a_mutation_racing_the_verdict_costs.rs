//! A store that lands between the trace's verdict and the owner's
//! judgement, and the control that shows the refusal came from the store.
//!
//! This is the staleness the exact test exists to absorb: the trace read
//! the counts of a ring nobody held, and by the time the owner reads
//! them one of the members is named from outside
//! (`rfc/model/gc/rc-cycle.md`, "Who judges, and what a trace is
//! worth").

use super::*;

/// A reference taken after the scan acquits the component, because what
/// the owner compares is the count as it stands at its own reading.
#[test]
fn a_reference_taken_after_the_verdict_acquits() {
    let _g = test_guard();
    let node = ClassBuilder::new("ExactRacedNode")
        .prop("next", true)
        .build();
    let holder = ClassBuilder::new("ExactRacedHolder")
        .prop("held", true)
        .build();

    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let first = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    let second = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    let keeper = unsafe { new_constructed(&mut context, holder, MemoryCategory::GcHeap) };

    unsafe {
        store_prop(&mut arena, first, prop_offset(0), second);
        store_prop(&mut arena, second, prop_offset(0), first);
        assert!(!ll_release(first as *mut RcHeader));
        assert!(!ll_release(second as *mut RcHeader));
    }

    let mut shadow_arena = unsafe { condemned_from(first, &[first, second]) };
    shadow_arena.reset();

    // The mutation, through the store barrier the mutator uses: the
    // keeper is a live root the trace never reached.
    unsafe { store_prop(&mut arena, keeper, prop_offset(0), first) };

    let mut members = [first as *mut RcHeader, second as *mut RcHeader];
    assert_eq!(
        unsafe { judge(&mut members, 0) },
        Judged::Acquitted,
        "one member carries a reference the component does not hold"
    );

    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
        ll_retain(first as *mut RcHeader);
        ll_retain(second as *mut RcHeader);
        store_prop(&mut arena, first, prop_offset(0), std::ptr::null_mut());
        store_prop(&mut arena, second, prop_offset(0), std::ptr::null_mut());
        for entity in [first, second] {
            assert!(ll_release(entity as *mut RcHeader));
            ll_object_die(entity);
        }
    }
}

/// The control: the same ring, judged with nothing stored into it, is
/// condemned. Without it the refusal above could be the fixture's rather
/// than the store's.
///
/// Condemned is where this step stops — the free is `PLAN.md` S36.5's,
/// and the fixture tears the ring down by hand.
#[test]
fn the_same_ring_without_the_store_is_condemned() {
    let _g = test_guard();
    let node = ClassBuilder::new("ExactControlNode")
        .prop("next", true)
        .build();
    let holder = ClassBuilder::new("ExactControlHolder")
        .prop("held", true)
        .build();

    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let first = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    let second = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    // Built here and written into nowhere: the two arms differ by the
    // store and by nothing else, allocations included.
    let keeper = unsafe { new_constructed(&mut context, holder, MemoryCategory::GcHeap) };

    unsafe {
        store_prop(&mut arena, first, prop_offset(0), second);
        store_prop(&mut arena, second, prop_offset(0), first);
        assert!(!ll_release(first as *mut RcHeader));
        assert!(!ll_release(second as *mut RcHeader));
    }

    let mut shadow_arena = unsafe { condemned_from(first, &[first, second]) };
    shadow_arena.reset();

    let mut members = [first as *mut RcHeader, second as *mut RcHeader];
    assert_eq!(
        unsafe { judge(&mut members, 0) },
        Judged::Condemned,
        "every reference into the ring comes from the ring"
    );

    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
        ll_retain(first as *mut RcHeader);
        ll_retain(second as *mut RcHeader);
        store_prop(&mut arena, first, prop_offset(0), std::ptr::null_mut());
        store_prop(&mut arena, second, prop_offset(0), std::ptr::null_mut());
        for entity in [first, second] {
            assert!(ll_release(entity as *mut RcHeader));
            ll_object_die(entity);
        }
    }
}
