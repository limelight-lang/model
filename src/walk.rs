//! Entity walking: the kind-dispatched tracer and the heap census —
//! build step 1 of the `rc-walk` cycle collector
//! (`rfc/model/gc/rc-walk.md`, "Build order"). No collector exists yet;
//! what this module delivers is the walking substrate: enumerate every
//! live entity through the region registry, and trace an entity's
//! counted children by its kind without touching `+8` unless the kind
//! carries a class pointer there.
//!
//! Knowledge split: `memory::heap` knows blocks, slots and occupancy
//! ([`for_each_entity_slot`]); this module knows entity kinds and what
//! each kind's out-edges are. Neither knows the other's internals.

use crate::memory::heap::for_each_entity_slot;
use crate::object::{Object, for_each_counted_child};
use crate::refcount::{ENTITY_KIND_MASK, ENTITY_KIND_SHIFT, EntityKind, RcHeader};

/// Visit every counted child of `entity`, dispatching on the kind bits
/// **before** touching `+8`: only Object (0) and Lazy (6) carry a class
/// pointer there, and reaching for `traced_runs` through a class that
/// does not exist is a wild read (`rfc/model/gc/rc-walk.md`, "What the
/// walker traces").
///
/// Kinds this crate does not yet produce (String, Array, Reference, Box,
/// WeakRef — A2) are skipped, which is conservative: an omitted source
/// only removes in-edges, so its targets are pinned as roots. Array and
/// Reference tracing must land with A2, before the collector ships —
/// String, WeakRef and Box stay skipped by design (no out-edge can close
/// a ring / untraceable C payload).
///
/// # Safety
/// `entity` must point to a live entity whose slots are still readable.
pub unsafe fn trace_entity(entity: *mut RcHeader, visit: impl FnMut(*mut RcHeader)) {
    let flags = unsafe { (*entity).flags };
    let kind = (flags & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT;
    const OBJECT: u32 = EntityKind::Object as u32;
    const LAZY: u32 = EntityKind::Lazy as u32;
    match kind {
        OBJECT | LAZY => unsafe { for_each_counted_child(entity as *mut Object, visit) },
        _ => {}
    }
}

/// A point-in-time census of the walked entity population.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Census {
    /// Occupied entity-block slots.
    pub entities: usize,
    /// Entities per kind code (index = kind bits; 7 is reserved).
    pub by_kind: [usize; 8],
    /// Counted out-edges of walked entities, targets anywhere.
    pub edges: usize,
}

/// Count every live entity in the entity-block population, by kind, with
/// its counted out-edges — the whole-heap leak-detector precursor of
/// build step 2.
///
/// # Safety
/// As [`for_each_entity_slot`]: a quiescent mutator.
pub unsafe fn heap_census() -> Census {
    let mut census = Census::default();
    unsafe {
        for_each_entity_slot(|entity| {
            census.entities += 1;
            let kind = ((*entity).flags & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT;
            census.by_kind[kind as usize] += 1;
            trace_entity(entity, |_child| census.edges += 1);
        });
    }
    census
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::ClassBuilder;
    use crate::memory::arena::Arena;
    use crate::memory::context::LLContext;
    use crate::object::new_constructed;
    use crate::refcount::{MemoryCategory, ll_release};
    use crate::value::{Tag, Value};

    /// Collect the addresses the walk currently yields. Tests assert
    /// membership, never totals: the registry is process-global, and
    /// other tests' leftovers (abandoned blocks with live objects) are
    /// legitimately visible here.
    fn walked_addresses() -> Vec<usize> {
        let mut seen = Vec::new();
        unsafe { for_each_entity_slot(|e| seen.push(e as usize)) };
        seen
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

    /// The census aggregates what the walk yields; with only objects
    /// produced today, every walked entity reports the Object kind.
    #[test]
    fn census_counts_objects_and_their_edges() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Counted").prop("child", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let before = unsafe { heap_census() };
        let parent = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let child = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            Object::prop_at(parent, 16).write(Value::entity(Tag::Object, child as *mut RcHeader));
        }

        let after = unsafe { heap_census() };
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
}
