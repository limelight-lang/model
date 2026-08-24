//! Measurement probe, not a correctness test: what an entity the walk
//! enrols nothing for still costs it.
//!
//! Node B7 of `rfc/model/gc/walk/questions.md` proposes telling the
//! collector which blocks it need not walk, and says the whole of what
//! that adds over node B1's per-entity skip is the residue a skipped
//! entity still pays. A draft of the node took that residue from B1's
//! sentence about what a leaf pays when it is **enrolled**, which is the
//! opposite quantity. This probe measures the residue itself.
//!
//! ## What the residue is, read off `walk_rows`
//!
//! Per slot, before anything is enrolled: the slot's address, one relaxed
//! 64-bit header load, and three tests over the word it returns —
//! occupancy, the epoch byte, the memory category. An entity that fails
//! the third pays exactly that and nothing else, because the census store
//! and the four row pushes all sit below it.
//!
//! ## The population that isolates it
//!
//! A `LongLived` entity allocates from the same entity blocks a `GcHeap`
//! one does and the walk skips it by category (`src/memory/routing.rs`),
//! so it reaches the third test and stops there. A kind skip, which the
//! crate does not have, would stop one register test later; the figure
//! here is that skip's residue minus one predicted compare.
//!
//! ```
//! cargo test --release --lib -- --ignored measure_skipped_entity_cost --nocapture
//! ```
//!
//! The figures obey `dev/BENCHMARKS.md`'s Method and are not comparable
//! with any table there: a different binary out of a different profile.

use std::time::Instant;

use super::*;
use crate::memory::barrier::drop_ref;
use crate::refcount::ll_release;
use crate::string::ll_string_new;

/// Objects in the fixed population, chained one to the next, so the walk
/// does the edge work a real heap makes it do. The same count the
/// leaf-row probe uses, so the two slopes sit on one scale.
const OBJECTS: usize = 100_000;

/// Skipped-entity counts measured. The same counts as the leaf-row probe
/// for the same reason.
const SKIPPED: [usize; 4] = [0, 100_000, 200_000, 400_000];

/// Timed epochs per point, after one warm-up whose time is discarded.
/// The warm-up also does the stamping: an entity is stamped by the first
/// walk that meets it and skipped at the epoch byte, so only the second
/// walk onward reaches the category test this probe is about.
const ROUNDS: usize = 5;

/// Least-squares slope in nanoseconds per skipped entity, and the largest
/// distance of a point from that line.
fn slope_ns_per_skip(totals_ns: &[f64]) -> (f64, f64) {
    let n = SKIPPED.len() as f64;
    let mean_x = SKIPPED.iter().sum::<usize>() as f64 / n;
    let mean_y = totals_ns.iter().sum::<f64>() / n;

    let mut covariance = 0.0;
    let mut variance = 0.0;
    for (skipped, total) in SKIPPED.iter().zip(totals_ns) {
        let dx = *skipped as f64 - mean_x;
        covariance += dx * (total - mean_y);
        variance += dx * dx;
    }

    let slope = covariance / variance;
    let intercept = mean_y - slope * mean_x;
    let residual = SKIPPED
        .iter()
        .zip(totals_ns)
        .map(|(skipped, total)| (total - (intercept + slope * *skipped as f64)).abs())
        .fold(0.0f64, f64::max);

    (slope, residual)
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn measure_skipped_entity_cost() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("SkippedResidue")
        .prop("child", true)
        .build();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    let mut medians: Vec<f64> = Vec::with_capacity(SKIPPED.len());

    for &skipped in &SKIPPED {
        let mut objects: Vec<*mut Object> = Vec::with_capacity(OBJECTS);
        for i in 0..OBJECTS {
            let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
            unsafe { ll_retain(obj as *mut RcHeader) };
            if i > 0 {
                unsafe { tie(objects[i - 1], 16, obj) };
            }

            objects.push(obj);
        }

        // Six distinct inline bytes: distinct so interning cannot fold the
        // population into one entity, inline so no payload is allocated
        // beside the entity and the slot is the whole of it.
        let mut skips: Vec<*mut RcHeader> = Vec::with_capacity(skipped);
        for i in 0..skipped {
            let bytes = format!("{i:06}");
            let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::LongLived, bytes.as_bytes()) };
            assert!(!s.is_null(), "the probe's population must allocate");
            skips.push(s as *mut RcHeader);
        }

        let census = unsafe { crate::walk::heap_census() };
        let mut samples: Vec<f64> = Vec::with_capacity(ROUNDS);
        for round in 0..=ROUNDS {
            let start = Instant::now();
            let mut epoch = Epoch::open();
            checkpoint();
            epoch.snapshot();
            epoch.walk();
            epoch.judge();
            assert_eq!(epoch.stats.candidates, 0, "the probe set must stay live");
            let _ = epoch.close();
            checkpoint();
            if round > 0 {
                samples.push(start.elapsed().as_nanos() as f64);
            }
        }

        samples.sort_by(|a, b| a.partial_cmp(b).expect("a duration is never NaN"));
        let median = samples[samples.len() / 2];
        medians.push(median);
        println!(
            "skipped_residue skipped={skipped} walked={} epoch={:.3} ms min={:.3} max={:.3}",
            census.entities,
            median / 1e6,
            samples[0] / 1e6,
            samples[samples.len() - 1] / 1e6,
        );

        // `string_die` frees only a `GcHeap` string, so the slot of a
        // long-lived one comes back here rather than through the ordinary
        // death path. Outside the measurement, and the only place this
        // probe does something the runtime does not.
        for s in skips {
            unsafe { drop_ref(MemoryCategory::LongLived, s) };
            unsafe { crate::memory::stdapi::ll_free(s as *mut u8) };
        }

        for &obj in &objects {
            unsafe { Object::prop_at(obj, 16).write(Value::null()) };
        }

        for &obj in &objects {
            let entity = obj as *mut RcHeader;
            unsafe { ll_release(entity) };
            unsafe { ll_release(entity) };
        }
    }

    let (slope, residual) = slope_ns_per_skip(&medians);
    println!(
        "skipped_residue slope={slope:.3} ns/skip residual={:.3} ms objects={OBJECTS}",
        residual / 1e6
    );
    arena.reset(|_| {});
}
