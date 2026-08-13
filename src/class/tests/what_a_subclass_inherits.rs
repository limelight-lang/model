//! What a descriptor built for a subclass takes from its parent, for the
//! two behaviours a subclass almost never redeclares: the teardown body
//! and the group that reaches cells outside the object's own body.
//!
//! Both are inherited because the alternative is silent. A subclass of a
//! class whose teardown is its own would get `ll_default_dispose` and
//! lose whatever that body released; a subclass of a class with outside
//! cells would trace its own properties and nothing else, making every
//! child of the outside storage a computed root.

use super::*;
use crate::walk::{Cell, OutsideCells, OutsideRead};

/// A group whose members are distinguishable from any other's: the walk
/// yields nothing and answers a version no real storage would carry, and
/// the rest record nothing. Identity is what these tests read, not
/// behaviour — S18.3 is where a group is made to work.
static PROBE: OutsideCells = OutsideCells {
    walk_plain: probe_walk,
    #[cfg(feature = "rc-walk")]
    walk_relaxed: probe_walk,
    recheck: probe_recheck,
    sever: probe_sever,
    free: probe_free,
};

unsafe fn probe_walk(
    _base: *mut u8,
    _cls: *const Class,
    _visit: &mut dyn FnMut(Cell),
) -> OutsideRead {
    OutsideRead::Version(PROBE_VERSION)
}

/// A group that always gives up, for the assertion below.
static QUITTER: OutsideCells = OutsideCells {
    walk_plain: quitter_walk,
    #[cfg(feature = "rc-walk")]
    walk_relaxed: quitter_walk,
    recheck: probe_recheck,
    sever: probe_sever,
    free: probe_free,
};

unsafe fn quitter_walk(
    _base: *mut u8,
    _cls: *const Class,
    _visit: &mut dyn FnMut(Cell),
) -> OutsideRead {
    OutsideRead::GaveUp
}

/// A group with no versioned storage behind its cells at all.
static UNVERSIONED: OutsideCells = OutsideCells {
    walk_plain: unversioned_walk,
    #[cfg(feature = "rc-walk")]
    walk_relaxed: unversioned_walk,
    recheck: probe_recheck,
    sever: probe_sever,
    free: probe_free,
};

unsafe fn unversioned_walk(
    _base: *mut u8,
    _cls: *const Class,
    _visit: &mut dyn FnMut(Cell),
) -> OutsideRead {
    OutsideRead::NoStorage
}

const PROBE_VERSION: usize = 0xC0FFEE;

unsafe fn probe_recheck(_base: *mut u8, _cls: *const Class, walked: usize) -> bool {
    walked == PROBE_VERSION
}

unsafe fn probe_sever(
    _entity: *mut crate::refcount::RcHeader,
    _out: &mut Vec<*mut crate::refcount::RcHeader>,
) {
}

unsafe fn probe_free(_entity: *mut crate::refcount::RcHeader) {}

extern "C" fn own_dispose(_obj: *mut crate::object::Object) {}

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

        // Through the accessor as well as the field, since that is what
        // every consumer calls.
        let group = Class::outside_cells(child).expect("the subclass answers no group");
        assert!(
            (group.recheck)(std::ptr::null_mut(), child, PROBE_VERSION),
            "the group reached is not the one installed"
        );
    }
}

/// The parent's teardown body, which was not inherited before S18.2 and
/// is what made this test worth writing at all.
#[test]
fn a_subclass_inherits_its_parents_dispose() {
    let _g = crate::memory::block_pool::test_guard();

    let parent = ClassBuilder::new("Session")
        .dispose(own_dispose as *const ())
        .build();
    assert!(!parent.is_null(), "immortal region refused the parent");
    let child = ClassBuilder::new("AdminSession").parent(parent).build();
    assert!(!child.is_null(), "immortal region refused the subclass");

    unsafe {
        assert_eq!(
            (*child).dispose,
            own_dispose as *const (),
            "the subclass fell back to the default teardown and would lose its parent's"
        );
    }
}

/// Declaring either replaces the parent's rather than adding to it.
#[test]
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

/// The walk's three answers are three, not two. The epoch may treat a
/// give-up as "no version"; a pass with no writer to lose to may not,
/// because it would be reading a class that declined to answer for no
/// reason and, in the arena reset's case, assigning a count from the
/// edges it did not find.
#[test]
#[should_panic(expected = "gave up its outside cells")]
fn a_plain_walk_may_not_be_given_up() {
    let _g = crate::memory::block_pool::test_guard();

    let cls = ClassBuilder::new("Quitter").outside_cells(&QUITTER).build();
    assert!(!cls.is_null(), "immortal region refused the class");

    let mut arena = crate::memory::arena::Arena::new();
    let mut ctx = crate::memory::context::LLContext { arena: &mut arena };
    let obj = unsafe {
        crate::object::ll_object_new(&mut ctx, cls, crate::refcount::MemoryCategory::RequestArena)
    };
    assert!(!obj.is_null(), "the factory refused");

    unsafe {
        crate::object::for_each_counted_cell::<crate::walk::PlainCells>(obj as *mut u8, cls, |_| {})
    };
}

/// A class whose outside cells sit in storage nothing replaces answers no
/// version, and the walk reports it as one.
#[test]
fn a_class_with_unversioned_storage_answers_no_version() {
    let _g = crate::memory::block_pool::test_guard();

    let cls = ClassBuilder::new("Fixed")
        .outside_cells(&UNVERSIONED)
        .build();
    assert!(!cls.is_null(), "immortal region refused the class");

    let mut arena = crate::memory::arena::Arena::new();
    let mut ctx = crate::memory::context::LLContext { arena: &mut arena };
    let obj = unsafe {
        crate::object::ll_object_new(&mut ctx, cls, crate::refcount::MemoryCategory::RequestArena)
    };
    assert!(!obj.is_null(), "the factory refused");

    let answered = unsafe {
        crate::object::for_each_counted_cell::<crate::walk::PlainCells>(obj as *mut u8, cls, |_| {})
    };
    assert_eq!(
        answered, None,
        "a class with no versioned storage named one"
    );

    let versioned = ClassBuilder::new("Moving").outside_cells(&PROBE).build();
    let obj2 = unsafe {
        crate::object::ll_object_new(
            &mut ctx,
            versioned,
            crate::refcount::MemoryCategory::RequestArena,
        )
    };
    let answered = unsafe {
        crate::object::for_each_counted_cell::<crate::walk::PlainCells>(
            obj2 as *mut u8,
            versioned,
            |_| {},
        )
    };
    assert_eq!(
        answered,
        Some(PROBE_VERSION),
        "the version the class answered did not reach the caller"
    );
}

/// A class that declares nothing carries the default teardown and no
/// group, which is every class the compiler emits today.
#[test]
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
