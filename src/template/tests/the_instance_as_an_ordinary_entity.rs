//! The value count is the instance's rather than the class's, so
//! every walker reads it from the shape: the read-only walk finds
//! the values, the death releases them, and the drain's sever
//! reaches them by lvalue, which the walk cannot do.

use super::*;

/// The values are the instance's, and the collector reaches them
/// through the shape — the class has no runs to reach them by, and one
/// class serves every site, so a class-driven walk would find nothing.
#[test]
fn the_walker_sees_a_templates_values() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("InterpolatedString").template().build();
    let shape = shape_of(&["id = ", ""]);

    with_ctx(|ctx| {
        let s = unsafe { ll_string_new(ctx, MemoryCategory::GcHeap, b"abc") };
        let held = [Value::entity(Tag::String, s as *mut RcHeader)];
        // GcHeap on both sides, so teardown is what gives the
        // reference back: an arena template's heap child is released
        // by the arena's release-at-reset record instead, and that
        // path says nothing about this walk.
        let t = unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::GcHeap) };
        assert!(!t.is_null());
        assert_eq!(
            unsafe { (*(s as *mut RcHeader)).refcount },
            2,
            "the template took its own reference, the caller kept its own"
        );

        let mut seen = Vec::new();
        unsafe {
            crate::object::for_each_counted_child(t as *mut crate::object::Object, |c| seen.push(c))
        };

        assert_eq!(seen, vec![s as *mut RcHeader], "the value is the one child");

        // Teardown gives that reference back.
        // Release reports the death; tearing down is the caller's,
        // exactly as the store barrier's `drop` does it.
        assert!(
            unsafe { ll_release(t as *mut RcHeader) },
            "the template dies"
        );
        unsafe { crate::object::ll_entity_die(t as *mut RcHeader) };
        assert_eq!(unsafe { (*(s as *mut RcHeader)).refcount }, 1);
        unsafe { ll_release(s as *mut RcHeader) };
    });
}

/// The instance is an ordinary entity, so a reference held in it is
/// released when it dies — and the retain/release pair below is the
/// whole ownership contract of the factory.
#[test]
fn a_dying_template_releases_what_it_held() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("InterpolatedString").template().build();
    let shape = shape_of(&["", ""]);

    with_ctx(|ctx| {
        let s = unsafe { ll_string_new(ctx, MemoryCategory::GcHeap, b"held") };
        unsafe { ll_retain(s as *mut RcHeader) };
        let held = [Value::entity(Tag::String, s as *mut RcHeader)];
        let t = unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::GcHeap) };
        assert_eq!(unsafe { (*(s as *mut RcHeader)).refcount }, 3);

        assert!(
            unsafe { ll_release(t as *mut RcHeader) },
            "the last reference to the template is its own death"
        );
        unsafe { crate::object::ll_entity_die(t as *mut RcHeader) };
        assert_eq!(
            unsafe { (*(s as *mut RcHeader)).refcount },
            2,
            "the template's own reference went back"
        );
        unsafe {
            ll_release(s as *mut RcHeader);
            ll_release(s as *mut RcHeader);
        }
    });
}

/// A ring that runs through a template is garbage like any other, and
/// the drain's walker is a second place that has to find a template's
/// values: it severs them by lvalue, which the read-only walk cannot
/// do.
#[test]
fn a_ring_through_a_template_is_collected() {
    let _g = crate::memory::block_pool::test_guard();
    let holder_class = ClassBuilder::new("Holder").prop("t", true).build();
    let cls = ClassBuilder::new("InterpolatedString").template().build();
    let shape = shape_of(&["", ""]);

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let holder =
        unsafe { crate::object::new_constructed(&mut ctx, holder_class, MemoryCategory::GcHeap) };

    let held = [Value::entity(Tag::Object, holder as *mut RcHeader)];
    let t = unsafe { ll_template_new(&mut ctx, cls, &*shape, &held, MemoryCategory::GcHeap) };
    // Close the ring: the holder takes the template, the template
    // already holds the holder.
    unsafe {
        crate::object::Object::prop_at(holder, 16)
            .write(Value::entity(Tag::Object, t as *mut RcHeader));
        crate::refcount::ll_retain(t as *mut RcHeader);
        ll_release(t as *mut RcHeader);
        // Every count in the ring must now be a heap edge and nothing
        // else — a test's local pointer is not a root here, so the
        // creation reference has to go or the ring reads as live.
        ll_release(holder as *mut RcHeader);
    }

    unsafe { crate::walk::collect_cycles() };
    let mut alive = Vec::new();
    unsafe { crate::memory::heap::for_each_entity_slot(|e| alive.push(e as usize)) };
    assert!(
        !alive.contains(&(t as usize)) && !alive.contains(&(holder as usize)),
        "a ring through a template outlived a whole-heap collection"
    );
    arena.reset(|_| {});
}
