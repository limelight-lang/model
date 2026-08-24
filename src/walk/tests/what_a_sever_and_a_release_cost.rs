//! Measurement probe, not a correctness test: what one severed cell and
//! one released child cost, taken directly rather than borrowed.
//!
//! Node D3 of `rfc/model/gc/walk/questions.md` prices the drain's batch
//! ceiling in cells, and until now it spent node B4's 43-47 ns on the
//! sever and on the release. B4 measured neither: it measured the walk
//! *reading* a cell, and the borrowing is recorded in that node as an
//! error already corrected once. This probe takes the two prices the
//! ceiling actually needs.
//!
//! ## What is measured, and against what
//!
//! **The sever, decomposed into three arms over the same walker.** Arm A
//! strides an object's body with `for_each_body_cell::<PlainCells>` and
//! only reads the child; arm B strides and records it; arm C is
//! `sever_cells`, which strides, empties and records. B − A is the
//! record, C − B is the empty, and C − A is the pair the second Sage
//! verdict made splittable at cell granularity. All three stride the same
//! class through the same walker — but A and B receive the class pointer
//! hoisted out of the loop while C loads it per entity off the object
//! header and then tests `outside_cells`, so C − B carries a per-parent
//! dispatch term amortised over eight cells beside the store it isolates.
//!
//! **The release, in two steps.** Arm R0 reads each child's flags word,
//! which is the scattered memory traffic a drop pays before it decides
//! anything; arm R1 drops a child holding one spare reference, so the
//! count falls and nothing dies; arm R2 drops a child holding only the
//! creation reference, so the teardown runs — the release children are
//! reachable from a vector of the probe's own and sit in no cell and no
//! entry. R1 − R0 is the counting alone and R2 − R1 is the teardown of an
//! empty leaf **on top of** the counting, which is a floor: a class with a
//! destructor or with children of its own pays more, and a child that dies
//! costs R1 − R0 plus R2 − R1 rather than the second alone.
//!
//! **The null pair is arm A against a second, identically built
//! population.** Its difference is zero by construction, so what it reads
//! bounds the two read-only object arms. It bounds nothing else: the array
//! arms and the three release arms have no identically built twin, and no
//! error bar is available for their differences.
//!
//! ## Why every round gets its own population
//!
//! A sever is destructive and one-shot: the second pass over a severed
//! object finds empty cells and yields nothing. So each arm consumes a
//! fresh population per round, and every population is built before the
//! first timed round rather than between them.
//!
//! ```
//! cargo test --release --lib -- --ignored measure_sever_and_release --nocapture
//! ```
//!
//! The figures obey `dev/BENCHMARKS.md`'s Method and are not comparable
//! with any table there: a different binary out of a different profile.

use std::hint::black_box;
use std::time::Instant;

use super::*;
use crate::class::Class;
use crate::memory::barrier::drop_ref;
use crate::object::for_each_body_cell;
use crate::refcount::{header_flags, ll_retain};

/// Counted cells per parent. Eight boxed properties, the first at offset
/// 16 and each `Value` sixteen bytes wide.
const CELLS: usize = 8;

/// Parents per timed round: 2 000 × 8 = 16 000 cells, which at a few
/// nanoseconds a cell is tens of microseconds — well clear of the clock's
/// granularity and short enough that 16 rounds of nine arms fit in memory
/// at once.
const PARENTS: usize = 2_000;

/// Children per release round, matched to the sever round's cell count
/// so the two halves are read at the same scale.
const CHILDREN: usize = PARENTS * CELLS;

/// Timed rounds per arm. Round 0 is a warm-up and its time is discarded,
/// exactly as the barrier probe does it.
const ROUNDS: usize = 15;

/// Elements per array in the two array arms. Longer than the object's
/// eight, because D3's example is one array of a million cells and a
/// short array would amortise the storage head over too few of them.
const ELEMENTS: usize = 128;

/// Arrays per timed round, chosen so the array arms stride the same
/// 16 000 cells the object arms do.
const ARRAYS: usize = PARENTS * CELLS / ELEMENTS;

/// The ten arms, in the order they are built and run.
const ARMS: usize = 10;

/// A multiplier coprime with the population sizes here, used to visit the
/// **release** populations in an order the prefetcher cannot follow. The
/// drain reaches its children through a component rather than through a
/// vector, so a sequential sweep would measure a shape it never has. The
/// sever and array arms are not scattered — they sweep their parents in
/// allocation order, which makes their figure a floor.
const SCATTER: usize = 2_654_435_761;

/// One round's worth of parents, each holding [`CELLS`] children.
struct Bodies {
    parents: Vec<*mut Object>,
    class: *const Class,
}

/// One round's worth of leaf children, reachable only from this vector.
struct Leaves {
    children: Vec<*mut RcHeader>,
}

/// One round's worth of arrays, each holding [`ELEMENTS`] children.
struct Arrays {
    arrays: Vec<*mut RcHeader>,
}

/// Build `PARENTS` parents, each with [`CELLS`] children tied into its
/// body. `tie` transfers the creation reference into the slot, so every
/// child ends at count one and the cell owns it.
unsafe fn build_bodies(
    ctx: &mut LLContext,
    parent_cls: *const Class,
    leaf_cls: *const Class,
) -> Bodies {
    let mut parents = Vec::with_capacity(PARENTS);
    for _ in 0..PARENTS {
        let parent = unsafe { new_constructed(ctx, parent_cls, MemoryCategory::GcHeap) };
        for c in 0..CELLS {
            let child = unsafe { new_constructed(ctx, leaf_cls, MemoryCategory::GcHeap) };
            unsafe { tie(parent, 16 + (c as u32) * 16, child) };
        }

        parents.push(parent);
    }

    Bodies {
        parents,
        class: parent_cls,
    }
}

/// Build `CHILDREN` leaf objects in a scattered order. `spare` adds one
/// reference beyond the creation one, which is what decides whether the
/// arm's drop kills the child or only counts it down.
unsafe fn build_leaves(ctx: &mut LLContext, leaf_cls: *const Class, spare: bool) -> Leaves {
    let mut children = Vec::with_capacity(CHILDREN);
    for _ in 0..CHILDREN {
        let child = unsafe { new_constructed(ctx, leaf_cls, MemoryCategory::GcHeap) };
        if spare {
            unsafe { ll_retain(child as *mut RcHeader) };
        }

        children.push(child as *mut RcHeader);
    }

    // One fixed permutation, the same for every arm, so the arms differ
    // in what they do to a child and never in the order they reach it.
    let n = children.len();
    let mut scattered = Vec::with_capacity(n);
    for i in 0..n {
        scattered.push(children[SCATTER.wrapping_mul(i) % n]);
    }

    Leaves {
        children: scattered,
    }
}

/// Build `ARRAYS` vector-backed arrays, each with [`ELEMENTS`] children
/// pushed into it. `push` takes the reference `ll_retain` makes for it,
/// and the creation reference is dropped, so every child ends at count
/// one owned by its entry.
unsafe fn build_arrays(ctx: &mut LLContext, leaf_cls: *const Class) -> Arrays {
    let mut arrays = Vec::with_capacity(ARRAYS);
    for _ in 0..ARRAYS {
        let a = unsafe { crate::array::entity::ll_array_new(MemoryCategory::GcHeap) };
        for _ in 0..ELEMENTS {
            let child = unsafe { new_constructed(ctx, leaf_cls, MemoryCategory::GcHeap) };
            let pushed = unsafe {
                crate::array::testing::push(a, Value::entity(Tag::Object, child as *mut RcHeader))
            };
            assert!(pushed, "the probe's array must take its element");
            // `push` retains for the entry it publishes, so the creation
            // reference is this loop's to give back.
            unsafe { ll_release(child as *mut RcHeader) };
        }

        arrays.push(a as *mut RcHeader);
    }

    Arrays { arrays }
}

/// Arm AA: stride an array's entries and read the child, the read-only
/// twin of the array sever and its control.
unsafe fn stride_arrays(a: &Arrays) {
    const ARRAY: u32 = crate::refcount::EntityKind::Array as u32;
    for i in 0..black_box(a.arrays.len()) {
        unsafe {
            trace_cells::<PlainCells>(a.arrays[i], ARRAY, |cell| {
                black_box(cell.child);
            })
        };
    }
}

/// Arm CA: the production sever over an array. `ll_array_new` builds a
/// mixed vector and `testing::push` fills one, so this reaches
/// `Vector::sever_entries`; a table-backed array is a different path and
/// this probe does not measure it.
unsafe fn sever_arrays(a: &Arrays, displaced: &mut Vec<*mut RcHeader>) {
    const ARRAY: u32 = crate::refcount::EntityKind::Array as u32;
    for i in 0..black_box(a.arrays.len()) {
        unsafe { sever_cells(a.arrays[i], ARRAY, displaced) };
    }
}

/// Arm A: stride the body and read the child, emptying and recording
/// nothing. The control for both other sever arms.
unsafe fn stride_only(b: &Bodies) {
    for i in 0..black_box(b.parents.len()) {
        let parent = b.parents[i];
        unsafe {
            for_each_body_cell::<PlainCells>(parent as *mut u8, b.class, &mut |cell| {
                black_box(cell.child);
            })
        };
    }
}

/// Arm B: stride and record, emptying nothing. B − A is the push.
unsafe fn stride_and_record(b: &Bodies, displaced: &mut Vec<*mut RcHeader>) {
    for i in 0..black_box(b.parents.len()) {
        let parent = b.parents[i];
        unsafe {
            for_each_body_cell::<PlainCells>(parent as *mut u8, b.class, &mut |cell| {
                displaced.push(cell.child);
            })
        };
    }
}

/// Arm C: the production sever. C − B is the store that empties the cell.
unsafe fn sever(b: &Bodies, displaced: &mut Vec<*mut RcHeader>) {
    const OBJECT: u32 = crate::refcount::EntityKind::Object as u32;
    for i in 0..black_box(b.parents.len()) {
        unsafe { sever_cells(b.parents[i] as *mut RcHeader, OBJECT, displaced) };
    }
}

/// Arm R0: the scattered header read a drop pays before it decides
/// anything, and the control for both release arms.
unsafe fn read_headers(l: &Leaves) {
    for i in 0..black_box(l.children.len()) {
        black_box(unsafe { header_flags(l.children[i] as *const RcHeader) });
    }
}

/// Arms R1 and R2: the production drop. Which of the two it is depends
/// on whether the population was built with a spare reference.
unsafe fn drop_children(l: &Leaves) {
    for i in 0..black_box(l.children.len()) {
        unsafe { drop_ref(MemoryCategory::GcHeap, l.children[i]) };
    }
}
/// Arm CLOCK: one monotonic clock read per item, over the same count the
/// sever arms use. Not part of the drain — this crate reads no clock in
/// production — but the number that decides whether ruling 3's time
/// ceiling can be checked per cell or has to be charged against a budget.
fn read_clock(n: usize) {
    for _ in 0..black_box(n) {
        black_box(Instant::now());
    }
}

/// Median, minimum and maximum of a sample, in nanoseconds.
fn reduce(samples: &mut [f64]) -> (f64, f64, f64) {
    samples.sort_by(|a, b| a.partial_cmp(b).expect("a duration is never NaN"));
    (
        samples[samples.len() / 2],
        samples[0],
        samples[samples.len() - 1],
    )
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn measure_sever_and_release() {
    let _g = crate::memory::block_pool::test_guard();

    let mut builder = ClassBuilder::new("SeverBody");
    for c in 0..CELLS {
        builder = builder.prop(&format!("c{c}"), true);
    }

    let parent_cls = builder.build();
    let leaf_cls = ClassBuilder::new("SeverLeaf").build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    // Everything is built before the first timed round: a sever consumes
    // its population, and building between rounds would put the
    // allocator inside the measurement.
    let rounds = ROUNDS + 1;
    let mut a_arms: Vec<Bodies> = Vec::with_capacity(rounds);
    let mut a2_arms: Vec<Bodies> = Vec::with_capacity(rounds);
    let mut b_arms: Vec<Bodies> = Vec::with_capacity(rounds);
    let mut c_arms: Vec<Bodies> = Vec::with_capacity(rounds);
    let mut r0_arms: Vec<Leaves> = Vec::with_capacity(rounds);
    let mut r1_arms: Vec<Leaves> = Vec::with_capacity(rounds);
    let mut r2_arms: Vec<Leaves> = Vec::with_capacity(rounds);
    let mut aa_arms: Vec<Arrays> = Vec::with_capacity(rounds);
    let mut ca_arms: Vec<Arrays> = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        a_arms.push(unsafe { build_bodies(&mut ctx, parent_cls, leaf_cls) });
        a2_arms.push(unsafe { build_bodies(&mut ctx, parent_cls, leaf_cls) });
        b_arms.push(unsafe { build_bodies(&mut ctx, parent_cls, leaf_cls) });
        c_arms.push(unsafe { build_bodies(&mut ctx, parent_cls, leaf_cls) });
        r0_arms.push(unsafe { build_leaves(&mut ctx, leaf_cls, false) });
        r1_arms.push(unsafe { build_leaves(&mut ctx, leaf_cls, true) });
        r2_arms.push(unsafe { build_leaves(&mut ctx, leaf_cls, false) });
        aa_arms.push(unsafe { build_arrays(&mut ctx, leaf_cls) });
        ca_arms.push(unsafe { build_arrays(&mut ctx, leaf_cls) });
    }

    let cells = (PARENTS * CELLS) as f64;
    let children = CHILDREN as f64;

    let mut samples: [Vec<f64>; ARMS] = std::array::from_fn(|_| Vec::with_capacity(ROUNDS));
    // The record arms need somewhere to push, one vector per round so
    // arm C's severed references survive the measurement and can be
    // given back. Capacity is reserved outside the timer in both, so the
    // difference between them is the push and never the regrowth:
    // regrowth is a per-component term the ceiling carries separately,
    // not a per-cell one.
    let mut b_displaced: Vec<Vec<*mut RcHeader>> = (0..rounds)
        .map(|_| Vec::with_capacity(PARENTS * CELLS))
        .collect();
    let mut c_displaced: Vec<Vec<*mut RcHeader>> = (0..rounds)
        .map(|_| Vec::with_capacity(PARENTS * CELLS))
        .collect();
    let mut ca_displaced: Vec<Vec<*mut RcHeader>> = (0..rounds)
        .map(|_| Vec::with_capacity(ARRAYS * ELEMENTS))
        .collect();

    for round in 0..rounds {
        // Rotate which arm goes first, so a systematic drift over a round
        // does not land on one arm.
        for step in 0..ARMS {
            let arm = (step + round) % ARMS;
            let start = Instant::now();
            match arm {
                0 => unsafe { stride_only(&a_arms[round]) },
                1 => unsafe { stride_only(&a2_arms[round]) },
                2 => unsafe { stride_and_record(&b_arms[round], &mut b_displaced[round]) },
                3 => unsafe { sever(&c_arms[round], &mut c_displaced[round]) },
                4 => unsafe { read_headers(&r0_arms[round]) },
                5 => unsafe { drop_children(&r1_arms[round]) },
                6 => unsafe { drop_children(&r2_arms[round]) },
                7 => unsafe { stride_arrays(&aa_arms[round]) },
                8 => unsafe { sever_arrays(&ca_arms[round], &mut ca_displaced[round]) },
                _ => read_clock(PARENTS * CELLS),
            }

            let elapsed = start.elapsed().as_nanos() as f64;
            if round > 0 {
                samples[arm].push(elapsed);
            }
        }
    }

    let names = [
        "A_stride",
        "A2_stride",
        "B_record",
        "C_sever",
        "R0_read",
        "R1_count",
        "R2_die",
        "AA_stride_array",
        "CA_sever_array",
        "CLOCK_read",
    ];
    let entries = (ARRAYS * ELEMENTS) as f64;
    let per = [
        cells, cells, cells, cells, children, children, children, entries, entries, cells,
    ];
    let mut median = [0.0f64; ARMS];
    for arm in 0..ARMS {
        let (m, lo, hi) = reduce(&mut samples[arm]);
        median[arm] = m / per[arm];
        println!(
            "sever_release arm={} ns/item={:.3} min={:.3} max={:.3} round_ns={:.0}",
            names[arm],
            median[arm],
            lo / per[arm],
            hi / per[arm],
            m,
        );
    }

    println!(
        "sever_release null_pair={:.3} record={:.3} empty={:.3} pair={:.3} \
         counting={:.3} teardown={:.3} array_pair={:.3} clock={:.3} ns",
        (median[1] - median[0]).abs(),
        median[2] - median[0],
        median[3] - median[2],
        median[3] - median[0],
        median[5] - median[4],
        median[6] - median[5],
        median[8] - median[7],
        median[9],
    );

    // Give it all back. A leaked population fails other tests rather than
    // this one (`dev/POSTMORTEM.md`).
    let mut scratch: Vec<*mut RcHeader> = Vec::with_capacity(PARENTS * CELLS);
    for bodies in a_arms
        .iter()
        .chain(a2_arms.iter())
        .chain(b_arms.iter())
        .chain(c_arms.iter())
    {
        scratch.clear();
        const OBJECT: u32 = crate::refcount::EntityKind::Object as u32;
        for &parent in &bodies.parents {
            // A second sever over arm C's parents finds empty cells and
            // yields nothing, so this is uniform across the four.
            unsafe { sever_cells(parent as *mut RcHeader, OBJECT, &mut scratch) };
        }

        for &child in &scratch {
            unsafe { drop_ref(MemoryCategory::GcHeap, child) };
        }

        for &parent in &bodies.parents {
            unsafe { ll_release(parent as *mut RcHeader) };
        }
    }

    // Arm C emptied its cells, so the loop above found nothing to sever
    // for those parents: the reference each of their cells held is the
    // one this vector recorded, and it is owed here.
    for round_displaced in c_displaced.iter() {
        for &child in round_displaced {
            unsafe { drop_ref(MemoryCategory::GcHeap, child) };
        }
    }

    for leaves in r0_arms.iter().chain(r1_arms.iter()) {
        for &child in &leaves.children {
            unsafe { drop_ref(MemoryCategory::GcHeap, child) };
        }
    }

    // The array arms: AA's entries still hold their children, so a sever
    // yields them; CA's were recorded and are owed here. Then the arrays
    // themselves, each at the count `ll_array_new` returned.
    let mut array_scratch: Vec<*mut RcHeader> = Vec::with_capacity(ARRAYS * ELEMENTS);
    const ARRAY_KIND: u32 = crate::refcount::EntityKind::Array as u32;
    for arrays in aa_arms.iter() {
        array_scratch.clear();
        for &a in &arrays.arrays {
            unsafe { sever_cells(a, ARRAY_KIND, &mut array_scratch) };
        }

        for &child in &array_scratch {
            unsafe { drop_ref(MemoryCategory::GcHeap, child) };
        }
    }

    for round_displaced in ca_displaced.iter() {
        for &child in round_displaced {
            unsafe { drop_ref(MemoryCategory::GcHeap, child) };
        }
    }

    for arrays in aa_arms.iter().chain(ca_arms.iter()) {
        for &a in &arrays.arrays {
            unsafe { ll_release(a) };
        }
    }

    arena.reset(|_| {});
}
