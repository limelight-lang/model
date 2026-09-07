//! The guard-reference subtraction, on a component whose guards are
//! outstanding.
//!
//! The re-verify runs with every member guarded, so the count it reads carries
//! one reference the component does not hold. With the guard references
//! subtracted, the same ring reads as it did before the guards; without that
//! subtraction, the guards leave it externally referenced and nothing would
//! ever be freed (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation", step 5). The
//! teardown that takes the guards and runs the destructor between them is
//! `PLAN.md` S36.3's and S36.4's; the fixture takes them by hand.

use super::*;
use crate::refcount::{mutator_guard_retain, mutator_unguard_release};

#[test]
fn a_guarded_ring_is_unreachable_only_when_the_guard_references_are_subtracted() {
    let _g = test_guard();
    let node = ClassBuilder::new("ExactGuardedNode")
        .prop("next", true)
        .build();

    let mut arena = Arena::new();
    let [first, second] = unsafe { traced_unreachable_ring(&mut arena, [node, node]) };

    let mut members = [first as *mut RcHeader, second as *mut RcHeader];
    for &member in &members {
        unsafe { mutator_guard_retain(member) };
    }

    assert_eq!(
        unsafe { validate_component(&Membership::listed(&mut members), 1) },
        ValidationResult::Unreachable,
        "the discount takes off the one reference the teardown itself added"
    );
    assert_eq!(
        unsafe { validate_component(&Membership::listed(&mut members), 0) },
        ValidationResult::ExternallyReferenced,
        "undiscounted, the guards read as references from outside"
    );

    unsafe {
        for &member in &members {
            assert_eq!(mutator_unguard_release(member), 1);
        }

        dismantle_ring(&mut arena, [first, second]);
    }
}
