//! The collector's progress: garbage whose trace runs into a live closure is
//! freed within a bounded number of grants and turnovers, whatever the
//! closure's size and however often the mutator recalls its token
//! (`PLAN.md`, S67.9).
//!
//! The garbage is a registered ring of two whose first member holds an edge
//! into a live core spread one member per heap block, so that every trace of
//! the garbage meets the core's rows before it can read the ring unreachable,
//! as a request's garbage meets the long-lived state on the web loads.

use super::the_batch::{keeper_class, served_by_a_collector};
use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::queue::reoffer_deferred_candidates;
use crate::cycle::queue::verdicts::{Verdict, standing_verdicts};
use crate::cycle::testing::{long_ring, move_prop};
use crate::gc::ll_gc_maybe_collect;
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, SlotState, ll_release, ll_retain, slot_state};
use crate::test_support::{prop_offset, store_prop};

/// Bytes of a member: a size class whose touched block reserves a row array
/// of about 2 KiB.
const MEMBER_BYTES: usize = 128;

/// A live ring of `members`, one per heap block, its first `roots` members
/// registered and a keeper holding the first.
struct SpreadCore {
    members: Vec<*mut Object>,
    fillers: Vec<*mut Object>,
    keeper: *mut Object,
}

fn member_class(name: &str) -> *const Class {
    let mut builder = ClassBuilder::new(name);
    for property in 0..(MEMBER_BYTES - 16) / 16 {
        builder = builder.prop(&format!("p{property}"), true);
    }

    builder.build()
}

/// # Safety
/// A quiescent heap under `test_guard`.
unsafe fn spread_core(arena: &mut Arena, name: &str, members: usize, roots: usize) -> SpreadCore {
    let class = member_class(name);
    let arena_ptr: *mut Arena = arena;
    let mut context = LLContext { arena };
    let per_block = crate::cycle::loads::slots_per_block(MEMBER_BYTES) - 1;
    let mut fillers = Vec::with_capacity(members * per_block);
    let ring: Vec<*mut Object> = (0..members)
        .map(|_| unsafe {
            let member = new_constructed(&mut context, class, MemoryCategory::GcHeap);
            for _ in 0..per_block {
                fillers.push(new_constructed(&mut context, class, MemoryCategory::GcHeap));
            }
            member
        })
        .collect();
    let keeper = unsafe {
        new_constructed(
            &mut context,
            keeper_class("CeilingKeeper"),
            MemoryCategory::GcHeap,
        )
    };
    unsafe {
        for (position, &member) in ring.iter().enumerate() {
            move_prop(member, prop_offset(0), ring[(position + 1) % members]);
        }

        for &root in &ring[..roots] {
            ll_retain(root as *mut RcHeader);
            assert!(!ll_release(root as *mut RcHeader), "an edge holds the root");
            crate::cycle::testing::as_of_the_second_generation(root as *mut RcHeader);
        }

        store_prop(arena_ptr, keeper, prop_offset(0), ring[0]);
    }
    SpreadCore {
        members: ring,
        fillers,
        keeper,
    }
}

/// Take the core apart by hand and give the fillers back.
///
/// # Safety
/// `core` came from [`spread_core`] on this thread, and no collection runs.
unsafe fn let_the_core_go(arena: &mut Arena, core: SpreadCore) {
    let arena_ptr: *mut Arena = arena;
    unsafe {
        store_prop(arena_ptr, core.keeper, prop_offset(0), std::ptr::null_mut());
        for &member in &core.members {
            ll_retain(member as *mut RcHeader);
        }

        for &member in &core.members {
            store_prop(arena_ptr, member, prop_offset(0), std::ptr::null_mut());
        }

        for &member in core.members.iter().chain(&core.fillers) {
            assert!(ll_release(member as *mut RcHeader), "the member's last");
            ll_object_die(member);
        }

        assert!(ll_release(core.keeper as *mut RcHeader));
        ll_object_die(core.keeper);
    }
    reset_lanes();
}

/// One serve, reading the batch: the served answer, the batch's trace and
/// the verdicts standing in P.
fn a_serve() -> (Served, testing::TracedBatch, Vec<Verdict>) {
    testing::read_traced_batches(true);
    let served = served_by_a_collector();
    let traced = testing::take_traced_batches();
    testing::read_traced_batches(false);
    assert_eq!(traced.len(), 1, "one batch");
    let verdicts = standing_verdicts()
        .iter()
        .map(|&(_, verdict)| verdict)
        .collect();
    (served, traced[0], verdicts)
}

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

/// A live closure over 150 heap blocks, which the trace in parts never
/// completed under its `B_max` — each grant's part at B and its retry failed,
/// the ring came back read live, and the lane offered it again at the
/// turnover to the same failure: the batch's one trace walks the closure
/// whole, the ring's roots read zero, and the ring is proposed and freed.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live in its chain, not in P (`crate::cycle::chain`)"
)]
fn garbage_behind_a_live_closure_of_150_blocks_is_freed_within_the_rounds() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let core = unsafe { spread_core(&mut arena, "ProgressWideClosure", 150, 0) };
    let garbage = unsafe { garbage_into(&mut arena, core.members[0]) };

    let mut rounds = 0;
    while rounds < ROUNDS && !unsafe { is_freed(&garbage) } {
        unsafe { &*record() }.set_batch_size(2);
        let _ = a_serve();
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

/// A closure over 300 heap blocks whose trace the mutator recalls in every
/// grant, between the mark and the scan: the stop posts the snapshot, the
/// ring's roots at zero proposed, and the owner's exact validation frees the
/// ring in the grant that read it.
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
        testing::between_the_next_phases(Box::new(move || {
            unsafe { &*(token as *const crate::cycle::token::TraceToken) }.recall_for_test(true)
        }));
        let (_, traced, _) = a_serve();
        assert!(!traced.complete, "the recall stopped the grant's trace");
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

/// A live core over 300 heap blocks whose every member is registered, as a
/// server's long-lived state is, its 300 roots standing in R ahead of the
/// garbage: the batches read the core's roots first, K growing by its own
/// rule, and reach the rings within the rounds, each trace walking the core
/// once whatever its roots. Under the parts each garbage ring's part met B
/// with the grant's retry spent and was deferred. No turnover is made, so the
/// core's live list is stamped in the epoch the next grant traces in; the
/// registered members are cut by no stamp.
#[test]
#[cfg_attr(miri, ignore = "300 blocks of objects are past what Miri affords")]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live in its chain, not in P (`crate::cycle::chain`)"
)]
fn garbage_behind_a_registered_core_is_freed_within_the_rounds() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let core = unsafe { spread_core(&mut arena, "ProgressRegistered", 300, 300) };
    let rings: Vec<Vec<*mut Object>> = (0..3)
        .map(|_| unsafe { garbage_into(&mut arena, core.members[0]) })
        .collect();
    let all_freed = || rings.iter().all(|ring| unsafe { is_freed(ring) });

    // K starts at 16 and grows by the batches' own rule: the 300 core roots
    // stand in R ahead of the rings, and a K held at 16 would not reach the
    // rings within the rounds whatever the trace did.
    unsafe { &*record() }.set_batch_size(16);
    let mut rounds = 0;
    while rounds < ROUNDS && !all_freed() {
        let _ = a_serve();
        unsafe { ll_gc_maybe_collect() };
        reoffer_deferred_candidates();
        rounds += 1;
    }

    assert!(all_freed(), "the garbage stands after {ROUNDS} grants");
    unsafe { let_the_core_go(&mut arena, core) };
}
