//! What a collection's close costs when its deaths happen inside the window
//! (`dev/BENCHMARKS.md`, "S44.4 the close against the chain").
//!
//! Ignored in the ordinary suite and run by hand, in a release build because
//! the reading is a time:
//!
//! ```text
//! cargo test --release --lib density::tests::the_death_loads -- --ignored --nocapture
//! ```
//!
//! The numbers themselves are in `dev/BENCHMARKS.md`; what stands here is the
//! construction that produced them.
//!
//! # The death count is the fixture's input, and it is reported as a check
//!
//! The fixture frees the component it built, so
//! [`deferred_slot_count`](crate::cycle::deferred_slot_reuse::deferred_slot_count)
//! answers the number the fixture chose. As a measurement that is the
//! harness's own input read back, and it is refused in that role (Sage,
//! 2026-09-04, `dev/DECISIONS.md`). It stands here as a check instead: the
//! crate's own free path withheld exactly the deaths the fixture made, which
//! fails if a death took another `ll_free` arm, if a candidate bit still
//! stood, or if a teardown killed something nobody counted.
//!
//! # The reading is a time, and the other arm is in another tree
//!
//! [`ActiveTrace`]'s drop is the whole close — the row sweep, the candidate
//! restore, the withheld returns and the arena's hand-back — and it is what is
//! timed, because the two designs differ inside it and share everything
//! around it. The arm this tree cannot run is the record chain, which the
//! crate does not carry (`dev/DECISIONS.md`, "one stack through the dead
//! entity holds every withheld return"); its reading is taken over a worktree
//! at the last commit that carried it, with this construction ported into it,
//! and the two are compared in one sitting under `dev/BENCHMARKS.md`'s Method.
//! Which commit that was, and what the two answered, is `dev/BENCHMARKS.md`,
//! "S44.4 the close against the chain".
//!
//! The reading is the **minimum** of [`TIMED_RUNS`] independent loads, each on
//! a heap of its own. A close cannot be repeated over one population — it
//! destroys the one it reads — so a run is a whole load, and the minimum is
//! the sample least disturbed by the box this crate is developed on.
//!
//! # In lines the stack answers by construction
//!
//! The close's pop reads one word of each dead entity, at
//! `heap::FREE_LIST_LINK_OFFSET`, and the `ll_free` behind that pop reads the
//! same entity's header. Both stand in the first sixteen bytes of a slot whose
//! alignment is its size class, so they share a cache line at every class and
//! the pop touches no line the return does not.
//! [`the_close_reads_no_line_of_its_own`] pins that rather than stating it.
//!
//! # What these loads may not claim
//!
//! The teardown is unbuilt — S36.3's guard, S36.4's destructors and S36.5's
//! sever and deferred drops are all open — so the frees below are the shape
//! of that path rather than the path. Two consequences:
//!
//! - **no tail.** A component's members here carry one property each and no
//!   external children, so no acyclic garbage dies behind them, and a real
//!   teardown's deferred drops would lengthen both arms' closes.
//! - **no destructor and no verdict.** A `__destruct` that allocates or
//!   resurrects moves the death set, and `cycle::validation` is what decides a
//!   component is garbage; here the fixture asserts it.

use super::*;

use std::time::{Duration, Instant};

use super::the_loads::{COLLECTIONS, slots_per_block};
use crate::cycle::deferred_slot_reuse::deferred_slot_count;
use crate::memory::gc_metadata;
use crate::memory::heap::FREE_LIST_LINK_OFFSET;
use crate::refcount::clear_candidate_bit;

/// The component size every load carries: the corpus's median closure, and
/// the size S40.1 reports its own widest readings at.
const MEMBERS: usize = 381;

/// A cache line, which the crate's own `LINE_SIZE` is not — that constant is
/// the 256-byte header line a block reserves.
const CACHE_LINE_BYTES: usize = 64;

/// Loads each timed arm runs, and the reading is the minimum of them.
///
/// Twenty rather than more because the whole load is rebuilt per run: the
/// sparse arm draws a block per component member, so one close of it costs a
/// build of some 24 MiB.
const TIMED_RUNS: usize = 20;

/// The size classes the dense arm is read at: the narrowest of the design's
/// four, where a block holds every member, and the one where a block holds
/// three quarters of them.
///
/// Class 16 is not among them and cannot be: `props_for(16)` is zero
/// properties, so a class-16 entity has no property to carry a ring edge and
/// no component can be built in it.
const DENSE_CLASSES: [usize; 2] = [32, 128];

/// The size class the sparse arm is read at, one component member to a block.
const SPARSE_CLASS: usize = 256;

/// One touched entity block and the deaths that landed in it.
#[derive(Clone, Copy)]
struct WalkedBlock {
    /// The block header's address, which is what a death's slot resolves to.
    block: usize,
    /// Component members freed inside the window whose slot is in this block.
    deaths: u32,
}

/// One killing collection: the reading taken before the first free, what the
/// deaths did, and what the close cost.
struct DeathReading {
    /// The density read after the trace and before any free, which is the
    /// arm the ordinary collections are compared against.
    density: TraceDensity,
    /// Blocks the trace touched, entity blocks alone.
    blocks: Vec<WalkedBlock>,
    /// Blocks the arena drew from the memory manager, which S40.1 recorded
    /// for the same load.
    arena_blocks: usize,
    /// Entities the fixture freed inside the window.
    freed: usize,
    /// Returns the window withheld, which equals `freed` or the run is void.
    withheld: usize,
    /// Distinct cache lines the withheld slots stand in, which is what the
    /// close's pop reads and what the returns behind it read anyway.
    close_lines: usize,
    /// What `ActiveTrace::drop` took: the sweep, the restore, every withheld
    /// return and the arena's hand-back.
    close: Duration,
    /// Manager blocks the thread held before the collection and after its
    /// close.
    blocks_before: usize,
    blocks_after: usize,
}

/// One load and everything read off it.
struct DeathLoad {
    class_bytes: usize,
    fillers_between: usize,
    /// The seven collections that change nothing, which are S40.1's control.
    ordinary: Vec<Reading>,
    killing: DeathReading,
}

/// Build a component of [`MEMBERS`] entities with `fillers_between`
/// unreferenced entities between each pair, trace it [`COLLECTIONS`] times,
/// and let the last collection kill it.
///
/// The killing collection is the last on this thread and nothing collects
/// after it: `ActiveTrace::drop` restores the batch, so the write lane then
/// holds records naming freed slots, and a ninth collection would offer them
/// to `trace_batch` as roots.
fn a_killing_load(class_bytes: usize, fillers_between: usize) -> DeathLoad {
    let name = format!("DeathLoad{class_bytes}x{fillers_between}");
    let class = a_class(&name, props_for(class_bytes));

    let mut positions = Vec::with_capacity(MEMBERS);
    let mut count = 0;
    for _ in 0..MEMBERS {
        positions.push(count);
        count += 1 + fillers_between;
    }

    let fixture = build(class, count, &positions);
    let ordinary = (0..COLLECTIONS - 1).map(|_| collect()).collect();
    let killing = collect_and_kill(fixture);

    DeathLoad {
        class_bytes,
        fillers_between,
        ordinary,
        killing,
    }
}

/// Trace, read the rows, then run the teardown S36.5 will run — inside the
/// still-open window, which is where a collection's own deaths happen — and
/// time the close that follows it.
///
/// The fillers outlive the window and are freed after it: they are the
/// occupants a production trace touches a block for without killing anything
/// in it, and a free inside the window would put them in the death count.
fn collect_and_kill(fixture: Fixture) -> DeathReading {
    let Fixture {
        mut arena,
        entities,
        ring,
    } = fixture;

    let blocks_before = gc_metadata::thread_stats().current_blocks();
    let mut active = ActiveTrace::open().expect("the pool funded the trace window");
    active.detach_candidates();

    let (trace_arena, batch) = active.rows_and_roots();
    assert_eq!(
        unsafe { trace_batch(trace_arena, batch) },
        TraceOutcome::Complete,
        "the trace completed, so its rows are a whole closure"
    );

    let density = unsafe { totals(trace_arena) };
    let arena_blocks = trace_arena.blocks_held();
    let mut blocks = unsafe { walked_blocks(trace_arena) };
    for &member in &ring {
        let block = entities[member] as usize & !BLOCK_MASK;
        let walked = blocks
            .iter_mut()
            .find(|walked| walked.block == block)
            .expect("a component member's own block is one the trace touched");
        walked.deaths += 1;
    }

    let dying: Vec<usize> = ring
        .iter()
        .map(|&member| entities[member] as usize)
        .collect();
    let close_lines = lines_of(&dying);

    // The shape `tear_down` uses, and S36.5's: hold every member while the
    // ring's edges go, then release and die. The candidate bit is cleared
    // first because `ll_free` refuses the queue window before it reaches the
    // trace window, and a member's registration still stands in the detached
    // batch.
    unsafe {
        for &member in &ring {
            ll_retain(entities[member] as *mut RcHeader);
        }

        for &member in &ring {
            store_prop(
                &mut arena,
                entities[member],
                prop_offset(0),
                std::ptr::null_mut(),
            );
        }

        for &member in &ring {
            let entity = entities[member];
            clear_candidate_bit(entity as *mut RcHeader);
            assert!(
                ll_release(entity as *mut RcHeader),
                "the ring's edges are gone, so this release is the last"
            );
            ll_object_die(entity);
        }
    }

    let withheld = deferred_slot_count();
    let start = Instant::now();
    drop(active);
    let close = start.elapsed();

    let blocks_after = gc_metadata::thread_stats().current_blocks();
    let mut is_member = vec![false; entities.len()];
    for &member in &ring {
        is_member[member] = true;
    }

    unsafe {
        for (index, &entity) in entities.iter().enumerate() {
            if is_member[index] {
                continue;
            }

            clear_candidate_bit(entity as *mut RcHeader);
            ll_release(entity as *mut RcHeader);
            ll_object_die(entity);
        }
    }

    DeathReading {
        density,
        blocks,
        arena_blocks,
        freed: ring.len(),
        withheld,
        close_lines,
        close,
        blocks_before,
        blocks_after,
    }
}

/// Distinct [`CACHE_LINE_BYTES`]-byte lines `addresses` fall in.
fn lines_of(addresses: &[usize]) -> usize {
    let mut lines: Vec<usize> = addresses
        .iter()
        .map(|address| address / CACHE_LINE_BYTES)
        .collect();
    lines.sort_unstable();
    lines.dedup();
    lines.len()
}

/// The entity blocks of the touched list.
///
/// # Safety
/// As `density::totals`: the arena has not been reset, every touched block is
/// still mapped, and the owner's half of an entity block's header is read on
/// the thread that owns it.
unsafe fn walked_blocks(arena: &TraceScratchArena) -> Vec<WalkedBlock> {
    let mut walked = Vec::new();
    let mut array = arena.touched_head();
    while !array.is_null() {
        if unsafe { (*array).population } == Population::Slotted {
            walked.push(WalkedBlock {
                block: unsafe { (*array).block } as usize,
                deaths: 0,
            });
        }

        array = unsafe { (*array).next };
    }

    walked
}

/// The control S40.1's `every_collection_agrees` is here: seven collections
/// over an unchanged population, and the killing collection's own reading
/// taken before it changes anything.
///
/// A repeat of the killing collection does not exist — the population it
/// reads is the one it destroys — so the arm compared is the instant before
/// the first free, which is where the two arms are still identical.
fn the_control_holds(load: &DeathLoad) {
    let first = load.ordinary[0];
    assert!(
        first.density.slotted.blocks > 0,
        "the load touched at least one block"
    );
    assert_eq!(
        first.density.slotted.rows_met, MEMBERS as u64,
        "and met one row per component member"
    );

    for (index, reading) in load.ordinary.iter().enumerate().skip(1) {
        assert_eq!(
            (
                reading.density,
                reading.mark_resolutions,
                reading.trace_resolutions,
                reading.arena_blocks
            ),
            (
                first.density,
                first.mark_resolutions,
                first.trace_resolutions,
                first.arena_blocks
            ),
            "collection {} disagrees with the first",
            index + 1
        );
    }

    assert_eq!(
        (load.killing.density, load.killing.arena_blocks),
        (first.density, first.arena_blocks),
        "the killing collection read a different population before it killed it"
    );
}

/// The construction check the death count stands as.
fn the_deaths_are_the_ones_the_fixture_made(load: &DeathLoad) {
    let killing = &load.killing;
    assert_eq!(
        killing.withheld, killing.freed,
        "the free path withheld a different number of returns than the fixture freed"
    );
    assert_eq!(
        killing.blocks_after, killing.blocks_before,
        "the collection gave back every manager block it drew"
    );

    let in_blocks: u32 = killing.blocks.iter().map(|walked| walked.deaths).sum();
    assert_eq!(
        in_blocks as usize, killing.freed,
        "a death landed in a block the trace never touched"
    );
}

/// The minimum, median and maximum of a timed arm, in microseconds.
struct CloseStats {
    minimum: f64,
    median: f64,
    maximum: f64,
}

/// Reduce one arm's closes to [`CloseStats`].
fn reduce(closes: &[Duration]) -> CloseStats {
    let mut micros: Vec<f64> = closes
        .iter()
        .map(|close| close.as_nanos() as f64 / 1_000.0)
        .collect();
    micros.sort_by(|a, b| a.partial_cmp(b).expect("a duration is never NaN"));
    CloseStats {
        minimum: micros[0],
        median: micros[micros.len() / 2],
        maximum: micros[micros.len() - 1],
    }
}

/// One placement's row of the table `dev/BENCHMARKS.md` records.
fn report(load: &DeathLoad, closes: &[Duration]) {
    let killing = &load.killing;
    let stats = reduce(closes);
    let deaths = killing.freed;

    println!(
        "\n== class {} ({} slots a block), {MEMBERS} members, {} fillers between ==",
        load.class_bytes,
        slots_per_block(load.class_bytes),
        load.fillers_between
    );
    println!(
        "  blocks touched {}, arena blocks drawn {}, deaths {deaths} in {} of them",
        killing.blocks.len(),
        killing.arena_blocks,
        killing.blocks.iter().filter(|w| w.deaths > 0).count()
    );
    println!(
        "  the close reads {deaths} links in {} lines, every one of them a line \
         its own return reads",
        killing.close_lines
    );
    println!(
        "  ActiveTrace::drop over {} runs: min {:.1} us, median {:.1}, max {:.1}",
        closes.len(),
        stats.minimum,
        stats.median,
        stats.maximum
    );
}

/// Run one placement [`TIMED_RUNS`] times, check every run, and report the
/// first run's structure beside all of their closes.
///
/// The structure is the first run's rather than an agreement across runs
/// because [`the_control_holds`] already fixes it per run: eight collections
/// of one load read one density, and a run whose placement differed would
/// fail there rather than reach this report.
fn a_timed_arm(class_bytes: usize, fillers_between: usize) {
    let mut closes = Vec::with_capacity(TIMED_RUNS);
    let mut first = None;

    for _ in 0..TIMED_RUNS {
        let load = on_a_fresh_thread(move || a_killing_load(class_bytes, fillers_between));
        the_control_holds(&load);
        the_deaths_are_the_ones_the_fixture_made(&load);
        closes.push(load.killing.close);
        first.get_or_insert(load);
    }

    report(&first.expect("a timed arm runs at least once"), &closes);
}

/// The premise the line reading rests on: a slot's header and the word the
/// stack links it through share a cache line, at every size class the design
/// has.
///
/// Not a measurement and not ignored — it is one arithmetic statement about
/// the layout, and a size class that broke it would make the close's reads
/// lines of their own without failing anything else.
#[test]
fn the_close_reads_no_line_of_its_own() {
    for &class_bytes in crate::memory::heap::SIZE_CLASSES {
        let last_slot = crate::memory::block_pool::BLOCK_PAYLOAD - class_bytes;
        for base in [0, class_bytes, last_slot] {
            assert_eq!(
                base / CACHE_LINE_BYTES,
                (base + FREE_LIST_LINK_OFFSET) / CACHE_LINE_BYTES,
                "class {class_bytes} puts the stack's link in a line of its own"
            );
        }
    }
}

/// The dense arm: the component's members allocated back to back, at the two
/// classes [`DENSE_CLASSES`] names.
#[test]
#[ignore = "a measurement, recorded in dev/BENCHMARKS.md; run with --ignored"]
fn the_dense_arm_dies_inside_the_window() {
    let _g = test_guard();
    for class_bytes in DENSE_CLASSES {
        a_timed_arm(class_bytes, 0);
    }
}

/// The sparse arm: one component member per block at class 256, which is the
/// placement that spreads the close's returns over the most blocks.
#[test]
#[ignore = "a measurement, recorded in dev/BENCHMARKS.md; run with --ignored"]
fn the_sparse_arm_dies_inside_the_window() {
    let _g = test_guard();
    a_timed_arm(SPARSE_CLASS, slots_per_block(SPARSE_CLASS) - 1);
}
