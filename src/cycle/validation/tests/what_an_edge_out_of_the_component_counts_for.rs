//! An edge from a member to an entity outside the component: the
//! component holds the reference rather than carrying it, so the count
//! it is judged on is the referent's and not its own.
//!
//! Without the membership test this is the direction that frees a live
//! ring — every out-edge would be subtracted from the component's own
//! side of the identity.

use super::*;

#[test]
fn a_reference_the_component_holds_is_no_reference_into_it() {
    let _g = test_guard();
    let node = ClassBuilder::new("ExactOutEdgeNode")
        .prop("next", true)
        .prop("out", true)
        .build();
    let outsider = ClassBuilder::new("ExactOutsider")
        .prop("next", true)
        .build();

    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let first = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    let second = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    let outside = unsafe { new_constructed(&mut context, outsider, MemoryCategory::GcHeap) };

    unsafe {
        store_prop(&mut arena, first, prop_offset(0), second);
        store_prop(&mut arena, second, prop_offset(0), first);
        store_prop(&mut arena, first, prop_offset(1), outside);
        assert!(!ll_release(first as *mut RcHeader));
        assert!(!ll_release(second as *mut RcHeader));
    }

    let mut shadow_arena = unsafe { traced_unreachable_from(first, &[first, second]) };
    assert_eq!(
        unsafe { row_color(outside as *mut RcHeader) },
        Color::Live,
        "the fixture's own reference is what holds the entity the ring points at"
    );
    shadow_arena.reset();

    let mut members = [first as *mut RcHeader, second as *mut RcHeader];
    assert_eq!(
        unsafe { validate_component(&mut members, 0) },
        ValidationResult::Unreachable,
        "the ring's own two edges account for both members' counts"
    );

    unsafe {
        ll_retain(first as *mut RcHeader);
        ll_retain(second as *mut RcHeader);
        store_prop(&mut arena, first, prop_offset(0), std::ptr::null_mut());
        store_prop(&mut arena, second, prop_offset(0), std::ptr::null_mut());
        store_prop(&mut arena, first, prop_offset(1), std::ptr::null_mut());
        for entity in [first, second, outside] {
            assert!(ll_release(entity as *mut RcHeader));
            ll_object_die(entity);
        }
    }
}
