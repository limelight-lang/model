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
//! **Both headers, not one.** A first version of this probe published into a
//! single slot, so the value each store displaced was the value the store
//! before it had just retained — warm by construction, and the figure it
//! produced was one cold retain against a warm release. Here every owner has
//! its own slot, pre-filled at setup and not touched since, so the displaced
//! header is as cold as the retained one.
//!
//! ## Method
//!
//! One shape, two arms, and the working set the only variable.
//!
//! - [`counted_round`] runs the lowering compiled PHP emits for an
//!   overwriting store: read the displaced value, `store_box` the new one,
//!   `drop_ref` the displaced one. `store_box` publishes only — "the
//!   displaced value is released by `drop_ref`" (`src/memory/barrier.rs`) —
//!   so the pair is both calls, and both touch a foreign header.
//! - [`plain_round`] makes the same scattered read out of the value vector
//!   and the same sixteen-byte write into a scattered owner's slot, with no
//!   count touched.
//!
//! Both arms therefore pay the same scattered vector read and the same
//! scattered slot write. What only the counted arm pays is the two foreign
//! headers, so the difference between the arms at one working set is the
//! pair — measured by subtraction inside one run, never across runs.
//!
//! The two arms write into two populations of owners. A plain write into a
//! counted slot would leave a reference the setup counted and nothing
//! released, so the counted arm owns the counted slots and the plain arm
//! owns slots that were never counted at all.
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
//! **The slot population conserves the counts.** Every counted owner slot
//! holds one child from setup to teardown: a store retains the arriving child
//! and releases the departing one, so the number of slot-held references is
//! the number of owners throughout, and no child can reach zero — each keeps
//! its creation reference besides. Teardown walks the slots once, releases
//! what each holds and nulls it, after which every child reads exactly one.
//!
//! ## Checking the instrument against one whose answer is known
//!
//! At working set 1 there is one owner and one child, so both headers are
//! the same warm line every store, and the difference between the arms must
//! land near the overwriting-store figure `dev/BENCHMARKS.md` already
//! records: a counted publish at 2.74-2.82 ns plus the displaced `drop_ref`
//! at 0.85. A narrow end that disagrees with that reads something other than
//! the pair, and nothing wider is then worth reading (`dev/BENCHMARKS.md`,
//! Method).
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
pub(super) const STORES: usize = 1_000;

/// Timed rounds per arm, after one warm-up round whose time is discarded.
/// The quoted figure is the median.
pub(super) const ROUNDS: usize = 15;

/// Child populations measured, in entities. The largest spreads headers over
/// tens of megabytes, past the 16 MiB last level this box has; the smallest
/// is the shape the recorded pair figure was taken in.
pub(super) const SETS: [usize; 5] = [1, 64, 4_096, 65_536, 1 << 20];

/// Odd multiplier that scatters the cursor over the population — the 64-bit
/// golden-ratio constant, coprime with every power of two, so the cursor
/// visits a power-of-two population without repeating early.
const GOLDEN: usize = 0x9E37_79B9_7F4A_7C15;

/// The child index store `i` of `round` publishes.
///
/// Rounds start a region apart so that a round reads headers the round
/// before it did not warm.
#[inline]
pub(super) fn cursor(round: usize, i: usize, mask: usize) -> usize {
    (round * STORES + i).wrapping_mul(GOLDEN) & mask
}

/// The trip count of a timed loop, hidden from the optimizer: a constant
/// bound is a different compilation from the run-time bound a loop filling
/// slots in compiled PHP has.
#[inline]
pub(super) fn trip(count: usize) -> usize {
    std::hint::black_box(count)
}

/// A class with one Box property at offset 16 — the slot both arms publish
/// into.
pub(super) fn holder_class(name: &str) -> *const Class {
    ClassBuilder::new(name).prop("value", true).build()
}

/// A class with no properties: the children the arms move around.
pub(super) fn leaf_class(name: &str) -> *const Class {
    ClassBuilder::new(name).build()
}

/// `n` constructed GC-heap entities, as the values a slot names.
///
/// # Safety
/// `ctx` is a mounted context and `class` outlives every entity built here.
pub(super) unsafe fn children(ctx: *mut LLContext, class: *const Class, n: usize) -> Vec<Value> {
    (0..n)
        .map(|_| {
            let child = unsafe { new_constructed(ctx, class, MemoryCategory::GcHeap) };
            Value::entity(Tag::Object, child as *mut RcHeader)
        })
        .collect()
}

/// `n` owners of one class, each with its own Box property at offset 16.
///
/// # Safety
/// `ctx` is a mounted context and `class` outlives every entity built here.
pub(super) unsafe fn owners(
    ctx: *mut LLContext,
    class: *const Class,
    n: usize,
) -> Vec<*mut Object> {
    (0..n)
        .map(|_| unsafe { new_constructed(ctx, class, MemoryCategory::GcHeap) })
        .collect()
}

/// Fill each counted owner's slot with the child of the same index, taking
/// the reference the slot holds.
///
/// After this every child reads two: its creation reference and the slot's.
/// The stores are sequential and untimed, so nothing they warm survives the
/// scattered rounds that follow at any population that does not fit cache.
///
/// # Safety
/// `owners` and `values` have the same length; both are live GC-heap
/// entities; no slot holds anything yet.
pub(super) unsafe fn prefill(arena: *mut Arena, owners: &[*mut Object], values: &[Value]) {
    for (owner, value) in owners.iter().zip(values) {
        unsafe {
            let slot = Object::prop_at(*owner, 16);
            assert!(store_box(arena, MemoryCategory::GcHeap, slot, *value));
        }
    }
}

/// One round of the counted publish: per store one scattered owner, one
/// scattered arriving child, and the child the slot held since setup —
/// three foreign lines, of which two are headers the pair reads and writes.
///
/// # Safety
/// `owners` and `values` are live, `mask + 1` is their common length and a
/// power of two, and every owner's slot holds a counted reference.
pub(super) unsafe fn counted_round(
    arena: *mut Arena,
    owners: &[*mut Object],
    values: &[Value],
    mask: usize,
    round: usize,
) -> Duration {
    unsafe {
        let start = Instant::now();
        for i in 0..trip(STORES) {
            let slot = Object::prop_at(owners[cursor(round, i, mask)], 16);
            let old = std::ptr::read(slot);
            assert!(store_box(
                arena,
                MemoryCategory::GcHeap,
                slot,
                values[cursor(round, i + STORES, mask)]
            ));
            if old.is_refcounted() {
                drop_ref(MemoryCategory::GcHeap, old.entity_ptr());
            }
        }

        start.elapsed()
    }
}

/// One round of the plain store: the same scattered owner and the same
/// scattered read out of `values`, writing the same sixteen bytes with no
/// count touched.
///
/// Its owners are a population of their own whose slots were never counted,
/// so a barrier-free write leaves nothing owed. They are nulled at teardown
/// without a release.
///
/// # Safety
/// `owners` are live GC-heap entities whose slots hold no counted
/// reference; `values` are live; `mask + 1` is the common length.
unsafe fn plain_round(
    owners: &[*mut Object],
    values: &[Value],
    mask: usize,
    round: usize,
) -> Duration {
    unsafe {
        let start = Instant::now();
        for i in 0..trip(STORES) {
            let slot = Object::prop_at(owners[cursor(round, i, mask)], 16);
            std::ptr::write(slot, values[cursor(round, i + STORES, mask)]);
        }

        start.elapsed()
    }
}

/// Median, minimum and maximum nanoseconds per store over [`ROUNDS`] rounds.
pub(super) struct ArmStats {
    pub(super) minimum: f64,
    pub(super) median: f64,
    pub(super) maximum: f64,
}

/// Reduce a round's samples to [`ArmStats`].
pub(super) fn reduce(mut samples: Vec<f64>) -> ArmStats {
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
/// As [`counted_round`] and [`plain_round`].
unsafe fn arms_at(
    arena: *mut Arena,
    counted_owners: &[*mut Object],
    plain_owners: &[*mut Object],
    values: &[Value],
    mask: usize,
) -> (ArmStats, ArmStats) {
    let mut counted: Vec<f64> = Vec::with_capacity(ROUNDS);
    let mut plain: Vec<f64> = Vec::with_capacity(ROUNDS);

    for round in 0..=ROUNDS {
        let per_store = |elapsed: Duration| elapsed.as_nanos() as f64 / STORES as f64;
        let (c, p) = if round % 2 == 0 {
            let c = unsafe { counted_round(arena, counted_owners, values, mask, round) };
            let p = unsafe { plain_round(plain_owners, values, mask, round) };
            (c, p)
        } else {
            let p = unsafe { plain_round(plain_owners, values, mask, round) };
            let c = unsafe { counted_round(arena, counted_owners, values, mask, round) };
            (c, p)
        };

        if round > 0 {
            counted.push(per_store(c));
            plain.push(per_store(p));
        }
    }

    (reduce(counted), reduce(plain))
}

/// Release what every counted slot holds and null both populations' slots,
/// then give every child and owner its creation reference back.
///
/// The check is an `assert_eq!` and not a `debug_assert!` because the probe
/// is a release-mode run: a child left holding more than its creation
/// reference means the rounds lost a release and every figure above it is
/// a different measurement from the one reported.
///
/// # Safety
/// Every argument is live; no slot outside these populations names any of
/// them.
unsafe fn teardown(counted_owners: &[*mut Object], plain_owners: &[*mut Object], values: &[Value]) {
    for owner in counted_owners {
        unsafe {
            let slot = Object::prop_at(*owner, 16);
            let held = std::ptr::read(slot);
            std::ptr::write(slot, Value::null());
            if held.is_refcounted() {
                drop_ref(MemoryCategory::GcHeap, held.entity_ptr());
            }
        }
    }

    for owner in plain_owners {
        unsafe { std::ptr::write(Object::prop_at(*owner, 16), Value::null()) };
    }

    for (index, value) in values.iter().enumerate() {
        let entity = value.entity_ptr();
        assert_eq!(
            unsafe { header_refcount(entity) },
            1,
            "child {index} was left holding a count the rounds did not intend"
        );

        unsafe { drop_ref(MemoryCategory::GcHeap, entity) };
    }

    for owner in counted_owners.iter().chain(plain_owners) {
        unsafe { drop_ref(MemoryCategory::GcHeap, *owner as *mut RcHeader) };
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
    // one arm at a time, so the whole probe runs once at a narrow set and the
    // answer is dropped.
    for (set, report) in [(64usize, false)]
        .into_iter()
        .chain(SETS.map(|set| (set, true)))
    {
        assert!(
            set.is_power_of_two(),
            "the cursor masks, so a set is a power of two"
        );

        unsafe {
            let counted_owners = owners(context_ptr, holder, set);
            let plain_owners = owners(context_ptr, holder, set);
            let values = children(context_ptr, leaf, set);
            prefill(arena_ptr, &counted_owners, &values);

            let (counted, plain) =
                arms_at(arena_ptr, &counted_owners, &plain_owners, &values, set - 1);

            if report {
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
            }

            teardown(&counted_owners, &plain_owners, &values);
        }
    }
}
