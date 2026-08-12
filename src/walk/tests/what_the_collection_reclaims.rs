//! A ring with no external reference is what no refcount path can
//! reclaim and the whole of this collector's job; two rings joined
//! by one edge are a single weakly-connected component and die
//! together with the acyclic subtree they hold. Neither an object
//! nor a candidate buffer has to appear anywhere in the ring: this
//! walk finds it from the entity blocks alone.

use super::*;

/// A pure two-object ring with no external references is exactly what
/// no refcount path can reclaim — and the whole of this collector's
/// job. No candidate buffer is involved: the walk finds it from the
/// entity blocks alone (the leak-detector property).
#[test]
fn a_pure_cycle_is_collected_and_destructed() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("RingNode")
        .prop("child", true)
        .destructor(counting_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe {
        tie(a, 16, b);
        tie(b, 16, a);
    }

    let stats = unsafe { collect_cycles() };
    assert!(stats.collected >= 2, "the ring is garbage");
    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        2,
        "__destruct ran for both"
    );
    let seen = walked_addresses();
    assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
    arena.reset(|_| {});
}

/// The smallest cycle: an object holding itself. Doubles as DC5's
/// runtime-side witness: the test body's raw pointer is exactly an
/// uncounted borrow, and it does not root — the compiler obligation
/// (`rfc/model/memory/static-lifetimes.md`, "What may own a borrow")
/// is the only thing that makes such borrows legal.
#[test]
fn a_self_loop_is_collected() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("SelfLoop").prop("child", true).build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe { tie(a, 16, a) };

    unsafe { collect_cycles() };
    assert!(!walked_addresses().contains(&(a as usize)));
    arena.reset(|_| {});
}

/// Two rings joined by one edge are ONE weakly-connected component
/// and die together in a single collection, along with a hanging
/// acyclic subtree the ring holds (its counts balance inside the
/// component, so the exact test covers it too).
#[test]
fn a_garland_and_its_hanging_subtree_die_as_one_unit() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("GarlandNode")
        .prop("child", true)
        .prop("link", true)
        .destructor(counting_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let mk = |ctx: &mut LLContext| unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
    let (a, b, c, d, leaf) = (
        mk(&mut ctx),
        mk(&mut ctx),
        mk(&mut ctx),
        mk(&mut ctx),
        mk(&mut ctx),
    );
    unsafe {
        tie(a, 16, b);
        tie(b, 16, a); // ring 1
        tie(c, 16, d);
        tie(d, 16, c); // ring 2
        crate::refcount::ll_retain(c as *mut RcHeader);
        tie(b, 32, c); // garland link: c now held by d's slot and b's slot
        tie(d, 32, leaf); // hanging subtree off ring 2
    }

    let stats = unsafe { collect_cycles() };
    assert!(stats.collected >= 5, "both rings and the leaf died");
    assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 5);
    let seen = walked_addresses();
    for &o in &[a, b, c, d, leaf] {
        assert!(!seen.contains(&(o as usize)));
    }

    arena.reset(|_| {});
}

/// A ring with no object anywhere in it. Phase 1 traced both arrays
/// and Phase 2 judged the component garbage; what was missing was the
/// arm that severs it, so the drain guarded the two members, severed
/// nothing, and un-guarded them back to the counts they started at.
/// The failure was not a crash and not a leak the next pass finds: the
/// collector reported a confirmed component and freed none of it, and
/// repeated the whole walk, judge, guard and un-guard on the same ring
/// on every later call. Seen failing at `collected: 0`.
#[test]
fn a_ring_of_two_arrays_and_no_object_is_collected() {
    use crate::array::entity::ll_array_new;
    use crate::refcount::{ll_release, ll_retain};
    let _g = crate::memory::block_pool::test_guard();

    // Both arrays are what the factory stamps, the mixed vector, so the
    // walk and the sever are exercised over the representation a fresh
    // array has. The hash's arms are `the_children_a_kind_has`.
    let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let b = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    unsafe {
        // Retained before the entry is published, per `Vector::push`.
        ll_retain(b as *mut RcHeader);
        assert!(crate::array::testing::push(
            a,
            Value::entity(Tag::Array, b as *mut RcHeader)
        ));
        ll_retain(a as *mut RcHeader);
        assert!(crate::array::testing::push(
            b,
            Value::entity(Tag::Array, a as *mut RcHeader)
        ));
        // Drop the creation references: each array is now held by the
        // other and by nothing else, which is the ring.
        assert!(!ll_release(a as *mut RcHeader), "a is still held by b");
        assert!(!ll_release(b as *mut RcHeader), "b is still held by a");
    }

    let stats = unsafe { collect_cycles() };
    // At least one, not exactly one: `collect_cycles` measures the
    // whole process and the tests share it, so an exact count is a
    // claim about every other test rather than about this ring. The
    // proof that severing happened is `collected` below, which is
    // what read zero before the arm existed.
    assert!(
        stats.candidate_components >= 1,
        "the ring was not judged garbage, so this proves nothing about severing"
    );
    assert!(
        stats.collected >= 2,
        "the component was confirmed and then not freed"
    );
    let seen = walked_addresses();
    assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
}

/// The rc-walk twin of
/// `gc::tests::a_ring_with_no_object_in_it::a_ring_whose_last_release_lands_on_a_reference_box_is_collected`:
/// `$a[0] = &$a`, with the ring's only external hold landing on the
/// box. This walk computes its roots and buffers no candidates, so
/// which kind took the last decrement cannot reach it. The twin runs
/// to check that independence rather than assume it.
#[test]
fn a_ring_through_a_reference_box_and_an_array_is_collected() {
    use crate::array::entity::ll_array_new;
    use crate::refcount::{ll_release, ll_retain};
    let _g = crate::memory::block_pool::test_guard();

    let array = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let boxed = crate::reference::ll_reference_new();
    unsafe {
        // The box takes the array's creation reference, as `&$a` does.
        (*boxed).value = Value::entity(Tag::Array, array as *mut RcHeader);
        // Retained before the entry is published, per `Vector::push`.
        ll_retain(boxed as *mut RcHeader);
        assert!(crate::array::testing::push(
            array,
            Value::entity(Tag::Reference, boxed as *mut RcHeader)
        ));
        assert!(
            !ll_release(boxed as *mut RcHeader),
            "the box is still held by the array's element"
        );
    }

    let stats = unsafe { collect_cycles() };
    assert!(
        stats.candidate_components >= 1,
        "the ring was not judged garbage, so this proves nothing about severing"
    );
    assert!(
        stats.collected >= 2,
        "the component was confirmed and then not freed"
    );
    let seen = walked_addresses();
    assert!(!seen.contains(&(array as usize)) && !seen.contains(&(boxed as usize)));
}
