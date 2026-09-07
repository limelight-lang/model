//! The cost of the premise check, which a debug build pays per component:
//! every member's cells are walked once, so the check is linear in the
//! component (`dev/CYCLE-COLLECTOR-REVIEW.md`, finding 5).

use super::*;

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
        unsafe { validate_component(&mut members, 0) },
        ValidationResult::Unreachable
    );
    assert_eq!(
        premise_cell_walks() - before,
        MEMBERS,
        "one walk per member: the in-degrees are counted in a single pass over the edges"
    );

    unsafe { dismantle_ring(&mut arena, ring) };
}
