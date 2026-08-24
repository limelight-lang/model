//! Measurement probe, not a correctness test: what an empty array costs
//! the walk, against a string, which is the cheapest row there is.
//!
//! The two kinds part company at one place in the tracing stride. A string
//! is filed under the kinds with no counted children, so its row is a
//! header read, an id-map entry and a count store (`crate::walk`). An
//! array reaches its cells through a second dereference: the storage head
//! is read under a version and given up when the two readings disagree
//! (`crate::array::head::StorageHead::coherent`), and only then is a
//! stride chosen from the tag. Empty, it strides nothing, so what the two
//! rows differ by is that coherent read and the dispatch around it — the
//! quantity node B4 of `rfc/model/gc/walk/questions.md` asks about.
//!
//! ## Method
//!
//! One population of chained objects, fixed, so the walk does the edge
//! work a real heap makes it do. Beside it a growing population of one
//! kind. The epoch is timed at each count and the slope over the count is
//! one row of that kind. Both arms take the same object population, the
//! same counts and the same rounds inside one binary, so the difference
//! of their slopes carries no drift between runs.
//!
//! A third arm fills each array with [`ELEMENTS`] vector entries naming
//! one shared entity, so it adds cells to trace and no rows to enrol, and
//! its excess over the empty arm divided by the element count is what one
//! cell of array storage costs.
//!
//! **What the arms do not share is the entity.** A string of these
//! bytes is one inline allocation; an empty array is a header and a
//! storage head, and its census row therefore touches more bytes. The
//! difference of the slopes is the array row's whole excess over a leaf
//! row, and the coherent read is a part of it rather than all of it.
//!
//! ```
//! cargo test --release --lib -- --ignored measure_array_row_cost --nocapture
//! ```
//!
//! The figures obey `dev/BENCHMARKS.md`'s Method and are not comparable
//! with any table there: a different binary out of a different profile.

use std::time::Instant;

use super::*;
use crate::Class;
use crate::array::entity::ll_array_new;
use crate::memory::barrier::drop_ref;
use crate::refcount::ll_release;
use crate::string::ll_string_new;

/// Objects in the fixed population, chained one to the next so every row
/// carries an edge to trace.
const OBJECTS: usize = 100_000;

/// Row counts measured. Zero anchors the line; the rest span an order so
/// the slope has a lever arm. The same counts as the leaf-row probe, so
/// the two files' figures sit on one scale.
const ROWS: [usize; 4] = [0, 100_000, 200_000, 400_000];

/// Timed epochs per point, after one warm-up whose time is discarded.
const ROUNDS: usize = 5;

/// Byte distance between two boxed properties, and the offset of the
/// first: a `Value` is one slot wide and the header sits below them.
const PROP_STRIDE: u32 = 16;

/// Elements in a populated array. Small, because the question is the
/// per-cell cost and a long array would measure the block allocator too.
const ELEMENTS: usize = 8;

/// Which kind the growing population is made of.
#[derive(Clone, Copy)]
enum Row {
    /// A string of distinct bytes: a leaf, and the cheapest row.
    Leaf,
    /// An array with no elements: a leaf that is reached through the
    /// storage head.
    EmptyArray,
    /// An array of [`ELEMENTS`] vector entries, every one naming the same
    /// entity, so the arm adds cells to trace and no rows to enrol.
    FilledArray,
    /// An object of [`ELEMENTS`] boxed properties, all unoccupied: the
    /// control for the arm below, and the object-side twin of the empty
    /// array.
    EmptyObject,
    /// The same object with every property naming the shared entity.
    FilledObject,
}

impl Row {
    fn label(self) -> &'static str {
        match self {
            Row::Leaf => "leaf",
            Row::EmptyArray => "empty_array",
            Row::FilledArray => "filled_array",
            Row::EmptyObject => "empty_object",
            Row::FilledObject => "filled_object",
        }
    }
}

/// Least-squares slope in nanoseconds per row, and the largest distance
/// of a point from that line.
///
/// The residual is what says whether the row cost is linear in the
/// population: a walk whose cost bends with the working set reports a
/// slope that describes none of its points.
fn slope_ns_per_row(totals_ns: &[f64]) -> (f64, f64) {
    let n = ROWS.len() as f64;
    let mean_x = ROWS.iter().sum::<usize>() as f64 / n;
    let mean_y = totals_ns.iter().sum::<f64>() / n;

    let mut covariance = 0.0;
    let mut variance = 0.0;
    for (rows, total) in ROWS.iter().zip(totals_ns) {
        let dx = *rows as f64 - mean_x;
        covariance += dx * (total - mean_y);
        variance += dx * dx;
    }

    let slope = covariance / variance;
    let intercept = mean_y - slope * mean_x;
    let residual = ROWS
        .iter()
        .zip(totals_ns)
        .map(|(rows, total)| (total - (intercept + slope * *rows as f64)).abs())
        .fold(0.0f64, f64::max);

    (slope, residual)
}

/// One arm: the median epoch time at each count in [`ROWS`], with the
/// population built of `row` and torn down before the next point.
///
/// Leaving a row alive would enter the next point's census and flatten
/// the slope, so every entity built here is dropped here.
fn medians_for(
    row: Row,
    ctx: &mut LLContext,
    cls: *const Class,
    wide: *const Class,
    element: *mut RcHeader,
) -> Vec<f64> {
    let mut medians: Vec<f64> = Vec::with_capacity(ROWS.len());

    for &rows in &ROWS {
        // The objects: every one externally retained, so the probe set
        // stays live and nothing is condemned; chained, so the walk
        // records an edge per row.
        let mut objects: Vec<*mut Object> = Vec::with_capacity(OBJECTS);
        for i in 0..OBJECTS {
            let obj = unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
            unsafe { ll_retain(obj as *mut RcHeader) };
            if i > 0 {
                unsafe { tie(objects[i - 1], 16, obj) };
            }

            objects.push(obj);
        }

        let mut population: Vec<*mut RcHeader> = Vec::with_capacity(rows);
        for i in 0..rows {
            let entity = match row {
                // Distinct bytes per string so interning cannot fold the
                // population into one entity.
                Row::Leaf => {
                    let bytes = format!("array-row-{i}");
                    let s = unsafe { ll_string_new(ctx, MemoryCategory::GcHeap, bytes.as_bytes()) };
                    s as *mut RcHeader
                }
                Row::EmptyArray => {
                    let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
                    a as *mut RcHeader
                }
                Row::FilledArray => {
                    let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
                    for _ in 0..ELEMENTS {
                        // Retained before the entry is published, per
                        // `Vector::push`; the array's teardown releases
                        // it back.
                        unsafe { ll_retain(element) };
                        let pushed = unsafe {
                            crate::array::testing::push(a, Value::entity(Tag::Object, element))
                        };
                        assert!(pushed, "the probe's array must take its element");
                    }

                    a as *mut RcHeader
                }
                Row::EmptyObject => {
                    let o = unsafe { new_constructed(ctx, wide, MemoryCategory::GcHeap) };
                    o as *mut RcHeader
                }
                Row::FilledObject => {
                    let o = unsafe { new_constructed(ctx, wide, MemoryCategory::GcHeap) };
                    for slot in 0..ELEMENTS {
                        unsafe { ll_retain(element) };
                        let at = PROP_STRIDE + slot as u32 * PROP_STRIDE;
                        unsafe {
                            Object::prop_at(o, at).write(Value::entity(Tag::Object, element));
                        }
                    }

                    o as *mut RcHeader
                }
            };
            assert!(!entity.is_null(), "the probe's population must allocate");
            population.push(entity);
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
            "array_row kind={} rows={rows} walked={} epoch={:.3} ms min={:.3} max={:.3}",
            row.label(),
            census.entities,
            median / 1e6,
            samples[0] / 1e6,
            samples[samples.len() - 1] / 1e6,
        );

        // Each entity holds exactly its creation reference, so the drop
        // is its death: `drop_ref` runs the teardown `ll_release` only
        // reports.
        for entity in population {
            unsafe { drop_ref(MemoryCategory::GcHeap, entity) };
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

    medians
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn measure_array_row_cost() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("ArrayRowCost")
        .prop("child", true)
        .build();
    // The object arms' class: as many boxed properties as a filled array
    // has entries, so the two containers hold the same number of cells.
    let mut builder = ClassBuilder::new("ArrayRowCostWide");
    for slot in 0..ELEMENTS {
        builder = builder.prop(&format!("child{slot}"), true);
    }

    let wide = builder.build();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    // One entity named by every cell of every filled array: the arm adds
    // edges to trace without adding rows to enrol, which is what makes
    // its slope a per-cell figure rather than a population figure.
    let element =
        unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) } as *mut RcHeader;
    unsafe { ll_retain(element) };

    let leaf = medians_for(Row::Leaf, &mut ctx, cls, wide, element);
    let empty = medians_for(Row::EmptyArray, &mut ctx, cls, wide, element);
    let filled = medians_for(Row::FilledArray, &mut ctx, cls, wide, element);
    let empty_object = medians_for(Row::EmptyObject, &mut ctx, cls, wide, element);
    let filled_object = medians_for(Row::FilledObject, &mut ctx, cls, wide, element);

    let (leaf_slope, _) = slope_ns_per_row(&leaf);
    let (empty_slope, _) = slope_ns_per_row(&empty);
    let (filled_slope, _) = slope_ns_per_row(&filled);
    let (empty_object_slope, _) = slope_ns_per_row(&empty_object);
    let (filled_object_slope, _) = slope_ns_per_row(&filled_object);
    println!(
        "array_row leaf={leaf_slope:.1} empty_array={empty_slope:.1} \
         filled_array={filled_slope:.1} empty_object={empty_object_slope:.1} \
         filled_object={filled_object_slope:.1} ns/row \
         head={:.1} array_cell={:.1} object_cell={:.1} ns objects={OBJECTS}",
        empty_slope - leaf_slope,
        (filled_slope - empty_slope) / ELEMENTS as f64,
        (filled_object_slope - empty_object_slope) / ELEMENTS as f64,
    );
    arena.reset(|_| {});
}
