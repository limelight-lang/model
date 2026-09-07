//! The cost of the premise check, which a debug build pays per component:
//! every member's cells are walked once, so the check is linear in the
//! component (`dev/CYCLE-COLLECTOR-REVIEW.md`, finding 5). Beside the walk
//! count stands what the two debug checks ask the global allocator, which is
//! the figure every deny case over a collection subtracts.

use super::*;
use crate::test_support::allocation_probe;

/// The number of members is the number of cell walks. A form that walks
/// the holders once per member reads the square of it, which for the
/// review's 381-member component is 145,161 walks.
#[cfg(debug_assertions)]
#[test]
fn the_premise_check_walks_every_member_s_cells_once() {
    let _g = test_guard();
    const MEMBERS: usize = 6;
    let node = ClassBuilder::new("ExactPremiseRingNode")
        .prop("next", true)
        .build();

    let mut arena = Arena::new();
    let ring = unsafe { ring(&mut arena, [node; MEMBERS]) };

    let mut members = ring.map(|member| member as *mut RcHeader);
    let before = premise_cell_walks();
    assert_eq!(
        unsafe { validate_component(&Membership::listed(&mut members), 0) },
        ValidationResult::Unreachable
    );
    assert_eq!(
        premise_cell_walks() - before,
        MEMBERS,
        "one walk per member: the in-degrees are counted in a single pass over the edges"
    );

    unsafe { dismantle_ring(&mut arena, ring) };
}

/// What one [`validate_component`] asks the global allocator: the two sorted
/// lists `members_in_address_order` builds for the two `debug_assert!`s and
/// the in-degree array between them, and none of the three in a release build.
///
/// The free count is read beside the allocation count because the question the
/// figure answers is what the call holds when it returns: a site that allocated
/// and kept the memory reads the same on the first counter as one that gave it
/// back.
///
/// This is the calibration a deny case over a collection stands on
/// (`PLAN.md` S36.9). Such a case asserts `heap == v *
/// EXEMPT_ALLOCATIONS_PER_VALIDATION` for the `v` validations its commit ran,
/// so a fourth allocation added here reddens every one of them.
#[test]
fn a_validation_allocates_what_its_debug_checks_allocate() {
    let _g = test_guard();
    const MEMBERS: usize = 6;
    let node = ClassBuilder::new("ExactPremiseCostRingNode")
        .prop("next", true)
        .build();

    let mut arena = Arena::new();
    let ring = unsafe { ring(&mut arena, [node; MEMBERS]) };
    let mut members = ring.map(|member| member as *mut RcHeader);
    let membership = Membership::listed(&mut members);

    let _ = allocation_probe::take_allocations();
    let _ = allocation_probe::take_heap_deallocations();
    let answer = unsafe { validate_component(&membership, 0) };
    let drawn = allocation_probe::take_allocations();
    let freed = allocation_probe::take_heap_deallocations();

    assert_eq!(answer, ValidationResult::Unreachable);
    assert_eq!(
        drawn,
        (EXEMPT_ALLOCATIONS_PER_VALIDATION, 0),
        "the two sorted lists and the in-degree array, and nothing out of the pool"
    );
    assert_eq!(
        freed, EXEMPT_ALLOCATIONS_PER_VALIDATION,
        "each of them is given back before the call returns"
    );

    unsafe { dismantle_ring(&mut arena, ring) };
}
