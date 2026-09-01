//! The guard discount, on a component whose guards are outstanding.
//!
//! The re-verify runs with every member guarded, so the count it reads
//! carries one reference the component does not hold. Discounted, the
//! same ring reads as it did before the guards; undiscounted, the guards
//! acquit it and nothing would ever be freed
//! (`rfc/model/gc/rc-cycle.md`, "Cycle teardown", step 5). The teardown
//! that takes the guards and runs the destructor between them is
//! `PLAN.md` S36.3's and S36.4's; the fixture takes them by hand.

use super::*;
use crate::refcount::{mutator_guard_retain, mutator_unguard_release};

#[test]
fn a_guarded_ring_is_condemned_under_the_discount_and_acquitted_without_it() {
    let _g = test_guard();
    let node = ClassBuilder::new("ExactGuardedNode")
        .prop("next", true)
        .build();

    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let first = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    let second = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };

    unsafe {
        store_prop(&mut arena, first, prop_offset(0), second);
        store_prop(&mut arena, second, prop_offset(0), first);
        assert!(!ll_release(first as *mut RcHeader));
        assert!(!ll_release(second as *mut RcHeader));
    }

    let mut shadow_arena = unsafe { condemned_from(first, &[first, second]) };
    shadow_arena.reset();

    let mut members = [first as *mut RcHeader, second as *mut RcHeader];
    for &member in &members {
        unsafe { mutator_guard_retain(member) };
    }

    assert_eq!(
        unsafe { validate_component(&mut members, 1) },
        ValidationResult::Unreachable,
        "the discount takes off the one reference the teardown itself added"
    );
    assert_eq!(
        unsafe { validate_component(&mut members, 0) },
        ValidationResult::ExternallyReferenced,
        "undiscounted, the guards read as references from outside"
    );

    unsafe {
        for &member in &members {
            assert_eq!(mutator_unguard_release(member), 1);
        }

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
