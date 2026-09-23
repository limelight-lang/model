//! What the poll costs per call on a registered thread with nothing to do —
//! unarmed, its queue empty, its byte `FREE` — which is the path a
//! statement pays (`rfc/dev/design/trace-token-handshake.md`, "Cost": one
//! acquire load of the byte in place of the per-poll peek of P); and the
//! same poll with one record standing in the deferred lane, which adds the
//! load of the collector's turnover byte beside the token byte
//! (`crate::cycle::queue::reoffer_deferred_if_epoch_moved`).
//!
//! A measurement probe: `cargo test --release --lib -- --ignored
//! what_the_poll_costs`, one binary per tree, the minimum beside the median
//! (`dev/BENCHMARKS.md`, Method).

use super::*;
use std::hint::black_box;
use std::time::Instant;

const POLLS: usize = 200_000;
const ROUNDS: usize = 9;

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn measure_what_the_poll_costs() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    assert!(crate::cycle::queue::refill_spares());
    crate::gc::disarm();

    // A warm-up round, dropped: the first measurement a process takes is
    // systematically slow (`dev/BENCHMARKS.md`, Method).
    let mut samples = Vec::with_capacity(ROUNDS);
    for round in 0..=ROUNDS {
        let start = Instant::now();
        for _ in 0..black_box(POLLS) {
            assert_eq!(unsafe { ll_gc_maybe_collect() }, 0, "nothing to collect");
        }

        let ns = start.elapsed().as_nanos() as f64 / POLLS as f64;
        if round > 0 {
            samples.push(ns);
        }
    }

    samples.sort_by(|a, b| a.partial_cmp(b).expect("no NaN"));
    println!(
        "poll_cost unarmed_empty_free: median={:.2} ns min={:.2} ns over {POLLS} per round, {ROUNDS} rounds",
        samples[samples.len() / 2],
        samples[0]
    );
}

/// The same poll with one live root standing in the deferred lane and no
/// advance made: the lane's occupancy is read off the base block's control
/// line and the turnover byte off the record's token line, both lines the
/// poll touches already. Recorded as an absolute figure beside the empty
/// arm (`dev/BENCHMARKS.md`, "S60.6 what the poll costs with a deferred
/// record standing").
#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn measure_what_the_poll_costs_with_a_deferred_record_standing() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    assert!(crate::cycle::queue::refill_spares());
    crate::cycle::epoch::turn_to_a_nonzero_epoch();
    crate::gc::disarm();

    let node = ClassBuilder::new("PollCostNode").prop("next", true).build();
    let mut arena = Arena::new();
    let members = unsafe { crate::cycle::testing::ring(&mut arena, [node, node]) };
    let keeper = {
        let mut context = LLContext { arena: &mut arena };
        unsafe {
            new_constructed(
                &mut context,
                keeper_class("PollCostKeeper"),
                MemoryCategory::GcHeap,
            )
        }
    };
    unsafe { store_prop(&mut arena, keeper, prop_offset(0), members[0]) };
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        0,
        "the keeper holds the ring"
    );
    assert_eq!(
        crate::cycle::queue::deferred_count(),
        2,
        "the ring's roots stand deferred"
    );
    assert_eq!(crate::cycle::queue::candidate_count(), 0);
    // The fill signalled the collector; in production the first poll wakes
    // or births it and the flag comes down. The harness births none, so
    // the wake is taken here, or every poll would run the refused birth.
    assert!(crate::cycle::queue::take_the_signal_for_test());

    let mut samples = Vec::with_capacity(ROUNDS);
    for round in 0..=ROUNDS {
        let start = Instant::now();
        for _ in 0..black_box(POLLS) {
            assert_eq!(unsafe { ll_gc_maybe_collect() }, 0, "nothing to collect");
        }

        let ns = start.elapsed().as_nanos() as f64 / POLLS as f64;
        if round > 0 {
            samples.push(ns);
        }
    }
    assert_eq!(
        crate::cycle::queue::deferred_count(),
        2,
        "no poll moved the lane"
    );

    samples.sort_by(|a, b| a.partial_cmp(b).expect("no NaN"));
    println!(
        "poll_cost unarmed_deferred_free: median={:.2} ns min={:.2} ns over {POLLS} per round, {ROUNDS} rounds",
        samples[samples.len() / 2],
        samples[0]
    );

    // The ring goes back: the keeper lets go, the advance the collector
    // would make after X is made by hand, and the poll after it takes it.
    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
    }
    crate::cycle::epoch::turn_this_threads_cell();
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2, "the ring went back");
}
