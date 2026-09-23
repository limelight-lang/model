//! What the epoch turnover re-offers, and what a deferral costs the component
//! that dies behind it (`dev/BENCHMARKS.md`, "S37.5 what a turnover re-offers,
//! and what a deferral costs").
//!
//! Ignored in the ordinary suite and run by hand:
//!
//! ```text
//! cargo test --lib mark::tests::the_volume_a_turnover_reoffers -- --ignored --nocapture
//! ```
//!
//! # Two readings, and why neither is one number
//!
//! The turnover period `N` is 64 of the collector's batches for the mutator
//! (`crate::cycle::epoch::BATCHES_PER_EPOCH`), taken from YRC and never
//! measured here. The probe's in-line collections stand in for those batches:
//! the harness turns the epoch cell after every `N` of them, as the
//! collector's count would (`crate::cycle::worker`, "The epoch clock"). In
//! production an in-line collection counts toward nothing, so the recall
//! reading's `N − d + 1` is the model's figure for a thread whose every
//! collection is a collector's batch, and follows from the stand-in by
//! construction; what the probe measures of the lane is the volume. Two figures decide it, and both follow a rate
//! the harness sets, so a single figure would be its own input read back
//! (`dev/DECISIONS.md`, 2026-09-19, "the calibration runs on a parameterized
//! test heap, and the entry names its parameters"): the volume the turnover
//! splices back, and the recall a deferral costs.
//!
//! # The volume reading
//!
//! `rate` live components are built before every collection and kept alive. A
//! collection whose reading finds the lane externally referenced defers the
//! whole batch, so one record per live root enters the lane and the occupancy
//! is `rate × collections`. The volume a turnover of `N` re-offers is that line
//! read at `N`. The run starts at a turn of the cell and stays inside the
//! epoch it opens, so nothing is re-offered while the line is taken.
//!
//! # The recall reading
//!
//! One component is let go at collection `d` of the epoch. Its record stands in
//! the deferred lane and a decrement adds no second record while it stands
//! (`crate::cycle::queue`), so nothing offers the ring to a trace until the lane
//! is re-offered. On a thread whose active lane keeps filling that is the
//! turnover, `N − d + 1` collections later. On a thread with nothing else
//! registered no batch counts toward `N`: the wait is X, the interval after
//! which the collector advances the cell on its own clock
//! (`crate::cycle::worker`, "The epoch clock"), and the poll after the advance
//! takes the ring — one collection, whatever `d`. The harness stands in for the
//! collector's advance, so the idle cells read that one collection and say
//! nothing about X's length; the background rate tells the two lanes apart.
//!
//! # The precondition both readings need
//!
//! **A close with no spare cell keeps the root in the active lane**, so a load
//! that never refills the spares reads an empty deferred lane whatever it does
//! (`cycle::collect::tests::when_the_turnover_reoffers`, "with no spare cell
//! the close keeps the root in the active lane"). Every poll here refills them.

use std::cell::Cell;
use std::ptr;

use super::*;
use crate::cycle::epoch::{self, BATCHES_PER_EPOCH};
use crate::cycle::queue::{
    deferred_count, refill_spares, release_queue_segments, reoffer_deferred_if_epoch_moved,
};
use crate::cycle::row::take_edge_dispatches;
use crate::cycle::testing::{move_prop, on_a_fresh_thread};
use crate::gc::ll_gc_collect_cycles;

/// Live components built before each collection, and with the collection count
/// the arrival rate the lane's growth answers.
const ARRIVALS: [usize; 4] = [1, 2, 4, 8];

/// Collections the arrival curve is read over. Sixteen is a quarter of the
/// turnover: enough to read a line, short enough to leave the epoch untouched.
const CURVE_COLLECTIONS: usize = 16;

/// Where in the epoch the ring is let go, for the recall reading.
const DEATHS_AT: [usize; 3] = [1, 16, 32];

/// Background arrivals a collection during the recall reading: none, which
/// empties the active lane and takes the early re-offer, and one, which keeps
/// the lane filled so that only the turnover reaches the record.
const BACKGROUND: [usize; 2] = [0, 1];

/// A live component, in the one shape whose live reading is the prune's own
/// (`cycle::collect::tests::when_the_turnover_reoffers`): a ring of two whose
/// member no lane names — its creation reference was moved into the root, so
/// no release ever registered it — under a keeper that holds the root. Once
/// the member matures the trace refuses the edge into it, cannot subtract its
/// in-edge to the root, and reads the component live.
struct Live {
    root: *mut Object,
    member: *mut Object,
    keeper: *mut Object,
}

unsafe fn build_live(arena: &mut Arena, class: *const Class) -> Live {
    unsafe {
        let root = a_held_object(arena, class);
        let member = a_held_object(arena, class);
        let keeper = a_held_object(arena, class);

        move_prop(root, prop_offset(0), member);
        store_prop(arena, member, prop_offset(0), root);
        store_prop(arena, keeper, prop_offset(1), root);
        assert!(
            !ll_release(root as *mut RcHeader),
            "the member and the keeper hold the root, so the release registers it"
        );

        Live {
            root,
            member,
            keeper,
        }
    }
}

/// The keeper lets the component go. Nothing is freed here: the ring holds
/// itself, and what is left is garbage whose root the prune reads live.
unsafe fn let_go(arena: &mut Arena, live: Live) {
    unsafe {
        store_prop(arena, live.keeper, prop_offset(1), ptr::null_mut());
        assert!(ll_release(live.keeper as *mut RcHeader));
        ll_object_die(live.keeper);
        let _ = live.root;
        let _ = live.member;
    }
}

thread_local! {
    /// Collections since the harness last turned this thread's cell: the
    /// stand-in for the collector's count of batches.
    static SINCE_THE_TURN: Cell<u8> = const { Cell::new(0) };
}

/// Turn this thread's cell as the collector would at an advance, and restart
/// the count of collections the next advance is measured by.
fn turn_the_cell() {
    epoch::turn_this_threads_cell();
    SINCE_THE_TURN.with(|since| since.set(0));
}

/// Stand at a turn into an epoch that is not zero, the count restarted.
fn stand_at_a_turn() {
    epoch::turn_to_a_nonzero_epoch();
    SINCE_THE_TURN.with(|since| since.set(0));
}

/// Close the epoch and collect until nothing is freed, so that a load gives
/// back everything it built: a deferred record waits out its epoch, and a
/// leaked component would fail some other case.
fn drain() {
    turn_the_cell();
    for _ in 0..8 {
        if poll_and_collect() == 0 {
            return;
        }
    }
    panic!("the load's own garbage outlived eight collections past a turnover");
}

/// One safepoint and the collection behind it, in the order the poll
/// (`crate::gc::ll_gc_maybe_collect`) takes them — the turnover comparison,
/// then the collection — with the poll's test of the lane's occupancy left
/// out. The collector's advance at `N` collections is made first, where it is
/// due, by the harness standing in for it.
fn poll_and_collect() -> usize {
    if SINCE_THE_TURN.with(Cell::get) == BATCHES_PER_EPOCH {
        turn_the_cell();
    }

    // A close with no spare cell keeps the root in the active lane, so the
    // deferral this load reads needs the spares topped up
    // (`cycle::collect::tests::when_the_turnover_reoffers`).
    let _ = refill_spares();
    let _ = reoffer_deferred_if_epoch_moved();
    let collected = unsafe { ll_gc_collect_cycles() };
    SINCE_THE_TURN.with(|since| since.set(since.get() + 1));
    let _ = take_edge_dispatches();
    collected
}

/// The deferred lane's occupancy per collection, with `rate` live components
/// built before every one: each is deferred at its first collection, read live
/// through its keeper, so the lane grows by `rate` a collection from the
/// first, and the volume a turnover of `N` re-offers is that line read at
/// `N`.
fn an_arrival_curve(rate: usize) -> Vec<(usize, usize)> {
    release_queue_segments();
    stand_at_a_turn();
    let class = node_class(&format!("ArrivalLoad{rate}"));
    let mut arena = Arena::new();
    let mut standing: Vec<Live> = Vec::new();

    let curve = (0..CURVE_COLLECTIONS)
        .map(|_| {
            for _ in 0..rate {
                standing.push(unsafe { build_live(&mut arena, class) });
            }
            let freed = poll_and_collect();
            (deferred_count(), freed)
        })
        .collect();

    for live in standing {
        unsafe { let_go(&mut arena, live) };
    }
    drain();
    curve
}

/// Collections from the death of a matured ring to the collection that takes
/// it, with the death `death_at` collections into the epoch and `background`
/// live components arriving before every collection.
fn a_recall_delay(death_at: usize, background: usize) -> usize {
    release_queue_segments();
    stand_at_a_turn();
    let class = node_class(&format!("RecallLoad{death_at}x{background}"));
    let mut arena = Arena::new();
    let mut standing: Vec<Live> = Vec::new();

    let dying = unsafe { build_live(&mut arena, class) };
    for _ in 0..death_at {
        for _ in 0..background {
            standing.push(unsafe { build_live(&mut arena, class) });
        }
        assert_eq!(poll_and_collect(), 0, "every ring is still held");
    }

    // The keeper lets go. The ring holds itself, so nothing is freed here; the
    // deferred record is what keeps a collection from seeing it. A thread with
    // no background reaches no count of batches: the harness makes the advance
    // the collector would make after X.
    unsafe { let_go(&mut arena, dying) };
    if background == 0 {
        turn_the_cell();
    }

    let mut waited = 0;
    let collected = loop {
        for _ in 0..background {
            standing.push(unsafe { build_live(&mut arena, class) });
        }
        let collected = poll_and_collect();
        waited += 1;
        if collected != 0 {
            break collected;
        }
        assert!(
            waited <= usize::from(BATCHES_PER_EPOCH),
            "a turnover closes inside one epoch's worth of collections"
        );
    };
    assert_eq!(collected, 2, "the ring of two is what was taken");

    // The background keepers go, and the collections behind the turnover take
    // what they held: a load leaves nothing of its own standing.
    for live in standing {
        unsafe { let_go(&mut arena, live) };
    }
    drain();
    waited
}

/// The lane's occupancy per collection, so that the volume a turnover of `N`
/// re-offers is the curve read at `N`.
#[test]
#[ignore = "a measurement, recorded in dev/BENCHMARKS.md; run with --ignored"]
fn the_lane_grows_by_the_rate_live_roots_arrive() {
    let _g = test_guard();
    for rate in ARRIVALS {
        let curve = on_a_fresh_thread(move || an_arrival_curve(rate));
        println!("\n== {rate} live roots arrive a collection ==");
        println!("  n  deferred  freed");
        for (index, &(standing, freed)) in curve.iter().enumerate() {
            let collections = index + 1;
            println!("  {:<2} {:<9} {}", collections, standing, freed);
            assert_eq!(
                standing,
                rate * collections,
                "{rate} a collection, collection {collections}: the lane's occupancy"
            );
            assert_eq!(freed, 0, "every component is held");
        }
    }
}

/// What a deferral costs the component that dies behind it: the rest of the
/// epoch on a thread whose active lane keeps filling, because nothing offers
/// the ring before the turnover; and X on one with nothing else registered,
/// read here as the one collection after the collector's advance.
#[test]
#[ignore = "a measurement, recorded in dev/BENCHMARKS.md; run with --ignored"]
fn a_deferred_death_waits_for_the_traffic_behind_it() {
    let _g = test_guard();
    println!("\n== recall, by where in the epoch the death falls ==");
    println!("  background  death_at  collections waited");
    for background in BACKGROUND {
        for death_at in DEATHS_AT {
            let waited = on_a_fresh_thread(move || a_recall_delay(death_at, background));
            println!("  {background:<11} {death_at:<9} {waited}");
            // The idle cells read the advance rather than the lane: the poll
            // after it re-offers the lane and takes the ring, whatever `d`.
            let expected = if background == 0 {
                1
            } else {
                usize::from(BATCHES_PER_EPOCH) - death_at + 1
            };
            assert_eq!(
                waited, expected,
                "background {background}, death at {death_at}: collections waited"
            );
        }
    }
}
