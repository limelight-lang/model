//! The calibration of the density reading: ten cases, each fixing an
//! answer by construction, one of which must contribute nothing.
//!
//! The instrument is code written for a measurement, so it is run
//! against known answers before it is trusted with an unknown one. One
//! anchor cannot do it: a walk that reported the reserved array instead
//! of the index space, or groups instead of rows, agrees with a single
//! reading and disagrees with the next.
//!
//! Every case runs on a thread of its own. The harness reuses threads
//! and a heap is per thread, so a block filled by an earlier case would
//! put occupants into the denominator that this case did not build.

use super::*;

use crate::class::ClassBuilder;
use crate::cycle::arena::RowLookup;
use crate::cycle::deferred_slot_reuse::ActiveTrace;
use crate::cycle::row::{EdgeTarget, RowKey, resolve_edge_target};
use crate::cycle::shadow;
use crate::cycle::trace::{TraceOutcome, trace_batch};
use crate::memory::arena::Arena;
use crate::memory::block_pool::BLOCK_MASK;
use crate::memory::block_pool::test_guard;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, ll_release, ll_retain};
use crate::test_support::{POOLED_FILLERS, prop_offset, store_prop, wide_class};

/// The row of slot `index` of an entity block, built the way
/// `row::resolve_edge_target` builds it from an entity's address.
fn slotted_row(block: *mut u8, index: u32) -> RowKey {
    RowKey {
        block: block as usize,
        index,
        population: Population::Slotted,
    }
}

/// The row `ensure_row` handed back, or a panic naming what it answered
/// instead. Every case here asks for a row it expects to get.
fn met(answer: RowLookup) -> *mut u32 {
    match answer {
        RowLookup::Ready { row, .. } => row,
        other => panic!("the arena refused a row: {other:?}"),
    }
}

/// Properties whose object is exactly one size class wide.
///
/// An object is a 16-byte header and 16 bytes per property, so a class
/// of `(bytes - 16) / 16` properties has `object_size` exactly `bytes`
/// and the heap serves it out of the class of that name.
const fn props_for(bytes: usize) -> usize {
    (bytes - 16) / 16
}

/// A class of `props` reference-carrying properties, named after itself
/// so two cases never share one.
fn a_class(name: &str, props: usize) -> *const crate::class::Class {
    let mut builder = ClassBuilder::new(name);
    // Named separately because `prop` borrows the name for the build.
    let names: Vec<String> = (0..props).map(|i| format!("p{i}")).collect();
    for name in &names {
        builder = builder.prop(name, true);
    }

    builder.build()
}

/// What one case built and has to take down.
struct Fixture {
    arena: Arena,
    entities: Vec<*mut Object>,
    /// Positions of `entities` linked into the ring and registered as
    /// candidates. The rest are occupants the trace never reaches.
    ring: Vec<usize>,
}

/// Allocate `count` entities of `class` on this thread's GC heap, link
/// the positions in `ring` into one cycle through property 0, and
/// register every ring member as a candidate.
///
/// A ring member ends at refcount one — its own reference released, the
/// ring's in-edge standing — so the trace subtracts one internal edge
/// and reads it as unreachable. An entity outside the ring keeps its
/// reference and is never registered, so it occupies a slot the trace
/// has no way to reach.
fn build(class: *const crate::class::Class, count: usize, ring: &[usize]) -> Fixture {
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let entities: Vec<*mut Object> = (0..count)
        .map(|_| unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) })
        .collect();

    for (position, &member) in ring.iter().enumerate() {
        let next = ring[(position + 1) % ring.len()];
        unsafe { store_prop(&mut arena, entities[member], prop_offset(0), entities[next]) };
    }

    for &member in ring {
        assert!(
            !unsafe { ll_release(entities[member] as *mut RcHeader) },
            "the ring's in-edge stands, so the release is not the last"
        );
    }

    Fixture {
        arena,
        entities,
        ring: ring.to_vec(),
    }
}

/// Break the ring and free everything.
///
/// The candidate bit is cleared by hand: nothing in production clears it
/// yet, and `ll_free`'s candidate arm would otherwise withhold every
/// ring member's slot for the life of the process
/// (`crate::refcount::clear_candidate_bit`).
fn tear_down(fixture: Fixture) {
    let Fixture {
        mut arena,
        entities,
        ring,
    } = fixture;
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

        for entity in entities {
            crate::refcount::clear_candidate_bit(entity as *mut RcHeader);
            ll_release(entity as *mut RcHeader);
            ll_object_die(entity);
        }
    }
}

/// What one collection leaves behind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Reading {
    /// Rows and groups met, per population.
    density: TraceDensity,
    /// Row resolutions the mark phase made: one per root the batch
    /// offered and one per counted child it descended into. An edge
    /// count is this less the roots.
    mark_resolutions: usize,
    /// Row resolutions both phases made together.
    trace_resolutions: usize,
    /// Blocks the arena drew from the memory manager past the thread's
    /// resident workspace. This is what a sparse trace costs the flat
    /// row form: an array is reserved for a block's whole index space
    /// whatever share of it the trace meets.
    arena_blocks: usize,
    /// Address of the newest row array of the touched list, or zero when
    /// no block was touched. Two collections of one thread land their
    /// arrays here, and a case about reused arena memory has to say so
    /// rather than assume it.
    newest_array: usize,
}

/// One collection over the candidates this thread holds, and the reading
/// it leaves.
///
/// The reading is taken before the arena's reset, which is what the
/// walk's contract requires, and the outcome is asserted first: a
/// refused trace subtracts an incomplete closure and its density would
/// be a number about a trace that did not happen.
fn collect() -> Reading {
    let mut active = ActiveTrace::open().expect("the pool funded the trace window");
    active.detach_candidates();

    // Zeroed here rather than at the case's start: opening the window
    // and detaching the chain dispatch over nothing, but a fixture built
    // before them does, and the count wanted is the trace's.
    let _ = crate::cycle::row::take_edge_dispatches();
    let (arena, batch) = active.rows_and_roots();
    assert_eq!(
        unsafe { trace_batch(arena, batch) },
        TraceOutcome::Complete,
        "the trace completed, so its rows are a whole closure"
    );

    let density = unsafe { totals(arena) };
    let newest_array = arena.touched_head() as usize;
    let arena_blocks = arena.blocks_held();
    Reading {
        density,
        arena_blocks,
        newest_array,
        mark_resolutions: crate::cycle::row::take_dispatches_in_mark_phase(),
        trace_resolutions: crate::cycle::row::take_edge_dispatches(),
    }
}

/// Run `case` on a thread whose heap no other case has touched.
fn on_a_fresh_thread<T: Send + 'static>(case: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served this thread"
        );
        case()
    })
    .join()
    .expect("the case finished")
}

// The loads the step records, which are ignored in the ordinary suite:
// the sparse arm allocates one full block per component member, and the
// gate runs this suite about a dozen times per commit.
mod the_loads;

// The same loads with a teardown inside the trace window, which is what
// prices the sweep S43 proposes against the chain it would delete.
mod the_death_loads;

/// Anchor A: a single self-referencing entity in an empty block.
///
/// The share is one row of the block's whole index space, and the group
/// reading is one group of the block's own. An instrument reporting the
/// reserved array rather than the rows the trace met answers the index
/// space here instead of one.
#[test]
fn one_traced_slot_reads_as_one_row_and_one_group() {
    let _g = test_guard();
    let density = on_a_fresh_thread(|| {
        let class = a_class("DensityAnchorA", props_for(64));
        let population = build(class, 1, &[0]);
        let density = collect().density;
        tear_down(population);
        density
    });

    let slotted = density.slotted;
    assert_eq!(slotted.blocks, 1, "one entity block was touched");
    assert_eq!(slotted.rows_met, 1, "and one row of it was met");
    assert_eq!(slotted.groups_met, 1, "which is one group");
    assert_eq!(
        slotted.index_space, 1020,
        "the 64-byte class holds 1,020 slots of a 65,280-byte payload"
    );
    assert_eq!(
        slotted.groups, 128,
        "and 1,020 rows round up to 1,024, which is 128 groups"
    );
    assert_eq!(slotted.occupied, 1, "the block holds the one entity");
    assert_eq!(
        slotted.rows_saturated, 0,
        "a refcount of one saturates no row"
    );
}

/// Anchor B: eight traced slots, laid out twice.
///
/// Consecutive they are one group; one per group they are eight. The two
/// readings share a row count and differ only in groups, which is what
/// separates an instrument that counts groups and reports rows from one
/// that does not.
#[test]
fn eight_traced_slots_are_one_group_consecutive_and_eight_spread() {
    let _g = test_guard();
    let consecutive = on_a_fresh_thread(|| {
        let class = a_class("DensityAnchorBConsecutive", props_for(64));
        let ring: Vec<usize> = (0..8).collect();
        let population = build(class, 64, &ring);
        let density = collect().density;
        tear_down(population);
        density
    });

    let spread = on_a_fresh_thread(|| {
        let class = a_class("DensityAnchorBSpread", props_for(64));
        let ring: Vec<usize> = (0..8).map(|group| group * 8).collect();
        let population = build(class, 64, &ring);
        let density = collect().density;
        tear_down(population);
        density
    });

    assert_eq!(consecutive.slotted.rows_met, 8);
    assert_eq!(spread.slotted.rows_met, 8, "both rings hold eight entities");
    assert_eq!(
        consecutive.slotted.groups_met, 1,
        "slots 0 to 7 share one group"
    );
    assert_eq!(
        spread.slotted.groups_met, 8,
        "and one slot per group meets eight"
    );
    assert_eq!(
        consecutive.slotted.occupied, 64,
        "the block holds the entities outside the ring too"
    );
    assert_eq!(spread.slotted.occupied, 64);
}

/// Anchor C: every slot of the block occupied and every one traced.
///
/// The only reading where the two denominators coincide, so a walk that
/// had swapped them passes every other anchor and fails this one. The
/// 8,192-byte class holds seven slots, which is what makes a whole block
/// cheap enough to fill.
#[test]
fn a_wholly_traced_block_reads_as_full_on_both_denominators() {
    let _g = test_guard();
    let density = on_a_fresh_thread(|| {
        let class = a_class("DensityAnchorC", props_for(8192));
        let ring: Vec<usize> = (0..7).collect();
        let population = build(class, 7, &ring);
        let density = collect().density;
        tear_down(population);
        density
    });

    let slotted = density.slotted;
    assert_eq!(
        slotted.index_space, 7,
        "the 8,192-byte class holds seven slots of a 65,280-byte payload"
    );
    assert_eq!(slotted.occupied, 7, "and the ring fills every one");
    assert_eq!(slotted.rows_met, 7, "and the trace met every one");
    assert_eq!(slotted.groups, 1, "seven rows round up to one group");
    assert_eq!(slotted.groups_met, 1);
}

/// Anchor D: a class whose slot count is not a whole number of groups.
///
/// The array reserves 256 rows for 255 slots, so a walk bounded at the
/// rounding rather than at the index space reads eight rows that are not
/// slots. Every other anchor sits on a class whose slots divide by
/// eight and none of them can see it.
#[test]
fn a_ragged_index_space_is_bounded_at_slots_and_not_at_the_rounding() {
    let _g = test_guard();
    let density = on_a_fresh_thread(|| {
        let class = a_class("DensityAnchorD", props_for(256));
        let population = build(class, 3, &[0, 1, 2]);
        let density = collect().density;
        tear_down(population);
        density
    });

    let slotted = density.slotted;
    assert_eq!(
        slotted.index_space, 255,
        "the 256-byte class holds 255 slots and the array reserves 256 rows"
    );
    assert_eq!(slotted.groups, 32, "the reserved 256 rows are 32 groups");
    assert_eq!(slotted.rows_met, 3);
    assert_eq!(slotted.groups_met, 1, "slots 0 to 2 share one group");
    assert_eq!(slotted.occupied, 3);
}

/// A second collection on the same thread reads its own rows and not the
/// first collection's.
///
/// The arena rewinds rather than zeroes, so the second collection's row
/// array stands on the first's bytes: same class, same row count, same
/// bump address. Rows in a group this collection never met therefore
/// carry the previous collection's colours, and the group bitmap is the
/// only thing that says so. Without the guard the second reading is the
/// first's.
///
/// The 4,096-byte class holds fifteen slots, which is two groups and one
/// full block — few enough to fill twice, and wide enough that a group
/// can be left unmet.
#[test]
fn a_second_collection_reads_past_no_stale_row_of_the_first() {
    let _g = test_guard();
    let (first, second) = on_a_fresh_thread(|| {
        let class = a_class("DensityAnchorReuse", props_for(4096));
        let whole: Vec<usize> = (0..15).collect();
        let filled = build(class, 15, &whole);
        let first = collect();

        // The first population stops being a root set: its records go
        // back and its bits come down, so the second collection detaches
        // the second ring alone. The entities stay alive, which keeps
        // their block full and sends the second population to a block of
        // its own.
        crate::cycle::queue::release_queue_segments();
        crate::memory::critical::drain_for_test();
        assert!(
            crate::cycle::queue::refill_spares(),
            "the growth path is funded again"
        );
        for &member in &filled.ring {
            unsafe {
                crate::refcount::clear_candidate_bit(filled.entities[member] as *mut RcHeader)
            };
        }

        let half: Vec<usize> = (0..8).collect();
        let partial = build(class, 15, &half);
        let second = collect();

        tear_down(partial);
        tear_down(filled);
        (first, second)
    });

    // The whole case rests on this: two different blocks, one array
    // address. Asserted rather than assumed, because an arena that drew
    // anything before the first touch would move the second array off
    // the first's bytes, and the case would then pass with the group bit
    // ignored.
    assert_eq!(
        first.newest_array, second.newest_array,
        "the second collection's array stands on the first collection's bytes"
    );

    let (first, second) = (first.density, second.density);
    assert_eq!(
        first.slotted.index_space, 15,
        "the 4,096-byte class holds 15"
    );
    assert_eq!(
        first.slotted.rows_met, 15,
        "the first ring filled the block"
    );
    assert_eq!(first.slotted.groups_met, 2, "which is both of its groups");
    assert_eq!(
        second.slotted.blocks, 1,
        "the second ring is a block of its own, the first still being full"
    );
    assert_eq!(
        second.slotted.rows_met, 8,
        "and only its own eight rows are met, whatever the reused bytes hold"
    );
    assert_eq!(
        second.slotted.groups_met, 1,
        "which is one of its two groups"
    );
}

/// The walk itself takes nothing from either allocator and moves
/// neither ledger figure.
///
/// The claim the module's own doc makes, and the one that keeps a
/// density from being a reading of the instrument: a walk that drew a
/// block would put its own bytes into the figures it reports.
#[test]
fn the_walk_draws_nothing_and_moves_no_ledger_figure() {
    let _g = test_guard();
    let (allocations, before, after) = on_a_fresh_thread(|| {
        let class = a_class("DensityWalkCost", props_for(64));
        let fixture = build(class, 16, &(0..16).collect::<Vec<usize>>());

        // The window and the trace run outside the bracket: what is
        // priced is the reading, and a collection's own draws are
        // S36.11's subject.
        let mut active = ActiveTrace::open().expect("the pool funded the trace window");
        active.detach_candidates();
        let (arena, batch) = active.rows_and_roots();
        assert_eq!(unsafe { trace_batch(arena, batch) }, TraceOutcome::Complete);

        crate::memory::gc_metadata::lower_thread_peak_to_current();
        let before = crate::memory::gc_metadata::thread_stats();
        let _ = crate::test_support::allocation_probe::take_allocations();
        let density = unsafe { totals(arena) };
        let allocations = crate::test_support::allocation_probe::take_allocations();
        let after = crate::memory::gc_metadata::thread_stats();
        assert_eq!(density.slotted.rows_met, 16, "the walk read the rows");

        drop(active);
        tear_down(fixture);
        (allocations, before, after)
    });

    assert_eq!(
        allocations,
        (0, 0),
        "the walk made no heap allocation and asked the pool for no block"
    );
    assert_eq!(
        after, before,
        "and moved no figure of the collection's ledger"
    );
}

/// A saturated row is met and counted apart.
///
/// A working count of [`shadow::COUNT_MAX`] means "at least this many
/// references", so `refcount - count` is not an in-edge count for it and
/// a pruned-edge simulation may not fold it in at zero. The rows are met
/// through `ensure_row` rather than through a trace: saturating one by
/// counting would need `2^30 - 1` references, and what is under test is
/// the reading rather than the arithmetic that produced it.
#[test]
fn a_saturated_row_is_met_and_counted_apart() {
    let _g = test_guard();
    let density = on_a_fresh_thread(|| {
        let class = a_class("DensitySaturated", props_for(64));
        let fixture = build(class, 2, &[]);
        let block = (fixture.entities[0] as usize & !BLOCK_MASK) as *mut u8;

        let mut arena = crate::cycle::testing::open_arena();
        met(unsafe { arena.ensure_row(slotted_row(block, 0), shadow::COUNT_MAX) });
        met(unsafe { arena.ensure_row(slotted_row(block, 1), 1) });
        let density = unsafe { totals(&arena) };
        arena.reset();
        tear_down(fixture);
        density
    });

    assert_eq!(density.slotted.rows_met, 2, "both rows were met");
    assert_eq!(
        density.slotted.rows_saturated, 1,
        "and one of the two carries a lower bound rather than a total"
    );
}

/// A retained block reports its survivor list as the index space, and
/// the share of it a trace meets is not fixed.
///
/// The second half is the point. A survivor list holds no free position,
/// so `occupied` equals `index_space` by construction — but only two of
/// the four survivors are met here, and the reading has to say so rather
/// than answer the full list.
#[test]
fn a_retained_block_reports_its_survivor_list_and_the_share_met_of_it() {
    let _g = test_guard();
    let (density, survivors) = on_a_fresh_thread(|| {
        let holder_class = a_class("DensityRetainedHolderCase", props_for(80));
        let member_class = a_class("DensityRetainedMemberCase", props_for(64));

        let mut arena = Arena::new();
        let mut context = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut context, holder_class, MemoryCategory::GcHeap) };
        let members: Vec<*mut Object> = (0..4)
            .map(|_| unsafe {
                new_constructed(&mut context, member_class, MemoryCategory::RequestArena)
            })
            .collect();

        // All four escape into the heap holder, so all four survive the
        // reset and their block turns retained.
        for (index, &member) in members.iter().enumerate() {
            unsafe { store_prop(&mut arena, holder, prop_offset(index as u32), member) };
        }

        unsafe { crate::promote::arena_reset_full(&mut arena) };
        let block = members[0] as usize & !BLOCK_MASK;
        // Asserted before anything is asked of the row dispatch: a case
        // that quietly built an arena block would pass on the wrong arm.
        let header = crate::memory::block_pool::BlockHeader::of_ptr(block as *const u8);
        assert_eq!(
            unsafe { crate::memory::block_pool::load_block_kind(&raw const (*header).kind) },
            crate::memory::block_pool::BLOCK_KIND_RETAINED,
            "the reset retained the members' block"
        );

        let survivors = unsafe { crate::memory::retained::survivor_list_copy(block) };
        assert_eq!(survivors.len(), 4, "one block holds all four");

        // Two of the four, so the reading is a share rather than a list.
        let mut arena_rows = crate::cycle::testing::open_arena();
        for &member in &members[..2] {
            let EdgeTarget::Tracked(row) =
                (unsafe { resolve_edge_target(member as *mut RcHeader) })
            else {
                panic!("a survivor of a retained block resolves to a row");
            };

            assert_eq!(row.population, Population::Retained);
            met(unsafe { arena_rows.ensure_row(row, 1) });
        }

        let density = unsafe { totals(&arena_rows) };
        arena_rows.reset();

        // The holder is the only reference the four survivors have, so
        // releasing it is what lets the retained block's count reach zero
        // and the block go back. Without this the case keeps a retained
        // block and an entity block for the life of the process, which
        // `-Zmiri-ignore-leaks` and every before/after pair in the crate
        // would both pass over.
        unsafe {
            assert!(ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }

        (density, survivors.len())
    });

    let retained = density.retained;
    assert_eq!(retained.blocks, 1, "one retained block was touched");
    assert_eq!(
        retained.index_space, survivors as u64,
        "and its index space is the survivor list, not a slot stride"
    );
    assert_eq!(
        retained.occupied, retained.index_space,
        "no survivor of this block has died, so the count word still \
         stands at the list's length"
    );
    assert_eq!(
        retained.rows_met, 2,
        "two of the four were met, and the reading is that share"
    );
    assert_eq!(density.slotted, PopulationDensity::default());
}

/// A large entity reports one row of one, out of its own block header.
///
/// The population with no array: it attaches a `RowArray` to the touched
/// list whose `row_count` is zero, and the row itself is a word of the
/// block's header. A walk that read the array's rows here would read
/// none and report the block as untouched.
#[test]
fn a_large_entity_reports_one_row_of_one() {
    let _g = test_guard();
    let density = on_a_fresh_thread(|| {
        let class = wide_class("DensityLargeEntity", POOLED_FILLERS, None);
        let mut arena = Arena::new();
        let mut context = LLContext { arena: &mut arena };
        let entity = unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) };

        let EdgeTarget::Tracked(row) = (unsafe { resolve_edge_target(entity as *mut RcHeader) })
        else {
            panic!("a large entity resolves to a row");
        };

        assert_eq!(row.population, Population::SingleEntity);
        let mut arena_rows = crate::cycle::testing::open_arena();
        met(unsafe { arena_rows.ensure_row(row, 1) });
        let density = unsafe { totals(&arena_rows) };
        arena_rows.reset();
        unsafe {
            assert!(ll_release(entity as *mut RcHeader));
            ll_object_die(entity);
        }

        density
    });

    let single = density.single_entity;
    assert_eq!(single.blocks, 1, "the entity's own block was touched");
    assert_eq!(single.index_space, 1, "which holds one row");
    assert_eq!(single.occupied, 1);
    assert_eq!(single.rows_met, 1, "and the trace met it");
    assert_eq!(single.groups, 1);
    assert_eq!(single.groups_met, 1);
    assert_eq!(density.slotted, PopulationDensity::default());
}

/// The negative anchor: a populated block no root reaches contributes
/// nothing.
///
/// Without it the walk could be summing over the thread's heap rather
/// than over the trace's touched list, and every share above would still
/// read plausibly.
#[test]
fn a_block_no_root_reaches_is_absent_from_the_reading() {
    let _g = test_guard();
    let density = on_a_fresh_thread(|| {
        let traced = a_class("DensityNegativeTraced", props_for(64));
        let untraced = a_class("DensityNegativeUntraced", props_for(128));
        // Built first, so its block is the older of the two and a walk
        // that ran off the heap would meet it before the traced one.
        let bystanders = build(untraced, 16, &[]);
        let population = build(traced, 1, &[0]);
        let density = collect().density;
        tear_down(population);
        tear_down(bystanders);
        density
    });

    let slotted = density.slotted;
    assert_eq!(slotted.blocks, 1, "only the ring's own block was touched");
    assert_eq!(slotted.rows_met, 1);
    assert_eq!(
        slotted.index_space, 1020,
        "and the denominator is that block's alone"
    );
    assert_eq!(density.retained, PopulationDensity::default());
    assert_eq!(density.single_entity, PopulationDensity::default());
}
