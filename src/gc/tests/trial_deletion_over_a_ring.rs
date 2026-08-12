//! Trial deletion subtracts the internal edges and reclaims what
//! reaches zero, restoring the counts of anything an external
//! reference holds. It has to trace through every kind carrying
//! counted slots — a reference box included, or the back-edge is
//! invisible and the object reads externally rooted — and through
//! both halves of the large-entity population. Acyclic garbage dies
//! by refcount and never reaches the collector at all.

use super::*;

#[test]
fn a_two_node_cycle_is_reclaimed() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = node_class();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe {
        link(&mut arena, a, 16, b); // a→b: b rc=2
        link(&mut arena, b, 16, a); // b→a: a rc=2
        // External references die: counts drop to 1, both buffered.
        assert!(!ll_release(a as *mut RcHeader));
        assert!(!ll_release(b as *mut RcHeader));
    }

    let freed = unsafe { collect_cycles() };
    assert_eq!(freed, 2, "the cycle is garbage and must be reclaimed");
    arena.reset(|_| {});
}

#[test]
fn a_self_cycle_is_reclaimed() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = node_class();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe {
        link(&mut arena, a, 16, a); // a→a: rc=2
        assert!(!ll_release(a as *mut RcHeader));
    }

    assert_eq!(unsafe { collect_cycles() }, 1);
    arena.reset(|_| {});
}

#[test]
fn an_externally_referenced_cycle_survives_with_counts_restored() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = node_class();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe {
        link(&mut arena, a, 16, b);
        link(&mut arena, b, 16, a);
        // Only b's external reference dies; a is still held (by us).
        assert!(!ll_release(b as *mut RcHeader));
    }

    assert_eq!(unsafe { collect_cycles() }, 0, "externally reachable");
    unsafe {
        assert_eq!((*a).rc.refcount, 2, "trial deletion fully restored");
        assert_eq!((*b).rc.refcount, 1);
    }

    // Now the external reference dies too: the cycle is garbage.
    unsafe { assert!(!ll_release(a as *mut RcHeader)) };
    assert_eq!(unsafe { collect_cycles() }, 2);
    arena.reset(|_| {});
}

/// A ring through a reference box (`$a->next = &$a`): trial deletion
/// must trace THROUGH the box by kind, or the box's back-edge is
/// invisible, the object reads externally rooted, and the ring leaks
/// silently forever.
#[test]
fn a_cycle_through_a_reference_box_is_reclaimed() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = node_class();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let r = crate::reference::ll_reference_new();
    unsafe {
        // a.next owns the box's initial ref; the box owns a's second.
        Object::prop_at(a, 16).write(Value::entity(Tag::Reference, r as *mut RcHeader));
        crate::refcount::ll_retain(a as *mut RcHeader);
        (*r).value = Value::entity(Tag::Object, a as *mut RcHeader);
        // The frame's reference dies: a is buffered as a candidate.
        assert!(!ll_release(a as *mut RcHeader));
    }

    let freed = unsafe { collect_cycles() };
    assert_eq!(freed, 2, "object + box are one garbage ring");
    arena.reset(|_| {});
}

/// The same ring, in the strategy that finds its roots from
/// decrements instead of from a walk. It exercises none of the
/// enumerators — this collector never reads a block header — and
/// what it pins is the other half: a large entity is an ordinary
/// candidate, and the teardown that frees the white set routes by
/// block kind (`rfc/model/memory/large-entities.md`).
#[test]
fn a_cycle_through_large_entities_is_collected_by_the_tracing_strategy() {
    let _g = crate::memory::block_pool::test_guard();
    let pooled_cls = wide_class("PooledTraceNode", POOLED_FILLERS, None);
    let run_cls = wide_class("RunTraceNode", RUN_FILLERS, None);

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    unsafe {
        let a = new_constructed(&mut ctx, pooled_cls, MemoryCategory::GcHeap);
        let b = new_constructed(&mut ctx, run_cls, MemoryCategory::GcHeap);
        let kind_of = |o: *mut Object| {
            *(((o as usize) & !crate::memory::block_pool::BLOCK_MASK) as *const u32)
        };

        assert_eq!(
            kind_of(a),
            crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE
        );
        assert_eq!(
            kind_of(b),
            crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE_RUN
        );

        let run_block = (b as usize) & !crate::memory::block_pool::BLOCK_MASK;
        link(&mut arena, a, 16, b);
        link(&mut arena, b, 16, a);
        assert!(!ll_release(a as *mut RcHeader));
        assert!(!ll_release(b as *mut RcHeader));

        assert_eq!(collect_cycles(), 2, "the ring is garbage here too");
        assert!(
            !crate::memory::large_entity::snapshot().contains(&run_block),
            "and the run's registry entry went with it"
        );
    }

    arena.reset(|_| {});
}

#[test]
fn acyclic_garbage_never_reaches_the_collector() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = node_class();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe {
        // Plain death: refcount to zero, no non-zero decrement ever.
        assert!(ll_release(a as *mut RcHeader));
        crate::object::ll_object_die(a);
    }

    assert_eq!(
        unsafe { (*candidate_buffer()).len() },
        0,
        "straight-line deaths never buffer"
    );
}
