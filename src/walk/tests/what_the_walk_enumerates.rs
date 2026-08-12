//! Occupancy is the refcount word, so an entity is visible from the
//! moment a factory publishes a header and invisible from the
//! instant teardown frees it, and a reserved cell reads exactly as a
//! free slot does. Raw buffers are a separate population the walk
//! never enters. The census aggregates what the walk yields, in
//! deltas rather than totals, the walk being process-global.

use super::*;

/// Reserved cells are invisible to the walker until construction
/// publishes a header (`rfc/model/memory/bulk-operations.md`): a
/// cell's slot still reads its final `rc 0` (or virgin zero), the
/// same occupancy answer as a free slot.
#[test]
fn a_reserved_cell_is_walker_invisible_until_constructed() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("CellReserved")
        .prop("child", true)
        .build();
    let size = unsafe { (*cls).object_size } as usize;

    let mut cells = [std::ptr::null_mut::<u8>(); 4];
    let mut contiguous = 0usize;
    let n = unsafe {
        crate::memory::heap::ll_entity_reserve(size, 4, cells.as_mut_ptr(), &mut contiguous)
    };

    assert!(n >= 2, "the probe needs at least two cells; got {n}");

    let seen = walked_addresses();
    for &c in &cells[..n] {
        assert!(
            !seen.contains(&(c as usize)),
            "an unconstructed cell was walked"
        );
    }

    let obj = unsafe { crate::object::ll_object_new_in(cells[0], cls) };
    assert!(
        walked_addresses().contains(&(obj as usize)),
        "constructed: walked"
    );

    unsafe { crate::memory::heap::ll_entity_cells_return(cells.as_ptr().add(1), n - 1) };
    assert!(unsafe { ll_release(obj as *mut RcHeader) });
    unsafe { crate::object::ll_object_die(obj) };
}

#[test]
fn walk_sees_gc_objects_and_not_raw_buffers() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("Walked").prop("child", true).build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let parent = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let child = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    // Raw C-ABI buffer of a comparable size: must never be walked.
    let buffer = unsafe { crate::memory::stdapi::ll_malloc(40) };

    let seen = walked_addresses();
    assert!(seen.contains(&(parent as usize)), "GcHeap object is walked");
    assert!(seen.contains(&(child as usize)));
    assert!(
        !seen.contains(&(buffer as usize)),
        "a raw buffer must live outside the entity population"
    );

    // The edge is visible to the kind-dispatched tracer.
    unsafe {
        Object::prop_at(parent, 16).write(Value::entity(Tag::Object, child as *mut RcHeader));
        let mut children = Vec::new();
        trace_entity(parent as *mut RcHeader, |c| children.push(c as usize));
        assert_eq!(children, vec![child as usize]);
    }

    // Tear down in dependency order; the child's count is owned by
    // the parent's slot.
    unsafe {
        assert!(ll_release(parent as *mut RcHeader));
        crate::object::ll_object_die(parent);
        crate::memory::stdapi::ll_free(buffer);
    }

    arena.reset(|_| {});
}

/// Occupancy is the refcount word: an entity is invisible to the walk
/// from the instant teardown frees it — no teardown stamp exists to
/// forget (`rfc/model/gc/rc-walk.md`, the retired FREE stamp).
#[test]
fn a_freed_entity_disappears_from_the_walk() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("Ephemeral").build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let addr = obj as usize;
    assert!(walked_addresses().contains(&addr));

    unsafe {
        assert!(ll_release(obj as *mut RcHeader));
        crate::object::ll_object_die(obj);
    }

    assert!(
        !walked_addresses().contains(&addr),
        "refcount 0 is the occupancy test; the freed slot must read free"
    );
    arena.reset(|_| {});
}

/// The census aggregates what the walk yields. The assertions are
/// deltas, never totals: the walk is process-global.
#[test]
fn census_counts_objects_and_their_edges() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("Counted").prop("child", true).build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let (before, before_addrs) = unsafe { census_with_addresses() };
    let parent = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let child = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe {
        Object::prop_at(parent, 16).write(Value::entity(Tag::Object, child as *mut RcHeader));
    }

    let (after, after_addrs) = unsafe { census_with_addresses() };
    if after.entities != before.entities + 2 {
        report_census_drift(&before_addrs, &after_addrs);
        for (name, e) in [("parent", parent), ("child", child)] {
            let addr = e as usize;
            let was = before_addrs
                .iter()
                .find(|&&(a, _)| a == addr)
                .map(|&(_, h)| h);
            let now = after_addrs
                .iter()
                .find(|&&(a, _)| a == addr)
                .map(|&(_, h)| h);
            eprintln!(
                "  {name} header_at_before {was:#x?} header_at_after {now:#x?} {}",
                crate::memory::heap::describe_slot(addr)
            );
        }
    }

    assert_eq!(after.entities, before.entities + 2);
    assert_eq!(
        after.by_kind[EntityKind::Object as usize],
        before.by_kind[EntityKind::Object as usize] + 2
    );
    assert_eq!(after.edges, before.edges + 1, "the parent→child edge");

    unsafe {
        assert!(ll_release(parent as *mut RcHeader));
        crate::object::ll_object_die(parent);
    }

    arena.reset(|_| {});
}
