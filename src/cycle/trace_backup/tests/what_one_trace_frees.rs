//! A garbage ring is destructed and freed by one trace; an object a destructor
//! allocates and stores in a global outlives that trace and the next; and a
//! destructor that resurrects its own member keeps its own component while the
//! trace frees the others.

use super::*;

/// Destructor bodies run by this file's rings since a case last cleared it.
static RUNS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_destructor(_object: *mut Object) {
    RUNS.fetch_add(1, Ordering::Relaxed);
}

#[test]
fn a_garbage_two_ring_with_destructors_is_destructed_and_freed_by_one_trace() {
    let _g = test_guard();
    RUNS.store(0, Ordering::Relaxed);
    let mut arena = Arena::new();
    let class = node_class("TbGarbageTwoRing", counting_destructor as *const ());
    let members = unsafe { ring(&mut arena, [class; 2]) };

    let trace = one_trace();

    assert_eq!(RUNS.load(Ordering::Relaxed), 2, "both destructors ran");
    assert!(
        members.iter().all(|&member| !live(member)),
        "both members were freed"
    );
    assert!(
        trace.freed >= 2 && trace.components >= 1,
        "the trace counted them: {trace:?}"
    );
}

/// The object the allocating destructor stored, holding its creation
/// reference.
static BORN: AtomicPtr<Object> = AtomicPtr::new(std::ptr::null_mut());
/// The class it is built from, set by the case before the trace.
static BORN_CLASS: AtomicPtr<Class> = AtomicPtr::new(std::ptr::null_mut());

/// Allocate one object and store it in [`BORN`], once per case: the global is
/// the only reference it has.
unsafe extern "C" fn allocating_destructor(_object: *mut Object) {
    if !BORN.load(Ordering::Relaxed).is_null() {
        return;
    }

    let mut arena = Arena::new();
    let born = object(&mut arena, BORN_CLASS.load(Ordering::Relaxed));
    BORN.store(born, Ordering::Relaxed);
}

#[test]
fn an_object_a_destructor_allocates_and_stores_in_a_global_survives() {
    let _g = test_guard();
    BORN.store(std::ptr::null_mut(), Ordering::Relaxed);
    BORN_CLASS.store(
        node_class("TbBornInADestructor", std::ptr::null()) as *mut Class,
        Ordering::Relaxed,
    );
    let mut arena = Arena::new();
    let class = node_class("TbAllocatingRing", allocating_destructor as *const ());
    let members = unsafe { ring(&mut arena, [class; 2]) };

    let _ = one_trace();
    let born = BORN.load(Ordering::Relaxed);
    assert!(!born.is_null(), "a destructor ran and allocated");
    assert!(
        members.iter().all(|&member| !live(member)),
        "the ring was freed"
    );
    assert!(live(born), "the object born in the trace stands");
    assert_eq!(
        unsafe { crate::refcount::header_refcount(born as *mut RcHeader) },
        1,
        "the global's reference is its only one"
    );

    let _ = one_trace();
    assert!(
        live(born),
        "a trace that walks it reads the global's reference as a root"
    );

    unsafe {
        if ll_release(born as *mut RcHeader) {
            crate::object::ll_entity_die(born as *mut RcHeader);
        }
    }
    BORN.store(std::ptr::null_mut(), Ordering::Relaxed);
}

/// The member the resurrecting destructor kept, holding the reference it took.
static KEPT: AtomicPtr<Object> = AtomicPtr::new(std::ptr::null_mut());

/// Store `$this` in [`KEPT`] with a counted reference of its own.
unsafe extern "C" fn resurrecting_destructor(object: *mut Object) {
    unsafe { ll_retain(object as *mut RcHeader) };
    KEPT.store(object, Ordering::Relaxed);
}

#[test]
fn a_resurrecting_destructor_keeps_its_own_component_and_the_others_are_freed() {
    let _g = test_guard();
    RUNS.store(0, Ordering::Relaxed);
    KEPT.store(std::ptr::null_mut(), Ordering::Relaxed);
    let mut arena = Arena::new();
    let resurrecting = node_class("TbResurrecting", resurrecting_destructor as *const ());
    let counting = node_class("TbFreedBeside", counting_destructor as *const ());
    let kept = unsafe { ring(&mut arena, [resurrecting, counting]) };
    let freed = unsafe { ring(&mut arena, [counting; 2]) };
    let also_freed = unsafe { ring(&mut arena, [counting; 3]) };

    let trace = one_trace();

    assert_eq!(KEPT.load(Ordering::Relaxed), kept[0], "the destructor ran");
    assert!(
        kept.iter().all(|&member| live(member)),
        "the resurrected component stands whole"
    );
    assert!(
        freed.iter().chain(&also_freed).all(|&member| !live(member)),
        "every other component was freed"
    );
    assert_eq!(
        RUNS.load(Ordering::Relaxed),
        6,
        "every member destructed once, the resurrected component's among them"
    );
    assert!(trace.components >= 3, "three components: {trace:?}");

    // Let go, the kept ring is garbage again, and its destructors are behind
    // it: the next trace frees it.
    unsafe { ll_release(kept[0] as *mut RcHeader) };
    KEPT.store(std::ptr::null_mut(), Ordering::Relaxed);
    let _ = one_trace();
    assert!(
        kept.iter().all(|&member| !live(member)),
        "the ring let go was freed by the next trace"
    );
    assert_eq!(RUNS.load(Ordering::Relaxed), 6, "no destructor ran twice");
}
