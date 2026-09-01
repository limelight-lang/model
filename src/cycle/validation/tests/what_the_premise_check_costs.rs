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
    let mut context = LLContext { arena: &mut arena };
    let ring: Vec<*mut Object> = (0..MEMBERS)
        .map(|_| unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) })
        .collect();

    unsafe {
        for (index, &holder) in ring.iter().enumerate() {
            store_prop(
                &mut arena,
                holder,
                prop_offset(0),
                ring[(index + 1) % MEMBERS],
            );
        }

        for &entity in &ring {
            assert!(!ll_release(entity as *mut RcHeader));
        }
    }

    let mut members: Vec<*mut RcHeader> = ring.iter().map(|&m| m as *mut RcHeader).collect();
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

    unsafe {
        for &entity in &ring {
            ll_retain(entity as *mut RcHeader);
        }

        for &entity in &ring {
            store_prop(&mut arena, entity, prop_offset(0), std::ptr::null_mut());
        }

        for &entity in &ring {
            assert!(ll_release(entity as *mut RcHeader));
            ll_object_die(entity);
        }
    }
}
