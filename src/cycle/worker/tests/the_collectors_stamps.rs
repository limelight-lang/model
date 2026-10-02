//! The collector's stamps: a batch whose trace completed stamps `{e, 1}`, under
//! its grant and before the release, on the live rows of the arrays its mark's
//! final drain touched first, the oldest array first; the next batch in the
//! same epoch prunes at a stamped target, registered or not, and a ring
//! stamped live in one epoch dies in the next (`crate::cycle::collector_stamps`;
//! `dev/plans/S67.md`, S67.9, revision 3's (1′)–(3′)).
//!
//! The collector is a thread of the case's that serves this thread's record,
//! as in `the_batch`. The fixture is a state behind a held entry: a registered
//! root `r` naming a registered entry `x` the case also holds, which names the
//! first member of a ring of unregistered members. `r` and `x` are large
//! entities, each the sole occupant of its block; `x`, met in the first region
//! as a registered target, is held, stays above zero through the passes, and
//! is expanded in the final drain, whose first touch of the ring's blocks
//! makes them the ones stamped. A batch of one root leaves the other
//! registered entries standing in R, which its serve answers as a backlog.

use super::the_batch::served_by_a_collector;
use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::collector_stamps::testing::{
    after_stamped_rows, record_the_order, take_stamping, take_stamps, take_the_order,
};
use crate::cycle::queue::reoffer_deferred_candidates;
use crate::cycle::testing::{move_prop, stamp_of};
use crate::gc::ll_gc_maybe_collect;
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::memory::context::LLContext;
use crate::memory::gc_metadata;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, SlotState, ll_release, ll_retain, slot_state};
use crate::test_support::{POOLED_FILLERS, prop_offset, store_prop, wide_class};

/// The members of the fixture's ring.
const RING: usize = 8;

/// The fixture (module doc).
struct State {
    root: *mut Object,
    entry: *mut Object,
    ring: Vec<*mut Object>,
}

/// One object of `class` at count one, in this thread's heap.
unsafe fn object(arena: &mut Arena, class: *const Class) -> *mut Object {
    let mut context = LLContext { arena };
    unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) }
}

/// Register `object` by a retain and a non-final release.
unsafe fn register(object: *mut Object) {
    unsafe {
        ll_retain(object as *mut RcHeader);
        assert!(!ll_release(object as *mut RcHeader), "a reference holds it");
    }
}

/// A class of one counted Box property, which the ring links through.
fn member_class(name: &str) -> *const Class {
    ClassBuilder::new(name).prop("next", true).build()
}

/// The fixture with a ring of `members` members of `member`'s class, `r`
/// registered before `x` so that a batch of one root takes `r`.
///
/// # Safety
/// A quiescent heap under `test_guard`, and `member` has a counted Box as its
/// first property.
unsafe fn a_state_behind_a_held_entry(
    arena: &mut Arena,
    name: &str,
    member: *const Class,
    members: usize,
) -> State {
    let arena_ptr: *mut Arena = arena;
    let root = unsafe {
        object(
            arena,
            wide_class(&format!("{name}Root"), POOLED_FILLERS, None),
        )
    };
    let entry = unsafe {
        object(
            arena,
            wide_class(&format!("{name}Entry"), POOLED_FILLERS, None),
        )
    };
    let ring: Vec<*mut Object> = (0..members)
        .map(|_| unsafe { object(arena, member) })
        .collect();
    unsafe {
        for (position, &each) in ring.iter().enumerate() {
            move_prop(each, prop_offset(0), ring[(position + 1) % members]);
        }

        store_prop(arena_ptr, entry, prop_offset(0), ring[0]);
        store_prop(arena_ptr, root, prop_offset(0), entry);
        register(root);
        register(entry);
    }
    State { root, entry, ring }
}

/// Take the fixture apart by hand: the edges out of `r` and `x` nulled, every
/// ring member still live retained, unlinked, released and died, then `r` and
/// `x`, then the lanes emptied.
///
/// # Safety
/// `state` came from [`a_state_behind_a_held_entry`] on this thread, and no
/// collection runs.
unsafe fn let_the_state_go(arena: &mut Arena, state: State) {
    let arena_ptr: *mut Arena = arena;
    unsafe {
        store_prop(arena_ptr, state.root, prop_offset(0), std::ptr::null_mut());
        store_prop(arena_ptr, state.entry, prop_offset(0), std::ptr::null_mut());
        let alive: Vec<*mut Object> = state
            .ring
            .iter()
            .copied()
            .filter(|&member| slot_state(member as *mut RcHeader) == SlotState::Live)
            .collect();
        for &member in &alive {
            ll_retain(member as *mut RcHeader);
        }

        for &member in &alive {
            store_prop(arena_ptr, member, prop_offset(0), std::ptr::null_mut());
        }

        for &member in &alive {
            assert!(ll_release(member as *mut RcHeader), "the member's last");
            ll_object_die(member);
        }

        for object in [state.root, state.entry] {
            assert!(ll_release(object as *mut RcHeader), "the case's last");
            ll_object_die(object);
        }
    }
    reset_lanes();
}

/// The epoch this thread's cell stands in, moved off zero first so that a
/// stamp of this epoch is told apart from a fresh header's zero byte.
fn a_nonzero_epoch() -> u32 {
    crate::cycle::epoch::turn_to_a_nonzero_epoch();
    crate::cycle::epoch::epoch_of(unsafe { &*record() }.turnovers())
}

/// The stamps `objects` carry, in their order.
fn stamps(objects: &[*mut Object]) -> Vec<(u32, u32)> {
    objects
        .iter()
        .map(|&object| unsafe { stamp_of(object) })
        .collect()
}

/// One serve, reading the batch the collector traced.
fn a_traced_serve() -> (Served, testing::TracedBatch) {
    testing::read_traced_batches(true);
    let served = served_by_a_collector();
    let traced = testing::take_traced_batches();
    testing::read_traced_batches(false);
    assert_eq!(traced.len(), 1, "one batch");
    (served, traced[0])
}

/// One serve on a thread of the case's, answering the serve and that
/// thread's GC figures before and after it.
fn served_reading_the_collectors_figures() -> (
    Served,
    gc_metadata::GcMemoryStats,
    gc_metadata::GcMemoryStats,
) {
    unsafe { &*record() }.clear_posted_for_test();
    let sent = crate::cycle::testing::Sent(record());
    testing::consent_while(std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the collector thread"
        );
        let before = gc_metadata::thread_stats();
        let served = unsafe { testing::serve_alone(sent.into_inner()) };
        (served, before, gc_metadata::thread_stats())
    }))
}

/// A completed batch stamps, before its release, the ring its final drain read
/// and nothing else: `r` and `x`, whose blocks the first region touched, stay
/// unstamped, and the stamps draw no block of the collector's. Red with the
/// stamps left to the owner, and with every live row stamped.
#[test]
fn a_completed_batch_stamps_what_its_final_drain_read_before_the_release() {
    let _g = test_guard();
    reset_lanes();
    let epoch = a_nonzero_epoch();
    let mut arena = Arena::new();
    let state = unsafe {
        a_state_behind_a_held_entry(
            &mut arena,
            "StampedState",
            member_class("StampedMember"),
            RING,
        )
    };
    unsafe { &*record() }.set_batch_size(1);
    let _ = (take_stamps(), take_stamping());

    let (served, collector_before, collector_after) = served_reading_the_collectors_figures();
    assert_eq!(
        served,
        Served::Batch {
            roots: 1,
            complete: true,
            backlog: true,
        }
    );
    // Under the chain the root read live waits in a chain block the
    // collector drew, unless the generations send it, young, into P
    // (`crate::cycle::chain`).
    let chain_block = usize::from(cfg!(all(
        feature = "collector-chain",
        not(feature = "hold-by-generation")
    )));
    assert_eq!(
        collector_after.current_blocks(),
        collector_before.current_blocks() + 1 + chain_block,
        "the collector keeps its workspace and draws nothing for the stamps"
    );
    assert_eq!(take_stamps(), RING, "one stamp per ring member");
    assert_eq!(
        stamps(&state.ring),
        vec![(epoch, 1); RING],
        "before any take"
    );
    assert_eq!(
        stamps(&[state.root, state.entry]),
        vec![(0, 0); 2],
        "the first region's blocks"
    );
    assert_eq!(take_stamping().walks, 1);

    unsafe { ll_gc_maybe_collect() };
    unsafe { let_the_state_go(&mut arena, state) };
}

/// The batch after one that stamped the ring, in the same epoch, prunes at the
/// ring's first member once it is registered: a stamped registered target is
/// pruned on a collector's batch (revision 3's (1′)). Red under the
/// exemption, where the batch walks the ring again.
#[test]
fn the_next_batch_prunes_at_a_stamped_registered_target() {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let state = unsafe {
        a_state_behind_a_held_entry(
            &mut arena,
            "PrunedState",
            member_class("PrunedMember"),
            RING,
        )
    };
    unsafe { &*record() }.set_batch_size(1);
    let (_, first) = a_traced_serve();
    assert_eq!(first.rows_met, 2 + RING, "the first batch walks the ring");
    unsafe { ll_gc_maybe_collect() };

    // R's front is now `x`, the batch of one root it takes next.
    unsafe { register(state.ring[0]) };
    unsafe { &*record() }.set_batch_size(1);
    let (served, second) = a_traced_serve();
    assert_eq!(
        served,
        Served::Batch {
            roots: 1,
            complete: true,
            backlog: true,
        }
    );
    assert!(
        second.edges_pruned > 0,
        "the mark stopped at the stamped ring"
    );
    assert_eq!(second.rows_met, 1, "the batch met its root and no more");

    unsafe { ll_gc_maybe_collect() };
    unsafe { let_the_state_go(&mut arena, state) };
}

/// A ring stamped live in one epoch and garbage in it is read live there, the
/// prune at its stamped members raising its root, and dies at the first batch
/// after the turn, whose mark reads the old stamps as none (revision 3, the
/// Sage B's H5: the request listed live at e and dead at e+1).
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "roots read live wait in the chain rather than in the deferred lane the case re-offers (`crate::cycle::chain`)"
)]
fn a_ring_stamped_live_in_one_epoch_dies_in_the_next() {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let state = unsafe {
        a_state_behind_a_held_entry(
            &mut arena,
            "TurnedState",
            member_class("TurnedMember"),
            RING,
        )
    };
    unsafe { &*record() }.set_batch_size(1);
    let _ = served_by_a_collector();
    unsafe { ll_gc_maybe_collect() };

    // The ring loses `x`'s edge, and its first member is registered by it.
    unsafe { store_prop(arena_ptr, state.entry, prop_offset(0), std::ptr::null_mut()) };
    unsafe { &*record() }.set_batch_size(2);
    let (_, in_the_epoch) = a_traced_serve();
    assert!(in_the_epoch.edges_pruned > 0, "pruned at the stamped ring");
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        0,
        "read live in its epoch"
    );
    assert!(
        state
            .ring
            .iter()
            .all(|&member| unsafe { slot_state(member as *mut RcHeader) } == SlotState::Live)
    );

    crate::cycle::epoch::turn_this_threads_cell();
    reoffer_deferred_candidates();
    unsafe { &*record() }.set_batch_size(3);
    let (_, after_the_turn) = a_traced_serve();
    assert_eq!(
        after_the_turn.edges_pruned, 0,
        "the old stamps read as none"
    );
    assert_eq!(unsafe { ll_gc_maybe_collect() }, RING, "the ring is freed");

    unsafe { let_the_state_go(&mut arena, state) };
}

/// The owner's collections read the stamps by their own rule: the explicit
/// call prunes at a stamped target no queue entry names, and the pressure
/// path reads no stamp. Red with the pressure path reading the stamps.
#[test]
fn the_explicit_call_prunes_at_the_stamps_and_the_pressure_path_reads_none() {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let state = unsafe {
        a_state_behind_a_held_entry(
            &mut arena,
            "OwnersState",
            member_class("OwnersMember"),
            RING,
        )
    };
    unsafe { &*record() }.set_batch_size(1);
    let _ = served_by_a_collector();
    unsafe { ll_gc_maybe_collect() };
    assert_eq!(stamps(&state.ring[..1])[0].1, 1, "the ring is stamped");

    let _ = crate::cycle::mark::take_edges_pruned();
    assert_eq!(unsafe { crate::gc::ll_gc_collect_cycles() }, 0);
    assert!(
        crate::cycle::mark::take_edges_pruned() > 0,
        "the explicit call pruned at the stamped ring"
    );

    reoffer_deferred_candidates();
    assert_eq!(
        unsafe { crate::cycle::collect::collect_under_pressure() },
        0
    );
    assert_eq!(
        crate::cycle::mark::take_edges_pruned(),
        0,
        "the pressure path read no stamp"
    );

    unsafe { let_the_state_go(&mut arena, state) };
}

/// The exit's rounds read no stamp. Red with the exit reading the stamps.
#[test]
fn the_exit_reads_no_stamp() {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let state = unsafe {
        a_state_behind_a_held_entry(&mut arena, "ExitState", member_class("ExitMember"), RING)
    };
    unsafe { &*record() }.set_batch_size(1);
    let _ = served_by_a_collector();
    unsafe { ll_gc_maybe_collect() };
    assert_eq!(stamps(&state.ring[..1])[0].1, 1, "the ring is stamped");

    let _ = crate::cycle::mark::take_edges_pruned();
    let _ = unsafe { crate::cycle::collect::collect_before_exit() };
    assert_eq!(
        crate::cycle::mark::take_edges_pruned(),
        0,
        "the exit's rounds read no stamp"
    );

    unsafe { let_the_state_go(&mut arena, state) };
}

/// The walk stamps the arrays its final drain touched first before the later
/// ones: a chain of large entities behind `x`, each the sole occupant of its
/// block, is stamped in the order the drain met it. Red with the touched list
/// walked newest first.
#[test]
fn the_walk_stamps_the_oldest_arrays_first() {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let state = unsafe {
        a_state_behind_a_held_entry(
            &mut arena,
            "OrderedState",
            wide_class("OrderedMember", POOLED_FILLERS, None),
            4,
        )
    };
    unsafe { &*record() }.set_batch_size(1);

    record_the_order();
    let _ = served_by_a_collector();
    let order = take_the_order();
    assert_eq!(
        order,
        state
            .ring
            .iter()
            .map(|&member| member as usize)
            .collect::<Vec<_>>(),
        "the ring in the order the drain met it"
    );

    unsafe { ll_gc_maybe_collect() };
    unsafe { let_the_state_go(&mut arena, state) };
}

/// A stop raised inside the stamps' walk is read within a stride of rows and
/// ends the batch before the release, and the stamps written before it stay:
/// they are live rows of a trace that completed (`dev/S65-PLAN-CRITIC.md`,
/// F1); a wind-down raised at the same row ends nothing, the walk stamping the
/// ring whole, as the scan runs on through one. The ring holds three strides
/// of live rows, so a walk that read the recall once, before it began, would
/// pass the stop by two strides. Red with the walk stopping at a wind-down.
#[test]
fn a_stop_inside_the_stamps_walk_is_read_within_a_stride_and_a_wind_down_is_not() {
    use crate::cycle::arena::RECALL_STRIDE;
    use crate::cycle::token::{RECALL_STOP, RECALL_WIND_DOWN};
    let members = 3 * RECALL_STRIDE;
    let raised_after = RECALL_STRIDE + 10;
    for level in [RECALL_WIND_DOWN, RECALL_STOP] {
        let _g = test_guard();
        reset_lanes();
        let mut arena = Arena::new();
        let state = unsafe {
            a_state_behind_a_held_entry(
                &mut arena,
                "RecalledState",
                member_class("RecalledMember"),
                members,
            )
        };
        unsafe { &*record() }.set_batch_size(1);
        let _ = take_stamps();

        let token = unsafe { &raw const (*record()).token } as usize;
        after_stamped_rows(
            raised_after,
            Box::new(move || {
                unsafe { &*(token as *const crate::cycle::token::TraceToken) }
                    .recall_at_level_for_test(level);
            }),
        );
        let (served, traced) = a_traced_serve();
        unsafe { &*record() }.token.recall_for_test(false);
        let stamped = take_stamps();
        if level == RECALL_WIND_DOWN {
            assert!(
                matches!(served, Served::Batch { complete: true, .. }),
                "the wind-down ended nothing: {served:?}"
            );
            assert_eq!(stamped, members, "the walk stamped the ring whole");
        } else {
            assert!(
                matches!(
                    served,
                    Served::Batch {
                        complete: false,
                        ..
                    }
                ),
                "the stop ended the batch: {served:?}"
            );
            let positions = traced
                .positions_after_the_hook
                .expect("the walk ran the hook");
            assert!(
                (1..=RECALL_STRIDE).contains(&positions),
                "the walk read the stop within a stride of the raise: {positions} positions"
            );
            assert!(
                (raised_after..raised_after + RECALL_STRIDE).contains(&stamped),
                "the stamps written up to the reading stayed: {stamped}"
            );
        }

        unsafe { ll_gc_maybe_collect() };
        unsafe { let_the_state_go(&mut arena, state) };
    }
}

/// A batch whose record's epoch moved between its trace and its stamps writes
/// none: a stamp of the epoch the trace read would read as none. Red without
/// the re-reading of the record's cell.
#[test]
fn an_epoch_moved_before_the_stamps_writes_none() {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let state = unsafe {
        a_state_behind_a_held_entry(&mut arena, "MovedState", member_class("MovedMember"), RING)
    };
    unsafe { &*record() }.set_batch_size(1);
    let _ = take_stamps();

    testing::turn_the_epoch_before_the_next_stamps();
    let _ = served_by_a_collector();
    assert_eq!(take_stamps(), 0);
    assert!(stamps(&state.ring).iter().all(|&(_, age)| age == 0));

    unsafe { ll_gc_maybe_collect() };
    unsafe { let_the_state_go(&mut arena, state) };
}
