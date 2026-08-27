//! What a block gets the first time the trace reaches into it, and what
//! the second reach must not do to it. Every test here reads the row
//! back through the pointer `meet` answered with, because the row is the
//! only state a collection leaves anywhere: an aborted mark has to be
//! undoable by nulling pointers alone.

use super::*;
use crate::class::ClassBuilder;
use crate::cycle::row::{Population, Row};
use crate::cycle::shadow::{self, Colour};
use crate::memory::arena::Arena;
use crate::memory::block_pool::{BLOCK_MASK, FORCE_OOM, test_guard};
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, ll_release};
use crate::test_support::{RUN_FILLERS, store_prop, wide_class};

/// The first touch does three things and a test that checked one would
/// pass on two thirds of an implementation: the block gets an array, the
/// array is stamped into the block's header, and the block is enrolled
/// for the sweep. The row itself comes back met, carrying the refcount
/// the caller read.
#[test]
fn a_first_touch_reserves_rows_stamps_the_block_and_enrols_it() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let (mut heap, slot, block) = an_entity_block();

    let mut arena = ShadowArena::new();
    assert!(
        unsafe { crate::memory::heap::block_shadow(block) }.is_null(),
        "no collection has touched this block"
    );

    let (row, first_reach) = met_first(unsafe { arena.meet(slot_row(block, 0), 7) });
    assert!(first_reach, "the collection had not seen this entity");

    let stamped = unsafe { crate::memory::heap::block_shadow(block) };
    assert!(!stamped.is_null(), "the block was stamped");
    assert_eq!(
        stamped as usize + size_of::<shadow::RowArray>(),
        row as usize,
        "and the row is the first of that array's rows"
    );
    assert_eq!(arena.touched_blocks(), 1, "and the block is enrolled once");
    assert_eq!(shadow::colour(unsafe { *row }), Colour::Met);
    assert_eq!(unsafe { shadow::count(*row) }, 7, "met at the refcount");

    arena.reset();
    unsafe { heap.free(slot) };
    crate::memory::critical::drain_for_test();
}

/// A second edge into the same entity subtracts from what the first left
/// there. Re-initialising from the refcount is the defect this pins:
/// the trace would then subtract one edge from a full count for every
/// edge it found, and a ring of two would never read zero.
#[test]
fn a_second_reach_leaves_the_working_count_alone() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let (mut heap, slot, block) = an_entity_block();

    let mut arena = ShadowArena::new();
    let row = met(unsafe { arena.meet(slot_row(block, 0), 3) });

    // What the mark does with the row it is handed.
    assert_eq!(unsafe { shadow::subtract(row, 1) }, 2);

    let (again, first_reach) = met_first(unsafe { arena.meet(slot_row(block, 0), 3) });
    assert_eq!(again, row, "the same slot resolves to the same row");
    assert!(
        !first_reach,
        "the second reach must not read as a first one, or the mark descends again"
    );
    assert_eq!(
        unsafe { shadow::count(*again) },
        2,
        "the second reach met an entity that was already met"
    );
    assert_eq!(arena.touched_blocks(), 1, "and enrolled the block once");

    arena.reset();
    unsafe { heap.free(slot) };
    crate::memory::critical::drain_for_test();
}

/// The reserved colour, and the case that needs it. A member of a
/// condemned component reads count zero, and so does a slot the trace
/// never reached; without a code that says "met", the second reach of a
/// condemned entity would read it as untouched and re-initialise it from
/// the refcount, acquitting the component.
#[test]
fn a_condemned_zero_row_is_told_from_an_untouched_slot() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let (mut heap, slot, block) = an_entity_block();

    let mut arena = ShadowArena::new();
    let row = met(unsafe { arena.meet(slot_row(block, 0), 1) });
    unsafe { *row = shadow::compose(Colour::Condemned, 0) };

    let again = met(unsafe { arena.meet(slot_row(block, 0), 1) });
    assert_eq!(
        shadow::colour(unsafe { *again }),
        Colour::Condemned,
        "a met row keeps its colour"
    );
    assert_eq!(unsafe { shadow::count(*again) }, 0, "and its count");

    arena.reset();
    unsafe { heap.free(slot) };
    crate::memory::critical::drain_for_test();
}

/// One array per block, one row per slot. Two slots of one block share
/// the array and take different rows: an implementation that reserved
/// per entity would pass every test above.
#[test]
fn two_slots_of_one_block_share_its_array_and_take_their_own_rows() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let (mut heap, first, block) = an_entity_block();
    let second = heap.alloc(crate::memory::heap::SIZE_CLASSES[0]);
    assert_eq!(
        (second as usize) & !BLOCK_MASK,
        block as usize,
        "the fixture's second slot is in the same block"
    );

    let mut arena = ShadowArena::new();
    let first_row = met(unsafe { arena.meet(slot_row(block, 0), 4) });
    let second_row = met(unsafe { arena.meet(slot_row(block, 1), 9) });

    assert_ne!(first_row, second_row);
    assert_eq!(arena.touched_blocks(), 1, "one array served both");
    assert_eq!(unsafe { shadow::count(*first_row) }, 4);
    assert_eq!(unsafe { shadow::count(*second_row) }, 9);

    arena.reset();
    unsafe { heap.free(first) };
    unsafe { heap.free(second) };
    crate::memory::critical::drain_for_test();
}

/// The retained population's index space is its object index, so its
/// array is sized by the occupant count rather than by a stride the
/// block does not have. Two survivors of different sizes land in one
/// block, and each takes the row its position in that index names.
#[test]
fn a_retained_block_gets_one_row_for_each_occupant() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let narrow = ClassBuilder::new("ShadowNarrow").prop("x", true).build();
    let wide = ClassBuilder::new("ShadowWide")
        .prop("a", true)
        .prop("b", true)
        .prop("c", true)
        .prop("d", true)
        .build();
    let holder_class = ClassBuilder::new("ShadowHolder")
        .prop("first", true)
        .prop("second", true)
        .build();

    let mut request = Arena::new();
    let mut ctx = LLContext {
        arena: &mut request,
    };
    let holder = unsafe { new_constructed(&mut ctx, holder_class, MemoryCategory::GcHeap) };
    let small = unsafe { new_constructed(&mut ctx, narrow, MemoryCategory::RequestArena) };
    let large = unsafe { new_constructed(&mut ctx, wide, MemoryCategory::RequestArena) };
    let block = small as usize & !BLOCK_MASK;
    assert_eq!(large as usize & !BLOCK_MASK, block, "one bump filled both");

    unsafe { store_prop(&mut request, holder, 16, small) };
    unsafe { store_prop(&mut request, holder, 32, large) };
    unsafe { crate::promote::arena_reset_full(&mut request) };

    let occupants = crate::memory::retained::occupant_count(block)
        .expect("the reset registered the block's occupant index");

    let mut arena = ShadowArena::new();
    let mut rows = Vec::new();
    for survivor in [small, large] {
        let position = crate::memory::retained::occupant_index(block, survivor as usize)
            .expect("a survivor is named by its block's index");
        let row = Row {
            block,
            index: position as u32,
            population: Population::Retained,
        };
        rows.push(met(unsafe { arena.meet(row, 1) }));
    }

    assert_ne!(rows[0], rows[1], "two occupants, two rows");
    assert_eq!(
        arena.touched_blocks(),
        1,
        "one array served both occupants, and the block is enrolled once"
    );
    let array =
        unsafe { crate::memory::heap::block_shadow(block as *mut u8) } as *mut shadow::RowArray;
    assert_eq!(
        unsafe { (*array).slots } as usize,
        occupants,
        "the array is sized by the index the rows are keyed by"
    );

    arena.reset();
    unsafe {
        assert!(ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    crate::memory::critical::drain_for_test();
}

/// A large entity has no array: its row is a word of its own block
/// header, and that word is where its met flag lives too. What the sweep
/// owes it is that word back at zero — a stale row would leave the next
/// collection subtracting from a count this one left behind, and the
/// entity would be condemned live on the first ring it joins.
#[test]
fn a_large_entity_is_met_in_its_own_header_and_swept_from_it() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let class = wide_class("ShadowSole", RUN_FILLERS, None);
    let mut request = Arena::new();
    let mut ctx = LLContext {
        arena: &mut request,
    };
    let entity: *mut Object = unsafe { new_constructed(&mut ctx, class, MemoryCategory::GcHeap) };
    let block = (entity as usize & !BLOCK_MASK) as *mut u8;
    let word = unsafe { crate::memory::large_entity::shadow_row(block) };
    assert_eq!(
        shadow::colour(unsafe { *word }),
        Colour::Untouched,
        "commissioning leaves the row untouched"
    );

    let mut arena = ShadowArena::new();
    let row = met(unsafe {
        arena.meet(
            Row {
                block: block as usize,
                index: 0,
                population: Population::Sole,
            },
            5,
        )
    });
    assert_eq!(row, word, "the row is the header word itself");
    assert_eq!(unsafe { shadow::count(*row) }, 5);
    assert_eq!(
        arena.touched_blocks(),
        1,
        "a block with no array is enrolled all the same"
    );

    let (_, first_reach) = met_first(unsafe {
        arena.meet(
            Row {
                block: block as usize,
                index: 0,
                population: Population::Sole,
            },
            5,
        )
    });
    assert!(!first_reach, "the second reach is not a first one");
    assert_eq!(
        arena.touched_blocks(),
        1,
        "and the second reach enrols it no further"
    );

    arena.sweep_touched();
    assert_eq!(
        shadow::colour(unsafe { *word }),
        Colour::Untouched,
        "the sweep gives the header word back untouched"
    );

    arena.reset();
    unsafe {
        assert!(ll_release(entity as *mut RcHeader));
        ll_object_die(entity);
    }

    crate::memory::critical::drain_for_test();
}

/// A block reaches the retained kind carrying whatever its previous life
/// left in the collector's line, and an entity block's life leaves an
/// array pointer there — the collection that stamped it nulls it again,
/// but the block is stamped retained by a reset that never asked. So the
/// reset publishes the word null before the kind says the block can be
/// traced; without that, the retained block's first touch would take a
/// dead collection's array for its own.
///
/// The stale value is written by hand, into the block while it is still
/// the arena's: a test that waited for the pool to hand back a dirty
/// block would be reading an accident.
#[test]
fn retention_publishes_a_block_with_no_rows() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let class = ClassBuilder::new("ShadowStale").prop("x", true).build();

    let mut request = Arena::new();
    let mut ctx = LLContext {
        arena: &mut request,
    };
    let holder = unsafe { new_constructed(&mut ctx, class, MemoryCategory::GcHeap) };
    let survivor = unsafe { new_constructed(&mut ctx, class, MemoryCategory::RequestArena) };
    let block = (survivor as usize & !BLOCK_MASK) as *mut u8;
    unsafe { store_prop(&mut request, holder, 16, survivor) };

    let stale = 0xDEAD_0000usize as *mut u8;
    unsafe { crate::memory::heap::set_block_shadow(block, stale) };
    assert_eq!(unsafe { crate::memory::heap::block_shadow(block) }, stale);

    unsafe { crate::promote::arena_reset_full(&mut request) };

    assert!(
        crate::memory::retained::occupant_count(block as usize).is_some(),
        "the fixture's survivor turned its block retained"
    );
    assert!(
        unsafe { crate::memory::heap::block_shadow(block) }.is_null(),
        "a retained block is published with no rows"
    );

    unsafe {
        assert!(ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    crate::memory::critical::drain_for_test();
}

/// The refusal at a block's first touch reaches the caller as an abort
/// rather than as a row, and the rows already written are left alone: an
/// abort undoes nothing but the pointers the sweep nulls. Driven on the
/// strided arm, whose refusal path the other two share — the retained
/// arm differs only in where its row count comes from.
#[test]
fn a_refusal_on_the_second_block_leaves_the_first_intact() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let (mut first_heap, first_slot, first) = an_entity_block();
    let (mut second_heap, second_slot, second) = an_entity_block();

    let mut arena = ShadowArena::new();
    let row = met(unsafe { arena.meet(slot_row(first, 0), 2) });
    unsafe { *row = shadow::compose(Colour::Met, 1) };

    // The arena's current block still has room, so the refusal has to be
    // driven from a block boundary: shut the door, then spend the block
    // in hand.
    FORCE_OOM.store(true, Ordering::Relaxed);
    while !arena.alloc(1024).is_null() {}

    assert_eq!(
        unsafe { arena.meet(slot_row(second, 0), 2) },
        Met::Refused,
        "the second block's rows have nowhere to come from"
    );
    FORCE_OOM.store(false, Ordering::Relaxed);

    assert_eq!(
        unsafe { shadow::count(*row) },
        1,
        "and the abort leaves the rows already written alone"
    );
    assert_eq!(arena.touched_blocks(), 1);

    arena.reset();
    for block in [first, second] {
        assert!(unsafe { crate::memory::heap::block_shadow(block) }.is_null());
    }

    unsafe { first_heap.free(first_slot) };
    unsafe { second_heap.free(second_slot) };
    crate::memory::critical::drain_for_test();
}

/// A block held for a payload and nothing else has a registry entry and
/// no object index, so an edge into it cannot be placed. The trace keeps
/// the referent alive instead of guessing a row: `Met::Unplaced` is the
/// same conservative answer `row::edge_to` gives an address the index
/// does not name.
#[test]
fn an_edge_into_a_block_with_no_object_index_is_unplaced() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let (mut heap, slot, block) = an_entity_block();

    // A pin without a `register`, which is the state a reset is in
    // between refusing to carry a payload out and building the index.
    crate::memory::retained::pin(block as usize);
    assert!(
        crate::memory::retained::occupant_count(block as usize).is_none(),
        "a pinned block carries no occupant index"
    );

    let mut arena = ShadowArena::new();
    let answer = unsafe {
        arena.meet(
            Row {
                block: block as usize,
                index: 0,
                population: Population::Retained,
            },
            1,
        )
    };
    assert_eq!(answer, Met::Unplaced);
    assert!(
        unsafe { crate::memory::heap::block_shadow(block) }.is_null(),
        "an edge it cannot place reserves nothing"
    );
    assert_eq!(arena.touched_blocks(), 0);

    arena.reset();
    assert!(crate::memory::retained::payload_freed(block as usize));
    unsafe { heap.free(slot) };
    crate::memory::critical::drain_for_test();
}

/// Two collections over one block, which is the state the sweep exists
/// to leave behind. The second one must meet the entity at its refcount
/// again: a row that survived its collection would carry a count the
/// first trace had already subtracted from, and the entity would look
/// condemned on evidence nobody gathered.
#[test]
fn a_second_collection_meets_a_slotted_block_at_the_refcount_again() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let (mut heap, slot, block) = an_entity_block();

    let mut first = ShadowArena::new();
    let row = met(unsafe { first.meet(slot_row(block, 0), 6) });
    assert_eq!(unsafe { shadow::subtract(row, 4) }, 2);
    first.sweep_touched();
    first.reset();

    let mut second = ShadowArena::new();
    let (row, first_reach) = met_first(unsafe { second.meet(slot_row(block, 0), 6) });
    assert!(first_reach, "the block came back untouched");
    assert_eq!(
        unsafe { shadow::count(*row) },
        6,
        "the second collection starts from the refcount, not from the first's arithmetic"
    );

    second.reset();
    unsafe { heap.free(slot) };
    crate::memory::critical::drain_for_test();
}

/// The bound, driven from an entity rather than from a constant: a
/// refcount above what thirty bits hold meets at the bound, and the row
/// says "at least this many" from then on. The count is written into the
/// header directly because reaching `2^30` references by retaining takes
/// longer than a test may run, and what the row reads is the only thing
/// under test.
#[test]
fn an_entity_referenced_past_the_field_is_met_at_the_bound() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let class = ClassBuilder::new("ShadowSaturated").prop("x", true).build();
    let mut request = Arena::new();
    let mut ctx = LLContext {
        arena: &mut request,
    };
    let entity: *mut Object = unsafe { new_constructed(&mut ctx, class, MemoryCategory::GcHeap) };
    let header = entity as *mut RcHeader;
    let block = (entity as usize & !BLOCK_MASK) as *mut u8;

    let held = shadow::COUNT_MAX as u64 + 8;
    unsafe { crate::refcount::set_header_refcount(header, held as u32) };
    let refcount = unsafe { crate::refcount::header_refcount(header) };
    assert!(
        refcount > shadow::COUNT_MAX,
        "the fixture is past the bound"
    );

    let mut arena = ShadowArena::new();
    let row = met(unsafe {
        arena.meet(
            slot_row(
                block,
                crate::memory::heap::entity_slot_index(entity as *mut u8),
            ),
            refcount,
        )
    });
    assert!(
        shadow::is_saturated(unsafe { *row }),
        "the row holds a floor rather than a total"
    );

    // Every edge the trace could find, and the entity is still live: the
    // subtraction cannot walk a floor down to zero.
    for _ in 0..4 {
        assert_eq!(
            unsafe { shadow::subtract(row, 1_000_000) },
            shadow::COUNT_MAX
        );
    }

    assert!(shadow::count(unsafe { *row }) > 0, "conservatively live");

    arena.reset();
    unsafe { crate::refcount::set_header_refcount(header, 1) };
    unsafe {
        assert!(ll_release(header));
        ll_object_die(entity);
    }

    crate::memory::critical::drain_for_test();
}

/// What a first touch costs in bytes written, counted on the collector's
/// own path. The block is the widest one there is — 4080 slots at the
/// smallest size class — so its array reserves 16 320 bytes of rows, and
/// the figure this test pins is two orders below that: the prologue, the
/// bitmap, and one group per group the trace reaches.
///
/// Why the probe is inside the crate rather than a benchmark beside it:
/// `dev/BENCHMARKS.md`, "what a block's first touch writes".
#[test]
fn a_first_touch_writes_the_bitmap_and_the_groups_it_reaches() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();
    let (mut heap, slot, block) = an_entity_block();

    let slots = unsafe { crate::memory::heap::collector_block_slots(block) };
    assert_eq!(slots, 4080, "the fixture takes the widest block there is");

    let prologue = size_of::<shadow::RowArray>();
    // One bit per group, rounded up to a byte — computed here rather than
    // asked of the module, so the test does not agree with itself.
    let bitmap = (slots as usize / shadow::GROUP as usize).div_ceil(8);
    assert_eq!(bitmap, 64, "510 groups fit 64 bytes of bitmap");
    let group = size_of::<u8>() + shadow::GROUP as usize * size_of::<u32>();

    let before = shadow::written_bytes();
    let mut arena = ShadowArena::new();
    met(unsafe { arena.meet(slot_row(block, 0), 1) });
    assert_eq!(
        shadow::written_bytes() - before,
        prologue + bitmap + group,
        "the block's first touch writes its head, its bitmap and one group"
    );

    met(unsafe { arena.meet(slot_row(block, 7), 1) });
    assert_eq!(
        shadow::written_bytes() - before,
        prologue + bitmap + group,
        "a second slot of the same group writes nothing further"
    );

    met(unsafe { arena.meet(slot_row(block, 8), 1) });
    let written = shadow::written_bytes() - before;
    assert_eq!(
        written,
        prologue + bitmap + 2 * group,
        "and a slot of the next group writes that group"
    );

    // The figure the design is written against: what a touched block
    // costs follows the bitmap and the trace, not the block's width.
    let greedy = slots as usize * size_of::<u32>();
    assert_eq!(greedy, 16_320);
    assert!(
        written * 50 < greedy,
        "{written} bytes written against {greedy} the array holds"
    );

    arena.reset();
    unsafe { heap.free(slot) };
    crate::memory::critical::drain_for_test();
}
