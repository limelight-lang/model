//! The zero-count rule: a component holding a member at count zero is
//! dropped before any field of any member is read.
//!
//! What produces the zero-count member is the teardown of another component.
//! The member it releases dies ordinarily — count zero published, slot withheld
//! — and the component that still names it is validated afterwards
//! (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation", step 1, and
//! "Zero-count entities pending slot reuse").

use super::*;

/// A member at count zero drops its component whole: the second member's count
/// is untouched, and so is the cell the zero-count member holds it through.
///
/// The fixture is two unreachable components — a ring, and the chain the ring
/// holds — and the ring's teardown is what releases into the chain. It is
/// modelled by the release alone, because the count is the whole of what the
/// zero-count rule reads. The residue therefore differs from the one a real
/// teardown leaves — there the zero-count member's cells are empty and its
/// children released — so what the assertions below compare is the state before
/// the call against the state after it, rather than a residue of their own.
#[test]
fn a_member_at_count_zero_drops_the_component_whole() {
    let _g = test_guard();
    let ring_node = ClassBuilder::new("ExactRingNode")
        .prop("next", true)
        .prop("out", true)
        .build();
    let chain_node = ClassBuilder::new("ExactChainNode")
        .prop("next", true)
        .build();

    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let first = unsafe { new_constructed(&mut context, ring_node, MemoryCategory::GcHeap) };
    let second = unsafe { new_constructed(&mut context, ring_node, MemoryCategory::GcHeap) };
    let head = unsafe { new_constructed(&mut context, chain_node, MemoryCategory::GcHeap) };
    let tail = unsafe { new_constructed(&mut context, chain_node, MemoryCategory::GcHeap) };

    unsafe {
        store_prop(&mut arena, first, prop_offset(0), second);
        store_prop(&mut arena, second, prop_offset(0), first);
        store_prop(&mut arena, first, prop_offset(1), head);
        store_prop(&mut arena, head, prop_offset(0), tail);

        // From here the graph holds every entity and nothing else does,
        // which leaves all four rows at zero and reads both components as
        // unreachable.
        for entity in [first, second, head, tail] {
            assert!(!ll_release(entity as *mut RcHeader));
        }
    }

    let mut shadow_arena = unsafe { traced_unreachable_from(first, &[first, second, head, tail]) };
    shadow_arena.reset();

    // The ring's teardown releases the chain's head, which reaches zero.
    assert!(unsafe { ll_release(head as *mut RcHeader) });

    // Read before and after rather than against a state of their own: a
    // guard would move the count, a sever would move the cell, and what
    // the drop owes is neither.
    let before = unsafe {
        (
            header_refcount(tail as *mut RcHeader),
            prop_entity(head, prop_offset(0)),
        )
    };

    let mut members = [head as *mut RcHeader, tail as *mut RcHeader];
    assert_eq!(
        unsafe { validate_component(&mut members, 0) },
        ValidationResult::ZeroCountMember,
        "the component holds a member at count zero"
    );
    assert_eq!(
        unsafe { header_refcount(tail as *mut RcHeader) },
        before.0,
        "the drop is whole: the second member took no guard"
    );
    assert_eq!(
        unsafe { prop_entity(head, prop_offset(0)) },
        before.1,
        "no field of the component was written"
    );

    unsafe {
        // The release above is put back before the fixture's own
        // references are, so the teardown below runs on true counts.
        ll_retain(head as *mut RcHeader);
        for entity in [first, second, head, tail] {
            ll_retain(entity as *mut RcHeader);
        }

        store_prop(&mut arena, first, prop_offset(0), std::ptr::null_mut());
        store_prop(&mut arena, second, prop_offset(0), std::ptr::null_mut());
        store_prop(&mut arena, first, prop_offset(1), std::ptr::null_mut());
        store_prop(&mut arena, head, prop_offset(0), std::ptr::null_mut());
        for entity in [first, second, head, tail] {
            assert!(ll_release(entity as *mut RcHeader));
            ll_object_die(entity);
        }
    }
}
