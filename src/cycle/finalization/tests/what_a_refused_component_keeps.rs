//! What a component the exact validation refuses keeps: its counts, its flags and
//! every weak cell that named it.
//!
//! The refusal has two results and the case takes both, because they are
//! refused for different reasons. A member named from outside is the staleness
//! the exact validation absorbs (`rfc/model/gc/rc-cycle.md`, "Speculative tracing
//! and exact validation"); a member already at count zero drops the proposal
//! before any field is read (the same document, "Cycle finalization and
//! reclamation", step 1). Neither may leave a guard reference behind or a
//! nulled cell: the component is offered to a later trace, and a cell nulled
//! here would never resolve again.

use super::*;

/// What the members carried before the finalization was asked about them.
///
/// The count and the flags together are what the two writes of a confirmed
/// component would move — the guard the counter half, the invalidation the
/// `HAS_WEAK_REFERENCES` gate in the flags half.
unsafe fn counts_and_flags(members: &[*mut Object]) -> Vec<(u32, u32)> {
    members
        .iter()
        .map(|&member| unsafe {
            (
                header_refcount(member as *mut RcHeader),
                entity_flags(member),
            )
        })
        .collect()
}

#[test]
fn an_externally_referenced_component_takes_no_guard_and_keeps_its_cell() {
    let _g = test_guard();
    let node = ClassBuilder::new("FinalizationRefusedNode")
        .prop("next", true)
        .build();
    let holder = ClassBuilder::new("FinalizationRefusedHolder")
        .prop("held", true)
        .build();

    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let first = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    let second = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    let keeper = unsafe { new_constructed(&mut context, holder, MemoryCategory::GcHeap) };
    let cell = unsafe { ll_weakref_create(&mut context, first as *mut RcHeader) };
    assert!(!cell.is_null(), "the fixture's weak cell");

    unsafe {
        store_prop(&mut arena, first, prop_offset(0), second);
        store_prop(&mut arena, second, prop_offset(0), first);
        assert!(!ll_release(first as *mut RcHeader));
        assert!(!ll_release(second as *mut RcHeader));
    }

    let mut shadow_arena = unsafe { traced_unreachable_from(first, &[first, second]) };
    shadow_arena.reset();

    // The store the exact validation exists to absorb: a reference taken after the
    // trace read the counts.
    unsafe { store_prop(&mut arena, keeper, prop_offset(0), first) };

    let before = unsafe { counts_and_flags(&[first, second]) };
    let mut finalization = Finalization::begin();
    let mut members = [first as *mut RcHeader, second as *mut RcHeader];
    assert_eq!(
        unsafe { finalization.confirm(&mut members) },
        ValidationResult::ExternallyReferenced,
        "one member carries a reference the component does not hold"
    );
    assert_eq!(
        finalization.seal().members(),
        0,
        "a refused component joins no finalization"
    );
    assert_eq!(
        unsafe { counts_and_flags(&[first, second]) },
        before,
        "a refused component keeps its counts and its flags: a guard left \
         standing is never released, and a cleared gate bit is a cell nobody \
         nulls"
    );
    assert_eq!(
        unsafe { ll_weakref_get(cell) },
        first as *mut RcHeader,
        "the cell still resolves"
    );

    unsafe {
        // `get` retained above.
        assert!(!ll_release(first as *mut RcHeader));
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

        assert!(ll_release(cell as *mut RcHeader));
        ll_entity_die(cell as *mut RcHeader);
    }
}

/// A member at count zero drops its component before any field is read, so
/// neither member takes a guard and the cell naming the second one still
/// resolves.
///
/// The zero-count member is modelled by the release alone, as
/// `cycle::validation`'s own case models it: the count is the whole of what
/// the rule reads.
#[test]
fn a_zero_count_member_leaves_the_component_and_its_cell_alone() {
    let _g = test_guard();
    let ring_node = ClassBuilder::new("FinalizationZeroRingNode")
        .prop("next", true)
        .prop("out", true)
        .build();
    let chain_node = ClassBuilder::new("FinalizationZeroChainNode")
        .prop("next", true)
        .build();

    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let first = unsafe { new_constructed(&mut context, ring_node, MemoryCategory::GcHeap) };
    let second = unsafe { new_constructed(&mut context, ring_node, MemoryCategory::GcHeap) };
    let head = unsafe { new_constructed(&mut context, chain_node, MemoryCategory::GcHeap) };
    let tail = unsafe { new_constructed(&mut context, chain_node, MemoryCategory::GcHeap) };
    let cell = unsafe { ll_weakref_create(&mut context, tail as *mut RcHeader) };
    assert!(!cell.is_null(), "the fixture's weak cell");

    unsafe {
        store_prop(&mut arena, first, prop_offset(0), second);
        store_prop(&mut arena, second, prop_offset(0), first);
        store_prop(&mut arena, first, prop_offset(1), head);
        store_prop(&mut arena, head, prop_offset(0), tail);
        for entity in [first, second, head, tail] {
            assert!(!ll_release(entity as *mut RcHeader));
        }
    }

    let mut shadow_arena = unsafe { traced_unreachable_from(first, &[first, second, head, tail]) };
    shadow_arena.reset();

    // The ring's own teardown is what releases the chain's head to zero.
    assert!(unsafe { ll_release(head as *mut RcHeader) });

    let before = unsafe { counts_and_flags(&[head, tail]) };
    let mut finalization = Finalization::begin();
    let mut members = [head as *mut RcHeader, tail as *mut RcHeader];
    assert_eq!(
        unsafe { finalization.confirm(&mut members) },
        ValidationResult::ZeroCountMember,
        "the component holds a member at count zero"
    );
    assert_eq!(finalization.seal().members(), 0);
    assert_eq!(
        unsafe { counts_and_flags(&[head, tail]) },
        before,
        "the drop is whole: neither member took a guard and neither cell was \
         nulled"
    );
    assert_eq!(
        unsafe { ll_weakref_get(cell) },
        tail as *mut RcHeader,
        "the cell naming the second member still resolves"
    );

    unsafe {
        // The `get` above and the release that made the zero-count member are
        // both put back before the fixture's own references are.
        assert!(!ll_release(tail as *mut RcHeader));
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

        assert!(ll_release(cell as *mut RcHeader));
        ll_entity_die(cell as *mut RcHeader);
    }
}
