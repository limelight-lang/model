//! Measurement probe, not a correctness test: what the counted pair costs
//! when the two foreign object headers it touches are out of cache.
//!
//! A counted publish reads and writes the header of the value it retains and
//! the header of the value it displaces, both at addresses the store itself
//! never touches. The recorded figures for that pair — 1.84-1.87 ns for the
//! pair, 2.4 ns for a publish (`dev/BENCHMARKS.md`) — are hot figures: they
//! were taken where those two headers are in cache. What a request pays when
//! they are not has never been measured here, and the answer decides whether
//! work on the store path is worth doing at all
//! (`rfc/model/gc/gc-horizon-v2/questions.md`, node N).
//!
//! [`super::what_a_store_costs_by_working_set`] does not answer it and says
//! so: its cold half walks scratch between rounds, but "the next round's
//! `children` writes all `mask + 1` headers before its timer starts, so the
//! child headers are warm in both halves".
//!
//! ## Method
//!
//! One shape, two arms, and the working set the only variable.
//!
//! - [`counted_round`] runs the lowering compiled PHP emits for an
//!   overwriting store: read the displaced value, `store_box` the new one,
//!   `drop_ref` the displaced one. `store_box` publishes only — "the
//!   displaced value is released by `drop_ref`" (`src/memory/barrier.rs`) —
//!   so the pair is both calls, and only both touch two foreign headers.
//! - [`plain_round`] reads the same `Value` out of the same slot of the same
//!   vector and writes the same sixteen bytes into the same property, with no
//!   count touched.
//!
//! Both arms therefore pay the same scattered read of the value vector and
//! the same warm slot write. What only the counted arm pays is the two
//! foreign headers, so the difference between the arms at one working set is
//! the pair — measured by subtraction inside one run, never across runs.
//!
//! The working set is the population of children, from one to `1 << 20`. At
//! the narrow end both arms hit the same header every store; at the wide end
//! the population's headers exceed any cache this box has.
//!
//! **The cursor scatters and moves between rounds.** `i * GOLDEN & mask`
//! spreads consecutive stores over the population instead of walking it in
//! address order, which a prefetcher would follow. Each round also starts the
//! cursor a region further on, so a round does not re-read the headers the
//! round before it warmed — without that the second round of every arm reads
//! a warm set and the wide end reads like the narrow one.
//!
//! **Counts telescope back to zero.** Publishing `v1 … vN` into one slot and
//! then publishing null gives every `vi` one retain and one release,
//! whatever the order and however often a child repeats, so a round leaves
//! every child holding exactly its creation reference and the next round
//! starts from the state this one did.
//!
//! ## Checking the instrument against one whose answer is known
//!
//! At working set 1 the two headers are the same line every store and that
//! line is warm, so the difference between the arms must land near the
//! overwriting-store figure `dev/BENCHMARKS.md` already records: a counted
//! publish at 2.74-2.82 ns plus the displaced `drop_ref` at 0.85. A narrow
//! end that disagrees with that reads something other than the pair, and
//! nothing wider is then worth reading (`dev/BENCHMARKS.md`, Method).
//!
//! ```
//! cargo test --release --lib -- --ignored measure_cold_pair_cost --nocapture
//! ```

use std::time::{Duration, Instant};

use super::*;
use crate::object::{Object, new_constructed};
use crate::refcount::header_refcount;
use crate::value::Tag;
use crate::{Class, ClassBuilder};

/// Publishes per timed region, matching
/// [`super::what_a_store_costs_by_working_set`] so the narrow end is
/// comparable with its figures.
const STORES: usize = 1_000;

/// Timed rounds per arm, after one warm-up round whose time is discarded.
/// The quoted figure is the median.
const ROUNDS: usize = 15;

/// Child populations measured, in entities. The largest spreads headers over
/// tens of megabytes, past the 16 MiB last level this box has; the smallest
/// is the shape the recorded pair figure was taken in.
const SETS: [usize; 5] = [1, 64, 4_096, 65_536, 1 << 20];

/// Odd multiplier that scatters the cursor over the population — the 64-bit
/// golden-ratio constant, coprime with every power of two, so the cursor
/// visits a power-of-two population without repeating early.
const GOLDEN: usize = 0x9E37_79B9_7F4A_7C15;

/// The child index store `i` of `round` publishes.
///
/// Rounds start a region apart so that a round reads headers the round
/// before it did not warm.
#[inline]
fn cursor(round: usize, i: usize, mask: usize) -> usize {
    (round * STORES + i).wrapping_mul(GOLDEN) & mask
}

/// The trip count of a timed loop, hidden from the optimizer: a constant
/// bound is a different compilation from the run-time bound a loop filling
/// slots in compiled PHP has.
#[inline]
fn trip(count: usize) -> usize {
    std::hint::black_box(count)
}

/// A class with one Box property at offset 16 — the slot both arms publish
/// into.
fn holder_class(name: &str) -> *const Class {
    ClassBuilder::new(name).prop("value", true).build()
}

/// A class with no properties: the children the arms move around.
fn leaf_class(name: &str) -> *const Class {
    ClassBuilder::new(name).build()
}

/// `n` constructed GC-heap entities, as the values a slot names.
///
/// # Safety
/// `ctx` is a mounted context and `class` outlives every entity built here.
unsafe fn children(ctx: *mut LLContext, class: *const Class, n: usize) -> Vec<Value> {
    (0..n)
        .map(|_| {
            let child = unsafe { new_constructed(ctx, class, MemoryCategory::GcHeap) };
            Value::entity(Tag::Object, child as *mut RcHeader)
        })
        .collect()
}

/// One round of the counted publish: `store_box` retains the value and
/// releases the one it displaces, two foreign headers per store.
///
/// Returns the time of the publishes alone; the closing null publish that
/// balances the counts is outside the timed region.
///
/// # Safety
/// `slot` is a Box property of a live GC-heap owner, `values` are live
/// GC-heap entities, and `mask + 1` is `values.len()` and a power of two.
unsafe fn counted_round(
    arena: *mut Arena,
    slot: *mut Value,
    values: &[Value],
    mask: usize,
    round: usize,
) -> Duration {
    unsafe {
        let start = Instant::now();
        for i in 0..trip(STORES) {
            let old = std::ptr::read(slot);
            assert!(store_box(
                arena,
                MemoryCategory::GcHeap,
                slot,
                values[cursor(round, i, mask)]
            ));
            if old.is_refcounted() {
                drop_ref(MemoryCategory::GcHeap, old.entity_ptr());
            }
        }

        let elapsed = start.elapsed();
        let last = std::ptr::read(slot);
        std::ptr::write(slot, Value::null());
        if last.is_refcounted() {
            drop_ref(MemoryCategory::GcHeap, last.entity_ptr());
        }

        elapsed
    }
}

/// One round of the plain store: the same scattered read out of `values` and
/// the same sixteen bytes into the same slot, with no count touched.
///
/// The slot is left null, so the owner's dispose finds nothing to release.
/// A barrier-free write into a counted slot is sound only under that
/// closing write.
///
/// # Safety
/// As [`counted_round`].
unsafe fn plain_round(slot: *mut Value, values: &[Value], mask: usize, round: usize) -> Duration {
    unsafe {
        let start = Instant::now();
        for i in 0..trip(STORES) {
            std::ptr::write(slot, values[cursor(round, i, mask)]);
        }

        let elapsed = start.elapsed();
        std::ptr::write(slot, Value::null());
        elapsed
    }
}

/// Median, minimum and maximum nanoseconds per store over [`ROUNDS`] rounds.
struct ArmStats {
    minimum: f64,
    median: f64,
    maximum: f64,
}

/// Reduce a round's samples to [`ArmStats`].
fn reduce(mut samples: Vec<f64>) -> ArmStats {
    samples.sort_by(|a, b| a.partial_cmp(b).expect("a duration is never NaN"));
    ArmStats {
        minimum: samples[0],
        median: samples[samples.len() / 2],
        maximum: samples[samples.len() - 1],
    }
}

/// Run both arms at one working set, interleaved round by round with the
/// starting arm rotating, and return them in the order counted, plain.
///
/// Interleaving is what makes machine drift common mode between the two
/// figures the difference is taken from; rotation is what keeps one arm from
/// always inheriting the other's cache state.
///
/// # Safety
/// As [`counted_round`].
unsafe fn arms_at(
    arena: *mut Arena,
    slot: *mut Value,
    values: &[Value],
    mask: usize,
) -> (ArmStats, ArmStats) {
    let mut counted: Vec<f64> = Vec::with_capacity(ROUNDS);
    let mut plain: Vec<f64> = Vec::with_capacity(ROUNDS);

    for round in 0..=ROUNDS {
        let per_store = |elapsed: Duration| elapsed.as_nanos() as f64 / STORES as f64;
        if round % 2 == 0 {
            let c = unsafe { counted_round(arena, slot, values, mask, round) };
            let p = unsafe { plain_round(slot, values, mask, round) };
            if round > 0 {
                counted.push(per_store(c));
                plain.push(per_store(p));
            }
        } else {
            let p = unsafe { plain_round(slot, values, mask, round) };
            let c = unsafe { counted_round(arena, slot, values, mask, round) };
            if round > 0 {
                counted.push(per_store(c));
                plain.push(per_store(p));
            }
        }
    }

    (reduce(counted), reduce(plain))
}

/// Give every child back its creation reference, checking first that the
/// round's telescoping left it holding exactly that.
///
/// An over-count leaks and reads as drift in the next size; an under-count
/// frees a child a slot still names. The check is an `assert_eq!` and not a
/// `debug_assert!` because the probe is a release-mode run.
///
/// # Safety
/// `values` are live GC-heap entities named by no slot any longer.
unsafe fn drain(values: &[Value]) {
    for (index, value) in values.iter().enumerate() {
        let entity = value.entity_ptr();
        assert_eq!(
            unsafe { header_refcount(entity) },
            1,
            "child {index} was left holding a count the round did not intend"
        );

        unsafe { drop_ref(MemoryCategory::GcHeap, entity) };
    }
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn measure_cold_pair_cost() {
    let _g = crate::memory::block_pool::test_guard();
    let holder = holder_class("ColdPairOwner");
    let leaf = leaf_class("ColdPairChild");
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;

    // The first measurement a process takes is systematically slow — cold
    // caches, cold predictors, a cold frequency state
    // (`dev/BENCHMARKS.md`, Method, "Throw away the first run"). The per-arm
    // warm-up round inside `arms_at` sits inside the interleaving and warms
    // one arm at a time, so the whole probe runs once at the narrow end and
    // the answer is dropped.
    unsafe {
        let owner = new_constructed(context_ptr, holder, MemoryCategory::GcHeap);
        let slot = Object::prop_at(owner, 16);
        let values = children(context_ptr, leaf, 64);
        arms_at(arena_ptr, slot, &values, 63);
        drain(&values);
        drop_ref(MemoryCategory::GcHeap, owner as *mut RcHeader);
    }

    for set in SETS {
        assert!(set.is_power_of_two(), "the cursor masks, so a set is a power of two");
        unsafe {
            let owner = new_constructed(context_ptr, holder, MemoryCategory::GcHeap);
            let slot = Object::prop_at(owner, 16);
            let values = children(context_ptr, leaf, set);

            let (counted, plain) = arms_at(arena_ptr, slot, &values, set - 1);
            println!(
                "cold_pair working_set={set} counted={:.3} ns/store min={:.3} max={:.3}",
                counted.median, counted.minimum, counted.maximum
            );
            println!(
                "cold_pair working_set={set} plain={:.3} ns/store min={:.3} max={:.3}",
                plain.median, plain.minimum, plain.maximum
            );
            println!(
                "cold_pair working_set={set} pair={:.3} ns/store",
                counted.median - plain.median
            );

            drain(&values);
            drop_ref(MemoryCategory::GcHeap, owner as *mut RcHeader);
        }
    }
}
