use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::finalization::{Finalization, Revalidated};
use crate::cycle::testing::{open_arena, traced_unreachable_from};
use crate::cycle::validation::ValidationResult;
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::memory::context::LLContext;
use crate::object::{Object, new_constructed};
use crate::refcount::{MemoryCategory, SlotState, ll_release, slot_state};
use crate::test_support::{prop_offset, store_prop};
use std::sync::atomic::{AtomicUsize, Ordering};

/// One object of `class`, constructed and holding its creation reference.
///
/// # Safety
/// `arena` is this thread's.
unsafe fn object(arena: &mut Arena, class: *const Class) -> *mut Object {
    let mut context = LLContext { arena };
    unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) }
}

/// Spend the creation reference of each entity, which is what leaves a graph
/// held by its own edges alone.
///
/// # Safety
/// Every entity is live and named by at least one edge.
unsafe fn spend_creation_references(entities: &[*mut Object]) {
    for &entity in entities {
        assert!(
            !unsafe { ll_release(entity as *mut RcHeader) },
            "an edge of the fixture holds this entity"
        );
    }
}

/// Read `members` as potentially unreachable through a real trace and let the
/// rows go, which is the state a component reaches the commit's chain in.
///
/// # Safety
/// As `traced_unreachable_from`.
unsafe fn read_as_unreachable(root: *mut Object, members: &[*mut Object]) {
    let mut arena = unsafe { traced_unreachable_from(root, members) };
    arena.reset();
}

/// Drive the whole commit over one component: the guards and the weak
/// invalidation, the destructor pass, the second reading, and the teardown this
/// module builds.
///
/// The chain is the driver's shape (`PLAN.md` S36.7) with one component in it,
/// which is what lets a case say what the teardown did rather than how it was
/// reached. A component the second reading finds externally referenced fails
/// the case: every fixture here is garbage nothing keeps.
///
/// # Safety
/// As [`reclaim`], and `members` is one component's whole membership.
unsafe fn commit(members: &mut [*mut RcHeader], arena: &mut TraceScratchArena) -> Reclaimed {
    let mut finalization = Finalization::begin();
    assert_eq!(
        unsafe { finalization.confirm(members) },
        ValidationResult::Unreachable
    );

    let mut pass = finalization.seal().destructors();
    unsafe { pass.run(members) };
    let mut revalidation = pass.close();
    let answer = match unsafe { revalidation.revalidate(members) } {
        Revalidated::Unreachable(component) => unsafe { reclaim(component, members, arena) },
        Revalidated::ExternallyReferenced => {
            panic!("the fixture's component is garbage nothing keeps")
        }
    };

    revalidation.close();
    answer
}

/// The members of a fixture as the header pointers the commit takes.
fn headers<const MEMBERS: usize>(members: [*mut Object; MEMBERS]) -> [*mut RcHeader; MEMBERS] {
    members.map(|member| member as *mut RcHeader)
}

/// A class with `next` and `child` Box properties at [`prop_offset`] 0 and 1,
/// and no destructor.
fn node_class(name: &str) -> *const Class {
    ClassBuilder::new(name)
        .prop("next", true)
        .prop("child", true)
        .build()
}

mod what_a_refused_reservation_keeps;
mod what_the_bound_owes_the_sever;
mod what_the_deferred_drops_carry;
mod what_the_teardown_frees;
