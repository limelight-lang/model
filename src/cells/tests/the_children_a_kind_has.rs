//! One stride serves every walker, so the two readers must return
//! the same child set on a quiescent heap — the pin the crate lacked
//! while the stride was written out once per walk, which is how the
//! template's moved value count reached two walkers of three and the
//! third was found by review. An array's out-edges are its elements
//! and its string keys alike, a reference box passes an edge
//! through, and a large entity is found in whichever half of the
//! population it landed in.

use super::*;

/// [`crate::test_support::wide_class`] with the counting destructor,
/// so the only edge is the one the ring ties.
fn wide_ring_class(name: &str, fillers: usize) -> *const crate::class::Class {
    crate::test_support::wide_class(name, fillers, Some(counting_destructor as *const ()))
}

/// A reference box passes an edge through: `a.child` is a box and the
/// box names `b`, so the tracer's reference arm must yield `b` for the
/// box and the box for `a`. Before that arm existed the chain stopped
/// at the box, which reads every object behind one as unreferenced.
///
/// The shape was a ring — `$a->r = &$a` — until 2026-08-26, and the
/// observable was that a collection reclaimed it. That observable died
/// with the collector; the contract it stood for is the enumeration
/// asserted here, and reclamation returns as a test of the commit stage
/// at S36 (`PLAN.md`).
#[test]
fn a_reference_box_passes_an_edge_through() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("RefRingHolder")
        .prop("child", true)
        .destructor(counting_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let r = crate::reference::ll_reference_new();
    unsafe {
        // a.child owns the box's initial ref; the box owns b's.
        Object::prop_at(a, 16).write(Value::entity(Tag::Reference, r as *mut RcHeader));
        (*r).value = Value::entity(Tag::Object, b as *mut RcHeader);
    }

    let census = unsafe { heap_census() };
    assert!(
        census.by_kind[EntityKind::Reference as usize] >= 1,
        "the box is enumerated"
    );

    let mut from_a = Vec::new();
    unsafe { trace_entity(a as *mut RcHeader, |c| from_a.push(c as usize)) };
    assert!(
        from_a.contains(&(r as usize)),
        "an object's counted slot yields the box it holds"
    );
    let mut from_box = Vec::new();
    unsafe { trace_entity(r as *mut RcHeader, |c| from_box.push(c as usize)) };
    assert_eq!(
        from_box,
        vec![b as usize],
        "the box yields exactly what it names"
    );

    unsafe {
        // Acyclic: releasing `a` frees the chain by counting alone.
        assert!(ll_release(a as *mut RcHeader));
        crate::object::ll_object_die(a);
    }
    assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2, "both destructors ran");
    let seen = walked_addresses();
    assert!(!seen.contains(&(a as usize)), "the object died");
    assert!(!seen.contains(&(r as usize)), "the box died");
    arena.reset(|_| {});
}

/// An array's out-edges are its elements **and** its string keys: the
/// table holds a reference to each string it keys on, so a walk that
/// counts only elements under-counts the in-edges of every key and
/// pins it as a root. Before the Array arm existed the walk yielded
/// nothing at all here, which is conservative but makes a ring
/// through an array uncollectable.
#[test]
fn an_array_is_traced_through_its_elements_and_its_string_keys() {
    use crate::array::table::Key;
    use crate::string::ll_string_new;
    let _g = crate::memory::block_pool::test_guard();

    let a = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
    let key = unsafe { ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, b"k") };
    let value = unsafe { ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, b"v") };
    unsafe {
        crate::array::testing::insert(
            a,
            Key::Str(key),
            Value::entity(Tag::String, value as *mut RcHeader),
        );
        // An integer key with a plain value adds no edge of its own,
        // and a hole must add none either.
        crate::array::testing::insert(a, Key::Int(7), Value::int(7));
        let _ = crate::array::testing::remove(a, Key::Int(7));
    }

    let mut seen = Vec::new();
    unsafe { trace_entity(a as *mut RcHeader, |child| seen.push(child as usize)) };

    assert!(
        seen.contains(&(key as usize)),
        "the string key is not an out-edge"
    );
    assert!(
        seen.contains(&(value as usize)),
        "the element is not an out-edge"
    );
    assert_eq!(seen.len(), 2, "a hole or an integer key produced an edge");

    unsafe {
        crate::array::entity::dispose_storage(a, crate::array::entity::category_of(a));
        // Each of the three is released before it is killed, so its
        // slot reaches the free list carrying the refcount-0 header
        // the process-global enumerators use as their occupancy test.
        // Killing at 1 leaves a freed slot that every later census in
        // the process reads as a live entity (`dev/POSTMORTEM.md`,
        // "an entity killed at refcount 1").
        assert!(ll_release(a as *mut RcHeader));
        crate::object::ll_entity_die(a as *mut RcHeader);
        assert!(ll_release(key as *mut RcHeader));
        crate::object::ll_entity_die(key as *mut RcHeader);
        assert!(ll_release(value as *mut RcHeader));
        crate::object::ll_entity_die(value as *mut RcHeader);
    }
}

/// A ring whose two nodes are both too large for any size class, one
/// in each half of the population: a pooled block of its own, which
/// the region scan reaches, and an OS-direct run, which no region
/// contains and only `large_entity`'s registry names. The walk has to
/// find both, and it has to count one slot in each — a stride would
/// read rows out of the nodes' own cells.
#[test]
fn a_cycle_through_large_entities_is_walked_and_collected() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let pooled_cls = wide_ring_class("PooledRingNode", POOLED_FILLERS);
    let run_cls = wide_ring_class("RunRingNode", RUN_FILLERS);

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let a = unsafe { new_constructed(&mut ctx, pooled_cls, MemoryCategory::GcHeap) };
    let b = unsafe { new_constructed(&mut ctx, run_cls, MemoryCategory::GcHeap) };
    unsafe {
        let kind_of = |o: *mut Object| {
            *(((o as usize) & !crate::memory::block_pool::BLOCK_MASK) as *const u32)
        };

        assert_eq!(
            kind_of(a),
            crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE,
            "the smaller node is a pooled block of its own"
        );
        assert_eq!(
            kind_of(b),
            crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE_RUN,
            "and the larger one is a run"
        );
        // Acyclic on purpose since 2026-08-26: the pair was a ring and
        // the observable was that a collection reclaimed it. The
        // contract this test carries is the enumeration below — a large
        // entity is found in whichever half of the population it landed
        // in — and it needs no collector. Reclaiming a ring of large
        // entities returns as a test of the commit stage at S36.
        tie(a, 16, b);
    }

    let seen = walked_addresses();
    assert!(
        seen.contains(&(a as usize)) && seen.contains(&(b as usize)),
        "both halves of the population are enumerated"
    );
    let mut from_a = Vec::new();
    unsafe { trace_entity(a as *mut RcHeader, |c| from_a.push(c as usize)) };
    assert!(
        from_a.contains(&(b as usize)),
        "a large entity's counted slot is an out-edge"
    );

    unsafe {
        assert!(ll_release(a as *mut RcHeader));
        crate::object::ll_object_die(a);
    }
    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        2,
        "__destruct ran for both"
    );
    let after = walked_addresses();
    assert!(!after.contains(&(a as usize)) && !after.contains(&(b as usize)));
    arena.reset(|_| {});
}
