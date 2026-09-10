//! Where each root of a traced batch stands once the collection that traced it
//! closes.
//!
//! One trace answers about as many components as its lane holds roots, and the
//! three answers go three ways: a root whose entity the teardown freed is
//! retired and gives its slot back, a root this collection read live waits in
//! the deferred lane until the turnover, and every other root is offered to
//! the next collection out of the active lane (`PLAN.md` S37.6).
//!
//! **The lane a root stands in is read through two figures, never one.**
//! `candidate_count` answers the active lane alone and `deferred_count` the
//! deferred one, so a disposition that dropped a token would leave both short
//! while a multiset over `collect_lane_tokens` still read whole; the two
//! together say which lane, and the multiset says which entities.
//!
//! **What no case here reaches is the second head.** A deferred lane takes one
//! segment per `SEGMENT_CAPACITY` records, which is 8,160, and the widest
//! population any case of this crate builds is the 4,077 of
//! `queue::tests::where_a_full_segment_comes_from::`
//! `a_bulk_release_polls_on_its_own_backedge`; so the append's growth arm and
//! the ledger term beside it are exercised by nothing, and `PLAN.md`'s
//! residual list carries the debt.

use super::*;
use crate::cycle::queue::{
    candidate_count, collect_lane_tokens, deferred_count, deferred_turnover_mirror, refill_spares,
    release_queue_segments, spare_count,
};

/// Every token this thread's queue holds, sorted.
fn lane_tokens() -> Vec<*mut RcHeader> {
    let mut tokens = Vec::new();
    collect_lane_tokens(&mut tokens);
    tokens.sort_unstable();
    tokens
}

/// `members` as the tokens a queue holding exactly them would yield.
fn as_tokens(members: &[*mut Object]) -> Vec<*mut RcHeader> {
    let mut tokens: Vec<*mut RcHeader> = members.iter().map(|&m| m as *mut RcHeader).collect();
    tokens.sort_unstable();
    tokens
}

/// A class with two counted Box properties, through which a case holds a ring
/// and whatever else it wants to keep alive from outside.
fn keeper_class(name: &str) -> *const Class {
    ClassBuilder::new(name)
        .prop("held", true)
        .prop("also_held", true)
        .build()
}

/// A ring member with a second counted property, through which the ring holds
/// something that outlives it.
fn node_with_a_side_class(name: &str) -> *const Class {
    ClassBuilder::new(name)
        .prop("next", true)
        .prop("side", true)
        .destructor(counting_destructor as *const ())
        .build()
}

/// A batch holding a ring the collection frees and a ring it reads live goes
/// two ways at the close, and neither way loses a token.
#[test]
fn a_freed_root_is_retired_and_a_live_one_is_deferred() {
    let _g = test_guard();
    release_queue_segments();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);

    let node = node_class("DisposedNode", counting_destructor as *const ());
    let mut arena = Arena::new();
    let keeper = {
        let mut context = LLContext { arena: &mut arena };
        unsafe {
            new_constructed(
                &mut context,
                keeper_class("DisposedKeeper"),
                MemoryCategory::GcHeap,
            )
        }
    };

    let held = unsafe { ring(&mut arena, [node, node]) };
    // The garbage ring holds a live object the case holds too, so the release
    // its teardown makes is not the last one: the entity is registered while
    // this collection's batch is out, and its record is the one the close has
    // no mark for.
    let side = node_with_a_side_class("DisposedSide");
    let garbage = unsafe { ring(&mut arena, [side, side]) };
    let shared = {
        let mut context = LLContext { arena: &mut arena };
        unsafe {
            new_constructed(
                &mut context,
                keeper_class("DisposedShared"),
                MemoryCategory::GcHeap,
            )
        }
    };
    unsafe {
        store_prop(&mut arena, garbage[0], prop_offset(1), shared);
        store_prop(&mut arena, keeper, prop_offset(0), held[1]);
        store_prop(&mut arena, keeper, prop_offset(1), shared);
    }
    assert_eq!(candidate_count(), 4, "four roots in one lane");

    // The close takes a segment for the deferred lane's head out of the spare
    // cells, which `release_queue_segments` left empty.
    assert!(refill_spares());
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        2,
        "the ring nothing holds is freed"
    );
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 2);

    let mut expected = as_tokens(&held);
    expected.push(shared as *mut RcHeader);
    expected.sort_unstable();
    assert_eq!(
        lane_tokens(),
        expected,
        "the freed ring's tokens went with its slots, the live ring's were \
         kept, and the teardown's own registration joined them"
    );
    assert_eq!(
        candidate_count(),
        1,
        "the record the close had no mark for stands where a trace finds it"
    );
    assert_eq!(
        deferred_count(),
        2,
        "and the live ring waits for a turnover"
    );
}

/// With both spare cells empty the deferred lane can take nothing, so a live
/// root joins the active lane instead — and is collected at the next
/// collection rather than at the turnover.
///
/// The fallback is the one destination that cannot refuse, which is what keeps
/// a token out of no lane at all (`rfc/model/gc/cycle/questions.md`, Y12
/// clause 8).
#[test]
fn a_live_root_the_deferred_lane_cannot_take_stays_offerable() {
    let _g = test_guard();
    release_queue_segments();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);

    let node = node_class("UndeferredNode", counting_destructor as *const ());
    let mut arena = Arena::new();
    let keeper = {
        let mut context = LLContext { arena: &mut arena };
        unsafe {
            new_constructed(
                &mut context,
                keeper_class("UndeferredKeeper"),
                MemoryCategory::GcHeap,
            )
        }
    };

    let held = unsafe { ring(&mut arena, [node, node]) };
    unsafe { store_prop(&mut arena, keeper, prop_offset(0), held[1]) };
    crate::memory::critical::drain_for_test();
    assert_eq!(spare_count(), 0, "no cell for the lane's head to come from");

    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0, "the keeper holds it");
    assert_eq!(deferred_count(), 0, "the lane could take neither record");
    assert_eq!(
        candidate_count(),
        2,
        "so both stand where a trace finds them"
    );
    assert_eq!(lane_tokens(), as_tokens(&held), "and no token was dropped");

    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
    }
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        2,
        "the next collection reaches the ring, which a deferral would have \
         made it wait a turnover for"
    );
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 2);
    assert!(lane_tokens().is_empty());
}

/// The re-offer's mirror is the count the reading that **started** the lane
/// saw, so a second deferral into a lane that already holds records leaves it
/// where it stands.
///
/// The oldest deferred record decides when the owner owes a re-offer; a mirror
/// that moved with every append would put that debt one collection further off
/// at each one, and a lane that never empties would never be re-offered at all
/// (`rfc/dev/DECISIONS.md`, "the deferred-candidate buffer is the owner's, and
/// the re-offer is a splice at the epoch's turn").
#[test]
fn the_mirror_is_the_count_the_lane_started_at() {
    let _g = test_guard();
    release_queue_segments();

    let node = node_class("MirroredNode", counting_destructor as *const ());
    let mut arena = Arena::new();
    let keeper_class = keeper_class("MirroredKeeper");
    let mut hold = |arena: &mut Arena, ring: *mut Object| {
        let keeper = {
            let mut context = LLContext { arena: &mut *arena };
            unsafe { new_constructed(&mut context, keeper_class, MemoryCategory::GcHeap) }
        };
        unsafe { store_prop(arena, keeper, prop_offset(0), ring) };
    };

    let first = unsafe { ring(&mut arena, [node, node]) };
    hold(&mut arena, first[1]);
    assert!(refill_spares());
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
    assert_eq!(deferred_count(), 2);
    let started_at = deferred_turnover_mirror();

    // A second live ring, so that the next collection has roots of its own to
    // defer into a lane that is no longer empty.
    let second = unsafe { ring(&mut arena, [node, node]) };
    hold(&mut arena, second[1]);
    assert!(refill_spares());
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 0);
    assert_eq!(deferred_count(), 4, "both rings wait for the turnover");
    assert_eq!(
        deferred_turnover_mirror(),
        started_at,
        "and the debt is still the first reading's"
    );
}
