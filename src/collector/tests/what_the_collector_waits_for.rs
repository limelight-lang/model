//! Measurement probe, not a correctness test: how long an epoch spends
//! waiting for the mutator rather than working.
//!
//! Node C4 of `rfc/model/gc/walk/questions.md` asks whether the pressure
//! ladder's fixpoint and stratification rungs earn their keep, and a
//! review round corrected the currency: rung 2 costs epoch **duration**,
//! not mutator cycles, and what it spends that duration on is the
//! collector waiting for a handshake ack. Nothing timed that wait.
//!
//! ## What is measured
//!
//! `run_epoch`'s three spin-yield waits, timed separately
//! (`src/collector.rs`): the ack after the epoch opens, the ack after
//! condemnation, and the wait for the verdict queue to drain. The body
//! between them is the same sequence `run_epoch` runs, copied here rather
//! than instrumented in place so no timer lands in the production shape.
//!
//! ## The free variable is the mutator, not the collector
//!
//! A checkpoint attends when the handshake flag is up, so the wait is
//! bounded by how often the mutator reaches one. The sweep is therefore
//! over the mutator's work between checkpoints, from a tight loop that
//! checkpoints continuously — which reports the floor, the handshake's
//! own latency — up to a mutator that reaches one rarely. A single
//! figure here would describe one workload and no other.
//!
//! **The threads are the way round the production shape is**: this test's
//! own thread is the mutator, because it owns the entities and the drain
//! runs where they live, and the collector is spawned. A first version
//! had them the other way and two of the three waits never ran — the
//! drain would have had to be reached by the thread already spinning on
//! it.
//!
//! ```
//! cargo test --release --lib -- --ignored measure_collector_wait --nocapture
//! ```
//!
//! The figures obey `dev/BENCHMARKS.md`'s Method and are not comparable
//! with any table there: a different binary out of a different profile.

use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use super::*;

/// Filler objects, externally retained, so the walk has a population to
/// stride and the waits are not the whole epoch.
const FILLER: usize = 20_000;

/// Iterations of a spin the mutator runs between two checkpoints. Zero
/// is the floor — checkpoints as fast as the loop allows.
const WORK: [usize; 5] = [0, 100, 1_000, 10_000, 100_000];

/// Timed epochs per point, after one warm-up whose times are discarded.
const ROUNDS: usize = 7;

/// Counts the checkpoints the mutator reached, so a point that reports a
/// long wait can be told from one that reports a stalled mutator.
static CHECKPOINTS: AtomicUsize = AtomicUsize::new(0);

/// The three waits of one epoch, in nanoseconds, beside its statistics.
struct Waits {
    open_ack: f64,
    condemn_ack: f64,
    drain: f64,
}

/// `run_epoch`'s sequence with a timer around each of its three waits.
///
/// Kept in step with `crate::collector::run_epoch` by hand: this file is
/// the only copy of that sequence, and a change there that is not made
/// here measures a shape the collector no longer has.
fn timed_epoch() -> (Waits, EpochStats) {
    let mut epoch = Epoch::open();
    let start = Instant::now();
    while !epoch.acked() {
        std::thread::yield_now();
    }

    let open_ack = start.elapsed().as_nanos() as f64;
    epoch.snapshot();
    epoch.walk();
    epoch.judge();
    if epoch.candidates.is_empty() {
        return (
            Waits {
                open_ack,
                condemn_ack: 0.0,
                drain: 0.0,
            },
            epoch.close(),
        );
    }

    epoch.condemn();
    let start = Instant::now();
    while !epoch.acked() {
        std::thread::yield_now();
    }

    let condemn_ack = start.elapsed().as_nanos() as f64;
    epoch.recheck_and_post();
    let start = Instant::now();
    while !epoch.can_close() {
        std::thread::yield_now();
    }

    let drain = start.elapsed().as_nanos() as f64;
    (
        Waits {
            open_ack,
            condemn_ack,
            drain,
        },
        epoch.close(),
    )
}

/// Median of a sample, in nanoseconds.
fn median(samples: &mut Vec<f64>) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).expect("a duration is never NaN"));
    samples[samples.len() / 2]
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn measure_collector_wait() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("CollectorWait")
        .prop("child", true)
        .build();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    // A population the walk has to stride, every member externally
    // retained so nothing is ever condemned and the epoch's work is the
    // same at every point of the sweep.
    let mut filler: Vec<*mut Object> = Vec::with_capacity(FILLER);
    for i in 0..FILLER {
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe { ll_retain(obj as *mut RcHeader) };
        if i > 0 {
            unsafe { tie(filler[i - 1], 16, obj) };
        }

        filler.push(obj);
    }

    for &work in &WORK {
        let mut open_acks: Vec<f64> = Vec::with_capacity(ROUNDS);
        let mut condemn_acks: Vec<f64> = Vec::with_capacity(ROUNDS);
        let mut drains: Vec<f64> = Vec::with_capacity(ROUNDS);
        let mut reached = 0usize;

        for round in 0..=ROUNDS {
            // A ring nothing outside holds: the epoch has something to
            // condemn, so the second ack and the drain wait run at all.
            let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
            let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
            unsafe {
                tie(a, 16, b);
                tie(b, 16, a);
            }

            // Two quiet epochs mature it: an entity is skipped by the
            // first walk that meets it and enrolled from the second.
            for _ in 0..2 {
                let mut quiet = Epoch::open();
                checkpoint();
                quiet.snapshot();
                quiet.walk();
                quiet.judge();
                let _ = quiet.close();
                checkpoint();
            }

            CHECKPOINTS.store(0, Ordering::Relaxed);
            let collector = std::thread::spawn(timed_epoch);
            // This thread is the mutator: a checkpoint, then `work`
            // iterations of a spin the optimiser may not remove.
            let (waits, stats) = loop {
                checkpoint();
                CHECKPOINTS.fetch_add(1, Ordering::Relaxed);
                let mut sink = 0usize;
                for i in 0..black_box(work) {
                    sink = black_box(sink.wrapping_add(i));
                }

                black_box(sink);
                if collector.is_finished() {
                    break collector
                        .join()
                        .expect("the collector thread must not panic");
                }
            };

            checkpoint(); // flush what the epoch parked
            assert_eq!(stats.confirmed, 1, "the ring must be collected");
            if round > 0 {
                open_acks.push(waits.open_ack);
                condemn_acks.push(waits.condemn_ack);
                drains.push(waits.drain);
                reached += CHECKPOINTS.load(Ordering::Relaxed);
            }
        }

        println!(
            "collector_wait work={work} open_ack={:.1} condemn_ack={:.1} drain={:.1} us \
             checkpoints_per_epoch={}",
            median(&mut open_acks) / 1e3,
            median(&mut condemn_acks) / 1e3,
            median(&mut drains) / 1e3,
            reached / ROUNDS,
        );
    }

    for &obj in &filler {
        unsafe { Object::prop_at(obj, 16).write(Value::null()) };
    }

    for &obj in &filler {
        let entity = obj as *mut RcHeader;
        unsafe { ll_release(entity) };
        unsafe { ll_release(entity) };
    }

    arena.reset(|_| {});
}
