//! The direct sweep frees the garbage no destructor and no weak cell can reach
//! without the finalization chain; the garbage a destructor can reach goes the
//! chain's way, whole while its destructors run; and a swept ring lets go of
//! what it held once its members are freed.

use super::*;

/// Destructor bodies run by this file's objects since a case last cleared it.
static RUNS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_destructor(_object: *mut Object) {
    RUNS.fetch_add(1, Ordering::Relaxed);
}

/// The members a [`checking_destructor`] reads, and how many of them it found
/// freed.
static REACHED: [AtomicPtr<Object>; 2] = [
    AtomicPtr::new(std::ptr::null_mut()),
    AtomicPtr::new(std::ptr::null_mut()),
];
static FOUND_FREED: AtomicUsize = AtomicUsize::new(0);

/// Count every member of [`REACHED`] whose slot no longer holds it.
unsafe extern "C" fn checking_destructor(_object: *mut Object) {
    RUNS.fetch_add(1, Ordering::Relaxed);
    for reached in &REACHED {
        let member = reached.load(Ordering::Relaxed);
        if !member.is_null() && !live(member) {
            FOUND_FREED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// A class with the ring's property at `prop_offset(0)` and a second counted
/// one at `prop_offset(1)`.
fn holding_class(name: &str) -> *const Class {
    ClassBuilder::new(name)
        .prop("next", true)
        .prop("held", true)
        .build()
}

#[test]
fn a_ring_without_destructors_is_swept() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let class = node_class("TbSweptRing", std::ptr::null());
    let members = unsafe { ring(&mut arena, [class; 3]) };

    let trace = one_trace();

    assert!(
        members.iter().all(|&member| !live(member)),
        "every member was freed"
    );
    assert!(trace.swept >= 3, "by the sweep: {trace:?}");
}

#[test]
fn a_ring_a_destructor_reaches_stands_whole_while_the_destructor_runs() {
    let _g = test_guard();
    RUNS.store(0, Ordering::Relaxed);
    FOUND_FREED.store(0, Ordering::Relaxed);
    let mut arena = Arena::new();
    let checking = node_class("TbCheckingHead", checking_destructor as *const ());
    let plain = node_class("TbReachedPlain", std::ptr::null());
    let members = unsafe { ring(&mut arena, [checking, plain, plain]) };
    REACHED[0].store(members[1], Ordering::Relaxed);
    REACHED[1].store(members[2], Ordering::Relaxed);

    let _ = one_trace();

    assert_eq!(RUNS.load(Ordering::Relaxed), 1, "the destructor ran");
    assert_eq!(
        FOUND_FREED.load(Ordering::Relaxed),
        0,
        "no member it reaches was freed before it ran"
    );
    assert!(
        members.iter().all(|&member| !live(member)),
        "the ring was freed after it"
    );
    for reached in &REACHED {
        reached.store(std::ptr::null_mut(), Ordering::Relaxed);
    }
}

#[test]
fn a_swept_ring_lets_go_of_a_live_object_it_holds() {
    let _g = test_guard();
    RUNS.store(0, Ordering::Relaxed);
    let mut arena = Arena::new();
    let holding = holding_class("TbSweptHolder");
    let plain = node_class("TbSweptMate", std::ptr::null());
    let members = unsafe { ring(&mut arena, [holding, plain]) };
    let held = object(
        &mut arena,
        node_class("TbHeldByTheSwept", counting_destructor as *const ()),
    );
    unsafe { store_prop(&mut arena, members[0], prop_offset(1), held) };

    let trace = one_trace();

    assert!(
        members.iter().all(|&member| !live(member)),
        "the ring was freed"
    );
    assert!(trace.swept >= 2, "by the sweep: {trace:?}");
    assert!(live(held), "the local's reference keeps the held object");
    assert_eq!(
        unsafe { crate::refcount::header_refcount(held as *mut RcHeader) },
        1,
        "the ring's reference was let go"
    );
    assert_eq!(
        unsafe { crate::refcount::trace_field_load(held as *mut RcHeader) },
        0,
        "the trace left the survivor's field clear"
    );
    assert_eq!(RUNS.load(Ordering::Relaxed), 0);

    unsafe {
        if ll_release(held as *mut RcHeader) {
            crate::object::ll_entity_die(held as *mut RcHeader);
        }
    }
    assert_eq!(RUNS.load(Ordering::Relaxed), 1);
}

#[test]
fn an_object_with_a_destructor_held_only_by_a_swept_ring_is_destructed_once_and_freed() {
    let _g = test_guard();
    RUNS.store(0, Ordering::Relaxed);
    let mut arena = Arena::new();
    let holding = holding_class("TbSweptOverDestructor");
    let plain = node_class("TbSweptOverMate", std::ptr::null());
    let members = unsafe { ring(&mut arena, [holding, plain]) };
    let held = object(
        &mut arena,
        node_class("TbDestructedBehind", counting_destructor as *const ()),
    );
    unsafe { store_prop(&mut arena, members[0], prop_offset(1), held) };
    assert!(!unsafe { ll_release(held as *mut RcHeader) });

    let _ = one_trace();

    assert!(
        members.iter().all(|&member| !live(member)),
        "the ring was freed"
    );
    assert!(!live(held), "the object it held was freed");
    assert_eq!(RUNS.load(Ordering::Relaxed), 1, "and destructed once");
}

#[test]
fn a_weakly_referenced_ring_has_its_cell_nulled_and_is_freed() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let class = node_class("TbWeaklyHeldRing", std::ptr::null());
    let members = unsafe { ring(&mut arena, [class; 2]) };
    let weak = {
        let mut context = LLContext { arena: &mut arena };
        unsafe { crate::weak::ll_weakref_create(&mut context, members[0] as *mut RcHeader) }
    };

    let _ = one_trace();

    assert!(
        members.iter().all(|&member| !live(member)),
        "the ring was freed"
    );
    assert!(
        unsafe { crate::weak::ll_weakref_get(weak) }.is_null(),
        "its weak cell reads null"
    );
    unsafe {
        if ll_release(weak as *mut RcHeader) {
            crate::object::ll_entity_die(weak as *mut RcHeader);
        }
    }
}

/// The member the resurrecting destructor kept.
static KEPT: AtomicPtr<Object> = AtomicPtr::new(std::ptr::null_mut());

unsafe extern "C" fn resurrecting_destructor(object: *mut Object) {
    unsafe { ll_retain(object as *mut RcHeader) };
    KEPT.store(object, Ordering::Relaxed);
}

#[test]
fn a_resurrected_component_is_left_with_its_field_clear() {
    let _g = test_guard();
    KEPT.store(std::ptr::null_mut(), Ordering::Relaxed);
    let mut arena = Arena::new();
    let resurrecting = node_class("TbResurrectedClear", resurrecting_destructor as *const ());
    let plain = node_class("TbResurrectedMate", std::ptr::null());
    let kept = unsafe { ring(&mut arena, [resurrecting, plain]) };

    let _ = one_trace();

    assert!(
        kept.iter().all(|&member| live(member)),
        "the component stands"
    );
    assert!(
        kept.iter().all(|&member| unsafe {
            crate::refcount::trace_field_load(member as *mut RcHeader)
        } == 0),
        "with every member's field clear"
    );

    unsafe { ll_release(kept[0] as *mut RcHeader) };
    KEPT.store(std::ptr::null_mut(), Ordering::Relaxed);
    let _ = one_trace();
    assert!(
        kept.iter().all(|&member| !live(member)),
        "let go, the next trace frees it"
    );
}
