use super::*;
use crate::class::ClassBuilder;
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::new_constructed;
use crate::refcount::{DESTRUCTOR_PENDING, ll_release};
use crate::value::Tag;

/// The `&` box is an ordinary entity of kind 3, and generated code
/// reaches its Value by a fixed offset.
mod the_box_layout {
    use super::*;

    #[test]
    fn a_reference_box_is_a_24_byte_kind_3_entity() {
        let _g = crate::memory::block_pool::test_guard();
        assert_eq!(size_of::<LLReference>(), 24);
        assert_eq!(core::mem::offset_of!(LLReference, rc), 0);
        assert_eq!(core::mem::offset_of!(LLReference, value), 8);

        let mut arena = Arena::new();
        let r = ll_reference_new();
        let rc = unsafe { &(*r).rc };
        assert_eq!(rc.refcount, 1);
        assert_eq!(
            rc.flags & crate::refcount::ENTITY_KIND_MASK,
            EntityKind::Reference.to_flags()
        );
        assert!(!crate::refcount::is_object(rc.flags));
        assert_eq!(unsafe { (*r).value }.tag(), Tag::Null);

        unsafe {
            assert!(ll_release(r as *mut RcHeader));
            crate::object::ll_entity_die(r as *mut RcHeader);
        }

        arena.reset(|_| {});
    }
}

/// A box holds one counted child, so its own death is a release that
/// cascades into the ordinary teardown, destructor included.
mod the_referent_at_death {
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
        assert_ne!(unsafe { (*obj).rc.flags } & DESTRUCTOR_PENDING, 0);
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
}
