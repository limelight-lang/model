//! The retry at the ceiling: a part that meets B is retried at once under
//! `B_max` for the same root, once per grant, and a retry that meets `B_max`
//! too posts every root its rows met read live, which the deferred lane holds
//! until the turnover, and the batch goes on with the next root; a retry the
//! mutator recalls ends the batch as a part would
//! (`dev/CYCLE-SPLIT-PACKAGE-3.md`, section 7; `rfc/model/gc/rc-cycle.md`,
//! "The retry at the ceiling").
//!
//! The rings are spread one member per heap block, so that the rows a part
//! meets are one row array per member and the blocks a trace draws are
//! arithmetic over the members.

use super::the_batch::{
    keeper_class, kept_root, record_batch_size, release_keeper, served_by_a_collector,
};
use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::queue::verdicts::{Verdict, standing_verdicts};
use crate::cycle::queue::{candidate_count, deferred_count, reoffer_deferred_candidates};
use crate::cycle::testing::move_prop;
use crate::gc::ll_gc_maybe_collect;
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, ll_release, ll_retain};
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

/// One serve under a part budget of `b` blocks, reading the batch.
fn a_serve_under(b: usize) -> (Served, testing::TracedBatch, Vec<Verdict>) {
    testing::budget_the_next_batch(b);
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

/// A ring over 300 heap blocks passes B = 8 and fits `B_max`: the part that
/// met B is retried in the same grant and finishes, so the root comes back
/// read live and waits in the deferred lane, where before it came back
/// unwalked at every take.
#[test]
#[cfg_attr(miri, ignore = "300 blocks of objects are past what Miri affords")]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_part_past_b_is_retried_under_b_max_in_the_same_grant() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let core = unsafe { spread_core(&mut arena, "CeilingRetried", 300, 1) };
    unsafe { &*record() }.set_batch_size(1);

    let (served, traced, verdicts) = a_serve_under(8);
    assert_eq!(
        served,
        Served::Batch {
            roots: 1,
            complete: true,
            backlog: false,
        }
    );
    assert_eq!(
        (traced.parts_met_budget, traced.retried),
        (1, true),
        "one part met B and its retry finished"
    );
    assert_eq!(verdicts, vec![Verdict::ReadLive]);
    unsafe { ll_gc_maybe_collect() };
    assert_eq!(deferred_count(), 1, "the root waits for the turnover");
    unsafe { let_the_core_go(&mut arena, core) };
}

/// A ring past `B_max` too: the retry is made once per grant, every root its
/// rows met comes back read live and K stands; the roots wait in the deferred
/// lane, R holding none of them for the epoch's next take, and the take after
/// the turnover retries the part once more.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_failed_retry_posts_the_roots_it_met_read_live_until_the_turnover() {
    let _g = test_guard();
    reset_lanes();
    let _retry = testing::retry_parts_under(2);
    let mut arena = Arena::new();
    let core = unsafe { spread_core(&mut arena, "CeilingFailed", 150, 2) };
    unsafe { &*record() }.set_batch_size(2);
    let _ = testing::take_parts_deferred();

    let (served, first, verdicts) = a_serve_under(1);
    assert_eq!(
        served,
        Served::Batch {
            roots: 2,
            complete: false,
            backlog: false,
        }
    );
    assert_eq!(
        (
            first.parts,
            first.parts_met_budget,
            first.retried,
            first.deferred_parts
        ),
        (1, 1, true, 1),
        "one part met B, and its retry met B_max"
    );
    assert_eq!(
        testing::take_parts_deferred(),
        first.deferred_parts,
        "the rig's count of deferred parts reads the batch's own"
    );
    assert_eq!(
        verdicts,
        vec![Verdict::ReadLive; 2],
        "both roots were met by the failed retry"
    );
    assert_eq!(record_batch_size(), 2, "K stands");
    unsafe { ll_gc_maybe_collect() };
    assert_eq!(deferred_count(), 2, "both roots wait for the turnover");
    assert_eq!(
        candidate_count(),
        0,
        "no root is left in R for the epoch's next take"
    );

    crate::cycle::epoch::turn_this_threads_cell();
    reoffer_deferred_candidates();
    unsafe { &*record() }.set_batch_size(2);
    let (_, second, verdicts) = a_serve_under(1);
    assert!(
        second.retried,
        "after the turnover the part is retried once more"
    );
    // The lane hands the roots back in either order, and a retry opened at
    // the second root meets the first only past `B_max`: the first then opens
    // a part of its own, which meets B with the retry spent.
    assert!((1..=2).contains(&second.parts_met_budget));
    assert_eq!(second.deferred_parts, second.parts_met_budget);
    assert_eq!(verdicts, vec![Verdict::ReadLive; 2]);
    unsafe { ll_gc_maybe_collect() };
    unsafe { let_the_core_go(&mut arena, core) };
}

/// A root the failed retry did not meet opens a part of its own: behind a
/// ring past `B_max`, the ring's root comes back read live from the retry and
/// an unrelated kept root of the same batch read live from its own part.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_root_the_failed_retry_did_not_meet_opens_a_part_of_its_own() {
    let _g = test_guard();
    reset_lanes();
    let _retry = testing::retry_parts_under(2);
    let mut arena = Arena::new();
    let core = unsafe { spread_core(&mut arena, "CeilingUnmet", 150, 1) };
    let (kept, keeper) = unsafe {
        kept_root(
            &mut arena,
            member_class("CeilingUnmetKept"),
            "CeilingUnmetKeeper",
        )
    };
    unsafe { &*record() }.set_batch_size(2);

    let (_, traced, _) = a_serve_under(1);
    assert_eq!(
        (traced.parts, traced.retried, traced.deferred_parts),
        (2, true, 1),
        "the ring's part and its failed retry, then the kept root's part"
    );
    let ring_root = core.members[0] as *mut RcHeader;
    assert_eq!(
        standing_verdicts(),
        vec![(ring_root, Verdict::ReadLive), (kept, Verdict::ReadLive)]
    );
    unsafe { ll_gc_maybe_collect() };
    unsafe { release_keeper(keeper) };
    unsafe { let_the_core_go(&mut arena, core) };
}

/// A retry the mutator recalls ends the batch as a part would: its root and
/// every root without a verdict come back `Unwalked`, and K stands, the
/// recall saying nothing of the batch's size.
#[test]
#[cfg_attr(miri, ignore = "300 blocks of objects are past what Miri affords")]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_recalled_retry_leaves_its_roots_unwalked_and_k_where_it_stands() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let core = unsafe { spread_core(&mut arena, "CeilingRecalled", 300, 2) };
    unsafe { &*record() }.set_batch_size(2);

    let token = unsafe { &raw const (*record()).token } as usize;
    testing::at_the_start_of_the_next_retry(Box::new(move || {
        unsafe { &*(token as *const crate::cycle::token::TraceToken) }.recall_for_test(true)
    }));
    let (served, traced, verdicts) = a_serve_under(8);
    unsafe { &*record() }.token.recall_for_test(false);
    assert!(matches!(
        served,
        Served::Batch {
            complete: false,
            ..
        }
    ));
    assert_eq!(
        (traced.parts, traced.parts_met_budget, traced.retried),
        (1, 1, true),
        "the part met B and its retry was recalled"
    );
    assert_eq!(verdicts, vec![Verdict::Unwalked; 2]);
    assert_eq!(record_batch_size(), 2, "a recall leaves K");
    unsafe { ll_gc_maybe_collect() };
    unsafe { let_the_core_go(&mut arena, core) };
}

/// A recall inside the part after a deferral ends the batch: the deferred
/// part's root stays read live and the recalled part's root comes back
/// `Unwalked`, the flag the deferral left not read as the recalled part's
/// budget.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_recall_in_the_part_after_a_deferral_leaves_its_root_unwalked() {
    let _g = test_guard();
    reset_lanes();
    let _retry = testing::retry_parts_under(2);
    let mut arena = Arena::new();
    let core = unsafe { spread_core(&mut arena, "CeilingThenRecall", 150, 1) };
    // A dense ring whose part fits the workspace and passes the recall's
    // stride: 200 members of seven counted properties.
    let ring = unsafe {
        crate::cycle::testing::long_ring(&mut arena, member_class("CeilingDenseRing"), 200)
    };
    unsafe { &*record() }.set_batch_size(2);

    let token = unsafe { &raw const (*record()).token } as usize;
    testing::before_the_trace_of(
        2,
        Box::new(move || {
            unsafe { &*(token as *const crate::cycle::token::TraceToken) }.recall_for_test(true)
        }),
    );
    let (served, traced, _) = a_serve_under(1);
    unsafe { &*record() }.token.recall_for_test(false);
    assert!(matches!(
        served,
        Served::Batch {
            complete: false,
            ..
        }
    ));
    assert_eq!(
        (traced.parts, traced.deferred_parts),
        (2, 1),
        "the ring's part deferred, then the dense ring's part was recalled"
    );
    assert_eq!(
        standing_verdicts(),
        vec![
            (core.members[0] as *mut RcHeader, Verdict::ReadLive),
            (ring[0] as *mut RcHeader, Verdict::Unwalked)
        ]
    );
    assert_eq!(record_batch_size(), 2, "a recall leaves K");
    unsafe { ll_gc_maybe_collect() };
    unsafe { crate::gc::ll_gc_collect_cycles() };
    unsafe { let_the_core_go(&mut arena, core) };
}

/// A retry the pool refuses ends the batch as a part would: the collector's
/// thread may draw no block once its retry starts, the reserve serves the
/// retry's first blocks, and the refusal after them leaves both roots
/// `Unwalked` with no part deferred and K where it stands.
#[test]
#[cfg_attr(miri, ignore = "600 blocks of objects are past what Miri affords")]
fn a_retry_the_pool_refuses_leaves_its_roots_unwalked() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    // Rows past the collector's reserve of `CRITICAL_BLOCKS` and inside
    // `B_max`: about twenty blocks of row arrays.
    let core = unsafe { spread_core(&mut arena, "CeilingRefused", 600, 2) };
    unsafe { &*record() }.set_batch_size(2);

    testing::at_the_start_of_the_next_retry(Box::new(|| {
        // The collector's thread ends with the serve, and its budget with it.
        std::mem::forget(crate::memory::block_pool::budget_blocks(0));
    }));
    let (served, traced, verdicts) = a_serve_under(8);
    assert!(matches!(
        served,
        Served::Batch {
            complete: false,
            ..
        }
    ));
    assert_eq!(
        (
            traced.parts,
            traced.parts_met_budget,
            traced.retried,
            traced.deferred_parts
        ),
        (1, 1, true, 0),
        "the part met B, and its retry was refused short of B_max"
    );
    assert_eq!(verdicts, vec![Verdict::Unwalked; 2]);
    assert_eq!(record_batch_size(), 2, "a refusal leaves K");
    unsafe { ll_gc_maybe_collect() };
    unsafe { let_the_core_go(&mut arena, core) };
}
