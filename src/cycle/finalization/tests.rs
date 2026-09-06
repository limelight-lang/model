use super::*;
use crate::class::ClassBuilder;
use crate::cycle::testing::traced_unreachable_from;
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_entity_die, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, entity_flags, header_refcount, ll_release, ll_retain};
use crate::test_support::{prop_offset, store_prop};
use crate::weak::{LLWeakRef, ll_weakref_create, ll_weakref_get};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Dismantle the ring the fixture built: take each guard reference off, break
/// both edges and free both members.
///
/// The finalization stops before the sever and the free (`PLAN.md` S36.5), so
/// a case that ran one owes this by hand.
///
/// **The guard comes off through `mutator_unguard_release`, which is the
/// counter's twin of the guard's `+1` and not the counted release S36.5 will
/// perform:** it stores a count and starts no teardown at zero.
///
/// # Safety
/// Both members are live objects of this thread's GC heap, each carrying one
/// guard reference, linked into a ring through property 0.
unsafe fn unwind_guarded_ring(arena: &mut Arena, ring: [*mut Object; 2]) {
    unsafe {
        for member in ring {
            crate::refcount::mutator_unguard_release(member as *mut RcHeader);
        }

        dismantle_ring(arena, ring);
    }
}

/// Break both edges of a ring nothing else holds and free both members.
///
/// The retain is what the sever below spends: a member whose edge is nulled
/// while its count is one dies inside `store_prop`'s barrier, under the loop
/// that is still walking the ring.
///
/// # Safety
/// Both members are live objects of this thread's GC heap, unguarded, linked
/// into a ring through property 0 and held by nothing else.
unsafe fn dismantle_ring(arena: &mut Arena, ring: [*mut Object; 2]) {
    unsafe {
        for member in ring {
            ll_retain(member as *mut RcHeader);
        }

        for member in ring {
            store_prop(arena, member, prop_offset(0), std::ptr::null_mut());
        }

        for member in ring {
            assert!(ll_release(member as *mut RcHeader));
            ll_object_die(member);
        }
    }
}

/// Release the fixture's own weak cell and free it.
///
/// # Safety
/// `cell` is a live weak cell this fixture created and nothing else holds.
unsafe fn drop_cell(cell: *mut LLWeakRef) {
    unsafe {
        assert!(ll_release(cell as *mut RcHeader));
        ll_entity_die(cell as *mut RcHeader);
    }
}

/// The refcount each member carries, in the caller's own order — which
/// `Finalization::confirm` does not keep, sorting the slice it is handed.
///
/// # Safety
/// Every member is a live object of this thread's GC heap.
unsafe fn refcounts(members: &[*mut Object]) -> Vec<u32> {
    members
        .iter()
        .map(|&member| unsafe { header_refcount(member as *mut RcHeader) })
        .collect()
}

/// What [`refcounts`] reads after a confirmed component took its guard
/// references: one more on every member, and on each of them rather than on
/// the component as a whole.
fn one_guard_each(before: &[u32]) -> Vec<u32> {
    before.iter().map(|count| count + 1).collect()
}

mod what_a_destructor_reads_through_a_weak_cell;
mod what_a_refused_component_keeps;
mod what_an_abandoned_finalization_costs;
mod what_the_destructor_pass_runs;
mod what_the_revalidation_answers;
