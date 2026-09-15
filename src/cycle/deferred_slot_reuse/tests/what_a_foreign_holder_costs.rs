//! What the foreign holder's window costs the owner, per return: a free
//! that withholds against a free that returns, a chunk free the same way,
//! and the pop that makes the withheld returns afterwards.
//!
//! The probe prices the mechanism and not a workload — the churn a trace
//! holds is this figure's reciprocal times the trace's length, and the
//! length is the corpus's (`dev/BENCHMARKS.md`, "S38.3 what a foreign
//! holder costs the owner"). Every arm frees `SLOTS` distinct slots, so
//! the two free arms stride the same memory and differ in the gate alone;
//! the arms interleave A, B, A, B over `ROUNDS`, and the minimum stands
//! beside the median (`dev/BENCHMARKS.md`, Method).

use super::*;
use crate::cycle::token::testing::HeldByACollector;
use crate::cycle::token::this_thread_token;
use crate::memory::buffer_arena::{buffer_alloc_longlived_payload, buffer_free_longlived_payload};
use std::hint::black_box;
use std::time::Instant;

const SLOTS: usize = 20_000;
const CHUNK_BYTES: usize = 256;
const ROUNDS: usize = 9;

struct Stats {
    minimum: f64,
    median: f64,
}

fn stats(samples: &mut [f64]) -> Stats {
    samples.sort_by(|a, b| a.partial_cmp(b).expect("no NaN"));
    Stats {
        minimum: samples[0],
        median: samples[samples.len() / 2],
    }
}

/// `SLOTS` dead entities, allocated and stamped, in allocation order.
unsafe fn dead_slots() -> Vec<*mut u8> {
    (0..SLOTS)
        .map(|_| {
            let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
            assert!(!slot.is_null(), "the heap served");
            unsafe { dead_entity(slot) };
            slot
        })
        .collect()
}

/// Nanoseconds per free over `slots`, freed in order.
unsafe fn ns_per_free(slots: &[*mut u8]) -> f64 {
    let start = Instant::now();
    for &slot in black_box(slots) {
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    start.elapsed().as_nanos() as f64 / black_box(slots.len()) as f64
}

/// Nanoseconds per chunk free over `chunks`.
unsafe fn ns_per_chunk_free(chunks: &[(*mut u8, usize)]) -> f64 {
    let start = Instant::now();
    for &(chunk, granted) in black_box(chunks) {
        unsafe { buffer_free_longlived_payload(chunk, granted) };
    }

    start.elapsed().as_nanos() as f64 / black_box(chunks.len()) as f64
}

fn chunks() -> Vec<(*mut u8, usize)> {
    (0..SLOTS)
        .map(|_| {
            let (chunk, granted) = buffer_alloc_longlived_payload(CHUNK_BYTES);
            assert!(!chunk.is_null(), "the buffer arena served");
            (chunk, granted)
        })
        .collect()
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn measure_what_a_foreign_holder_costs() {
    let _guard = test_guard();
    let mut returned = Vec::new();
    let mut withheld = Vec::new();
    let mut popped = Vec::new();
    let mut chunk_returned = Vec::new();
    let mut chunk_withheld = Vec::new();
    let mut chunk_popped = Vec::new();

    // A warm-up round of every arm, dropped: the first measurement a process
    // takes is systematically slow (`dev/BENCHMARKS.md`, Method).
    let mut allocated = Vec::new();
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let slots = unsafe { dead_slots() };
        let alloc = start.elapsed().as_nanos() as f64 / SLOTS as f64;
        let a = unsafe { ns_per_free(&slots) };

        let slots = unsafe { dead_slots() };
        let mut holder = HeldByACollector::take(this_thread_token(), false);
        let b = unsafe { ns_per_free(&slots) };
        assert_eq!(foreign_withheld_count(), SLOTS);
        holder.release();
        let start = Instant::now();
        unsafe { make_returns_withheld_under_a_foreign_trace() };
        let c = start.elapsed().as_nanos() as f64 / SLOTS as f64;
        assert_eq!(foreign_withheld_count(), 0);

        let batch = chunks();
        let d = unsafe { ns_per_chunk_free(&batch) };

        let batch = chunks();
        let mut holder = HeldByACollector::take(this_thread_token(), false);
        let e = unsafe { ns_per_chunk_free(&batch) };
        assert_eq!(foreign_withheld_chunks(), SLOTS);
        holder.release();
        let start = Instant::now();
        unsafe { make_returns_withheld_under_a_foreign_trace() };
        let f = start.elapsed().as_nanos() as f64 / SLOTS as f64;
        assert_eq!(foreign_withheld_chunks(), 0);

        if round == 0 {
            continue;
        }

        allocated.push(alloc);
        returned.push(a);
        withheld.push(b);
        popped.push(c);
        chunk_returned.push(d);
        chunk_withheld.push(e);
        chunk_popped.push(f);
    }

    for (label, samples) in [
        ("slot_alloc_and_stamp", &mut allocated),
        ("slot_free_returned", &mut returned),
        ("slot_free_withheld", &mut withheld),
        ("slot_pop", &mut popped),
        ("chunk_free_returned", &mut chunk_returned),
        ("chunk_free_withheld", &mut chunk_withheld),
        ("chunk_pop", &mut chunk_popped),
    ] {
        let stats = stats(samples);
        println!(
            "foreign_holder_cost {label}: median={:.2} ns min={:.2} ns over {SLOTS} per round, {ROUNDS} rounds",
            stats.median, stats.minimum
        );
    }
}
