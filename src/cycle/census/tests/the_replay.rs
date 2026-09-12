//! The census replayed through both row forms (`PLAN.md` S40.5): the flat
//! form's replay against the counters of the collection it reads, on every
//! load S40.3 records, and the specified chunked form's replay beside it.
//!
//! The matrix is ignored in the ordinary suite and run by hand:
//!
//! ```text
//! cargo test --lib census::tests::the_replay -- --ignored --nocapture
//! ```
//!
//! The numbers are in `dev/BENCHMARKS.md`; what the ordinary suite runs is
//! the calibration on two loads, one inside the workspace and one past it.

use super::*;
use crate::cycle::census::replay::{
    self, CollectionShape, DrawBound, Outcome, chunked, chunked_bound, flat, flat_bound,
};

/// The report of the first collection over `load`, on a thread of its own,
/// the ring let go first where `garbage` says so.
fn first_collection(load: Load, garbage: bool) -> CollectionReport {
    let (sender, receiver) = std::sync::mpsc::channel();
    on_a_fresh_thread(move || {
        release_queue_segments();
        let _epoch = epoch::pin(0);
        let mut built = unsafe { loads::build(load) };
        if garbage {
            unsafe { loads::release_ring(&mut built) };
        }

        let _armed = arm();
        let Collected { freed, report, .. } = collect_once();
        assert_eq!(freed, if garbage { load.members } else { 0 });
        the_bump_balances(&report);
        sender.send(report).expect("the report crosses the thread");
        if !garbage {
            // The ring goes the way every load's does, through the keeper's
            // release and the collection after it.
            unsafe { loads::release_ring(&mut built) };
            let torn_down = collect_once().freed;
            if load.population == LoadPopulation::Ordinary {
                assert_eq!(
                    torn_down, load.members,
                    "the ring is garbage without the keeper"
                );
            }
        }
    });

    receiver.recv().expect("the thread sent its report")
}

/// The flat replay agrees with the counters of the collection it was taken
/// from: the same requests, grants, draws, tails and remainder.
fn the_flat_replay_matches(report: &CollectionReport, replayed: &Outcome, context: &str) {
    let counters = report.counters;
    let close = report.close.expect("the close was reached");
    assert_eq!(
        (
            replayed.row_requests,
            replayed.row_bytes_requested,
            replayed.row_bytes_granted
        ),
        (
            counters.row_arrays,
            counters.row_bytes_requested,
            counters.granted.rows
        ),
        "{context}: the rows' requests and grants"
    );
    assert_eq!(
        replayed.draws,
        counters.drawn_from_pool + counters.drawn_from_reserve,
        "{context}: the blocks drawn"
    );
    assert_eq!(
        (replayed.tails, replayed.tail_bytes),
        (counters.tails_abandoned, counters.tail_bytes_abandoned),
        "{context}: the tails abandoned"
    );
    assert_eq!(
        replayed.remainder, close.remainder,
        "{context}: the remainder"
    );
    let bound = flat_bound(&CollectionShape::of(report));
    assert!(
        (bound.least..=bound.most).contains(&replayed.draws),
        "{context}: the flat bound {bound:?} brackets {}",
        replayed.draws
    );
}

/// One line of the record: the flat form as observed and replayed, the
/// chunked form as replayed, and the chunked form's bracket over every order.
fn report_line(name: &str, flat_replay: &Outcome, chunked_replay: &Outcome, bound: &DrawBound) {
    println!(
        "{name:<40} flat req {:<3} bytes {:>7}/{:<7} written {:<6} drawn {} tails {}/{:<5} \
         rem {:<6} | chunked req {:<4} bytes {:>7}/{:<7} written {:<6} cont {:<2} drawn {} \
         tails {}/{:<5} rem {:<6} | drawn {}..={} cont <= {}",
        flat_replay.row_requests,
        flat_replay.row_bytes_requested,
        flat_replay.row_bytes_granted,
        flat_replay.first_touch_writes,
        flat_replay.draws,
        flat_replay.tails,
        flat_replay.tail_bytes,
        flat_replay.remainder,
        chunked_replay.row_requests,
        chunked_replay.row_bytes_requested,
        chunked_replay.row_bytes_granted,
        chunked_replay.first_touch_writes,
        chunked_replay.continuations,
        chunked_replay.draws,
        chunked_replay.tails,
        chunked_replay.tail_bytes,
        chunked_replay.remainder,
        bound.least,
        bound.most,
        bound.continuations_at_most,
    );
}

/// Replay `load` through both forms, check the flat one against the census
/// and print the line.
fn replay_load(name: &str, load: Load, garbage: bool) {
    let report = first_collection(load, garbage);
    let shape = CollectionShape::of(&report);
    let flat_replay = flat(&shape);
    the_flat_replay_matches(&report, &flat_replay, name);
    let chunked_replay = chunked(&shape);
    let bound = chunked_bound(&shape);
    assert!(
        (bound.least..=bound.most).contains(&chunked_replay.draws),
        "{name}: the chunked bound {bound:?} brackets {}",
        chunked_replay.draws
    );
    report_line(name, &flat_replay, &chunked_replay, &bound);
}

fn ordinary(class_bytes: usize, members: usize, fillers: usize, second_edge: bool) -> Load {
    Load {
        class_bytes,
        members,
        fillers,
        population: LoadPopulation::Ordinary,
        second_edge,
    }
}

/// The calibration the ordinary suite runs: a ring inside the workspace and
/// one past it, the flat replay agreeing with the census on both, and the
/// chunked replay of the sparse ring drawing no block where the flat form
/// drew one.
#[test]
fn the_flat_replay_matches_the_census_inside_and_past_the_workspace() {
    let _g = test_guard();
    let inside = first_collection(ordinary(256, 16, 0, false), false);
    let inside_shape = CollectionShape::of(&inside);
    the_flat_replay_matches(&inside, &flat(&inside_shape), "16 at class 256");
    assert_eq!(
        chunked(&inside_shape).row_bytes_granted,
        replay::directory_bytes(32) + 2 * replay::CHUNK_BYTES,
        "16 consecutive rows are two groups behind one directory"
    );

    let past = first_collection(
        ordinary(32, 8, loads::slots_per_block(32) - 1, false),
        false,
    );
    let past_shape = CollectionShape::of(&past);
    let flat_past = flat(&past_shape);
    the_flat_replay_matches(&past, &flat_past, "8 one per block at class 32");
    assert_eq!(
        flat_past.draws, 1,
        "eight class-32 arrays are past the bump"
    );
    let chunked_past = chunked(&past_shape);
    assert_eq!(
        (chunked_past.draws, chunked_past.continuations),
        (0, 0),
        "eight directories of one chunk are 4,608 bytes"
    );
}

/// The matrix S40.3 records, replayed: the base rings dense over the four
/// classes and one per block at class 256, the retained ring, the two group
/// occupancies, the full block, the second edge, and the garbage ring.
#[test]
#[ignore = "a measurement, recorded in dev/BENCHMARKS.md; run with --ignored"]
fn the_matrix_replayed_through_both_forms() {
    let _g = test_guard();
    const SIZES: [usize; 4] = [2, 16, 256, 381];
    const DESIGN_CLASSES: [usize; 4] = [32, 64, 128, 256];
    let one_per_block = loads::slots_per_block(256) - 1;
    for class_bytes in DESIGN_CLASSES {
        for members in SIZES {
            let name = format!("dense, class {class_bytes}, {members}");
            replay_load(&name, ordinary(class_bytes, members, 0, false), false);
        }
    }

    for members in SIZES {
        let name = format!("one per block, class 256, {members}");
        replay_load(&name, ordinary(256, members, one_per_block, false), false);
    }

    for members in [2, 381] {
        let name = format!("retained, class 256, {members}");
        let load = Load {
            class_bytes: 256,
            members,
            fillers: 0,
            population: LoadPopulation::Retained,
            second_edge: false,
        };
        replay_load(&name, load, false);
    }

    replay_load(
        "32 at class 256, consecutive",
        ordinary(256, 32, 0, false),
        false,
    );
    replay_load(
        "32 at class 256, one per group",
        ordinary(256, 32, 7, false),
        false,
    );
    for class_bytes in [256, 32] {
        let members = loads::slots_per_block(class_bytes);
        let name = format!("full block, class {class_bytes} ({members})");
        replay_load(&name, ordinary(class_bytes, members, 0, false), false);
    }

    replay_load(
        "two edges, dense, class 256, 381",
        ordinary(256, 381, 0, true),
        false,
    );
    replay_load(
        "two edges, one per block, 381",
        ordinary(256, 381, one_per_block, true),
        false,
    );
    for fillers in [0, one_per_block] {
        for members in SIZES {
            let placement = if fillers == 0 {
                "dense"
            } else {
                "one per block"
            };
            let name = format!("garbage, {placement}, class 256, {members}");
            replay_load(&name, ordinary(256, members, fillers, false), true);
        }
    }
}
