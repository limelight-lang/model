use super::*;

use crate::cycle::arena::TraceScratchArena;
use crate::cycle::row::{EdgeTarget, resolve_edge_target};
use crate::cycle::shadow;
use crate::memory::Arena;
use crate::memory::block_pool::{
    BLOCK_KIND_FREE, BLOCK_KIND_RETAINED, BlockHeader, force_oom, test_guard,
};
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{EntityKind, MemoryCategory, RcHeader};

const ENTITY_SIZE: usize = 64;

unsafe fn dead_entity(slot: *mut u8) -> *mut RcHeader {
    let header = slot as *mut RcHeader;
    unsafe {
        header.write(RcHeader::new(
            MemoryCategory::GcHeap,
            EntityKind::Object.to_flags(),
        ));
        crate::refcount::set_header_refcount(header, 0);
    }
    header
}

unsafe fn live_entity(slot: *mut u8, count: u32) -> *mut RcHeader {
    let header = unsafe { dead_entity(slot) };
    unsafe { crate::refcount::set_header_refcount(header, count) };
    header
}

fn met(answer: crate::cycle::arena::RowLookup) -> *mut u32 {
    match answer {
        crate::cycle::arena::RowLookup::Ready { row, .. } => row,
        other => panic!("the arena refused a row: {other:?}"),
    }
}

unsafe fn ensure_row(arena: &mut TraceScratchArena, entity: *mut RcHeader, count: u32) -> *mut u32 {
    let EdgeTarget::Tracked(row) = (unsafe { resolve_edge_target(entity) }) else {
        panic!("the entity heap did not resolve to a shadow row");
    };
    met(unsafe { arena.ensure_row(row, count) })
}

/// Promote one arena object in place so its slot belongs to the retained
/// population and one heap holder owns its last reference.
unsafe fn retained_survivor() -> (Arena, *mut Object, *mut RcHeader, *mut BlockHeader) {
    let survivor_class = crate::class::ClassBuilder::new("RetainedTraceMember").build();
    let holder_class = crate::class::ClassBuilder::new("RetainedTraceHolder")
        .prop("member", true)
        .build();
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let holder = unsafe { new_constructed(&mut context, holder_class, MemoryCategory::GcHeap) };
    let survivor =
        unsafe { new_constructed(&mut context, survivor_class, MemoryCategory::RequestArena) };
    unsafe { crate::test_support::store_prop(&mut arena, holder, 16, survivor) };
    let block = BlockHeader::of_ptr(survivor as *const u8);
    unsafe { crate::promote::arena_reset_full(&mut arena) };
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_RETAINED
    );
    (arena, holder, survivor as *mut RcHeader, block)
}

/// Red without the trace window: the free-list is LIFO, so the allocation below
/// receives `dead` and `ensure_row` returns the row already initialised from
/// the dead occupant's count zero.
#[test]
fn a_reused_slot_cannot_inherit_the_dead_occupants_row() {
    let _guard = test_guard();
    let dead = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!dead.is_null());
    let dead = unsafe { dead_entity(dead) };

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), dead, 0) };
    assert!(!row.is_null());
    assert_eq!(unsafe { shadow::count(*row) }, 0);

    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    assert_eq!(deferred_slot_count(), 1);

    let fresh = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!fresh.is_null());
    assert_ne!(
        fresh, dead as *mut u8,
        "the traced slot was reused mid-window"
    );
    let fresh = unsafe { live_entity(fresh, 7) };
    let fresh_row = unsafe { ensure_row(window.arena(), fresh, 7) };
    assert_eq!(unsafe { shadow::count(*fresh_row) }, 7);

    unsafe { crate::refcount::set_header_refcount(fresh, 0) };
    unsafe { crate::memory::stdapi::ll_free(fresh as *mut u8) };
    drop(window);
    assert_eq!(deferred_slot_count(), 0);
}

#[test]
fn the_queue_window_may_close_before_the_trace_window() {
    let _guard = test_guard();
    let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!slot.is_null());
    let header = unsafe { dead_entity(slot) };
    unsafe {
        crate::refcount::update_header_flags(header, |flags| flags | crate::refcount::CANDIDATE_BIT)
    };

    let window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { crate::memory::stdapi::ll_free(slot) };
    assert_eq!(
        deferred_slot_count(),
        0,
        "the queue entry is the first record"
    );

    unsafe { crate::refcount::clear_candidate_bit(header) };
    unsafe { crate::memory::stdapi::ll_free(slot) };
    assert_eq!(
        deferred_slot_count(),
        1,
        "the collection takes over the return"
    );

    let other = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert_ne!(other, slot, "one closed window released through the other");

    drop(window);
    let reused = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert_eq!(reused, slot, "the last window's close returned the slot");
    unsafe { dead_entity(reused) };
    unsafe { crate::memory::stdapi::ll_free(reused) };
    unsafe { dead_entity(other) };
    unsafe { crate::memory::stdapi::ll_free(other) };
}

#[test]
fn the_trace_window_may_close_before_the_queue_window() {
    let _guard = test_guard();
    let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!slot.is_null());
    let header = unsafe { dead_entity(slot) };
    unsafe {
        crate::refcount::update_header_flags(header, |flags| flags | crate::refcount::CANDIDATE_BIT)
    };

    let window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { crate::memory::stdapi::ll_free(slot) };
    assert_eq!(
        deferred_slot_count(),
        0,
        "the queue entry is the first record"
    );
    drop(window);
    assert_eq!(
        deferred_slot_count(),
        0,
        "the trace close leaves the queue entry standing"
    );

    let other = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert_ne!(other, slot, "one closed window released through the other");

    unsafe { crate::refcount::clear_candidate_bit(header) };
    unsafe { crate::memory::stdapi::ll_free(slot) };
    let reused = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert_eq!(reused, slot, "the last window's close returned the slot");
    unsafe { dead_entity(reused) };
    unsafe { crate::memory::stdapi::ll_free(reused) };
    unsafe { dead_entity(other) };
    unsafe { crate::memory::stdapi::ll_free(other) };
}

#[test]
fn a_retained_blocks_last_occupant_waits_for_the_trace_row() {
    let _guard = test_guard();
    let (_arena, holder, survivor, block) = unsafe { retained_survivor() };
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), survivor, 1) };
    assert_eq!(unsafe { shadow::count(*row) }, 1);

    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
    assert_eq!(unsafe { crate::refcount::header_refcount(survivor) }, 0);
    assert_eq!(
        deferred_slot_count(),
        2,
        "the retained survivor and its heap holder both park"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_RETAINED,
        "the last occupant returned the whole block under its trace row"
    );

    drop(window);
    assert_eq!(deferred_slot_count(), 0);
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the row sweep precedes the last occupant's replayed return"
    );
}

#[test]
fn a_retained_blocks_last_occupant_waits_for_its_queue_entry() {
    let _guard = test_guard();
    let (_arena, holder, survivor, block) = unsafe { retained_survivor() };
    unsafe {
        // Stand in for the entry a previous non-final decrement wrote. The
        // entity is live when the bit goes up; its holder's death below is the
        // later final decrement.
        crate::refcount::update_header_flags(survivor, |flags| {
            flags | crate::refcount::CANDIDATE_BIT
        });
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_RETAINED,
        "the entry let the last occupant return its whole block"
    );

    unsafe {
        crate::refcount::clear_candidate_bit(survivor);
        crate::memory::stdapi::ll_free(survivor as *mut u8);
    }
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE
    );
}

#[test]
fn a_pooled_large_entity_waits_for_its_header_row() {
    let _guard = test_guard();
    let entity = crate::memory::large_entity::alloc(crate::memory::heap::MAX_SMALL + 16);
    assert!(!entity.is_null());
    let entity = unsafe { dead_entity(entity) };
    let block = BlockHeader::of_ptr(entity as *const u8);
    let kind = unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) };
    assert_eq!(kind, crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE);

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), entity, 0) };
    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        kind,
        "the pooled block returned while its header row was live"
    );

    drop(window);
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE
    );
}

#[test]
fn an_os_direct_large_entity_waits_for_its_header_row() {
    let _guard = test_guard();
    let entity = crate::memory::large_entity::alloc(crate::memory::block_pool::BLOCK_PAYLOAD + 1);
    assert!(!entity.is_null());
    let entity = unsafe { dead_entity(entity) };
    let block = BlockHeader::of_ptr(entity as *const u8) as usize;
    assert!(crate::memory::large_entity::snapshot().contains(&block));

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), entity, 0) };
    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
    assert!(
        crate::memory::large_entity::snapshot().contains(&block),
        "the run was unregistered and unmapped while its header row was live"
    );

    drop(window);
    assert!(
        !crate::memory::large_entity::snapshot().contains(&block),
        "closing the trace replayed the OS-direct return"
    );
}

/// Blocks the memory manager reports as collection's. Stable for the duration
/// of a `test_guard`, which every test asserting on it holds.
fn gc_blocks() -> usize {
    crate::memory::gc_metadata::stats().current_blocks()
}

#[test]
fn a_trace_window_allocates_nothing_through_the_global_allocator() {
    let _guard = test_guard();
    let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!slot.is_null());
    let dead = unsafe { dead_entity(slot) };

    let _ = crate::test_support::allocation_probe::take_allocations();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), dead, 0) };
    assert!(!row.is_null());
    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    assert_eq!(deferred_slot_count(), 1);
    drop(window);

    let (heap, _pool) = crate::test_support::allocation_probe::take_allocations();
    assert_eq!(
        heap, 0,
        "the trace window reached the global allocator: open, withhold, \
         replay and close are all manager-backed"
    );
}

#[test]
fn the_window_draws_one_manager_block_and_the_withheld_return_draws_none() {
    let _guard = test_guard();
    let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!slot.is_null());
    let dead = unsafe { dead_entity(slot) };

    let held_before = gc_blocks();
    let _ = crate::test_support::allocation_probe::take_allocations();
    let window = ActiveTrace::open().expect("the pool funds the trace window");
    let (heap, pool) = crate::test_support::allocation_probe::take_allocations();
    assert_eq!((heap, pool), (0, 1), "the chain's first block, and only it");
    assert_eq!(gc_blocks(), held_before + 1);

    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    let (heap, pool) = crate::test_support::allocation_probe::take_allocations();
    assert_eq!(
        (heap, pool),
        (0, 0),
        "the withheld return asks no allocation path: the capacity was drawn at the window"
    );

    drop(window);
    assert_eq!(gc_blocks(), held_before, "the close gives the chain back");
}

#[test]
fn an_aborted_window_replays_its_returns_with_both_allocation_paths_refusing() {
    let _guard = test_guard();
    let first = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    let second = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!first.is_null() && !second.is_null());
    let first = unsafe { dead_entity(first) };
    let second = unsafe { dead_entity(second) };

    let held_before = gc_blocks();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), first, 0) };
    unsafe { crate::memory::stdapi::ll_free(first as *mut u8) };
    unsafe { crate::memory::stdapi::ll_free(second as *mut u8) };
    assert_eq!(deferred_slot_count(), 2);

    // The abort is a collection that gives up where memory ran out, so the path
    // is exercised with both allocation paths refusing: closing the window may
    // need no memory to return what it withheld.
    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    let _ = crate::test_support::allocation_probe::take_allocations();
    drop(window);
    let (heap, _pool) = crate::test_support::allocation_probe::take_allocations();
    drop(oom);

    assert_eq!(heap, 0, "the abort path reached the global allocator");
    assert_eq!(deferred_slot_count(), 0);
    assert_eq!(gc_blocks(), held_before, "the abort gave the chain back");

    let reused = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(
        reused == first as *mut u8 || reused == second as *mut u8,
        "the abort lost a physical return"
    );
    unsafe { dead_entity(reused) };
    unsafe { crate::memory::stdapi::ll_free(reused) };
}

#[test]
fn a_window_neither_allocation_path_can_fund_does_not_open() {
    let _guard = test_guard();
    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    let refused = ActiveTrace::open();
    drop(oom);
    assert!(
        refused.is_none(),
        "the window opened on memory neither allocation path granted"
    );

    // A shut window withholds nothing, which is what makes the refusal
    // answerable: the collection does not start and no slot is in hand.
    let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!slot.is_null());
    let dead = unsafe { dead_entity(slot) };
    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    assert_eq!(deferred_slot_count(), 0);

    let reused = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert_eq!(reused, dead as *mut u8, "a shut window withheld a return");
    unsafe { dead_entity(reused) };
    unsafe { crate::memory::stdapi::ll_free(reused) };
}

#[test]
fn the_append_moves_into_a_second_block_when_the_first_is_full() {
    let _guard = test_guard();
    let held_before = gc_blocks();
    let bytes_before = in_use_bytes();
    crate::memory::gc_metadata::lower_peak_to_current();
    let window = ActiveTrace::open().expect("the pool funds the trace window");

    // One OS-direct large entity at each end of the chain. Its return is the
    // one a reader can see from outside — the run leaves the registry and the
    // address space only when the replay reaches its record — so a replay that
    // walks one block of two is caught whichever block it skips.
    let first_marker = unsafe { withheld_large_entity() };
    let mut withheld = Vec::with_capacity(RECORDS_PER_BLOCK);
    for _ in 0..RECORDS_PER_BLOCK - 1 {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
        withheld.push(slot);
    }

    assert_eq!(deferred_slot_count(), RECORDS_PER_BLOCK);
    assert_eq!(
        gc_blocks(),
        held_before + 1,
        "the first block holds exactly its capacity"
    );

    let second_marker = unsafe { withheld_large_entity() };
    assert_eq!(deferred_slot_count(), RECORDS_PER_BLOCK + 1);
    assert_eq!(
        gc_blocks(),
        held_before + 2,
        "the record past the capacity drew the second block"
    );
    assert_eq!(
        in_use_bytes(),
        bytes_before + BLOCK_PAYLOAD,
        "the block the append left is charged whole and nothing else is"
    );

    drop(window);
    assert_eq!(deferred_slot_count(), 0);
    for marker in [first_marker, second_marker] {
        assert!(
            !crate::memory::large_entity::snapshot().contains(&marker),
            "the close left a withheld return standing in one of the two blocks"
        );
    }

    assert_eq!(
        gc_blocks(),
        held_before,
        "both blocks of the chain went back"
    );
    assert_eq!(
        in_use_bytes(),
        bytes_before,
        "the close discharges exactly what the growth charged"
    );
    assert_eq!(
        crate::memory::gc_metadata::stats().peak_bytes_in_use(),
        bytes_before + BLOCK_PAYLOAD + size_of::<*mut u8>(),
        "the grown chain enters a full head block and the one record behind \
         it; the second block's control line is reserved and not written"
    );

    let reused = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(
        withheld.contains(&reused),
        "the close across two blocks lost a slotted return"
    );
    unsafe { dead_entity(reused) };
    unsafe { crate::memory::stdapi::ll_free(reused) };
}

/// One OS-direct large entity, dead and offered back while a window is open,
/// which withholds it. The answer is the block address the registry lists it
/// under, so a caller can ask whether the run is still mapped.
unsafe fn withheld_large_entity() -> usize {
    let entity = crate::memory::large_entity::alloc(crate::memory::block_pool::BLOCK_PAYLOAD + 1);
    assert!(!entity.is_null());
    let entity = unsafe { dead_entity(entity) };
    let block = BlockHeader::of_ptr(entity as *const u8) as usize;
    assert!(crate::memory::large_entity::snapshot().contains(&block));
    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
    assert!(
        crate::memory::large_entity::snapshot().contains(&block),
        "the run was unmapped while the window was open"
    );
    block
}

/// Bytes the memory manager reports as in use inside the blocks collection
/// holds. Stable for the duration of a `test_guard`, as `gc_blocks` is.
fn in_use_bytes() -> usize {
    crate::memory::gc_metadata::stats().current_bytes_in_use()
}

#[test]
fn one_window_charges_no_byte_and_enters_its_consumption_at_the_close() {
    let _guard = test_guard();
    let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!slot.is_null());
    let dead = unsafe { dead_entity(slot) };

    let bytes_before = in_use_bytes();
    crate::memory::gc_metadata::lower_peak_to_current();
    let window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    assert_eq!(
        in_use_bytes(),
        bytes_before,
        "the append charged a byte; charging none is what keeps the ledger off \
         the free path"
    );

    drop(window);
    let after = crate::memory::gc_metadata::stats();
    assert_eq!(after.current_bytes_in_use(), bytes_before);
    assert_eq!(
        after.peak_bytes_in_use(),
        bytes_before + size_of::<DeferredReturnChain>() + size_of::<*mut u8>(),
        "the block under the cursor is entered in the high-water figure by the \
         transition that ends it, control line and one record"
    );
}

#[test]
fn the_critical_reserve_funds_a_window_the_pool_refuses() {
    let _guard = test_guard();
    let held_before = gc_blocks();
    let reserve_before = crate::memory::critical::blocks_held();
    assert!(
        reserve_before > 0,
        "the reserve is the second allocation path here"
    );

    let oom = force_oom();
    let window = ActiveTrace::open().expect("the reserve funds what the pool refused");
    drop(oom);
    assert_eq!(
        crate::memory::critical::blocks_held(),
        reserve_before - 1,
        "the reserve lent the block the pool refused"
    );
    assert_eq!(gc_blocks(), held_before + 1);

    drop(window);
    assert_eq!(
        crate::memory::critical::blocks_held(),
        reserve_before,
        "what the reserve lent goes back to the reserve"
    );
    assert_eq!(gc_blocks(), held_before);
}

#[test]
fn the_high_water_figure_holds_both_residues_of_one_collection() {
    let _guard = test_guard();
    let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!slot.is_null());
    let dead = unsafe { dead_entity(slot) };

    let bytes_before = in_use_bytes();
    crate::memory::gc_metadata::lower_peak_to_current();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), dead, 0) };
    assert!(!row.is_null());
    let arena_residue = window.arena().residue();
    assert!(
        arena_residue > 0,
        "the trace reserved no row to residue over"
    );
    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };

    drop(window);
    assert_eq!(
        crate::memory::gc_metadata::stats().peak_bytes_in_use(),
        bytes_before + arena_residue + size_of::<DeferredReturnChain>() + size_of::<*mut u8>(),
        "the rows and the withheld return stood together and were entered apart"
    );
}

#[test]
fn a_growth_the_pool_refuses_draws_the_reserve_and_gives_it_back() {
    let _guard = test_guard();
    let held_before = gc_blocks();
    let window = ActiveTrace::open().expect("the pool funds the trace window");

    // The first block is filled to its capacity before the ordinary allocation
    // path starts refusing, so the refusal lands on the growth and on nothing
    // else.
    for _ in 0..RECORDS_PER_BLOCK {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert_eq!(deferred_slot_count(), RECORDS_PER_BLOCK);
    let over_capacity = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!over_capacity.is_null());
    unsafe { dead_entity(over_capacity) };

    let reserve_before = crate::memory::critical::blocks_held();
    assert!(
        reserve_before > 0,
        "the reserve is the second allocation path here"
    );
    let oom = force_oom();
    unsafe { crate::memory::stdapi::ll_free(over_capacity) };
    drop(oom);

    assert_eq!(
        crate::memory::critical::blocks_held(),
        reserve_before - 1,
        "the growth took the reserve allocation path while the ordinary one refused"
    );
    assert_eq!(gc_blocks(), held_before + 2);

    drop(window);
    assert_eq!(
        crate::memory::critical::blocks_held(),
        reserve_before,
        "a block the reserve lent went back to the pool"
    );
    assert_eq!(gc_blocks(), held_before);
}
