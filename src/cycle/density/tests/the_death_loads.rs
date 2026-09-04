//! What a trace's sweep would walk against what its deaths would record
//! (`PLAN.md` S43.1).
//!
//! Ignored in the ordinary suite and run by hand:
//!
//! ```text
//! cargo test --lib density::tests::the_death_loads -- --ignored --nocapture
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
//! What the crate chooses, and what both prices are read off, is where the
//! heap put those entities: the blocks the trace touched, the two walk bounds
//! each of them carries, and how the deaths cluster across them.
//!
//! # The two prices, and they are counted rather than timed
//!
//! Neither side exists yet — S43.2 writes the mark and S43.4 sweeps it — so
//! there is nothing to time, and every figure below is arithmetic over
//! counted structure. Two units are reported because they disagree in
//! direction:
//!
//! - **cache lines.** The sweep reads one word per walked slot, and at a
//!   stride under 64 bytes that walk reads the block through. The chain
//!   writes eight records to a line and reads them back once at the replay.
//! - **memory operations.** The sweep is one load and one test per walked
//!   slot against
//!   [`defer_reuse_if_tracing`](crate::cycle::deferred_slot_reuse)'s
//!   documented three loads and two stores per death, plus the replay's own
//!   load per record.
//!
//! # Two bounds on the walk, and the production one is the wider
//!
//! `heap::for_each_entity_slot` walks `0..bump`, not `0..slots`, and this
//! fixture's blocks are young: a component of 381 members at class 32 leaves
//! the cursor at 381 of 2,040. The bump bound is what a walk over *this*
//! population would cost and the slot bound is what it costs once the block
//! has filled, since the cursor never retreats
//! (`crate::memory::heap::block_bump`). Both are reported per load.
//!
//! # What these loads may not claim
//!
//! The teardown is unbuilt — S36.3's guard, S36.4's destructors and S36.5's
//! sever and deferred drops are all open — so the frees below are the shape
//! of that path rather than the path. Three consequences, and the first two
//! pull in opposite directions:
//!
//! - **no tail.** A component's members here carry one property each and no
//!   external children, so no acyclic garbage dies behind them. Production
//!   deaths are therefore understated, which favours the chain.
//! - **every touched block holds a death.** A production trace touches blocks
//!   holding survivors alone, which the sweep pays for and the chain does not.
//!   That favours the sweep.
//! - **no destructor and no verdict.** A `__destruct` that allocates or
//!   resurrects moves the death set, and `cycle::validation` is what decides a
//!   component is garbage; here the fixture asserts it.

use super::*;

use super::the_loads::{COLLECTIONS, DESIGN_CLASSES, slots_per_block};
use crate::cycle::deferred_slot_reuse::{RETURNS_BASE_RECORDS, deferred_slot_count};
use crate::memory::block_pool::BLOCK_PAYLOAD;
use crate::memory::gc_metadata;
use crate::memory::heap::block_bump;
use crate::refcount::clear_candidate_bit;

/// The component size every load carries: the corpus's median closure, and
/// the size S40.1 reports its own widest readings at.
const MEMBERS: usize = 381;

/// A cache line, which the crate's own `LINE_SIZE` is not — that constant is
/// the 256-byte header line a block reserves.
const CACHE_LINE_BYTES: usize = 64;

/// Bytes one withheld return takes in the chain: an address, and the record
/// carries nothing beside it (`crate::cycle::records`).
const RECORD_BYTES: usize = size_of::<*mut u8>();

/// Memory operations the sweep's walk makes per slot: the load of the slot's
/// first word and the test on it.
const SWEEP_OPERATIONS_PER_SLOT: usize = 2;

/// The store S43.2 puts in a dead slot's own first word, which is what the
/// sweep design pays per death.
const MARK_OPERATIONS_PER_DEATH: usize = 1;

/// Memory operations one append makes with a window open and room in the
/// segment: three loads — the thread-local control line, the cursor and the
/// limit — and two stores (`crate::cycle::deferred_slot_reuse`,
/// `defer_reuse_if_tracing`).
const APPEND_OPERATIONS_PER_DEATH: usize = 5;

/// The replay's own load of the record at the window's close. What follows it
/// is the entry into `ll_free` that both designs make, so it is not counted
/// on either side.
const REPLAY_OPERATIONS_PER_DEATH: usize = 1;

/// One touched entity block, with the two bounds a per-slot walk of it could
/// run to and the deaths that landed in it.
#[derive(Clone, Copy)]
struct WalkedBlock {
    /// The block header's address, which is what a death's slot resolves to.
    block: usize,
    /// The block's index space, which is its slot count.
    slots: u32,
    /// Slots handed out at least once, and the bound
    /// `heap::for_each_entity_slot` uses.
    bump: u32,
    /// Component members freed inside the window whose slot is in this block.
    deaths: u32,
}

impl WalkedBlock {
    /// Bytes between two slots of this block, which is the size class.
    fn stride(&self) -> usize {
        BLOCK_PAYLOAD / self.slots as usize
    }

    /// Distinct cache lines a walk of `slots` slots from the block's base
    /// reads.
    ///
    /// Two regimes, and the stride decides which: at 64 bytes and above every
    /// slot's first word is a line of its own, and below it the walk reads a
    /// span of consecutive lines. Slots are contiguous and the base is
    /// line-aligned, so the last slot's offset gives the span.
    fn lines_for(&self, slots: u32) -> usize {
        if slots == 0 {
            return 0;
        }

        let span = (slots as usize - 1) * self.stride() / CACHE_LINE_BYTES + 1;
        span.min(slots as usize)
    }
}

/// One killing collection: the reading taken before the first free, and what
/// the deaths did.
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
    /// Returns the chain withheld, which equals `freed` or the run is void.
    withheld: usize,
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
/// still-open window, which is where a collection's own deaths happen.
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
    drop(active);

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
        blocks_before,
        blocks_after,
    }
}

/// The entity blocks of the touched list, with both walk bounds read off each
/// block's own header.
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
            let block = unsafe { (*array).block };
            walked.push(WalkedBlock {
                block: block as usize,
                slots: unsafe { (*array).row_count },
                bump: unsafe { block_bump(block) },
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

/// The construction check the death count stands as, and the budget the
/// window is asserted to keep.
fn the_deaths_are_the_ones_the_fixture_made(load: &DeathLoad) {
    let killing = &load.killing;
    assert_eq!(
        killing.withheld, killing.freed,
        "the free path withheld a different number of returns than the fixture freed"
    );
    assert!(
        killing.withheld <= RETURNS_BASE_RECORDS,
        "the chain grew past its region, so the window drew a manager block"
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

/// One load's row of the table `dev/BENCHMARKS.md` records.
fn report(load: &DeathLoad) {
    let killing = &load.killing;
    let blocks = killing.blocks.len();
    let slots: usize = killing.blocks.iter().map(|w| w.slots as usize).sum();
    let bump: usize = killing.blocks.iter().map(|w| w.bump as usize).sum();
    let slot_lines: usize = killing.blocks.iter().map(|w| w.lines_for(w.slots)).sum();
    let bump_lines: usize = killing.blocks.iter().map(|w| w.lines_for(w.bump)).sum();
    let deaths = killing.freed;

    // The chain's line is written at the append and read at the replay, so a
    // record's line is touched twice where a walked slot's is touched once.
    let record_lines = deaths.div_ceil(CACHE_LINE_BYTES / RECORD_BYTES);
    let chain_lines = 2 * record_lines;
    let chain_operations = deaths * (APPEND_OPERATIONS_PER_DEATH + REPLAY_OPERATIONS_PER_DEATH);

    println!(
        "\n== class {} ({} slots a block), {MEMBERS} members, {} fillers between ==",
        load.class_bytes,
        slots_per_block(load.class_bytes),
        load.fillers_between
    );
    println!(
        "  blocks touched {blocks}, arena blocks drawn {}, deaths {deaths} in {} of them",
        killing.arena_blocks,
        killing.blocks.iter().filter(|w| w.deaths > 0).count()
    );
    println!("  walk at the slot bound: {slots} slots, {slot_lines} lines");
    println!("  walk at the bump bound: {bump} slots, {bump_lines} lines");
    println!(
        "  chain at {deaths} deaths: {} bytes, {record_lines} lines written and read back \
         ({chain_lines} line touches)",
        deaths * RECORD_BYTES
    );
    println!(
        "  sweep operations at the slot bound {}, at the bump bound {}, chain operations {}",
        slots * SWEEP_OPERATIONS_PER_SLOT + deaths * MARK_OPERATIONS_PER_DEATH,
        bump * SWEEP_OPERATIONS_PER_SLOT + deaths * MARK_OPERATIONS_PER_DEATH,
        chain_operations
    );
    println!(
        "  break-even deaths by lines: {} at the slot bound, {} at the bump bound",
        break_even_by_lines(slot_lines),
        break_even_by_lines(bump_lines)
    );
    println!(
        "  break-even deaths by operations: {} at the slot bound, {} at the bump bound",
        break_even_by_operations(slots),
        break_even_by_operations(bump)
    );
    println!(
        "  deaths the load can reach at all: {slots} at the slot bound, {bump} at the bump bound"
    );
}

/// Deaths at which the chain's line touches reach the sweep's walk.
///
/// The sweep reads `walk_lines` lines once; the chain touches a line per
/// eight records, twice. They meet at `4 × walk_lines` deaths, so a walk that
/// reads more than one line for every four slots it visits stands above the
/// deaths its own blocks can hold.
fn break_even_by_lines(walk_lines: usize) -> usize {
    4 * walk_lines
}

/// Deaths at which the chain's memory operations reach the sweep's.
///
/// `2 × walked + deaths` against `6 × deaths`, which meet at `2 × walked / 5`.
fn break_even_by_operations(walked: usize) -> usize {
    let per_death =
        APPEND_OPERATIONS_PER_DEATH + REPLAY_OPERATIONS_PER_DEATH - MARK_OPERATIONS_PER_DEATH;
    walked * SWEEP_OPERATIONS_PER_SLOT / per_death
}

/// The dense arm: the component's members allocated back to back, over each
/// of the design's four size classes.
///
/// Class 16 is not here and its figure is arithmetic: `props_for(16)` is zero
/// properties, so a class-16 entity has no property to carry a ring edge and
/// no component can be built in it.
#[test]
#[ignore = "a measurement, recorded in dev/BENCHMARKS.md; run with --ignored"]
fn the_dense_arm_dies_inside_the_window() {
    let _g = test_guard();
    for class_bytes in DESIGN_CLASSES {
        let load = on_a_fresh_thread(move || a_killing_load(class_bytes, 0));
        the_control_holds(&load);
        the_deaths_are_the_ones_the_fixture_made(&load);
        report(&load);
    }
}

/// The sparse arm: one component member per block at class 256, which is the
/// placement the sweep pays most for and the chain least.
#[test]
#[ignore = "a measurement, recorded in dev/BENCHMARKS.md; run with --ignored"]
fn the_sparse_arm_dies_inside_the_window() {
    let _g = test_guard();
    let class_bytes = 256;
    let load =
        on_a_fresh_thread(move || a_killing_load(class_bytes, slots_per_block(class_bytes) - 1));
    the_control_holds(&load);
    the_deaths_are_the_ones_the_fixture_made(&load);
    report(&load);
}
