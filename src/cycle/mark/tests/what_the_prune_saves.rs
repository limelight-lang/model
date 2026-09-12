//! The edges the prune refuses and the rows the mark no longer resolves, at
//! `k` of 1, 2 and 3, on a load whose answer is fixed by construction
//! (`PLAN.md` S40.1, the synthetic pruning arm).
//!
//! Ignored in the ordinary suite and run by hand:
//!
//! ```text
//! cargo test --lib mark::tests::what_the_prune_saves -- --ignored --nocapture
//! ```
//!
//! The numbers are in `dev/BENCHMARKS.md`; what stands here is the
//! construction that produced them, so a later run can be compared against
//! the same population.
//!
//! # The load
//!
//! A registered ring of two under a keeper, with a held ring of `n` members
//! hanging off the first member's second property. The registered ring is the
//! root population; the held ring is what the prune is about, since every
//! member keeps its creation reference, no queue entry names one, and the
//! trace reaches the whole of it through one edge. The sizes are S40.3's: 2,
//! 16, 256 and 381.
//!
//! The stamps are the commit's and not the harness's. The held ring is one
//! strongly connected component, so commit `c` writes age `min(c, 3)` on every
//! member (`crate::cycle::maturation`), and the collection after the one that
//! wrote `k` is the first that stops at the entry. The rows the mark resolves
//! are `n + 5` until then — two roots, the ring's two edges, the entry edge and
//! the `n` edges of the held ring — and 4 after, whatever `n` is, because the
//! prune spares a target and everything behind it.
//!
//! # What the reading can say
//!
//! The collection at which the prune takes effect, the edges it refuses and
//! the rows the mark no longer resolves, read off `take_edges_pruned` and
//! `take_dispatches_in_mark_phase`; both are cleared before every collection.
//! A *share* on this load is the harness's own liveness schedule read back,
//! and a production `k` waits on the corpus arm (`PLAN.md` S40.1). What the
//! run calibrates is the instrument that arm will read, at each `k` the age
//! field can hold, against the producer the crate has built rather than
//! against a simulation of it (`dev/BENCHMARKS.md`, 2026-09-09, withdrawn).

use std::ptr;

use super::*;
use crate::cycle::epoch;
use crate::cycle::queue::{release_queue_segments, reoffer_deferred_candidates};
use crate::cycle::row::{
    take_dispatches_before_the_descent, take_dispatches_in_mark_phase, take_edge_dispatches,
};
use crate::cycle::testing::{dismantle_ring, on_a_fresh_thread, ring, stamp_of};
use crate::gc::ll_gc_collect_cycles;

/// The held ring's sizes: S40.3's component sizes, so that the two runs can
/// be quoted in one sentence. 381 is the corpus's median closure.
const HELD_RING_SIZES: [usize; 4] = [2, 16, 256, 381];

/// Collections run over each load. Four is the least a `k` of three needs to
/// show the prune; eight leaves the same room `density`'s loads take.
const COLLECTIONS: usize = 8;

/// What one collection left.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Reading {
    /// Edges the mark refused to take.
    pruned: usize,
    /// Row resolutions the mark made: one per root and one per counted child
    /// it dispatched.
    mark_rows: usize,
    /// Row resolutions the mark and the scan made together.
    trace_rows: usize,
    /// The stamp on the held ring's entry, `(epoch, age)`.
    entry: (u32, u32),
    /// The stamp on the member across the held ring from the entry. A
    /// fixture check and not evidence of the component-wide minimum: every
    /// held member is met by exactly the collections that meet the entry, so
    /// a per-entity age would read the same here. The minimum is
    /// `crate::cycle::maturation`'s case, over a member joined late.
    far: (u32, u32),
}

/// One collection over the load, with the deferred lane re-offered first.
///
/// A root read live stands in the deferred lane at the close and the explicit
/// fire re-offers nothing by itself; the exit's rounds re-offer by hand
/// (`crate::cycle::collect::collect_before_exit`), and so does this, so every
/// collection traces both roots.
fn collect(held: &[*mut Object]) -> Reading {
    reoffer_deferred_candidates();
    take_edges_pruned();
    let _ = take_edge_dispatches();
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        0,
        "the keeper holds the roots and the roots hold the held ring"
    );

    let reading = Reading {
        pruned: take_edges_pruned(),
        mark_rows: take_dispatches_in_mark_phase(),
        trace_rows: take_dispatches_before_the_descent(),
        entry: unsafe { stamp_of(held[0]) },
        far: unsafe { stamp_of(held[held.len() / 2]) },
    };
    let _ = take_edge_dispatches();
    reading
}

/// Build the load under a pinned threshold, collect [`COLLECTIONS`] times and
/// take it apart.
fn a_pruning_load(threshold: u32, members: usize) -> Vec<Reading> {
    release_queue_segments();
    let _epoch = epoch::pin(0);
    let _threshold = pin_threshold(threshold);
    let class = node_class(&format!("PruneLoad{threshold}x{members}"));
    let mut arena = Arena::new();

    let roots = unsafe { ring(&mut arena, [class, class]) };
    let keeper = unsafe { a_held_object(&mut arena, class) };
    unsafe { store_prop(&mut arena, keeper, prop_offset(1), roots[0]) };

    let held: Vec<*mut Object> = (0..members)
        .map(|_| unsafe { a_held_object(&mut arena, class) })
        .collect();
    for (position, &member) in held.iter().enumerate() {
        let next = held[(position + 1) % members];
        unsafe { store_prop(&mut arena, member, prop_offset(0), next) };
    }

    unsafe { store_prop(&mut arena, roots[0], prop_offset(1), held[0]) };

    let readings = (0..COLLECTIONS).map(|_| collect(&held)).collect();

    // The entry loses the roots' edge first, so that each held member's
    // creation reference is the last one standing when its ring edge is
    // nulled; then the keeper lets the roots go, which is the state
    // `dismantle_ring` asks for.
    unsafe {
        store_prop(&mut arena, roots[0], prop_offset(1), ptr::null_mut());
        for &member in &held {
            store_prop(&mut arena, member, prop_offset(0), ptr::null_mut());
        }

        for &member in &held {
            assert!(ll_release(member as *mut RcHeader));
            ll_object_die(member);
        }

        store_prop(&mut arena, keeper, prop_offset(1), ptr::null_mut());
        dismantle_ring(&mut arena, roots);
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
    }

    readings
}

/// Every reading against what the construction fixes: no edge is pruned
/// until the commit that wrote `k` has closed, exactly one is pruned at every
/// collection after it, the mark's rows fall from `n + 5` to 4 at the same
/// collection and the trace's from `2n + 10` to 9 — the scan dispatches every
/// edge the mark did, and the pruned one besides, where it finds no row — and
/// the held ring's stamp stops at `k` because a target the descent does not
/// reach is not restamped.
fn every_collection_reads_as_constructed(threshold: u32, members: usize, readings: &[Reading]) {
    for (index, reading) in readings.iter().enumerate() {
        let collection = index + 1;
        let pruned = collection > threshold as usize;
        let context = format!("k = {threshold}, n = {members}, collection {collection}");
        assert_eq!(
            reading.pruned,
            usize::from(pruned),
            "{context}: edges pruned"
        );
        assert_eq!(
            reading.mark_rows,
            if pruned { 4 } else { members + 5 },
            "{context}: rows the mark resolved"
        );
        assert_eq!(
            reading.trace_rows,
            if pruned { 9 } else { 2 * members + 10 },
            "{context}: rows the mark and the scan resolved together"
        );
        let age = (collection as u32).min(threshold);
        assert_eq!(reading.entry, (0, age), "{context}: the entry's stamp");
        assert_eq!(
            reading.far, reading.entry,
            "{context}: the far member was met exactly when the entry was"
        );
    }
}

/// One line per collection, as `dev/BENCHMARKS.md` records it. The last
/// column is the rows the prune spared at that collection against the rows
/// the same load resolves with nothing pruned, which its first collection
/// reads.
fn report(threshold: u32, members: usize, readings: &[Reading]) {
    println!("\n== k = {threshold}, held ring of {members} ==");
    println!("  n  pruned  mark_rows  trace_rows  entry_age  rows_spared/unpruned");
    let unpruned = readings[0].mark_rows;
    for (index, reading) in readings.iter().enumerate() {
        let spared = unpruned - reading.mark_rows;
        println!(
            "  {:<2} {:<7} {:<10} {:<11} {:<10} {}/{} ({:.1} %)",
            index + 1,
            reading.pruned,
            reading.mark_rows,
            reading.trace_rows,
            reading.entry.1,
            spared,
            unpruned,
            spared as f64 * 100.0 / unpruned as f64
        );
    }
}

/// The three thresholds over the four sizes, each load on a thread of its
/// own so that the first collection's reading is the same on every run.
#[test]
#[ignore = "a measurement, recorded in dev/BENCHMARKS.md; run with --ignored"]
fn the_prune_at_every_threshold_the_age_field_holds() {
    let _g = test_guard();
    for threshold in 1..=MATURATION_AGE_MAX {
        for members in HELD_RING_SIZES {
            let readings = on_a_fresh_thread(move || a_pruning_load(threshold, members));
            every_collection_reads_as_constructed(threshold, members, &readings);
            report(threshold, members, &readings);
        }
    }
}
