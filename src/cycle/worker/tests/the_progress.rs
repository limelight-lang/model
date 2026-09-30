//! The collector's progress: garbage whose trace runs into a live closure is
//! freed within a bounded number of grants and turnovers, whatever the
//! closure's size and however often the mutator recalls its token
//! (`PLAN.md`, S67.9).
//!
//! The garbage is a registered ring of two whose first member holds an edge
//! into a live core the ceiling's cases build, so that every trace of the
//! garbage meets the core's rows before it can read the ring unreachable, as
//! a request's garbage meets the long-lived state on the web loads.

use super::the_ceiling::{a_serve_under, let_the_core_go, member_class, spread_core};
use super::*;
use crate::cycle::queue::reoffer_deferred_candidates;
use crate::cycle::testing::long_ring;
use crate::gc::ll_gc_maybe_collect;
use crate::memory::arena::Arena;
use crate::object::Object;
use crate::refcount::{RcHeader, SlotState, slot_state};
use crate::test_support::{prop_offset, store_prop};

/// Grants, each followed by a turnover, that a case waits for the garbage
/// before it reads the garbage as never freed.
const ROUNDS: usize = 8;

/// A registered garbage ring of two whose first member holds an edge into
/// `core_member`.
///
/// # Safety
/// A quiescent heap under `test_guard`; `core_member` is live.
unsafe fn garbage_into(arena: &mut Arena, core_member: *mut Object) -> Vec<*mut Object> {
    let arena_ptr: *mut Arena = arena;
    let ring = unsafe { long_ring(arena, member_class("ProgressGarbage"), 2) };
    unsafe { store_prop(arena_ptr, ring[0], prop_offset(1), core_member) };
    ring
}

/// # Safety
/// No allocation has run since the ring's members could have been freed.
unsafe fn is_freed(ring: &[*mut Object]) -> bool {
    ring.iter()
        .all(|&member| unsafe { slot_state(member as *const RcHeader) } != SlotState::Live)
}

/// A closure past `B_max`: every grant's part at B and its retry fail, the
/// ring comes back read live, and the lane offers it again at the turnover
/// to the same failure.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live in its chain, not in P (`crate::cycle::chain`)"
)]
fn garbage_behind_a_live_closure_past_b_max_is_freed_within_the_rounds() {
    let _g = test_guard();
    reset_lanes();
    let _retry = testing::retry_parts_under(2);
    let mut arena = Arena::new();
    let core = unsafe { spread_core(&mut arena, "ProgressPastBMax", 150, 0) };
    let garbage = unsafe { garbage_into(&mut arena, core.members[0]) };

    let mut rounds = 0;
    while rounds < ROUNDS && !unsafe { is_freed(&garbage) } {
        unsafe { &*record() }.set_batch_size(2);
        let _ = a_serve_under(1);
        unsafe { ll_gc_maybe_collect() };
        crate::cycle::epoch::turn_this_threads_cell();
        reoffer_deferred_candidates();
        rounds += 1;
    }

    assert!(
        unsafe { is_freed(&garbage) },
        "the garbage stands after {ROUNDS} grants and turnovers"
    );
    unsafe { let_the_core_go(&mut arena, core) };
}

/// A closure past B and inside `B_max` whose retry the mutator recalls in
/// every grant: the ring comes back unwalked, the collection over P writes it
/// back into R, and the next grant starts its trace from nothing.
#[test]
#[cfg_attr(miri, ignore = "300 blocks of objects are past what Miri affords")]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn garbage_behind_a_closure_recalled_in_every_grant_is_freed_within_the_rounds() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let core = unsafe { spread_core(&mut arena, "ProgressRecalled", 300, 0) };
    let garbage = unsafe { garbage_into(&mut arena, core.members[0]) };
    let token = unsafe { &raw const (*record()).token } as usize;

    let mut rounds = 0;
    while rounds < ROUNDS && !unsafe { is_freed(&garbage) } {
        unsafe { &*record() }.set_batch_size(2);
        testing::at_the_start_of_the_next_retry(Box::new(move || {
            unsafe { &*(token as *const crate::cycle::token::TraceToken) }.recall_for_test(true)
        }));
        let _ = a_serve_under(8);
        unsafe { &*record() }.token.recall_for_test(false);
        unsafe { ll_gc_maybe_collect() };
        crate::cycle::epoch::turn_this_threads_cell();
        reoffer_deferred_candidates();
        rounds += 1;
    }

    assert!(
        unsafe { is_freed(&garbage) },
        "the garbage stands after {ROUNDS} recalled grants and turnovers"
    );
    unsafe { let_the_core_go(&mut arena, core) };
}

/// A live core inside `B_max` whose every member is registered, as a server's
/// long-lived state is, and whose roots stand ahead of the garbage in every
/// batch: the core's part meets B and spends the grant's one retry, which
/// finishes and reads the core live, and each garbage ring's part meets B with
/// the retry spent and is deferred. No turnover is made, so the core's live
/// list is stamped in the epoch the next grant traces in; the registered
/// members are cut by no stamp.
#[test]
#[cfg_attr(miri, ignore = "300 blocks of objects are past what Miri affords")]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live in its chain, not in P (`crate::cycle::chain`)"
)]
fn garbage_behind_a_registered_core_that_spends_the_retry_is_freed_within_the_rounds() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let core = unsafe { spread_core(&mut arena, "ProgressRegistered", 300, 300) };
    let rings: Vec<Vec<*mut Object>> = (0..3)
        .map(|_| unsafe { garbage_into(&mut arena, core.members[0]) })
        .collect();
    let all_freed = || rings.iter().all(|ring| unsafe { is_freed(ring) });

    let mut rounds = 0;
    while rounds < ROUNDS && !all_freed() {
        unsafe { &*record() }.set_batch_size(16);
        let _ = a_serve_under(8);
        unsafe { ll_gc_maybe_collect() };
        reoffer_deferred_candidates();
        rounds += 1;
    }

    assert!(all_freed(), "the garbage stands after {ROUNDS} grants");
    unsafe { let_the_core_go(&mut arena, core) };
}
