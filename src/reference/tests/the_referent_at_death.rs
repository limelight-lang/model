//! A box holds one counted child, so its own death is a release that
//! cascades into the ordinary teardown, destructor included.

use super::*;

/// The kind switch at death: a dying box releases the entity behind
/// its Value, cascading the ordinary teardown (destructor included).
#[test]
fn a_dying_reference_releases_its_referent() {
    let _g = crate::memory::block_pool::test_guard();
    use std::sync::atomic::{AtomicUsize, Ordering};
    static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn counting(_o: *mut crate::object::Object) {
        DESTRUCTS.fetch_add(1, Ordering::Relaxed);
    }

    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("Referent")
        .destructor(counting as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    assert_ne!(
        unsafe { crate::refcount::entity_flags(obj) } & DESTRUCTOR_PENDING,
        0
    );
    let r = ll_reference_new();
    // The box's slot takes over the object's initial reference.
    unsafe { (*r).value = Value::entity(Tag::Object, obj as *mut RcHeader) };

    unsafe {
        assert!(ll_release(r as *mut RcHeader));
        crate::object::ll_entity_die(r as *mut RcHeader);
    }

    assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 1, "referent cascaded");
    arena.reset(|_| {});
}
