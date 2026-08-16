//! Measurement probe, not a correctness test: what one publish costs in
//! the shape lowering emits — the micro-ops inlined and `owner_cat` a
//! constant — and how much of that cost belongs to the working set rather
//! than to the barrier.
//!
//! `benches/barrier.rs` answers neither question, by construction. A bench
//! is a separate crate, so every micro-op is reached through a call the
//! optimizer keeps; and each of its arms publishes one child a thousand
//! times, so every store reads a header line the store before it has just
//! written. Compiled PHP filling a thousand slots with a thousand children
//! has neither the call nor that chain (`dev/BENCHMARKS.md`, "the store
//! barrier's three directions, and the arena's logging inside them").
//!
//! Two working sets separate them: one child, which is the harness's shape,
//! and `WIDE` children, which spread consecutive stores over as many header
//! lines. What differs between the two is the chain; what they share is the
//! store.
//!
//! ## What prices the release-at-reset record
//!
//! The record is the arena's write-side log, appended when a heap child
//! enters an arena container. Two shapes price it, and neither subtracts one
//! direction from another:
//!
//! - [`heap_into_heap`] against [`heap_into_arena`]. Both take the same
//!   counted retain from the same allocator, both read the child's category
//!   out of the same header, and only the second appends a record. The
//!   category of the *child* cannot separate them — `ll_retain` returns early
//!   exactly where the log stays silent — so what varies is the owner.
//! - [`sweep_round`]. It holds both loops fixed inside one timed region and
//!   varies only `k`, the number of stores that go to the arena owner, so
//!   `d(ns)/dk` is the record's marginal cost without subtracting one round
//!   from another. The subtraction moves rather than disappears: the two
//!   loops are two code bodies at two alignments writing two slots in two
//!   allocators, and **nothing in the sweep itself bounds what they differ by
//!   apart from the log**. The control that does is [`null_sweep_round`]: the
//!   same two loops at the same `k` with both owners on the GC heap, so its
//!   slope is zero by construction and whatever slope it reads is the
//!   two-loops-two-slots term the sweep's slope has to clear.
//!
//! Neither instrument is authoritative over the other, and the probe carries
//! a null pair that says how far either can be trusted: `sweep k=0` and
//! [`heap_into_heap`] run the same publishes into the same slot, so their
//! difference is the instrument's zero. It reads 0.05 ns per store hot and
//! 1.22 cold, which is why only the hot figure stands
//! (`dev/BENCHMARKS.md`, 2026-08-15, "what the release-at-reset record costs,
//! and the statistic that decides the answer", and the retraction at its
//! head).
//!
//! **A per-record figure carries a fraction of a segment carve, and it lands
//! in the slope.** A log segment holds `LOG_SEG_RECORDS` = 500 records and is
//! carved from the arena's own bump, so `k` records cost `1 + (k - 1) / 500`
//! carves and the sweep's five points stand at 0, 1, 1, 2 and 2 of them. That
//! step is nearly collinear with `k`: regressed against it, a carve of `c`
//! contributes `0.002 * c` per record to the slope and leaves only `0.2 * c`
//! for the residual. The residual is therefore blind to it and measures
//! something else. Both terms move with `STORES`.
//!
//! **Every timed loop is bounded at run time.** The directions would bound
//! theirs by a constant and the sweep bounds its two by `k`, and a constant
//! trip count is a different compilation: with `STORES` visible, `sweep_k=0`
//! and `heap_into_heap` ran the same thousand publishes into the same slot
//! and disagreed by 0.11 ns per store. Hiding both bounds behind [`trip`] is
//! what makes the sweep and its cross-check one shape, and a run-time bound
//! is also what a loop filling slots in compiled PHP has.
//!
//! ## Hot and cold
//!
//! Every figure is taken twice. The reset's own drain reads exactly the log
//! lines the next round writes, and there are more of them as `k` grows, so
//! a round following a round measures a record landing in a warm line. That
//! is not what a request pays: it writes a record into a line it never
//! revisits. The cold half walks [`SCRATCH_BYTES`] untimed between rounds.
//!
//! **What the walk reaches is narrower than the whole round.** It runs after
//! a round rather than before one, and the next round's [`children`] writes
//! all `mask + 1` headers before its timer starts, so the child headers are
//! warm in both halves. The log's own pages, the TLB and the instruction
//! lines are what the walk takes out.
//!
//! The two halves cannot be interleaved arm by arm, an arm's cache state
//! being made by whatever ran before its round, so each working set is
//! reported three times: `hot`, `cold`, `hot_again`. The two hot passes
//! bracket the cold one and bound the drift the difference between them has
//! to clear (`dev/BENCHMARKS.md`, Method).
//!
//! ```
//! cargo test --release --lib -- --ignored measure_store_cost --nocapture
//! cargo test --release --lib --no-default-features -- --ignored measure_store_cost --nocapture
//! ```
//!
//! The figures obey `dev/BENCHMARKS.md`'s Method and are **not** comparable
//! with the harness table in that file: a different binary out of a
//! different profile. What may be compared is one shape against another
//! inside one run.

use std::ops::Range;
use std::time::{Duration, Instant};

use super::*;
use crate::object::{Object, new_constructed};
use crate::promote::arena_reset_full;
use crate::refcount::header_refcount;
use crate::value::Tag;
use crate::{Class, ClassBuilder};

/// Publishes per timed region: enough that the two clock reads around the
/// region cost well under a percent of it.
const STORES: usize = 1_000;

/// The wide working set. A power of two, so the cursor over it is one AND
/// in both shapes — the narrow shape pays the same instruction rather than
/// none, and the difference between the shapes stays the header lines.
const WIDE: usize = 64;

/// Timed rounds per shape, taken after one warm-up round whose time is
/// discarded; the median of them is the quoted figure (`dev/BENCHMARKS.md`,
/// Method, and [`stats_ns_per_store`]).
///
/// Fifteen rather than the five the directions alone would want, because
/// what the sweep is read for is a slope, and a slope over `k` is the
/// difference of two of these figures: it carries the noise of both, and
/// more rounds is what brings that difference above it.
const ROUNDS: usize = 15;

/// The sweep's points: how many of a region's `STORES` publishes go to the
/// arena owner. Five, spanning the whole region, so the slope has a lever
/// arm and the residual has something to show.
const SWEEP: [usize; 5] = [0, 250, 500, 750, 1_000];

/// Scratch walked untimed between rounds to take the round before out of
/// cache. It has to exceed the last level, which is 16 MiB on this box; a
/// machine with a larger one needs a larger figure here, and the hot and
/// cold figures coming out equal is what that would look like.
const SCRATCH_BYTES: usize = 32 << 20;

/// Cache line, the stride the eviction walk stores at.
const LINE_BYTES: usize = 64;

/// The trip count of a timed loop, hidden from the optimizer — see the
/// module doc, "Every timed loop is bounded at run time".
#[inline]
fn trip(count: usize) -> usize {
    std::hint::black_box(count)
}

/// A class with one Box property at offset 16 — the slot every shape
/// publishes into.
fn holder_class(name: &str) -> *const Class {
    ClassBuilder::new(name).prop("value", true).build()
}

/// A class with no properties: the children the shapes move around.
fn leaf_class(name: &str) -> *const Class {
    ClassBuilder::new(name).build()
}

/// `n` constructed entities of one category, as the values a slot names.
///
/// # Safety
/// `ctx` mounts the arena the arena-category entities come from.
unsafe fn children(
    ctx: *mut LLContext,
    class: *const Class,
    category: MemoryCategory,
    n: usize,
) -> Vec<Value> {
    (0..n)
        .map(|_| {
            let child = unsafe { new_constructed(ctx, class, category) };
            Value::entity(Tag::Object, child as *mut RcHeader)
        })
        .collect()
}

/// How many of the publishes `range` makes land on child `index`, the cursor
/// over the children being `i & mask`.
///
/// A round's owed count is read from this rather than from
/// `range.len() / (mask + 1)`: `WIDE` does not divide `STORES`, so the first
/// children take one publish more than the last.
fn stores_hitting(index: usize, mask: usize, range: Range<usize>) -> u32 {
    range.filter(|i| i & mask == index).count() as u32
}

/// Check every child against the count its round owes it, then give each
/// reference back — the last one tears the child down.
///
/// `owed` is the references a child holds **beyond** its creation one.
///
/// The check is an `assert_eq!` and not a `debug_assert!` because the probe
/// is a release-mode run, where the debug form is gone and `panic = "abort"`
/// is the failure mode: an over-count leaks and reads as drift, an
/// under-count frees a child a slot still names and the next round stores
/// through it.
///
/// # Safety
/// `values` are live GC-heap entities, each holding its creation reference
/// plus `owed(index)` more, and named by no slot any longer.
unsafe fn drain_children(values: &[Value], owed: impl Fn(usize) -> u32) {
    for (index, value) in values.iter().enumerate() {
        let entity = value.entity_ptr();
        let expected = owed(index) + 1;
        assert_eq!(
            unsafe { header_refcount(entity) },
            expected,
            "child {index} was left holding a count the round did not intend"
        );

        for _ in 0..expected {
            unsafe { drop_ref(MemoryCategory::GcHeap, entity) };
        }
    }
}

/// One round of the arena→arena publish: the retain returns early on the
/// category, nothing is logged, and the slot write is the work.
///
/// # Safety
/// `ctx` mounts `arena`; both classes outlive the call.
unsafe fn arena_into_arena(
    ctx: *mut LLContext,
    arena: *mut Arena,
    holder: *const Class,
    leaf: *const Class,
    mask: usize,
) -> Duration {
    unsafe {
        let owner = new_constructed(ctx, holder, MemoryCategory::RequestArena);
        let slot = Object::prop_at(owner, 16);
        let values = children(ctx, leaf, MemoryCategory::RequestArena, mask + 1);

        let start = Instant::now();
        for i in 0..trip(STORES) {
            assert!(store_box(
                arena,
                MemoryCategory::RequestArena,
                slot,
                values[i & mask]
            ));
        }

        let elapsed = start.elapsed();
        // Owner, children and the log are all the arena's, so the reset
        // takes them and hands the blocks back.
        arena_reset_full(arena);
        elapsed
    }
}

/// One round of the heap→arena publish: one release-at-reset record per
/// store, and the retain each record owes.
///
/// The slot is **not** cleared before the owner dies, where
/// [`heap_into_heap`] must clear it: an arena owner's death is the reset,
/// which drops pages and disposes nothing, so nothing here reads the slot
/// back.
///
/// # Safety
/// As [`arena_into_arena`].
unsafe fn heap_into_arena(
    ctx: *mut LLContext,
    arena: *mut Arena,
    holder: *const Class,
    leaf: *const Class,
    mask: usize,
) -> Duration {
    unsafe {
        let values = children(ctx, leaf, MemoryCategory::GcHeap, mask + 1);
        let owner = new_constructed(ctx, holder, MemoryCategory::RequestArena);
        let slot = Object::prop_at(owner, 16);

        let start = Instant::now();
        for i in 0..trip(STORES) {
            assert!(store_box(
                arena,
                MemoryCategory::RequestArena,
                slot,
                values[i & mask]
            ));
        }

        let elapsed = start.elapsed();
        // The reset owns one release per record it logged, which returns
        // each child to the creation reference this round holds.
        arena_reset_full(arena);
        drain_children(&values, |_| 0);
        elapsed
    }
}

/// One round of the heap→heap publish: the same counted retain from the
/// same allocator as [`heap_into_arena`], the child's category read out of
/// the same header, and no record. The direction that holds the retain
/// constant while the log varies.
///
/// # Safety
/// As [`arena_into_arena`].
unsafe fn heap_into_heap(
    ctx: *mut LLContext,
    arena: *mut Arena,
    holder: *const Class,
    leaf: *const Class,
    mask: usize,
) -> Duration {
    unsafe {
        let values = children(ctx, leaf, MemoryCategory::GcHeap, mask + 1);
        let owner = new_constructed(ctx, holder, MemoryCategory::GcHeap);
        let slot = Object::prop_at(owner, 16);

        let start = Instant::now();
        for i in 0..trip(STORES) {
            assert!(store_box(
                arena,
                MemoryCategory::GcHeap,
                slot,
                values[i & mask]
            ));
        }

        let elapsed = start.elapsed();
        // Nothing this round allocated is the arena's. The reset runs all
        // the same, so that the untimed half of the two directions is the
        // same work and the block pool reaches the next round in the same
        // state.
        arena_reset_full(arena);

        // The owner's dispose releases whatever its slot names
        // (`ll_default_dispose`), so the slot is cleared before the drain
        // frees the child it holds. The clear moves no count: a publish of
        // null retains nothing, and releasing the displaced entity is
        // `drop_ref`'s job, which the drain does.
        assert!(store_box(
            arena,
            MemoryCategory::GcHeap,
            slot,
            Value::null()
        ));
        drain_children(&values, |index| stores_hitting(index, mask, 0..STORES));
        drop_ref(MemoryCategory::GcHeap, owner as *mut RcHeader);
        elapsed
    }
}

/// One round of the arena→heap publish: the first store of a child logs it
/// as an escapee, the rest raise its hold-count.
///
/// # Safety
/// As [`arena_into_arena`].
unsafe fn arena_into_heap(
    ctx: *mut LLContext,
    arena: *mut Arena,
    holder: *const Class,
    leaf: *const Class,
    mask: usize,
) -> Duration {
    unsafe {
        let owner = new_constructed(ctx, holder, MemoryCategory::GcHeap);
        let slot = Object::prop_at(owner, 16);
        let values = children(ctx, leaf, MemoryCategory::RequestArena, mask + 1);

        let start = Instant::now();
        for i in 0..trip(STORES) {
            assert!(store_box(
                arena,
                MemoryCategory::GcHeap,
                slot,
                values[i & mask]
            ));
        }

        let elapsed = start.elapsed();
        // Every store raised a hold-count that no slot owns once the next
        // store displaced it. Give all of them back and clear the slot, or
        // the reset finds held escapees and promotes them — and the next
        // round would then measure a heap→heap store.
        for i in 0..STORES {
            drop_ref(MemoryCategory::GcHeap, values[i & mask].entity_ptr());
        }

        assert!(store_box(
            arena,
            MemoryCategory::GcHeap,
            slot,
            Value::null()
        ));
        arena_reset_full(arena);
        drop_ref(MemoryCategory::GcHeap, owner as *mut RcHeader);
        elapsed
    }
}

/// One round of the null sweep: the same two loops as [`sweep_round`] at the
/// same `k`, with **both** owners on the GC heap. No store appends a record,
/// so the slope over `k` is zero by construction, and the slope actually
/// measured is the two-loops-two-slots term — the bound the sweep's slope is
/// read against.
///
/// # Safety
/// As [`arena_into_arena`].
unsafe fn null_sweep_round(
    ctx: *mut LLContext,
    arena: *mut Arena,
    holder: *const Class,
    leaf: *const Class,
    mask: usize,
    k: usize,
) -> Duration {
    unsafe {
        let values = children(ctx, leaf, MemoryCategory::GcHeap, mask + 1);
        let first_owner = new_constructed(ctx, holder, MemoryCategory::GcHeap);
        let first_slot = Object::prop_at(first_owner, 16);
        let second_owner = new_constructed(ctx, holder, MemoryCategory::GcHeap);
        let second_slot = Object::prop_at(second_owner, 16);

        let k = trip(k);
        let start = Instant::now();
        for i in 0..k {
            assert!(store_box(
                arena,
                MemoryCategory::GcHeap,
                first_slot,
                values[i & mask]
            ));
        }

        for i in k..trip(STORES) {
            assert!(store_box(
                arena,
                MemoryCategory::GcHeap,
                second_slot,
                values[i & mask]
            ));
        }

        let elapsed = start.elapsed();
        // No record was appended, so the reset releases nothing here; it
        // runs so that the untimed half stays the same work as in every
        // other arm and the block pool reaches the next round in the same
        // state.
        arena_reset_full(arena);
        assert!(store_box(
            arena,
            MemoryCategory::GcHeap,
            first_slot,
            Value::null()
        ));
        assert!(store_box(
            arena,
            MemoryCategory::GcHeap,
            second_slot,
            Value::null()
        ));
        drain_children(&values, |index| stores_hitting(index, mask, 0..STORES));
        drop_ref(MemoryCategory::GcHeap, first_owner as *mut RcHeader);
        drop_ref(MemoryCategory::GcHeap, second_owner as *mut RcHeader);
        elapsed
    }
}

/// One round of the sweep: of the region's `STORES` publishes, the first `k`
/// name an arena owner and the rest a heap owner, out of the same children
/// throughout. What varies across `k` is the record and nothing else.
///
/// Two loops rather than one branching loop, because `owner_cat` is the
/// compile-time constant this whole probe exists to measure the shape of;
/// they share one timed region, so the region's time is
/// `k · heap→arena + (STORES - k) · heap→heap` plus a constant.
///
/// # Safety
/// As [`arena_into_arena`].
unsafe fn sweep_round(
    ctx: *mut LLContext,
    arena: *mut Arena,
    holder: *const Class,
    leaf: *const Class,
    mask: usize,
    k: usize,
) -> Duration {
    unsafe {
        let values = children(ctx, leaf, MemoryCategory::GcHeap, mask + 1);
        let arena_owner = new_constructed(ctx, holder, MemoryCategory::RequestArena);
        let arena_slot = Object::prop_at(arena_owner, 16);
        let heap_owner = new_constructed(ctx, holder, MemoryCategory::GcHeap);
        let heap_slot = Object::prop_at(heap_owner, 16);

        let k = trip(k);
        let start = Instant::now();
        for i in 0..k {
            assert!(store_box(
                arena,
                MemoryCategory::RequestArena,
                arena_slot,
                values[i & mask]
            ));
        }

        for i in k..trip(STORES) {
            assert!(store_box(
                arena,
                MemoryCategory::GcHeap,
                heap_slot,
                values[i & mask]
            ));
        }

        let elapsed = start.elapsed();
        // The reset releases one reference per record and takes the arena
        // owner with it, so what the children are left owing is the heap
        // half of the region — `STORES - k`, an error in which is linear in
        // `k` and would read as slope.
        arena_reset_full(arena);
        assert!(store_box(
            arena,
            MemoryCategory::GcHeap,
            heap_slot,
            Value::null()
        ));
        drain_children(&values, |index| stores_hitting(index, mask, k..STORES));
        drop_ref(MemoryCategory::GcHeap, heap_owner as *mut RcHeader);
        elapsed
    }
}

/// One measured shape: the name it is reported under, and the round that
/// times it.
struct Arm {
    label: String,
    round: Box<dyn FnMut() -> Duration>,
}

/// Store into every cache line of `scratch`, so that what the round before
/// it touched is out of cache. An empty slice is the hot mode and evicts
/// nothing.
fn evict(scratch: &mut [u8]) {
    for line in scratch.chunks_mut(LINE_BYTES) {
        line[0] = line[0].wrapping_add(1);
    }

    std::hint::black_box(scratch);
}

/// One arm's `ROUNDS` figures reduced to the order statistics the report
/// prints. The median is the quoted figure; the minimum and maximum decide
/// between the two accounts of round-to-round spread — a tight floor with a
/// right tail is interference, a wide spread is layout, each round
/// allocating its children and log segments afresh. Under the rotated arm
/// order the two statistics agree — the fixed order was what held them
/// 0.38 against 0.72 ns apart on the wide-set record
/// (`dev/BENCHMARKS.md`, 2026-08-16, "the null sweep bounds the
/// instrument, and rotation settles the statistic").
struct ArmStats {
    minimum: f64,
    median: f64,
    maximum: f64,
}

/// `ROUNDS` timed rounds for each arm, in nanoseconds per store, taken after
/// one warm-up round whose time is discarded (`dev/BENCHMARKS.md`, Method),
/// reduced per arm to [`ArmStats`].
///
/// The arms are interleaved round by round rather than run one arm's rounds
/// and then the next arm's: the block pool hands blocks back in LIFO order
/// and the machine drifts over a run, and both are common mode only if every
/// arm meets them at the same point of it. Within a round the starting arm
/// rotates with the round index: at a fixed order every arm inherits the
/// cache and pool state of the same neighbour every time — the sweep's
/// points run monotone in `k` with nothing evicted between them in the hot
/// half — where rotation spreads that inheritance over every neighbour.
///
/// `scratch` is walked untimed after each round, empty for the hot half.
fn stats_ns_per_store(arms: &mut [Arm], scratch: &mut [u8]) -> Vec<ArmStats> {
    let mut taken: Vec<Vec<f64>> = vec![Vec::with_capacity(ROUNDS); arms.len()];
    for round in 0..=ROUNDS {
        for position in 0..arms.len() {
            let index = (round + position) % arms.len();
            let elapsed = (arms[index].round)();
            if round > 0 {
                taken[index].push(elapsed.as_nanos() as f64 / STORES as f64);
            }

            evict(scratch);
        }
    }

    taken
        .iter_mut()
        .map(|samples| {
            samples.sort_by(|a, b| a.partial_cmp(b).expect("a duration is never NaN"));
            ArmStats {
                minimum: samples[0],
                median: samples[samples.len() / 2],
                maximum: samples[samples.len() - 1],
            }
        })
        .collect()
}

/// Every shape measured at one working set, in the order they are reported:
/// the four directions, then the sweep that prices the record, then the null
/// sweep that bounds the sweep's own instrument term.
///
/// The round bodies share a publish-reset-drain skeleton and stay three
/// copies on purpose: a shared parameterized body would change the compiled
/// shape of the timed loops, which is the thing the probe measures.
fn arms_for(
    ctx: *mut LLContext,
    arena: *mut Arena,
    holder: *const Class,
    leaf: *const Class,
    mask: usize,
) -> Vec<Arm> {
    let mut arms = vec![
        Arm {
            label: "arena_into_arena".to_string(),
            round: Box::new(move || unsafe { arena_into_arena(ctx, arena, holder, leaf, mask) }),
        },
        Arm {
            label: "heap_into_arena".to_string(),
            round: Box::new(move || unsafe { heap_into_arena(ctx, arena, holder, leaf, mask) }),
        },
        Arm {
            label: "heap_into_heap".to_string(),
            round: Box::new(move || unsafe { heap_into_heap(ctx, arena, holder, leaf, mask) }),
        },
        Arm {
            label: "arena_into_heap".to_string(),
            round: Box::new(move || unsafe { arena_into_heap(ctx, arena, holder, leaf, mask) }),
        },
    ];

    for k in SWEEP {
        arms.push(Arm {
            label: format!("sweep_k={k}"),
            round: Box::new(move || unsafe { sweep_round(ctx, arena, holder, leaf, mask, k) }),
        });
    }

    for k in SWEEP {
        arms.push(Arm {
            label: format!("null_k={k}"),
            round: Box::new(move || unsafe { null_sweep_round(ctx, arena, holder, leaf, mask, k) }),
        });
    }

    arms
}

/// Least-squares slope of the sweep in nanoseconds **per record**, and the
/// largest distance of a point from that line.
///
/// The figures arrive as nanoseconds per store over a region of `STORES`
/// stores, so the region's time is `STORES` times each and the slope against
/// `k` is one record. The residual reads what the line cannot: curvature, and
/// a sweep that is not monotone at all, which the cold half has been. It does
/// **not** read the segment carve, which is nearly collinear with `k` and so
/// lands in the slope — see the module doc.
fn sweep_slope(figures: &[f64]) -> (f64, f64) {
    let n = SWEEP.len() as f64;
    let mean_k = SWEEP.iter().sum::<usize>() as f64 / n;
    let mean_ns = figures.iter().sum::<f64>() * STORES as f64 / n;

    let mut covariance = 0.0;
    let mut variance = 0.0;
    for (k, figure) in SWEEP.iter().zip(figures) {
        let dk = *k as f64 - mean_k;
        covariance += dk * (figure * STORES as f64 - mean_ns);
        variance += dk * dk;
    }

    let slope = covariance / variance;
    let intercept = mean_ns - slope * mean_k;
    let residual = SWEEP
        .iter()
        .zip(figures)
        .map(|(k, figure)| (figure * STORES as f64 - (intercept + slope * *k as f64)).abs())
        .fold(0.0f64, f64::max);

    (slope, residual)
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn measure_store_cost() {
    let _g = crate::memory::block_pool::test_guard();
    let holder = holder_class("StoreCostOwner");
    let leaf = leaf_class("StoreCostChild");
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    let mut scratch = vec![0u8; SCRATCH_BYTES];

    // The first measurement a process takes is systematically slow — cold
    // caches, cold predictors, a cold frequency state, and here also a
    // scratch buffer whose pages are not faulted in yet
    // (`dev/BENCHMARKS.md`, Method, "Throw away the first run"). The per-arm
    // warm-up round inside `stats_ns_per_store` cannot cover that: it sits
    // inside the interleaving and warms one arm at a time. This pass runs the
    // whole probe once and drops the answer.
    let mut warm_up = arms_for(context_ptr, arena_ptr, holder, leaf, WIDE - 1);
    stats_ns_per_store(&mut warm_up, &mut scratch);

    for set in [1usize, WIDE] {
        let mask = set - 1;
        let mut arms = arms_for(context_ptr, arena_ptr, holder, leaf, mask);
        // Hot, cold, hot again: the cache mode cannot be interleaved into
        // the arms — an arm's cache state is made by whatever ran before its
        // round, so a mixed round would leave every hot arm cold. What
        // separates the hot/cold difference from drift across a block is
        // therefore the Method's other control, A then B then A again
        // (`dev/BENCHMARKS.md`, Method): the two hot passes bracket the cold
        // one, and a disagreement between them is the size of the drift the
        // difference has to clear.
        let hot = stats_ns_per_store(&mut arms, &mut []);
        let cold = stats_ns_per_store(&mut arms, &mut scratch);
        let hot_again = stats_ns_per_store(&mut arms, &mut []);

        for (log, figures) in [("hot", &hot), ("cold", &cold), ("hot_again", &hot_again)] {
            for (arm, stats) in arms.iter().zip(figures.iter()) {
                println!(
                    "store_cost working_set={set} log={log} {}={:.3} ns/store \
                     min={:.3} max={:.3}",
                    arm.label, stats.median, stats.minimum, stats.maximum
                );
            }

            let medians: Vec<f64> = figures.iter().map(|stats| stats.median).collect();
            let sweep = &medians[medians.len() - 2 * SWEEP.len()..medians.len() - SWEEP.len()];
            let (slope, residual) = sweep_slope(sweep);
            println!(
                "store_cost working_set={set} log={log} sweep_slope={slope:.3} ns/record \
                 residual={residual:.1} ns/region"
            );

            let null = &medians[medians.len() - SWEEP.len()..];
            let (null_slope, null_residual) = sweep_slope(null);
            println!(
                "store_cost working_set={set} log={log} null_slope={null_slope:.3} ns/record \
                 residual={null_residual:.1} ns/region"
            );
        }
    }
}
