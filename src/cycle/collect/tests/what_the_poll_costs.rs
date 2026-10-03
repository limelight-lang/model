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
    // would make after X is made by hand, the poll after it re-offers the
    // lane, and the explicit fire stands in for the collector's batch.
    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
    }
    crate::cycle::epoch::turn_this_threads_cell();
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        0,
        "the poll collects nothing"
    );
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 2, "the ring went back");
}

/// Objects of the acyclic tree the cascade probe frees: the largest request
/// `web-heap` draws (`worker::tests::the_web_loads`, `MOST_OBJECTS`).
const CASCADE_OBJECTS: usize = 400_000;

/// What reference counting alone pays to free a tree of `CASCADE_OBJECTS`
/// objects by the release of its root: the least cost a collection over a posted
/// set of that size is read against (`dev/plans/S67.md`, S67.12). A binary
/// tree of plain objects, two counted properties each, built in one arena
/// and released once; the minimum and the median of `ROUNDS`, in ms and per
/// object.
#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn measure_an_acyclic_cascade_of_a_requests_size() {
    let _g = test_guard();
    let class = ClassBuilder::new("CascadeNode")
        .prop("left", true)
        .prop("right", true)
        .build();
    let mut samples = Vec::with_capacity(ROUNDS);
    for round in 0..=ROUNDS {
        let mut arena = Arena::new();
        let nodes: Vec<*mut Object> = (0..CASCADE_OBJECTS)
            .map(|_| {
                let mut context = LLContext { arena: &mut arena };
                unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) }
            })
            .collect();
        // Node i holds nodes 2i + 1 and 2i + 2, each child's creation
        // reference moved into its parent's property, so no decrement registers
        // a candidate.
        for (index, &node) in nodes.iter().enumerate() {
            for (property, child) in [2 * index + 1, 2 * index + 2].into_iter().enumerate() {
                if child < CASCADE_OBJECTS {
                    unsafe {
                        crate::cycle::testing::move_prop(
                            node,
                            prop_offset(property as u32),
                            nodes[child],
                        )
                    };
                }
            }
        }

        let start = Instant::now();
        assert!(
            unsafe { ll_release(nodes[0] as *mut RcHeader) },
            "the root's release is its last"
        );
        unsafe { crate::object::ll_object_die(nodes[0]) };
        let took = start.elapsed();
        let last = nodes[CASCADE_OBJECTS - 1] as *mut RcHeader;
        assert_ne!(
            unsafe { crate::refcount::slot_state(last) },
            crate::refcount::SlotState::Live,
            "the release reached the tree's last leaf"
        );
        if round > 0 {
            samples.push(took);
        }
    }

    samples.sort();
    let (minimum, median) = (samples[0], samples[samples.len() / 2]);
    println!(
        "acyclic_cascade objects={CASCADE_OBJECTS}: min={:.1} ms median={:.1} ms, {:.0} ns an object at the minimum",
        minimum.as_secs_f64() * 1e3,
        median.as_secs_f64() * 1e3,
        minimum.as_nanos() as f64 / CASCADE_OBJECTS as f64
    );
}
