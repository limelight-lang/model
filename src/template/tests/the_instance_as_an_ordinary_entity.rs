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
            unsafe { crate::refcount::entity_refcount(s) },
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
        assert_eq!(unsafe { crate::refcount::entity_refcount(s) }, 1);
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
        assert_eq!(unsafe { crate::refcount::entity_refcount(s) }, 3);

        assert!(
            unsafe { ll_release(t as *mut RcHeader) },
            "the last reference to the template is its own death"
        );
        unsafe { crate::object::ll_entity_die(t as *mut RcHeader) };
        assert_eq!(
            unsafe { crate::refcount::entity_refcount(s) },
            2,
            "the template's own reference went back"
        );
        unsafe {
            ll_release(s as *mut RcHeader);
            ll_release(s as *mut RcHeader);
        }
    });
}

/// A refused store frees memory the factory never published, and that free
/// needs the slot handed back first: the head of `ll_free` took it, and a free
/// of a slot `ll_free` still holds is read as a repeat
/// (`crate::refcount::DEAD_IN_PLACE`). Without the clear the instance's slot
/// stays out of circulation for the life of the process.
///
/// **Two premises, and the case pins both.** The slot the factory draws must
/// be one a free has taken, or the clear is a no-op over a virgin slot and
/// nothing is exercised: the class's virgin space is drained through a
/// reservation and given straight back, which leaves the class free lists and
/// no tail. And the build must fail at the store rather than at its own
/// allocation, or no free is reached at all: an allocation of the same class
/// is made under the same forced refusal and must succeed.
///
/// The refusal itself is the one the barrier has: an arena COW string stored
/// into a `GcHeap` template is copied out of the arena, and that copy is an
/// allocation the pool can refuse.
#[test]
fn a_refused_store_gives_the_instances_slot_back() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("InterpolatedString").template().build();
    let shape = shape_of(&["", ""]);

    with_ctx(|ctx| {
        let payload = vec![b'x'; crate::memory::heap::MAX_SMALL + 16];
        let s = unsafe { ll_string_new(ctx, MemoryCategory::RequestArena, &payload) };
        assert!(!s.is_null());
        let held = [Value::entity(Tag::String, s as *mut RcHeader)];

        let size = VALUES_OFFSET + size_of::<Value>();
        let mut drained = vec![std::ptr::null_mut::<u8>(); 4096];
        let mut contiguous = 0usize;
        let n = unsafe {
            crate::memory::heap::ll_entity_reserve(
                size,
                4096,
                drained.as_mut_ptr(),
                &raw mut contiguous,
            )
        };
        assert!(n > 1, "the class served nothing to drain; got {n}");
        unsafe { crate::memory::heap::ll_entity_cells_return(drained.as_ptr(), n) };

        let oom = crate::memory::block_pool::force_oom();
        let probe = unsafe { crate::memory::heap::entity_alloc(size) };
        assert!(
            !probe.is_null(),
            "the drained class serves under the forced refusal, so the build's \
             own allocation is not what fails below"
        );
        assert_eq!(
            unsafe { crate::refcount::slot_state(probe as *const RcHeader) },
            crate::refcount::SlotState::DeadInPlace,
            "and it serves a slot a free has taken, which is what the build's \
             own free has to hand back"
        );
        unsafe { crate::refcount::clear_dead_in_place(probe as *mut RcHeader) };
        unsafe { crate::memory::stdapi::ll_free(probe) };
        let _ = crate::memory::stdapi::take_refused_frees();

        let t = unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::GcHeap) };
        drop(oom);
        assert!(t.is_null(), "the escape copy was refused, so the build was");
        assert_eq!(
            crate::memory::stdapi::take_refused_frees(),
            0,
            "the refused build handed its own slot back before it freed it"
        );
    });
}
