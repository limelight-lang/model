//! What the poll costs per call on a registered thread with nothing to do —
//! unarmed, its queue empty, its byte `FREE` — which is the path a
//! statement pays (`rfc/dev/design/trace-token-handshake.md`, "Cost": one
//! acquire load of the byte in place of the per-poll peek of P).
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
