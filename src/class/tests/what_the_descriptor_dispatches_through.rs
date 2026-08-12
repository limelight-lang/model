//! A method keeps its slot down the hierarchy, so an override is
//! written in place rather than appended, and the destructor slot
//! inherits with it. The interface tables live in the descriptor's
//! own tail, and `instanceof` is a Cohen display read. Three of
//! these compare a function against the pointer built from it, which
//! Miri cannot model — it gives one function several addresses.

use super::*;

extern "C" fn m3() {}

extern "C" fn m2_override() {}

/// Not runnable under Miri: it compares vtable entries against the
/// functions they should hold, and Miri does not give a function a
/// single address — `m2 as *const ()` yields a different pointer at
/// the builder's cast site than at the assertion's. The dispatch
/// tables round-trip exactly (verified by reading back what was
/// written); it is function *identity* Miri cannot model. On real
/// targets a function has one address, so this runs normally under
/// `cargo test` and the contract is unweakened.
#[test]
#[cfg_attr(miri, ignore = "Miri gives one function several addresses")]
fn slots_are_stable_and_overrides_land_in_place() {
    let _g = crate::memory::block_pool::test_guard();
    let animal = base();
    let dog = ClassBuilder::new("Dog")
        .parent(animal)
        .method("eat", m2_override as *const ())
        .method("fetch", m3 as *const ())
        .build();

    let (animal_ptr, dog_ptr) = (animal, dog);
    let (animal, dog) = unsafe { (&*animal, &*dog) };
    let eat = intern_str("eat");
    let speak = intern_str("speak");

    assert_eq!(animal.find_method(eat), dog.find_method(eat), "slot stable");
    assert_eq!(animal.find_method(speak), dog.find_method(speak));

    let slot = dog.find_method(eat).unwrap() as usize;
    assert_eq!(unsafe { Class::vtbl(animal_ptr) }[slot], m2 as *const ());
    assert_eq!(
        unsafe { Class::vtbl(dog_ptr) }[slot],
        m2_override as *const (),
        "override in place"
    );
    assert_eq!(dog.vtbl_len, animal.vtbl_len + 1, "fetch appended");
}

/// Miri-ignored for the same reason as
/// [`slots_are_stable_and_overrides_land_in_place`]: function identity.
#[test]
#[cfg_attr(miri, ignore = "Miri gives one function several addresses")]
fn inherited_itable_sees_the_override() {
    let _g = crate::memory::block_pool::test_guard();
    let feedable = ClassBuilder::interface("Feedable");

    let animal = ClassBuilder::new("Animal")
        .method("eat", m2 as *const ())
        .implement(unsafe { &*feedable }, vec![0]) // interface slot 0 → vtbl slot 0 (eat)
        .build();
    let dog = ClassBuilder::new("Dog")
        .parent(animal)
        .method("eat", m2_override as *const ())
        .build();

    let id = unsafe { (*feedable).interface_id };
    let animal_it = unsafe { ll_find_itable(animal, id) };
    let dog_it = unsafe { ll_find_itable(dog, id) };
    assert!(!animal_it.is_null() && !dog_it.is_null());
    unsafe {
        assert_eq!(*animal_it, m2 as *const ());
        assert_eq!(
            *dog_it, m2_override as *const (),
            "inherited itable must be re-linked against the subclass vtable"
        );
    }

    let missing = unsafe { ll_find_itable(animal, 9999) };
    assert!(missing.is_null());
}

/// Miri-ignored for the same reason as
/// [`slots_are_stable_and_overrides_land_in_place`]: function identity.
#[test]
#[cfg_attr(miri, ignore = "Miri gives one function several addresses")]
fn itables_ride_the_descriptor_tail() {
    let _g = crate::memory::block_pool::test_guard();
    let i1 = ClassBuilder::interface("A");
    let i2 = ClassBuilder::interface("B");

    let cls = ClassBuilder::new("Train")
        .method("x", m1 as *const ())
        .method("y", m2 as *const ())
        .implement(unsafe { &*i1 }, vec![0, 1])
        .implement(unsafe { &*i2 }, vec![1])
        .build();

    let c = unsafe { &*cls };
    let tail_start = cls as usize + size_of::<Class>();
    let vtbl_end = tail_start + c.vtbl_len as usize * 8;
    let train_end = vtbl_end + 3 * 8; // 2 + 1 itable entries

    let (id1, id2) = unsafe { ((*i1).interface_id, (*i2).interface_id) };
    let t1 = unsafe { ll_find_itable(cls, id1) } as usize;
    let t2 = unsafe { ll_find_itable(cls, id2) } as usize;
    assert!(
        (vtbl_end..train_end).contains(&t1) && (vtbl_end..train_end).contains(&t2),
        "itables must live in the descriptor's own tail"
    );
    unsafe {
        assert_eq!(*(t1 as *const *const ()), m1 as *const ());
        assert_eq!(*(t2 as *const *const ()), m2 as *const ());
    }
}

#[test]
fn destructor_slot_is_tracked_through_inheritance() {
    let _g = crate::memory::block_pool::test_guard();
    let animal = ClassBuilder::new("Animal")
        .destructor(m1 as *const ())
        .build();
    let dog = ClassBuilder::new("Dog").parent(animal).build();

    let (ca, cd) = unsafe { (&*animal, &*dog) };
    assert!(ca.has_destructor());
    assert!(cd.has_destructor(), "destructor presence inherits");
    assert_eq!(ca.destruct_slot, cd.destruct_slot);
    assert_ne!(ca.destruct_slot, NO_DESTRUCT_SLOT);
}

#[test]
fn cohen_display_answers_instanceof_in_o1() {
    let _g = crate::memory::block_pool::test_guard();
    let a = base();
    let b = ClassBuilder::new("Dog").parent(a).build();
    let c = ClassBuilder::new("Puppy").parent(b).build();
    let other = ClassBuilder::new("Rock").build();

    let (ca, cb, cc, co) = unsafe { (&*a, &*b, &*c, &*other) };
    assert!(cc.instance_of_class(ca));
    assert!(cc.instance_of_class(cb));
    assert!(cc.instance_of_class(cc));
    assert!(cb.instance_of_class(ca));
    assert!(!ca.instance_of_class(cb), "parent is not a child");
    assert!(!co.instance_of_class(ca));
    assert_eq!(cc.display_len, 3);
}
