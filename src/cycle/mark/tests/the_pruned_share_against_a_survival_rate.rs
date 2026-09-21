//! The share of prunable edges the prune refuses, as a response to a named
//! survival rate, at `k` of 1, 2 and 3 (`dev/BENCHMARKS.md`, "S37.7 the pruned
//! share against a named survival rate").
//!
//! Ignored in the ordinary suite and run by hand:
//!
//! ```text
//! cargo test --lib mark::tests::the_pruned_share_against_a_survival_rate -- --ignored --nocapture
//! ```
//!
//! # Why a response and not a figure
//!
//! A pruned share is a property of the workload's age distribution, and on a
//! built population the harness sets that distribution. One number taken here
//! would be the harness's own input read back, which is what the sibling load
//! `super::what_the_prune_saves` says about itself and why it reports no share
//! at all. Edmond ruled on 2026-09-19 that no corpus over this crate's heap is
//! coming and the calibration runs on test data (`dev/DECISIONS.md`, "the
//! calibration runs on a parameterized test heap, and the entry names its
//! parameters"), so the reading that stays honest is the response: the share
//! against the rate that produces it, with the rate named.
//!
//! # The load, and the arithmetic it is asserted against
//!
//! [`UNITS`] units stand at once. A unit is a registered ring of two under a
//! keeper, with a held ring of [`MEMBERS`] hanging off the first root's second
//! property — the shape of the sibling load, so the two runs are comparable —
//! and it offers the trace exactly one prunable edge, the entry into its held
//! ring.
//!
//! Before every collection the oldest `kills` units are taken apart and the
//! same number of fresh ones is built, so the population size never moves and
//! a unit lives exactly `UNITS / kills` collections. The retirement rate
//! `q = kills / UNITS` is the one parameter of this load; the survival rate
//! the name refers to is `1 - q`, and the population's age distribution is
//! fixed with it — uniform over `0..1 / q` — which is what makes the share
//! linear in `k` here and not on every heap.
//!
//! A unit is pruned at every collection after the one that wrote age `k`
//! (`super::what_the_prune_saves`), so of the `L = 1 / q` collections it lives
//! it is pruned in `L - k` of them, and in the steady state the share of units
//! pruned at one collection is `max(0, 1 - k * q)`. That is what the case
//! asserts, exactly, over the collections past the first `L`, and what the
//! report prints beside the reading.
//!
//! `q = 0` is the load that never dies, whose share is 1 from collection
//! `k + 1` on; it is the sibling load's population read as a share.

use std::ptr;

use super::*;
use crate::cycle::epoch;
use crate::cycle::queue::{release_queue_segments, reoffer_deferred_candidates};
use crate::cycle::row::{take_dispatches_in_mark_phase, take_edge_dispatches};
use crate::cycle::testing::{dismantle_ring, on_a_fresh_thread, ring};
use crate::gc::ll_gc_collect_cycles;

/// Units standing at once. Thirty-two divides every rate below into whole
/// units, which keeps the predicted share exact arithmetic rather than a
/// rounding.
const UNITS: usize = 32;

/// Members of a unit's held ring. Sixteen is the sibling load's second size:
/// large enough that a pruned edge spares rows worth counting, small enough
/// that the whole run stays inside a debug build's patience.
const MEMBERS: usize = 16;

/// Units retired before each collection, and with `UNITS` the retirement rate
/// `q`. Zero is the population that never dies; 16 is half of it, where a
/// unit lives two collections and no `k` above one can prune at all.
const KILLS: [usize; 4] = [0, 4, 8, 16];

/// Rows the mark resolves for a unit whose entry edge it pruned: the two
/// roots and the registered ring's two edges, whatever the held ring's size.
const ROWS_OF_A_PRUNED_UNIT: usize = 4;

/// Rows the mark resolves for a unit it descended into: the pruned unit's four
/// and the `MEMBERS + 1` the entry edge reaches, the sibling load's `n + 5`.
const ROWS_OF_AN_UNPRUNED_UNIT: usize = ROWS_OF_A_PRUNED_UNIT + MEMBERS + 1;

/// Collections per load. Twice the longest life below (eight collections at
/// four kills), so the steady state is reached and then read for as long
/// again.
const COLLECTIONS: usize = 16;

/// One unit of the population: the two registered roots, the keeper that
/// holds them, and the held ring the entry edge reaches.
struct Unit {
    roots: [*mut Object; 2],
    keeper: *mut Object,
    held: Vec<*mut Object>,
}

/// What one collection left.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Reading {
    /// Edges the mark refused to take: one per unit whose entry has matured.
    pruned: usize,
    /// Row resolutions the mark made.
    mark_rows: usize,
}

/// Build one unit. The entry edge is stored last, so the held ring is whole
/// before the roots name it.
unsafe fn build_unit(arena: &mut Arena, class: *const Class) -> Unit {
    unsafe {
        let roots = ring(arena, [class, class]);
        let keeper = a_held_object(arena, class);
        store_prop(arena, keeper, prop_offset(1), roots[0]);

        let held: Vec<*mut Object> = (0..MEMBERS).map(|_| a_held_object(arena, class)).collect();
        for (position, &member) in held.iter().enumerate() {
            store_prop(
                arena,
                member,
                prop_offset(0),
                held[(position + 1) % MEMBERS],
            );
        }

        store_prop(arena, roots[0], prop_offset(1), held[0]);
        Unit {
            roots,
            keeper,
            held,
        }
    }
}

/// Take a unit apart, in the sibling load's order: the entry edge first, so
/// each held member's creation reference is the last one standing when its
/// ring edge is nulled, then the keeper lets the roots go.
unsafe fn drop_unit(arena: &mut Arena, unit: Unit) {
    unsafe {
        store_prop(arena, unit.roots[0], prop_offset(1), ptr::null_mut());
        for &member in &unit.held {
            store_prop(arena, member, prop_offset(0), ptr::null_mut());
        }
        for &member in &unit.held {
            assert!(ll_release(member as *mut RcHeader));
            ll_object_die(member);
        }

        store_prop(arena, unit.keeper, prop_offset(1), ptr::null_mut());
        dismantle_ring(arena, unit.roots);
        assert!(ll_release(unit.keeper as *mut RcHeader));
        ll_object_die(unit.keeper);
    }
}

/// One collection over the standing population, with the deferred lane
/// re-offered first: a root read live is deferred at the close, and the
/// explicit fire re-offers nothing by itself.
fn collect() -> Reading {
    reoffer_deferred_candidates();
    take_edges_pruned();
    let _ = take_edge_dispatches();
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        0,
        "every standing unit is held by its keeper; the retired ones died by count"
    );

    let reading = Reading {
        pruned: take_edges_pruned(),
        mark_rows: take_dispatches_in_mark_phase(),
    };
    let _ = take_edge_dispatches();
    reading
}

/// Build the population under a pinned threshold, run [`COLLECTIONS`]
/// collections retiring `kills` units before each, and take what is left
/// apart.
fn a_survival_load(threshold: u32, kills: usize) -> Vec<Reading> {
    release_queue_segments();
    let _epoch = epoch::pin(0);
    let _threshold = pin_threshold(threshold);
    let class = node_class(&format!("SurvivalLoad{threshold}x{kills}"));
    let mut arena = Arena::new();

    let mut population: std::collections::VecDeque<Unit> = (0..UNITS)
        .map(|_| unsafe { build_unit(&mut arena, class) })
        .collect();

    let readings = (0..COLLECTIONS)
        .map(|_| {
            for _ in 0..kills {
                let oldest = population
                    .pop_front()
                    .expect("the population never empties");
                unsafe { drop_unit(&mut arena, oldest) };
                population.push_back(unsafe { build_unit(&mut arena, class) });
            }
            collect()
        })
        .collect();

    while let Some(unit) = population.pop_front() {
        unsafe { drop_unit(&mut arena, unit) };
    }

    readings
}

/// The share the construction fixes at a collection: nothing until the
/// commit that wrote `k` has closed, and in the steady state `1 - k * q` of
/// the standing units, which is `UNITS - k * kills` of them.
fn predicted(threshold: u32, kills: usize, collection: usize) -> Option<usize> {
    if collection <= threshold as usize {
        return Some(0);
    }
    // The steady state is reached once every unit built before the run has
    // been retired, which takes a full life. Before that the population still
    // holds units older than the load's own arithmetic describes.
    let life = if kills == 0 {
        usize::MAX
    } else {
        UNITS / kills
    };
    if life != usize::MAX && collection <= life {
        return None;
    }
    Some(UNITS.saturating_sub(threshold as usize * kills))
}

/// Every reading against the arithmetic above, where the arithmetic applies.
fn every_collection_reads_as_constructed(threshold: u32, kills: usize, readings: &[Reading]) {
    for (index, reading) in readings.iter().enumerate() {
        let collection = index + 1;
        let Some(expected) = predicted(threshold, kills, collection) else {
            continue;
        };
        assert_eq!(
            reading.pruned, expected,
            "k = {threshold}, kills = {kills}, collection {collection}: edges pruned"
        );
        // The rows follow the same arithmetic (`super::what_the_prune_saves`).
        assert_eq!(
            reading.mark_rows,
            expected * ROWS_OF_A_PRUNED_UNIT + (UNITS - expected) * ROWS_OF_AN_UNPRUNED_UNIT,
            "k = {threshold}, kills = {kills}, collection {collection}: rows the mark resolved"
        );
    }
}

/// One line per collection, as `dev/BENCHMARKS.md` records it.
fn report(threshold: u32, kills: usize, readings: &[Reading]) {
    let q = kills as f64 / UNITS as f64;
    println!("\n== k = {threshold}, kills = {kills} of {UNITS} a collection (q = {q:.3}) ==");
    println!("  n  pruned  share  predicted  mark_rows");
    for (index, reading) in readings.iter().enumerate() {
        let collection = index + 1;
        let expected = match predicted(threshold, kills, collection) {
            Some(units) => format!("{units}"),
            None => "-".to_string(),
        };
        println!(
            "  {:<2} {:<7} {:<6.3} {:<10} {}",
            collection,
            reading.pruned,
            reading.pruned as f64 / UNITS as f64,
            expected,
            reading.mark_rows
        );
    }
}

/// The three thresholds over the four rates, each load on a thread of its
/// own so that the first collection reads the same on every run.
#[test]
#[ignore = "a measurement, recorded in dev/BENCHMARKS.md; run with --ignored"]
fn the_pruned_share_at_every_threshold_and_rate() {
    let _g = test_guard();
    for threshold in 1..=MATURATION_AGE_MAX {
        for kills in KILLS {
            let readings = on_a_fresh_thread(move || a_survival_load(threshold, kills));
            every_collection_reads_as_constructed(threshold, kills, &readings);
            report(threshold, kills, &readings);
        }
    }
}
