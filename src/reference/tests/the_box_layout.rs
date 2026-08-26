//! The `&` box is an ordinary entity of kind 3, and generated code
//! reaches its Value by a fixed offset.

use super::*;

#[test]
fn a_reference_box_is_a_24_byte_kind_3_entity() {
    let _g = crate::memory::block_pool::test_guard();
    assert_eq!(size_of::<LLReference>(), 24);
    assert_eq!(core::mem::offset_of!(LLReference, rc), 0);
    assert_eq!(core::mem::offset_of!(LLReference, value), 8);

    let mut arena = Arena::new();
    let r = ll_reference_new();
    assert_eq!(unsafe { crate::refcount::entity_refcount(r) }, 1);
    assert_eq!(
        unsafe { crate::refcount::entity_flags(r) } & crate::refcount::ENTITY_KIND_MASK,
        EntityKind::Reference.to_flags()
    );
    assert!(!crate::refcount::is_object(unsafe {
        crate::refcount::entity_flags(r)
    }));
    assert_eq!(unsafe { (*r).value }.tag(), Tag::Null);

    unsafe {
        assert!(ll_release(r as *mut RcHeader));
        crate::object::ll_entity_die(r as *mut RcHeader);
    }

    arena.reset(|_| {});
}
