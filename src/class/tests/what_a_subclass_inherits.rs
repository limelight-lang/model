//! What a descriptor built for a subclass takes from its parent, and
//! what it deliberately does not: the group that reaches cells outside
//! the object's own body is inherited, and a specialized teardown is not.
//!
//! The asymmetry is the subject. A specialized `dispose` releases the
//! slots of the class that declared it, and a subclass has more, so
//! handing one down would leak every slot the subclass added. What a
//! subclass needs is the default teardown, which strides the full runs it
//! inherited and frees the group's storage as its last act — which is why
//! the group has to be inherited for that to be safe.

use super::*;
use crate::cells::Cell;
use crate::cells::OutsideCells;
use crate::cells::OutsideRead;

const PROBE_VERSION: usize = 0xC0FFEE;

/// A group whose members are distinguishable from any other's. Identity
/// is what most of these tests read, not behaviour — S18.3 is where a
/// group is made to work.
static PROBE: OutsideCells = OutsideCells {
    walk_plain: probe_walk,
    walk_relaxed: probe_walk_racing,
    recheck: probe_recheck,
    sever: probe_sever,
    free: probe_free,
    carry: probe_carry,
};

/// A group whose cells sit in storage nothing replaces.
static UNVERSIONED: OutsideCells = OutsideCells {
    walk_plain: unversioned_walk,
    walk_relaxed: unversioned_walk_racing,
    recheck: probe_recheck,
    sever: probe_sever,
    free: probe_free,
    carry: probe_carry,
};

/// A group whose racing walk gives the entity up. Only the racing one
/// can: `walk_plain`'s return type has no variant for it, which is the
/// point of the two members having different types.
static QUITTER: OutsideCells = OutsideCells {
    walk_plain: unversioned_walk,
    walk_relaxed: quitter_walk,
    recheck: probe_recheck,
    sever: probe_sever,
    free: probe_free,
    carry: probe_carry,
};

unsafe fn probe_walk(_: *mut u8, _: *const Class, _: &mut dyn FnMut(Cell)) -> Option<usize> {
    Some(PROBE_VERSION)
}

unsafe fn probe_walk_racing(_: *mut u8, _: *const Class, _: &mut dyn FnMut(Cell)) -> OutsideRead {
    OutsideRead::Version(PROBE_VERSION)
}

unsafe fn unversioned_walk(_: *mut u8, _: *const Class, _: &mut dyn FnMut(Cell)) -> Option<usize> {
    None
}

unsafe fn unversioned_walk_racing(
    _: *mut u8,
    _: *const Class,
    _: &mut dyn FnMut(Cell),
) -> OutsideRead {
    OutsideRead::NoStorage
}

unsafe fn quitter_walk(_: *mut u8, _: *const Class, _: &mut dyn FnMut(Cell)) -> OutsideRead {
    OutsideRead::GaveUp
}

unsafe fn probe_recheck(_: *mut u8, _: *const Class, walked: usize) -> bool {
    walked == PROBE_VERSION
}

unsafe fn probe_sever(
    _: *mut crate::refcount::RcHeader,
    _: &mut Vec<*mut crate::refcount::RcHeader>,
) {
}

unsafe fn probe_free(_: *mut crate::refcount::RcHeader) {}

unsafe fn probe_carry(
    _: *mut crate::memory::arena::Arena,
    _: *mut crate::refcount::RcHeader,
) -> crate::cells::OutsideCarry {
    crate::cells::OutsideCarry::Nothing
}

/// A stand-in with the real signature. `Class::dispose` is transmuted to
/// `unsafe extern "C" fn(*mut Object) -> bool`, so a body returning
/// nothing would leave the caller branching on whatever the register
/// held — and that branch decides whether the object is freed.
unsafe extern "C" fn own_dispose(obj: *mut crate::object::Object) -> bool {
    unsafe { crate::object::ll_default_dispose(obj) }
}

/// The group is on the parent and the subclass declares none of its own.
#[test]
fn a_subclass_inherits_the_group_and_the_flag_that_finds_it() {
    let _g = crate::memory::block_pool::test_guard();

    let parent = ClassBuilder::new("Coroutine").outside_cells(&PROBE).build();
    assert!(!parent.is_null(), "immortal region refused the parent");
    let child = ClassBuilder::new("Timer")
        .parent(parent)
        .prop("at", false)
        .build();
    assert!(!child.is_null(), "immortal region refused the subclass");

    unsafe {
        assert_ne!(
            (*child).flags & CLASS_OUTSIDE_CELLS,
            0,
            "the subclass lost the flag, so nothing would look for the group"
        );
        assert_eq!(
            (*child).outside,
            (*parent).outside,
            "the subclass got a different group from its parent's"
        );
        assert!(
            Class::outside_cells(child).is_some(),
            "the flag and the pointer disagree"
        );
    }
}

/// The other half of the asymmetry, and the one that would be a leak if
/// it went the other way: a subclass adds slots, so it keeps the default
/// teardown, which strides the runs it inherited rather than only its
/// parent's.
#[test]
#[cfg_attr(miri, ignore = "Miri gives one function several addresses")]
fn a_subclass_does_not_inherit_a_specialized_dispose() {
    let _g = crate::memory::block_pool::test_guard();

    let parent = ClassBuilder::new("Session")
        .dispose(own_dispose as *const ())
        .prop("conn", true)
        .build();
    assert!(!parent.is_null(), "immortal region refused the parent");
    let child = ClassBuilder::new("AdminSession")
        .parent(parent)
        .prop("audit", true)
        .build();
    assert!(!child.is_null(), "immortal region refused the subclass");

    unsafe {
        assert_eq!(
            (*child).dispose,
            crate::object::ll_default_dispose as *const (),
            "the subclass took its parent's teardown, which knows nothing of `audit`"
        );
        assert_eq!(
            (*parent).dispose,
            own_dispose as *const (),
            "the parent lost its own"
        );
    }
}

/// Declaring either keeps it, and neither reaches back up to the parent.
#[test]
#[cfg_attr(miri, ignore = "Miri gives one function several addresses")]
fn a_subclass_that_declares_its_own_keeps_it() {
    let _g = crate::memory::block_pool::test_guard();

    let parent = ClassBuilder::new("Base").build();
    assert!(!parent.is_null(), "immortal region refused the parent");
    let child = ClassBuilder::new("Derived")
        .parent(parent)
        .dispose(own_dispose as *const ())
        .outside_cells(&PROBE)
        .build();
    assert!(!child.is_null(), "immortal region refused the subclass");

    unsafe {
        assert_eq!((*child).dispose, own_dispose as *const ());
        assert_ne!((*child).flags & CLASS_OUTSIDE_CELLS, 0);
        assert_eq!(
            (*parent).flags & CLASS_OUTSIDE_CELLS,
            0,
            "the parent gained a flag from its own subclass"
        );
    }
}

/// The version the class answers reaches the caller, and a class whose
/// storage nothing replaces answers none.
#[test]
fn the_version_a_class_answers_reaches_the_walk() {
    let _g = crate::memory::block_pool::test_guard();

    let fixed = ClassBuilder::new("Fixed")
        .outside_cells(&UNVERSIONED)
        .build();
    let moving = ClassBuilder::new("Moving").outside_cells(&PROBE).build();
    assert!(
        !fixed.is_null() && !moving.is_null(),
        "immortal region refused a class"
    );

    let mut arena = crate::memory::arena::Arena::new();
    let mut ctx = crate::memory::context::LLContext { arena: &mut arena };
    for (cls, expected) in [(fixed, None), (moving, Some(PROBE_VERSION))] {
        let obj = unsafe {
            crate::object::ll_object_new(&mut ctx, cls, crate::refcount::MemoryCategory::GcHeap)
        };
        assert!(!obj.is_null(), "the factory refused");
        let answered = unsafe {
            crate::object::for_each_counted_cell::<crate::cells::PlainCells>(
                obj as *mut u8,
                cls,
                |_| {},
            )
        };
        assert_eq!(
            answered, expected,
            "the class's answer did not reach the walk"
        );

        unsafe {
            assert!(crate::refcount::ll_release(
                obj as *mut crate::refcount::RcHeader
            ));
            crate::object::ll_entity_die(obj as *mut crate::refcount::RcHeader);
        }
    }
}

/// A give-up is the racing reader's alone, and it reaches the caller as
/// "no version" rather than as a version of its own.
#[test]
fn a_racing_walk_that_gives_up_answers_no_version() {
    let _g = crate::memory::block_pool::test_guard();

    let cls = ClassBuilder::new("Quitter").outside_cells(&QUITTER).build();
    assert!(!cls.is_null(), "immortal region refused the class");

    let mut arena = crate::memory::arena::Arena::new();
    let mut ctx = crate::memory::context::LLContext { arena: &mut arena };
    let obj = unsafe {
        crate::object::ll_object_new(&mut ctx, cls, crate::refcount::MemoryCategory::GcHeap)
    };
    assert!(!obj.is_null(), "the factory refused");

    let answered = unsafe {
        crate::object::for_each_counted_cell::<crate::cells::RelaxedCells>(
            obj as *mut u8,
            cls,
            |_| {},
        )
    };
    assert_eq!(
        answered, None,
        "a given-up entity named a version the re-check would then trust"
    );

    unsafe {
        assert!(crate::refcount::ll_release(
            obj as *mut crate::refcount::RcHeader
        ));
        crate::object::ll_entity_die(obj as *mut crate::refcount::RcHeader);
    }
}

/// A class that declares nothing carries the default teardown and no
/// group, which is every class the compiler emits today.
#[test]
#[cfg_attr(miri, ignore = "Miri gives one function several addresses")]
fn a_class_that_declares_neither_carries_the_default_and_no_group() {
    let _g = crate::memory::block_pool::test_guard();

    let cls = ClassBuilder::new("Plain").prop("x", false).build();
    assert!(!cls.is_null(), "immortal region refused the class");

    unsafe {
        assert_eq!(
            (*cls).dispose,
            crate::object::ll_default_dispose as *const ()
        );
        assert_eq!((*cls).flags & CLASS_OUTSIDE_CELLS, 0);
        assert!((*cls).outside.is_null());
        assert!(Class::outside_cells(cls).is_none());
    }
}

/// The default teardown frees a class's outside storage, which is what
/// makes the group safe to inherit with no dispose beside it.
#[test]
fn the_default_teardown_frees_the_outside_storage() {
    let _g = crate::memory::block_pool::test_guard();

    FREED.store(0, std::sync::atomic::Ordering::Relaxed);
    let cls = ClassBuilder::new("Counted")
        .outside_cells(&COUNTING)
        .build();
    assert!(!cls.is_null(), "immortal region refused the class");

    let mut arena = crate::memory::arena::Arena::new();
    let mut ctx = crate::memory::context::LLContext { arena: &mut arena };
    let obj = unsafe {
        crate::object::ll_object_new(&mut ctx, cls, crate::refcount::MemoryCategory::GcHeap)
    };
    assert!(!obj.is_null(), "the factory refused");

    // The release decides, the caller tears down: an entity killed while
    // its refcount still reads one is the trap of `dev/POSTMORTEM.md`,
    // 2026-08-06, and `ll_free` asserts on it.
    let owed = unsafe { crate::refcount::ll_release(obj as *mut crate::refcount::RcHeader) };
    assert!(owed, "the release did not hand the teardown back");
    unsafe { crate::object::ll_entity_die(obj as *mut crate::refcount::RcHeader) };
    assert_eq!(
        FREED.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the object died and its outside storage was never freed"
    );
}

static FREED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

static COUNTING: OutsideCells = OutsideCells {
    walk_plain: unversioned_walk,
    walk_relaxed: unversioned_walk_racing,
    recheck: probe_recheck,
    sever: probe_sever,
    free: counting_free,
    carry: probe_carry,
};

unsafe fn counting_free(_: *mut crate::refcount::RcHeader) {
    FREED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}
