//! Measurement probe, not a correctness test: what an entity that cannot
//! sit on a cycle costs the walk by being enumerated at all.
//!
//! The census enrols every occupied entity slot and gives each a row —
//! a refcount read, an id-map entry, an out-edge trace
//! (`crate::walk::heap_census`, `collect_cycles_inner`). For a string, a
//! weak cell or an FFI box the trace finds nothing: `trace_entity` files
//! them under "the kinds with no counted children" (`src/walk.rs`), and a
//! leaf cannot be a ring member, so the row is pure walk load. The design
//! has an acyclic skip for exactly this and does not take it; whether
//! taking it is worth building is node B1 of
//! `rfc/model/gc/walk/questions.md`, and it needs two numbers: what one
//! such row costs, and how many of them a real heap holds. This probe
//! answers the first. The second is a corpus question and stays open.
//!
//! ## Method
//!
//! One population of chained objects, fixed, so the walk does the edge
//! work a real heap makes it do. Beside it a growing population of
//! strings, which add rows and no edges. The epoch is timed at each
//! string count and the slope over the count is one leaf row.
//!
//! Slope rather than a difference of two arms: a difference of two sizes
//! carries whatever else changed with the size, and four points let the
//! residual say whether the cost is linear at all.
//!
//! ```
//! cargo test --release --lib -- --ignored measure_leaf_row_cost --nocapture
//! ```
//!
//! The figures obey `dev/BENCHMARKS.md`'s Method and are not comparable
//! with any table there: a different binary out of a different profile.

use std::time::Instant;

use super::*;
use crate::memory::barrier::drop_ref;
use crate::refcount::ll_release;
use crate::string::ll_string_new;

/// Objects in the fixed population, chained one to the next so every row
/// carries an edge to trace.
const OBJECTS: usize = 100_000;

/// Leaf counts measured. Zero anchors the line; the rest span an order
/// so the slope has a lever arm.
const LEAVES: [usize; 4] = [0, 100_000, 200_000, 400_000];

/// Timed epochs per point, after one warm-up whose time is discarded.
const ROUNDS: usize = 5;

/// Least-squares slope in nanoseconds per leaf row, and the largest
/// distance of a point from that line.
///
/// The residual is what says whether the row cost is linear in the
/// population: a walk whose cost bends with the working set reports a
/// slope that describes none of its points.
fn slope_ns_per_leaf(totals_ns: &[f64]) -> (f64, f64) {
    let n = LEAVES.len() as f64;
    let mean_x = LEAVES.iter().sum::<usize>() as f64 / n;
    let mean_y = totals_ns.iter().sum::<f64>() / n;

    let mut covariance = 0.0;
    let mut variance = 0.0;
    for (leaves, total) in LEAVES.iter().zip(totals_ns) {
        let dx = *leaves as f64 - mean_x;
        covariance += dx * (total - mean_y);
        variance += dx * dx;
    }

    let slope = covariance / variance;
    let intercept = mean_y - slope * mean_x;
    let residual = LEAVES
        .iter()
        .zip(totals_ns)
        .map(|(leaves, total)| (total - (intercept + slope * *leaves as f64)).abs())
        .fold(0.0f64, f64::max);

    (slope, residual)
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn measure_leaf_row_cost() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("LeafRowCost").prop("child", true).build();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    let mut medians: Vec<f64> = Vec::with_capacity(LEAVES.len());

    for &leaves in &LEAVES {
        // The objects: every one externally retained, so the probe set
        // stays live and nothing is condemned; chained, so the walk
        // records an edge per row.
        let mut objects: Vec<*mut Object> = Vec::with_capacity(OBJECTS);
        for i in 0..OBJECTS {
            let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
            unsafe { ll_retain(obj as *mut RcHeader) };
            if i > 0 {
                unsafe { tie(objects[i - 1], 16, obj) };
            }

            objects.push(obj);
        }

        // The leaves: distinct bytes per string so interning cannot fold
        // the population into one entity.
        let mut strings: Vec<*mut RcHeader> = Vec::with_capacity(leaves);
        for i in 0..leaves {
            let bytes = format!("leaf-row-{i}");
            let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, bytes.as_bytes()) };
            assert!(!s.is_null(), "the probe's population must allocate");
            strings.push(s as *mut RcHeader);
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
            "leaf_row leaves={leaves} walked={} strings_in_census={} \
             epoch={:.3} ms min={:.3} max={:.3}",
            census.entities,
            census.by_kind[crate::refcount::EntityKind::String as usize],
            median / 1e6,
            samples[0] / 1e6,
            samples[samples.len() - 1] / 1e6,
        );

        // Each string holds exactly its creation reference, so the drop
        // is its death: `drop_ref` runs the teardown `ll_release` only
        // reports. A leaf left alive would enter the next point's census
        // and flatten the slope.
        for s in strings {
            unsafe { drop_ref(MemoryCategory::GcHeap, s) };
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

    let (slope, residual) = slope_ns_per_leaf(&medians);
    println!(
        "leaf_row slope={slope:.3} ns/leaf residual={:.3} ms objects={OBJECTS}",
        residual / 1e6
    );
    arena.reset(|_| {});
}
