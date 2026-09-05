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

/// Promote two arena objects in place, each held by a heap holder of its
/// own, so that one retained block counts two occupants a mark can reach.
unsafe fn retained_survivor_pair() -> (
    Arena,
    [*mut Object; 2],
    [*mut RcHeader; 2],
    *mut BlockHeader,
) {
    let survivor_class = crate::class::ClassBuilder::new("RetainedPairMember").build();
    let holder_class = crate::class::ClassBuilder::new("RetainedPairHolder")
        .prop("member", true)
        .build();
    let mut arena = Arena::new();
    let mut holders = [std::ptr::null_mut(); 2];
    let mut survivors = [std::ptr::null_mut(); 2];

    // Each borrow of the arena ends before the next begins: `store_prop`
    // takes one of its own, and a context held across it is a second live
    // `&mut` to the same arena.
    for index in 0..2 {
        let holder = {
            let mut context = LLContext { arena: &mut arena };
            unsafe { new_constructed(&mut context, holder_class, MemoryCategory::GcHeap) }
        };
        let survivor = {
            let mut context = LLContext { arena: &mut arena };
            unsafe { new_constructed(&mut context, survivor_class, MemoryCategory::RequestArena) }
        };
        unsafe { crate::test_support::store_prop(&mut arena, holder, 16, survivor) };
        holders[index] = holder;
        survivors[index] = survivor as *mut RcHeader;
    }

    let block = BlockHeader::of_ptr(survivors[0] as *const u8);
    assert_eq!(
        block,
        BlockHeader::of_ptr(survivors[1] as *const u8),
        "one arena reset promotes both survivors into one retained block"
    );

    unsafe { crate::promote::arena_reset_full(&mut arena) };
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_RETAINED
    );
    assert_eq!(
        unsafe { crate::memory::retained::live_occupant_count(block as usize) },
        2
    );
    (arena, holders, survivors, block)
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
    assert_eq!(
        deferred_slot_count(),
        0,
        "the window is closed, which is what a count of zero reads after a drop"
    );
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
        "the retained survivor and its heap holder are both withheld"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_RETAINED,
        "the last occupant returned the whole block under its trace row"
    );

    drop(window);
    assert_eq!(
        deferred_slot_count(),
        0,
        "the window is closed, which is what a count of zero reads after a drop"
    );
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
    crate::memory::gc_metadata::thread_stats().current_blocks()
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
fn neither_the_window_nor_the_withheld_return_draws_a_manager_block() {
    let _guard = test_guard();
    let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!slot.is_null());
    let dead = unsafe { dead_entity(slot) };

    let held_before = gc_blocks();
    let _ = crate::test_support::allocation_probe::take_allocations();
    let window = ActiveTrace::open().expect("this thread's workspace is in hand");
    let (heap, pool) = crate::test_support::allocation_probe::take_allocations();
    assert_eq!(
        (heap, pool),
        (0, 0),
        "the open stands on the workspace and asks no allocation path"
    );
    assert_eq!(gc_blocks(), held_before);

    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    let (heap, pool) = crate::test_support::allocation_probe::take_allocations();
    assert_eq!(
        (heap, pool),
        (0, 0),
        "and the withheld return goes into the region the workspace already holds"
    );
    assert_eq!(deferred_slot_count(), 1);

    drop(window);
    assert_eq!(gc_blocks(), held_before, "the close drew a block");
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
    let mut window = ActiveTrace::open().expect("this thread's workspace is in hand");
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
    assert_eq!(
        deferred_slot_count(),
        0,
        "the window is closed, which is what a count of zero reads after a drop"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "the abort drew nothing: two records fit the workspace's own region"
    );

    let reused = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(
        reused == first as *mut u8 || reused == second as *mut u8,
        "the abort lost a physical return"
    );
    unsafe { dead_entity(reused) };
    unsafe { crate::memory::stdapi::ll_free(reused) };
}

/// The same abort past a full region. The case above withholds two returns,
/// which the region holds, so it says nothing about a close that has run out
/// of records: this one fills the region first, and the abort then has a mark
/// to dispose of beside the records it replays.
#[test]
fn an_abort_past_the_full_region_asks_no_allocation_path() {
    let _guard = test_guard();
    let held_before = gc_blocks();

    // Allocated before the fill, because every slot the fill withholds stays
    // out of the allocator's hands and the block the row stamps would
    // otherwise have no free slot left to hand out.
    let victim = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!victim.is_null());
    let victim = unsafe { live_entity(victim, 1) };

    let mut window = ActiveTrace::open().expect("this thread's workspace is in hand");
    let row = unsafe { ensure_row(window.arena(), victim, 1) };
    assert!(!row.is_null());

    // One OS-direct large entity inside the region, whose return a reader can
    // see from outside: a close that disposes of the mark and skips the
    // records is caught by it.
    let region_marker = unsafe { withheld_large_entity() };
    for _ in 0..RETURNS_BASE_RECORDS - 1 {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS);

    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the death past the region was marked"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and the region drew nothing to record it in"
    );

    // The abort is a collection that gives up where memory ran out, so the
    // close runs with both allocation paths refusing: walking a marked block
    // and replaying what the region holds may need no memory.
    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    let _ = crate::test_support::allocation_probe::take_allocations();
    drop(window);
    let requests = crate::test_support::allocation_probe::take_allocations();
    drop(oom);

    assert_eq!(requests, (0, 0), "the abort asked an allocation path");
    assert_eq!(gc_blocks(), held_before, "and drew no block over the close");
    assert!(
        !crate::memory::large_entity::snapshot().contains(&region_marker),
        "the replay left the region's withheld return standing"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::Free,
        "and the marked slot went back with the records"
    );

    crate::memory::critical::drain_for_test();
}

/// A thread that has collected once holds its workspace until it exits, and
/// the window stands in that workspace, so the ordinary allocation path
/// refusing everything takes no window down. The refusal this leaves is the
/// one a thread's *first* collection meets, which is the case below.
#[test]
fn a_window_opens_with_both_allocation_paths_refusing() {
    let _guard = test_guard();
    let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!slot.is_null());
    // `test_guard` draws this thread's workspace before the case begins, which
    // is the state the claim is about: a thread that has collected once.
    let dead = unsafe { dead_entity(slot) };

    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    let window = ActiveTrace::open().expect("the workspace was in hand before the refusal");
    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    assert_eq!(deferred_slot_count(), 1, "and it withheld a return");
    drop(window);
    drop(oom);

    let reused = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert_eq!(
        reused, dead as *mut u8,
        "the close lost the physical return"
    );
    unsafe { dead_entity(reused) };
    unsafe { crate::memory::stdapi::ll_free(reused) };
    crate::memory::critical::drain_for_test();
}

/// The one refusal left: a thread whose first collection cannot draw its
/// workspace. A shut window withholds nothing, which is what makes that
/// refusal answerable — the collection does not start and no slot is in hand.
///
/// On a thread of its own, because every other thread in this suite has
/// collected already and holds a workspace this case would find.
#[test]
fn a_first_collection_that_cannot_draw_a_workspace_does_not_open() {
    let _guard = test_guard();

    std::thread::spawn(|| {
        assert!(crate::memory::heap::ll_thread_init());
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        let dead = unsafe { dead_entity(slot) };

        // The workspace comes off the ordinary allocation path alone, so the
        // pool refusing is the whole of the refusal here.
        let oom = force_oom();
        let refused = ActiveTrace::open();
        assert!(
            refused.is_none(),
            "the window opened without a workspace the pool granted"
        );

        unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
        assert_eq!(deferred_slot_count(), 0);
        drop(oom);

        let reused = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert_eq!(reused, dead as *mut u8, "a shut window withheld a return");
        unsafe { dead_entity(reused) };
        unsafe { crate::memory::stdapi::ll_free(reused) };
    })
    .join()
    .unwrap();
}

/// A full region draws no block for the death past it: no row of this
/// collection names that run, so the return proceeds physically and the
/// window owes nothing for it.
///
/// Red on a chain that grows — the run stays mapped and a block appears —
/// and red on a close that loses what the region itself holds.
#[test]
fn a_death_past_the_full_region_draws_no_block() {
    let _guard = test_guard();
    let held_before = gc_blocks();
    let anchor = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!anchor.is_null());
    let anchor = unsafe { live_entity(anchor, 1) };

    let bytes_before = in_use_bytes();
    crate::memory::gc_metadata::lower_thread_peak_to_current();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");

    // A row, so that the arena has a residue for the close to enter: without
    // one the high-water reading below cannot tell "the window adds none of
    // its own" from "nothing enters at all".
    let row = unsafe { ensure_row(window.arena(), anchor, 1) };
    assert!(!row.is_null());
    let arena_residue = window.arena().residue();
    assert!(
        arena_residue > 0,
        "the trace reserved no row to residue over"
    );

    // One OS-direct large entity inside the region. Its return is the one a
    // reader can see from outside — the run leaves the registry and the
    // address space only when the replay reaches its record — so a close that
    // skips the region is caught here.
    let region_marker = unsafe { withheld_large_entity() };
    let mut withheld = Vec::with_capacity(RETURNS_BASE_RECORDS);
    for _ in 0..RETURNS_BASE_RECORDS - 1 {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
        withheld.push(slot);
    }

    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS);
    assert_eq!(
        gc_blocks(),
        held_before,
        "the workspace's region holds exactly its capacity and draws nothing"
    );

    unsafe { large_entity_returned_at_once() };
    assert_eq!(
        deferred_slot_count(),
        RETURNS_BASE_RECORDS,
        "the death past the region took no record"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and drew no block to record it in"
    );
    assert_eq!(in_use_bytes(), bytes_before, "and charged no byte for one");

    drop(window);
    assert_eq!(
        deferred_slot_count(),
        0,
        "the window is closed, which is what a count of zero reads after a drop"
    );
    assert!(
        !crate::memory::large_entity::snapshot().contains(&region_marker),
        "the close left the region's own withheld return standing"
    );

    assert_eq!(gc_blocks(), held_before);
    assert_eq!(in_use_bytes(), bytes_before);
    assert_eq!(
        crate::memory::gc_metadata::thread_stats().peak_bytes_in_use(),
        bytes_before + arena_residue,
        "the high-water figure holds the arena's residue and nothing besides, \
         the window having no memory of its own to enter beside it"
    );

    let reused = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(
        withheld.contains(&reused),
        "the close lost a slotted return"
    );
    unsafe { dead_entity(reused) };
    unsafe { crate::memory::stdapi::ll_free(reused) };
    unsafe { crate::refcount::set_header_refcount(anchor, 0) };
    unsafe { crate::memory::stdapi::ll_free(anchor as *mut u8) };
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

/// The same run offered back past a full region, where the window has no
/// record for it and no row of the collection names it, so it goes back at
/// once. The answer is the block address the registry listed it under, for a
/// caller that wants to say the run is gone rather than that it went.
unsafe fn large_entity_returned_at_once() -> usize {
    let entity = crate::memory::large_entity::alloc(crate::memory::block_pool::BLOCK_PAYLOAD + 1);
    assert!(!entity.is_null());
    let entity = unsafe { dead_entity(entity) };
    let block = BlockHeader::of_ptr(entity as *const u8) as usize;
    assert!(crate::memory::large_entity::snapshot().contains(&block));
    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
    assert!(
        !crate::memory::large_entity::snapshot().contains(&block),
        "the run was withheld although no row of this collection named it"
    );
    block
}

/// Bytes the memory manager reports as in use inside the blocks collection
/// holds. Stable for the duration of a `test_guard`, as `gc_blocks` is.
fn in_use_bytes() -> usize {
    crate::memory::gc_metadata::thread_stats().current_bytes_in_use()
}

#[test]
fn a_window_inside_the_workspace_region_charges_and_enters_no_byte() {
    let _guard = test_guard();
    let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!slot.is_null());
    let dead = unsafe { dead_entity(slot) };

    let bytes_before = in_use_bytes();
    crate::memory::gc_metadata::lower_thread_peak_to_current();
    let window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    assert_eq!(
        in_use_bytes(),
        bytes_before,
        "the append charged a byte; charging none is what keeps the ledger off \
         the free path"
    );

    drop(window);
    let after = crate::memory::gc_metadata::thread_stats();
    assert_eq!(after.current_bytes_in_use(), bytes_before);
    assert_eq!(
        after.peak_bytes_in_use(),
        bytes_before,
        "and enters none either: a chain that never left the workspace's own \
         region has no residue of its own to enter"
    );
}

/// The reserve exists for the pressure collection, where a refused pool is
/// what started the collection at all. A window that drew a block at its open
/// would spend the reserve on every such collection before a single return was
/// withheld; standing in the workspace, it spends none — and past the region
/// it spends none either, a death there being answered in the dying entity's
/// own memory (`a_death_past_the_full_region_draws_no_block`).
#[test]
fn a_windows_open_and_close_leave_the_critical_reserve_untouched() {
    let _guard = test_guard();
    let held_before = gc_blocks();
    let reserve_before = crate::memory::critical::blocks_held();
    assert!(
        reserve_before > 0,
        "the reserve has something to be spent here"
    );

    let oom = force_oom();
    let window = ActiveTrace::open().expect("the workspace was in hand before the refusal");
    assert_eq!(
        crate::memory::critical::blocks_held(),
        reserve_before,
        "the open asked neither allocation path"
    );
    assert_eq!(gc_blocks(), held_before);

    drop(window);
    drop(oom);
    assert_eq!(crate::memory::critical::blocks_held(), reserve_before);
    assert_eq!(gc_blocks(), held_before);
}

/// A death the region cannot record, in a block the trace has stamped, is
/// marked in the slot itself: no block is drawn, no record is made, and the
/// slot reads as neither live nor free.
///
/// This is the path a refusal takes, with no process end anywhere on it, and
/// it is reached here with the pool healthy — the region's capacity is the
/// whole trigger (`dev/DECISIONS.md`, "the chain stays and the mark answers
/// its refusal").
///
/// **The mark is read by no census**, the three states collapsing to two
/// wherever a walk asks only whether a slot is live. `slot_state` and
/// `describe_slot` are the whole of its observable surface here, and the
/// walk and the allocation below pin the withholding rather than the mark;
/// what returns the slot is the close, which is
/// `the_close_returns_a_marked_slot`'s subject.
#[test]
fn a_stamped_slot_past_the_region_is_marked_rather_than_recorded() {
    let _guard = test_guard();
    let held_before = gc_blocks();

    // Allocated before the region fills, because every slot the fill withholds
    // stays out of the allocator's hands and the block the row addresses would
    // otherwise have no free slot left to hand out.
    let victim = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!victim.is_null());
    let victim = unsafe { live_entity(victim, 1) };
    let block = (victim as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8;

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), victim, 1) };
    assert!(!row.is_null());
    assert!(
        !unsafe { crate::memory::heap::block_shadow(block) }.is_null(),
        "the row stamped the victim's own block"
    );

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS);
    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };

    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };

    assert_eq!(
        deferred_slot_count(),
        RETURNS_BASE_RECORDS,
        "the mark took the place of the record"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and nothing was drawn to hold it, which is what takes the abort off this path"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the slot is neither live nor free"
    );
    assert_eq!(
        unsafe { crate::refcount::header_refcount(victim) },
        0,
        "and its count still reads zero, which is what a queue reader depends on"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before,
        "the block's occupancy falls at the return and not at the mark"
    );

    // The walker every census goes through passes over it, and the class it
    // belongs to hands it to nobody: the slot is on no free list and below its
    // block's bump cursor.
    let mut live_in_block = 0;
    unsafe {
        crate::memory::heap::for_each_entity_slot(|slot| {
            if (slot as usize & !crate::memory::block_pool::BLOCK_MASK) == block as usize {
                live_in_block += 1;
            }
        });
    }

    assert_eq!(
        live_in_block, 0,
        "the walk passes over a zero-count slot, marked or not"
    );
    let served = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!served.is_null());
    assert_ne!(
        served, victim as *mut u8,
        "the slot is on no free list and below its block's bump cursor, so the \
         allocator cannot reach it"
    );
    assert!(
        crate::memory::heap::describe_slot(victim as usize).contains("state DeadInPlace"),
        "and the slot describes itself as neither live nor free"
    );

    drop(window);

    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::Free,
        "the close returned the slot the mark held, which is \
         `the_close_returns_a_marked_slot`'s subject and this case's cleanup"
    );

    unsafe { dead_entity(served) };
    unsafe { crate::memory::stdapi::ll_free(served) };
}

/// A death the region cannot record, in a block no row of this collection
/// addresses, is returned at once.
///
/// The block's shadow pointer is the whole test, and this is the case that
/// justifies the load: a block this collection never touched carries no row
/// for any of its slots, so a new occupant of this one inherits nothing and
/// the window has nothing to withhold.
#[test]
fn an_unstamped_block_past_the_region_is_returned_at_once() {
    let _guard = test_guard();
    let held_before = gc_blocks();

    // A stamped block has to stand for the shadow-pointer test to be doing any
    // work: with none in the process the case passes for an implementation
    // that reads the wrong block's shadow, or that asks whether a window is
    // open at all.
    let stamped = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!stamped.is_null());
    let stamped = unsafe { live_entity(stamped, 1) };

    // Another size class, so the death below comes from a block of its own
    // and the row addresses none of it; and a keeper in that block, so the
    // return does not empty it and hand it to the pool under the reads that
    // follow.
    let keeper = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 2) };
    assert!(!keeper.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    let over_capacity = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 2) };
    assert!(!over_capacity.is_null());

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), stamped, 1) };
    assert!(!row.is_null());

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS);

    let block = crate::memory::block_pool::BlockHeader::of_ptr(over_capacity) as *mut u8;
    let stamped_block =
        crate::memory::block_pool::BlockHeader::of_ptr(stamped as *const u8) as *mut u8;
    assert_ne!(
        block, stamped_block,
        "the two deaths are in different blocks"
    );
    assert!(
        !unsafe { crate::memory::heap::block_shadow(stamped_block) }.is_null(),
        "the row stamped its own block"
    );
    assert!(
        unsafe { crate::memory::heap::block_shadow(block) }.is_null(),
        "and no row addresses this one"
    );

    let occupancy_before = unsafe { crate::memory::heap::block_occupancy(block) };

    // With both allocation paths refusing, so that the answer is read as the
    // one that asks for nothing rather than as the one that got lucky.
    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    unsafe { dead_entity(over_capacity) };
    unsafe { crate::memory::stdapi::ll_free(over_capacity) };
    drop(oom);

    assert_eq!(
        deferred_slot_count(),
        RETURNS_BASE_RECORDS,
        "the return was recorded rather than made"
    );
    assert_eq!(gc_blocks(), held_before, "and a block was drawn to hold it");
    assert_eq!(
        unsafe { crate::refcount::slot_state(over_capacity as *const RcHeader) },
        crate::refcount::SlotState::Free,
        "the slot carries a mark"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupancy_before - 1,
        "the return reached the block's own count"
    );

    let served = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 2) };
    assert_eq!(
        served, over_capacity,
        "the slot reached its free list inside the window, which is what \
         separates a return from a withholding of any kind"
    );

    drop(window);
    assert_eq!(gc_blocks(), held_before, "the close drew a block");

    unsafe { crate::refcount::set_header_refcount(stamped, 0) };
    unsafe { crate::memory::stdapi::ll_free(stamped as *mut u8) };
    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
    unsafe { dead_entity(served) };
    unsafe { crate::memory::stdapi::ll_free(served) };
    crate::memory::critical::drain_for_test();
}

/// A death past a full region in a stamped block **this thread does not
/// own** is marked and stacked rather than listed: the close returns it
/// through the block's own stack of cross-thread frees, and the block is
/// never walked, a walk of it being bounded by a cursor its owner moves.
///
/// The block here is one an exited thread abandoned with its occupants still
/// live, which leaves it owned by nobody — the shape a thread that dies
/// inside another thread's reach leaves behind. It is filled to capacity and
/// its class is this case's own, so that the adoption at the end has nowhere
/// to draw from but the block's own stack of cross-thread frees, which is
/// what says the return was made rather than dropped.
#[test]
fn a_stamped_block_this_thread_does_not_own_is_marked_and_stacked() {
    const CLASS: usize = ENTITY_SIZE * 6;

    let _guard = test_guard();

    // The entities outlive their thread: `ll_thread_exit` puts a block with a
    // live occupant on the abandoned list rather than back in the pool.
    let held = std::thread::spawn(|| {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the second thread"
        );
        let first = unsafe { crate::memory::heap::entity_alloc(CLASS) };
        assert!(!first.is_null());
        let block = crate::memory::block_pool::BlockHeader::of_ptr(first) as *mut u8;
        let capacity = unsafe { crate::memory::heap::collector_block_slots(block) } as usize;
        let mut held = vec![unsafe { live_entity(first, 1) } as usize];
        for _ in 1..capacity {
            let slot = unsafe { crate::memory::heap::entity_alloc(CLASS) };
            assert!(!slot.is_null());
            assert_eq!(
                crate::memory::block_pool::BlockHeader::of_ptr(slot) as *mut u8,
                block,
                "the class opened a second block before the first was full"
            );
            held.push(unsafe { live_entity(slot, 1) } as usize);
        }

        crate::memory::heap::ll_thread_exit();
        held
    })
    .join()
    .expect("the second thread finished");

    let foreign = held[held.len() / 2] as *mut RcHeader;
    let block = crate::memory::block_pool::BlockHeader::of_ptr(foreign as *const u8) as *mut u8;
    assert!(
        !unsafe { crate::memory::heap::block_is_owned_by_this_thread(block) },
        "the block belongs to a heap that no longer exists"
    );

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), foreign, 1) };
    assert!(!row.is_null());
    assert!(
        !unsafe { crate::memory::heap::block_shadow(block) }.is_null(),
        "the row stamped the foreign block, so only ownership separates this case \
         from the marked one"
    );

    // The fill takes another size class: a refill of the foreign block's own
    // class would adopt it, and an adopted block is this thread's to sweep.
    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 2) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS);
    assert!(
        !unsafe { crate::memory::heap::block_is_owned_by_this_thread(block) },
        "the fill left the foreign block unadopted"
    );

    let occupancy_before = unsafe { crate::memory::heap::block_occupancy(block) };
    unsafe { crate::refcount::set_header_refcount(foreign, 0) };
    unsafe { crate::memory::stdapi::ll_free(foreign as *mut u8) };

    assert_eq!(
        deferred_slot_count(),
        RETURNS_BASE_RECORDS,
        "the return was recorded rather than marked"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(foreign) },
        crate::refcount::SlotState::DeadInPlace,
        "the slot carries no mark"
    );
    assert!(
        unsafe { &*crate::memory::heap::marked_link(block) }
            .load(Ordering::Relaxed)
            .is_null(),
        "the block was listed, which would send the close walking a bump cursor \
         this thread has no right to read"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupancy_before,
        "the owner has not heard of the death, so its count still holds the slot \
         and the block cannot reach the pool under the mark"
    );

    drop(window);
    assert_eq!(
        unsafe { crate::refcount::slot_state(foreign) },
        crate::refcount::SlotState::Free,
        "the close left the mark standing in a block nothing walks"
    );

    // The return itself: the block is full, so the adoption below has nothing
    // to serve from but the cross-thread frees the close posted onto it
    // (`crate::memory::heap::Heap::alloc_block_full`). A close that cleared
    // the mark and returned nothing leaves this drawing a fresh block.
    let served = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert_eq!(
        served, foreign as *mut u8,
        "the close cleared the mark without making the return it deferred"
    );

    unsafe { dead_entity(served) };
    unsafe { crate::memory::stdapi::ll_free(served) };
    for slot in held {
        let slot = slot as *mut RcHeader;
        if slot == foreign {
            continue;
        }

        unsafe { crate::refcount::set_header_refcount(slot, 0) };
        unsafe { crate::memory::stdapi::ll_free(slot as *mut u8) };
    }
}

/// A retained survivor dying past a full region, in a block the trace has
/// stamped, carries the mark in its own header, and its block still counts
/// it: the count is what the withheld return owes, so the block cannot go
/// home while the mark stands.
///
/// The holder is given a row of its own. Its slot is an ordinary entity
/// slot, and left unstamped it would take a record and draw the block this
/// case asserts nothing drew.
#[test]
fn a_stamped_retained_survivor_past_the_region_is_marked_rather_than_recorded() {
    let _guard = test_guard();
    let (_arena, holder, survivor, block) = unsafe { retained_survivor() };
    let held_before = gc_blocks();

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), survivor, 1) };
    assert_eq!(unsafe { shadow::count(*row) }, 1);
    unsafe { ensure_row(window.arena(), holder as *mut RcHeader, 1) };
    assert!(
        !unsafe { crate::memory::heap::block_shadow(block as *mut u8) }.is_null(),
        "the row stamped the retained block"
    );

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS);

    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    assert_eq!(
        deferred_slot_count(),
        RETURNS_BASE_RECORDS,
        "two marks took the place of the survivor's record and the holder's"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and nothing was drawn to hold them, which is what takes the abort off this path"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(survivor) },
        crate::refcount::SlotState::DeadInPlace,
        "the survivor is neither live nor free"
    );
    assert_eq!(
        unsafe { crate::refcount::header_refcount(survivor) },
        0,
        "and its count still reads zero"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(holder as *const RcHeader) },
        crate::refcount::SlotState::DeadInPlace,
        "and the holder's own slot took the second mark rather than nothing"
    );
    assert_eq!(
        unsafe { crate::memory::retained::live_occupant_count(block as usize) },
        1,
        "a marked survivor is a counted survivor: its decrement is what the \
         withheld return owes"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_RETAINED,
        "so the block is not the pool's while the mark stands"
    );

    drop(window);
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the close returned both marks, and the survivor's spent the last hold \
         on the block (`the_close_returns_a_marked_retained_survivor`)"
    );
}

/// A pooled large entity dying past a full region, under a row of its own,
/// carries the mark in the entity header its block holds, and the block
/// stays out of the pool until the marked return is made.
#[test]
fn a_stamped_pooled_large_entity_past_the_region_is_marked_rather_than_recorded() {
    let _guard = test_guard();
    let entity = crate::memory::large_entity::alloc(crate::memory::heap::MAX_SMALL + 16);
    assert!(!entity.is_null());
    let entity = unsafe { dead_entity(entity) };
    let block = BlockHeader::of_ptr(entity as *const u8);
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE
    );

    let held_before = gc_blocks();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), entity, 0) };

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS);
    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };

    assert_eq!(
        deferred_slot_count(),
        RETURNS_BASE_RECORDS,
        "the mark took the place of the record"
    );
    assert_eq!(gc_blocks(), held_before, "and nothing was drawn to hold it");
    assert_eq!(
        unsafe { crate::refcount::slot_state(entity) },
        crate::refcount::SlotState::DeadInPlace,
        "the one slot the block holds is neither live nor free"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE,
        "so the block is not the pool's while the mark stands"
    );

    drop(window);
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the close returned the marked entity, and the block with it \
         (`the_close_returns_a_marked_pooled_large_entity`)"
    );
}

/// An OS-direct run dying past a full region, under a row of its own,
/// carries the mark in the entity header its run holds, and the mapping
/// stands until the marked return is made.
#[test]
fn a_stamped_run_past_the_region_is_marked_rather_than_recorded() {
    let _guard = test_guard();
    let entity = crate::memory::large_entity::alloc(crate::memory::block_pool::BLOCK_PAYLOAD + 1);
    assert!(!entity.is_null());
    let entity = unsafe { dead_entity(entity) };
    let block = BlockHeader::of_ptr(entity as *const u8) as usize;
    assert!(crate::memory::large_entity::snapshot().contains(&block));

    let held_before = gc_blocks();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), entity, 0) };

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS);
    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };

    assert_eq!(
        deferred_slot_count(),
        RETURNS_BASE_RECORDS,
        "the mark took the place of the record"
    );
    assert_eq!(gc_blocks(), held_before, "and nothing was drawn to hold it");
    assert_eq!(
        unsafe { crate::refcount::slot_state(entity) },
        crate::refcount::SlotState::DeadInPlace,
        "the one slot the run holds is neither live nor free"
    );
    assert!(
        crate::memory::large_entity::snapshot().contains(&block),
        "so the mapping stands while the mark does"
    );

    drop(window);
    assert!(
        !crate::memory::large_entity::snapshot().contains(&block),
        "the close returned the marked entity, which unmapped the run \
         (`the_close_returns_a_marked_run`)"
    );
}

/// A large entity dying past a full region, in a block no trace has met, is
/// unmapped at once.
///
/// Its own block header word is the stamp, and this is the case that gives
/// that word its work: an untouched row is one no new occupant could inherit,
/// there being no new occupant of a run at all, so the window has nothing to
/// hold the run for.
#[test]
fn an_unmet_large_entity_past_the_region_is_returned_at_once() {
    let _guard = test_guard();

    // A met large entity has to stand, or the case passes for an
    // implementation that reads another block's row or asks only whether a
    // window is open at all.
    let met = crate::memory::large_entity::alloc(crate::memory::heap::MAX_SMALL + 16);
    assert!(!met.is_null());
    let met = unsafe { live_entity(met, 1) };
    let met_block = BlockHeader::of_ptr(met as *const u8) as *mut u8;

    let unmet = crate::memory::large_entity::alloc(crate::memory::block_pool::BLOCK_PAYLOAD + 1);
    assert!(!unmet.is_null());
    let unmet = unsafe { dead_entity(unmet) };
    let unmet_block = BlockHeader::of_ptr(unmet as *const u8) as usize;

    let held_before = gc_blocks();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), met, 1) };
    assert_ne!(
        shadow::color(unsafe { *crate::memory::large_entity::shadow_row(met_block) }),
        shadow::Color::Untouched,
        "the row met its own block"
    );
    assert_eq!(
        shadow::color(unsafe { *crate::memory::large_entity::shadow_row(unmet_block as *mut u8) }),
        shadow::Color::Untouched,
        "and no row met this one"
    );

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS);

    // With both allocation paths refusing, so that the answer is read as the
    // one that asks for nothing rather than as the one that got lucky.
    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    unsafe { crate::memory::stdapi::ll_free(unmet as *mut u8) };
    drop(oom);

    assert_eq!(
        deferred_slot_count(),
        RETURNS_BASE_RECORDS,
        "the return was recorded rather than made"
    );
    assert_eq!(gc_blocks(), held_before, "and a block was drawn to hold it");
    assert!(
        !crate::memory::large_entity::snapshot().contains(&unmet_block),
        "the run outlived the free that no row of this collection could have \
         made unsafe"
    );

    drop(window);
    assert_eq!(gc_blocks(), held_before, "the close drew a block");

    unsafe { crate::refcount::set_header_refcount(met, 0) };
    unsafe { crate::memory::stdapi::ll_free(met as *mut u8) };
    crate::memory::critical::drain_for_test();
}

/// A retained survivor dying past a full region, in a block no trace has
/// stamped, is returned at once — and the return empties its block, which
/// goes home inside the window.
#[test]
fn an_unstamped_retained_survivor_past_the_region_is_returned_at_once() {
    let _guard = test_guard();
    let (_stamped_arena, stamped_holder, stamped_survivor, stamped_block) =
        unsafe { retained_survivor() };
    let (_arena, holder, _survivor, block) = unsafe { retained_survivor() };
    assert_ne!(
        stamped_block, block,
        "the two survivors are in different retained blocks"
    );

    let held_before = gc_blocks();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), stamped_survivor, 1) };
    assert!(
        !unsafe { crate::memory::heap::block_shadow(stamped_block as *mut u8) }.is_null(),
        "the row stamped its own block"
    );
    assert!(
        unsafe { crate::memory::heap::block_shadow(block as *mut u8) }.is_null(),
        "and no row addresses this one"
    );

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS);

    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    assert_eq!(
        deferred_slot_count(),
        RETURNS_BASE_RECORDS,
        "the survivor and its holder were recorded rather than returned"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and a block was drawn to record the pair in"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the survivor's return was withheld, so its block never emptied"
    );

    drop(window);
    assert_eq!(gc_blocks(), held_before, "the close drew a block");

    unsafe {
        assert!(crate::refcount::ll_release(stamped_holder as *mut RcHeader));
        ll_object_die(stamped_holder);
    }
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*stamped_block).kind) },
        BLOCK_KIND_FREE
    );
}

/// The close returns a marked entity slot, which is what makes the mark a
/// deferral rather than a leak: the slot reads free, its block's occupancy
/// falls, and the class hands the address out again.
///
/// The victim stands in a size class of its own so that the block it is in
/// is the only one of that class and the allocation below is the block's own
/// free list answering. A second entity keeps that block off the pool, which
/// is what makes the readings after the close the block's own and not a
/// stranger's; the block that empties entirely is
/// `the_close_returns_a_marked_retained_survivor`'s.
#[test]
fn the_close_returns_a_marked_slot() {
    let _guard = test_guard();
    let held_before = gc_blocks();

    let keeper = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert!(!keeper.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    let victim = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert!(!victim.is_null());
    let victim = unsafe { live_entity(victim, 1) };
    let block = (victim as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8;
    assert_eq!(
        block,
        (keeper as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8,
        "both entities are in the one block of their class"
    );

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), victim, 1) };
    assert!(!row.is_null());

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS);
    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };

    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was marked rather than recorded"
    );

    let _ = take_marked_slots_visited();
    drop(window);

    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::Free,
        "the close cleared the mark and returned the slot"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before - 1,
        "the return went through the owner's `used`, which is what retires a block"
    );
    assert_eq!(
        take_marked_slots_visited(),
        unsafe { crate::memory::heap::block_bump(block) } as usize,
        "the walk read the listed block's slots to its bump cursor and no other \
         block's"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and the walk that made it drew nothing"
    );

    let served = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert_eq!(
        served, victim as *mut u8,
        "the slot is the class's again: the return reached the free list rather \
         than only clearing the bit"
    );
    unsafe { dead_entity(served) };
    unsafe { crate::memory::stdapi::ll_free(served) };
    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// The walk of an entity block ends at the return that empties it, and the
/// slots behind that return are never read.
///
/// The reading is the visited count: the victim is the block's first slot
/// and its last occupant, so a walk that stops reads one slot and a walk
/// that runs to the bump cursor reads two. What the stop is worth is that
/// the second read would be of a block the return may have given to the
/// pool.
#[test]
fn the_walk_of_a_block_ends_at_the_return_that_empties_it() {
    let _guard = test_guard();

    let victim = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 5) };
    let spare = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 5) };
    assert!(!victim.is_null() && !spare.is_null());
    let victim = unsafe { live_entity(victim, 1) };
    let block = (victim as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8;

    // The cursor stands at two slots and the block holds one occupant, so a
    // walk that does not stop has a second slot to read.
    unsafe { dead_entity(spare) };
    unsafe { crate::memory::stdapi::ll_free(spare) };
    assert_eq!(unsafe { crate::memory::heap::block_bump(block) }, 2);
    assert_eq!(unsafe { crate::memory::heap::block_occupancy(block) }, 1);

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), victim, 1) };

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was marked rather than recorded"
    );

    let _ = take_marked_slots_visited();
    drop(window);

    assert_eq!(
        take_marked_slots_visited(),
        1,
        "the walk stopped at the return that emptied the block"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::Free,
        "and it made that return"
    );
}

/// The abort returns a marked retained survivor, and the block it empties
/// reaches the pool with both allocation paths refusing.
#[test]
fn an_aborted_window_returns_a_marked_retained_survivor() {
    let _guard = test_guard();
    let (_arena, holder, survivor, block) = unsafe { retained_survivor() };

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    window.detach_candidates();
    unsafe { ensure_row(window.arena(), survivor, 1) };
    unsafe { ensure_row(window.arena(), holder as *mut RcHeader, 1) };

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    assert_eq!(
        unsafe { crate::refcount::slot_state(survivor) },
        crate::refcount::SlotState::DeadInPlace,
        "the survivor's death was marked rather than recorded"
    );

    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    let _ = crate::test_support::allocation_probe::take_allocations();
    drop(window);
    let (heap, _pool) = crate::test_support::allocation_probe::take_allocations();
    drop(oom);

    assert_eq!(
        heap, 0,
        "the retained return stands on memory the thread already holds"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the abort spent the last hold and gave the block back"
    );
}

/// The abort returns a marked pooled large entity, whose block goes back to
/// a pool that is refusing every draw.
#[test]
fn an_aborted_window_returns_a_marked_large_entity() {
    let _guard = test_guard();
    let entity = crate::memory::large_entity::alloc(crate::memory::heap::MAX_SMALL + 16);
    assert!(!entity.is_null());
    let entity = unsafe { dead_entity(entity) };
    let block = BlockHeader::of_ptr(entity as *const u8);

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    window.detach_candidates();
    unsafe { ensure_row(window.arena(), entity, 0) };

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(entity) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was marked rather than recorded"
    );

    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    let _ = crate::test_support::allocation_probe::take_allocations();
    drop(window);
    let (heap, _pool) = crate::test_support::allocation_probe::take_allocations();
    drop(oom);

    assert_eq!(
        heap, 0,
        "the large return stands on memory the thread already holds"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the abort gave the block the marked entity held back"
    );
}

/// A panic inside the close leaves no block listed and no mark standing, and
/// makes the return the mark deferred.
///
/// The panic is staged inside the replay, which the ordered close runs after
/// the row sweep. So this collection's rows are gone by the time the unwind
/// reaches the chain's drop, and the marked slot goes back through the
/// owner's free list rather than being abandoned — which is the disposition
/// `DeferredReturnChain::swept` decides.
///
/// What the unwind may not leave behind is a block whose link word still
/// names a list nothing heads: membership is that word against null, so such
/// a block would refuse every later mark of itself, and a mark left standing
/// on it would reach an adopting thread through abandonment.
///
/// The panic is staged by the assertion that guards a live free: a recorded
/// slot whose refcount is raised while the window is open is one `ll_free`
/// refuses in a test build. That record is the one return the unwind cannot
/// make, and the case gives it back by hand.
///
/// A live entity keeps the victim's block off the pool, so the block's own
/// words can be read after the unwind.
#[test]
fn a_panic_in_the_close_leaves_no_block_listed() {
    let _guard = test_guard();

    let keeper = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert!(!keeper.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    let victim = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    let recorded = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!victim.is_null() && !recorded.is_null());
    let victim = unsafe { live_entity(victim, 1) };
    let recorded = unsafe { dead_entity(recorded) };
    let block = (victim as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8;
    assert_eq!(
        (keeper as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8,
        block,
        "the keeper stands in the victim's own block, which is what keeps that \
         block off the pool once the victim goes back"
    );
    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), victim, 1) };
    unsafe { crate::memory::stdapi::ll_free(recorded as *mut u8) };

    // Held, because the replay stops at the first of these records and the
    // rest are the memory that unwind loses. A case that walked away from
    // them would leave its blocks abandoned full, and the next case to fill
    // this class would adopt one as its first block (`dev/POSTMORTEM.md`,
    // "an abandoned block of a class another case fills is a fixture, not a
    // leftover").
    let mut lost = Vec::with_capacity(RETURNS_BASE_RECORDS);
    for _ in 0..RETURNS_BASE_RECORDS - 1 {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
        lost.push(slot as usize);
    }

    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was marked rather than recorded"
    );

    // The replay will find this record naming a slot that reads live, which
    // is the free `ll_free` refuses.
    unsafe { crate::refcount::set_header_refcount(recorded, 1) };
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(window);
    }));
    assert!(refused.is_err(), "the replay was expected to refuse");

    assert!(
        unsafe { crate::memory::heap::marked_link(block).as_ref() }
            .expect("the block's link word")
            .load(std::sync::atomic::Ordering::Acquire)
            .is_null(),
        "the unwind took the block off the list, so a later mark of it is listed again"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::Free,
        "and took the mark off, so no mark reaches an adopting thread"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before - 1,
        "and made the return the mark deferred, through the owner's `used`"
    );

    // The records the unwind lost are returned here, the first of them being
    // the one whose refusal raised: none of them reached a free list, so this
    // is their first return rather than a second.
    unsafe { crate::refcount::set_header_refcount(recorded, 0) };
    unsafe { crate::memory::stdapi::ll_free(recorded as *mut u8) };
    for slot in lost {
        unsafe { crate::memory::stdapi::ll_free(slot as *mut u8) };
    }

    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// A collection that took no mark walks no slot at its close, which is the
/// whole of what the mark costs an ordinary collection: the list is empty, so
/// no block is read and `clear_touched_rows` stays one store per touched
/// block.
#[test]
fn a_collection_that_took_no_mark_walks_no_slot() {
    let _guard = test_guard();
    let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!slot.is_null());
    let dead = unsafe { dead_entity(slot) };

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), dead, 0) };
    assert!(!row.is_null());
    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    assert_eq!(
        deferred_slot_count(),
        1,
        "the region had room, so the death took a record"
    );

    let _ = take_marked_slots_visited();
    drop(window);

    assert_eq!(
        take_marked_slots_visited(),
        0,
        "a collection that took no mark of either kind reads no slot at its close"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(dead) },
        crate::refcount::SlotState::Free,
        "and the recorded return was made by the replay"
    );
}

/// The close returns a marked retained survivor through the count word, and
/// the block that empties by it retires to the pool.
#[test]
fn the_close_returns_a_marked_retained_survivor() {
    let _guard = test_guard();
    let (_arena, holder, survivor, block) = unsafe { retained_survivor() };

    // One live entity of the holder's own class, so that the holder's block
    // is still this thread's when its slot is read back after the close. The
    // block that does empty here is the retained one, and that is what the
    // kind word below reads.
    let holder_block = (holder as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8;
    let (_, holder_stride, _) =
        unsafe { crate::memory::heap::entity_block_slot_bounds(holder_block) };
    let keeper = unsafe { crate::memory::heap::entity_alloc(holder_stride) };
    assert!(!keeper.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    assert_eq!(
        holder_block,
        (keeper as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8,
        "the keeper stands in the holder's own block"
    );

    let held_before = gc_blocks();

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), survivor, 1) };
    unsafe { ensure_row(window.arena(), holder as *mut RcHeader, 1) };

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS);

    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    assert_eq!(
        unsafe { crate::refcount::slot_state(survivor) },
        crate::refcount::SlotState::DeadInPlace,
        "the survivor's death was marked rather than recorded"
    );

    drop(window);

    assert_eq!(
        unsafe { crate::refcount::slot_state(holder as *const RcHeader) },
        crate::refcount::SlotState::Free,
        "the close returned the holder's own slot, whose block it did not retire"
    );
    // The survivor's own header is not read back: its block reached the pool
    // at the return, and reading a slot of a returned block is what the walk
    // itself is forbidden to do. The kind word is the pool's own, written by
    // the return.
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the survivor's return spent the last hold, so the block went to the pool"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and the walk that made both returns drew nothing"
    );

    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// A retained block holding two marked survivors comes back once: the walk
/// returns both, and what hands the block to the pool is the release of the
/// hold the walk itself took, not either survivor's own decrement.
#[test]
fn the_close_returns_two_marked_survivors_of_one_block() {
    let _guard = test_guard();
    let (_arena, holders, survivors, block) = unsafe { retained_survivor_pair() };
    let held_before = gc_blocks();

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    for index in 0..2 {
        unsafe { ensure_row(window.arena(), survivors[index], 1) };
        unsafe { ensure_row(window.arena(), holders[index] as *mut RcHeader, 1) };
    }

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    for holder in holders {
        unsafe {
            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }
    }

    for survivor in survivors {
        assert_eq!(
            unsafe { crate::refcount::slot_state(survivor) },
            crate::refcount::SlotState::DeadInPlace,
            "both deaths were marked rather than recorded"
        );
    }

    assert_eq!(
        unsafe { crate::memory::retained::live_occupant_count(block as usize) },
        2,
        "and neither has been counted down: the mark is what owes the decrement"
    );

    drop(window);

    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the walk returned both survivors and gave the block back once"
    );
    assert_eq!(gc_blocks(), held_before, "and drew nothing to do it");
}

/// The close returns a marked pooled large entity: its block goes back to the
/// pool at the walk rather than at the next collection.
#[test]
fn the_close_returns_a_marked_pooled_large_entity() {
    let _guard = test_guard();
    let entity = crate::memory::large_entity::alloc(crate::memory::heap::MAX_SMALL + 16);
    assert!(!entity.is_null());
    let entity = unsafe { dead_entity(entity) };
    let block = BlockHeader::of_ptr(entity as *const u8);

    let held_before = gc_blocks();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), entity, 0) };

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS);
    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(entity) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was marked rather than recorded"
    );

    drop(window);

    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the close returned the block the marked entity held"
    );
    assert_eq!(gc_blocks(), held_before, "and the walk drew nothing");
}

/// The close returns a marked OS-direct run: the registry loses the entry and
/// the mapping goes back to the operating system.
#[test]
fn the_close_returns_a_marked_run() {
    let _guard = test_guard();
    let entity = crate::memory::large_entity::alloc(crate::memory::block_pool::BLOCK_PAYLOAD + 1);
    assert!(!entity.is_null());
    let entity = unsafe { dead_entity(entity) };
    let block = BlockHeader::of_ptr(entity as *const u8) as usize;
    assert!(crate::memory::large_entity::snapshot().contains(&block));

    let held_before = gc_blocks();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), entity, 0) };

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS);
    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(entity) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was marked rather than recorded"
    );

    drop(window);

    assert!(
        !crate::memory::large_entity::snapshot().contains(&block),
        "the close unmapped the run the marked entity held"
    );
    assert_eq!(gc_blocks(), held_before, "and the walk drew nothing");
}

/// The abort path returns marks as the ordered close does, and it does so
/// with both allocation paths refusing: the walk asks no allocation path,
/// which is what lets a collection that ran out of memory give its marks back.
///
/// The abort is staged the way a collection gives up — a batch detached and
/// never disposed of, rows standing over the block — so the drop runs its
/// whole order, `restore_candidates` included, rather than the bare close the
/// success case makes.
#[test]
fn an_aborted_window_returns_its_marked_slots() {
    let _guard = test_guard();
    let held_before = gc_blocks();

    let keeper = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert!(!keeper.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    let victim = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert!(!victim.is_null());
    let victim = unsafe { live_entity(victim, 1) };
    let block = (victim as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8;

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    window.detach_candidates();
    unsafe { ensure_row(window.arena(), victim, 1) };

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };
    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was marked rather than recorded"
    );

    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    let _ = crate::test_support::allocation_probe::take_allocations();
    drop(window);
    let (heap, _pool) = crate::test_support::allocation_probe::take_allocations();
    drop(oom);

    assert_eq!(
        heap, 0,
        "the marked walk stands on memory the thread already holds"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::Free,
        "the abort returned the marked slot"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before - 1,
        "through the owner's `used`, as the ordered close does"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and the abort drew nothing to make the return"
    );

    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// The close of a window that marked a slot in a **live** owner's block gives
/// that slot back to the owner, through the block's own stack of cross-thread
/// frees.
///
/// The owner fills one block of its class to capacity and holds every slot,
/// so its next allocation has nowhere to go but that stack
/// (`crate::memory::heap::Heap::alloc_block_full`). A close that dropped the
/// stacked slot, or returned it to the marking thread's own heap, leaves the
/// owner allocating out of a second block instead.
#[test]
fn the_close_returns_a_marked_slot_to_a_live_owner() {
    use std::sync::mpsc;

    let _guard = test_guard();
    let (to_tracer, from_owner) = mpsc::channel::<usize>();
    let (to_owner, from_tracer) = mpsc::channel::<()>();

    let owner = std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the second thread"
        );

        let first = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!first.is_null());
        let block = crate::memory::block_pool::BlockHeader::of_ptr(first) as *mut u8;
        let capacity = unsafe { crate::memory::heap::collector_block_slots(block) } as usize;
        let mut held = vec![unsafe { live_entity(first, 1) }];
        for _ in 1..capacity {
            let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
            assert!(!slot.is_null());
            assert_eq!(
                crate::memory::block_pool::BlockHeader::of_ptr(slot) as *mut u8,
                block,
                "the class opened a second block before the first was full"
            );
            held.push(unsafe { live_entity(slot, 1) });
        }

        let victim = held.swap_remove(capacity / 2);
        to_tracer
            .send(victim as usize)
            .expect("the tracer is waiting");
        from_tracer.recv().expect("the tracer closed its window");

        let served = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert_eq!(
            served, victim as *mut u8,
            "the block is full, so an allocation answering with anything but \
             the marked slot is a slot the close lost"
        );

        unsafe { dead_entity(served) };
        unsafe { crate::memory::stdapi::ll_free(served) };
        for slot in held {
            unsafe { crate::refcount::set_header_refcount(slot, 0) };
            unsafe { crate::memory::stdapi::ll_free(slot as *mut u8) };
        }

        crate::memory::heap::ll_thread_exit();
    });

    let victim = from_owner.recv().expect("the owner handed out a slot") as *mut RcHeader;
    let block = crate::memory::block_pool::BlockHeader::of_ptr(victim as *const u8) as *mut u8;
    assert!(
        !unsafe { crate::memory::heap::block_is_owned_by_this_thread(block) },
        "the block belongs to the thread that is still holding it"
    );

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), victim, 1) };
    assert!(!row.is_null());

    // Another size class, so the fill neither adopts the owner's block nor
    // allocates out of it.
    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 2) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS);
    let occupancy_before = unsafe { crate::memory::heap::block_occupancy(block) };

    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the death in a stamped foreign block was marked"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupancy_before,
        "and the owner cannot hand the slot out again, never having heard of \
         the death"
    );

    let _ = take_marked_slots_visited();
    drop(window);
    assert_eq!(
        take_marked_slots_visited(),
        1,
        "the close read the one stacked slot and no other"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::Free,
        "and cleared the mark before making the return"
    );

    to_owner.send(()).expect("the owner is waiting");
    owner.join().expect("the owner thread finished");
}

/// The unwind's half for a stacked slot: the drop clears its mark as it
/// clears a listed block's, and makes the return through the block's own
/// stack of cross-thread frees.
///
/// The panic is staged where `a_panic_in_the_close_leaves_no_block_listed`
/// stages it: a recorded slot whose refcount is raised while the window is
/// open is one `ll_free` refuses in a test build. The sweep has run by then,
/// so the disposition is the returning one.
///
/// The block is filled to capacity and its class is this case's own, so the
/// adoption at the end has nowhere to draw from but the frees the drop
/// posted — which is what says the return was made rather than dropped, and
/// which leaves the class as the case found it.
#[test]
fn a_panic_in_the_close_leaves_no_stacked_mark_standing() {
    const CLASS: usize = ENTITY_SIZE * 7;

    let _guard = test_guard();

    // The entities outlive their thread: `ll_thread_exit` puts a block with a
    // live occupant on the abandoned list rather than back in the pool.
    let held = std::thread::spawn(|| {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the second thread"
        );
        let first = unsafe { crate::memory::heap::entity_alloc(CLASS) };
        assert!(!first.is_null());
        let block = crate::memory::block_pool::BlockHeader::of_ptr(first) as *mut u8;
        let capacity = unsafe { crate::memory::heap::collector_block_slots(block) } as usize;
        let mut held = vec![unsafe { live_entity(first, 1) } as usize];
        for _ in 1..capacity {
            let slot = unsafe { crate::memory::heap::entity_alloc(CLASS) };
            assert!(!slot.is_null());
            assert_eq!(
                crate::memory::block_pool::BlockHeader::of_ptr(slot) as *mut u8,
                block,
                "the class opened a second block before the first was full"
            );
            held.push(unsafe { live_entity(slot, 1) } as usize);
        }

        crate::memory::heap::ll_thread_exit();
        held
    })
    .join()
    .expect("the second thread finished");

    let foreign = held[held.len() / 2] as *mut RcHeader;

    let recorded = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 2) };
    assert!(!recorded.is_null());
    let recorded = unsafe { dead_entity(recorded) };

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), foreign, 1) };
    unsafe { crate::memory::stdapi::ll_free(recorded as *mut u8) };

    // The fill takes the recorded slot's class rather than the foreign
    // block's: a refill of that block's class would adopt it. Held, because
    // the replay stops at the first of these records and the rest are the
    // memory that unwind loses — a case that walked away from them would
    // leave its blocks abandoned full, and the next case to fill this class
    // would adopt one as its first block (`dev/POSTMORTEM.md`, "an abandoned
    // block of a class another case fills is a fixture, not a leftover").
    let mut lost = Vec::with_capacity(RETURNS_BASE_RECORDS);
    for _ in 0..RETURNS_BASE_RECORDS - 1 {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 2) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
        lost.push(slot as usize);
    }

    unsafe { crate::refcount::set_header_refcount(foreign, 0) };
    unsafe { crate::memory::stdapi::ll_free(foreign as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(foreign) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was marked rather than recorded"
    );

    // The replay will find this record naming a slot that reads live, which
    // is the free `ll_free` refuses.
    unsafe { crate::refcount::set_header_refcount(recorded, 1) };
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(window);
    }));
    assert!(refused.is_err(), "the replay was expected to refuse");

    assert_eq!(
        unsafe { crate::refcount::slot_state(foreign) },
        crate::refcount::SlotState::Free,
        "the unwind took the mark off, so no mark reaches an adopting thread"
    );

    // The return itself: the block is full, so the adoption below has nothing
    // to serve from but the cross-thread frees the drop posted onto it
    // (`crate::memory::heap::Heap::alloc_block_full`). An unwind that cleared
    // the mark and returned nothing leaves this drawing a fresh block.
    let served = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert_eq!(
        served, foreign as *mut u8,
        "the unwind cleared the mark without making the return it deferred"
    );

    unsafe { dead_entity(served) };
    unsafe { crate::memory::stdapi::ll_free(served) };
    for slot in held {
        let slot = slot as *mut RcHeader;
        if slot == foreign {
            continue;
        }

        unsafe { crate::refcount::set_header_refcount(slot, 0) };
        unsafe { crate::memory::stdapi::ll_free(slot as *mut u8) };
    }

    // The records the unwind lost are returned here, the first of them being
    // the one whose refusal raised: none of them reached a free list, so this
    // is their first return rather than a second.
    unsafe { crate::refcount::set_header_refcount(recorded, 0) };
    unsafe { crate::memory::stdapi::ll_free(recorded as *mut u8) };
    for slot in lost {
        unsafe { crate::memory::stdapi::ll_free(slot as *mut u8) };
    }
}

/// The reset's whole-block sentinel — a return whose address is the block
/// header rather than an entity — is returned at once past a full region.
///
/// It has no header of its own to mark, and it needs none: `retain_block`
/// clears the collector line before it publishes the kind, so no row of this
/// collection can address the block.
///
/// The sentinel is staged through the primitives the reset returns it with
/// rather than through a reset: a pin held across the survivor's death, and
/// the release that finds the block held by nothing
/// (`promote::arena_reset_full`, the `emptied` loop). A stamped block stands
/// beside it, so the case cannot pass for a window that stamped nothing.
///
/// **What no case here separates** is the `ptr == block` arm from the
/// unstamped arm below it: both answer `ReturnNow`, and the state that would
/// tell them apart — a sentinel whose block this collection stamped — is the
/// one the paragraph above says cannot arise. The `debug_assert!` in that arm
/// is what would report it.
#[test]
fn the_whole_block_sentinel_past_the_region_is_returned_at_once() {
    let _guard = test_guard();
    let (_arena, holder, _survivor, block) = unsafe { retained_survivor() };

    let stamped = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!stamped.is_null());
    let stamped = unsafe { live_entity(stamped, 1) };

    // The reset's own pin, which is what keeps the block standing while its
    // last occupant dies.
    unsafe { crate::memory::retained::pin(block as usize) };
    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_RETAINED,
        "the pin held the block through its survivor's death"
    );

    let held_before = gc_blocks();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), stamped, 1) };
    assert!(!row.is_null());
    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS);
    assert!(
        unsafe { crate::memory::retained::hold_released(block as usize) },
        "the pin was the last thing holding the block"
    );

    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    unsafe { crate::memory::stdapi::ll_free(block as *mut u8) };
    drop(oom);

    assert_eq!(
        deferred_slot_count(),
        RETURNS_BASE_RECORDS,
        "the sentinel was recorded rather than made"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and a block was drawn to record it"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the block was withheld although no row of this collection named it"
    );

    drop(window);
    unsafe { crate::refcount::set_header_refcount(stamped, 0) };
    unsafe { crate::memory::stdapi::ll_free(stamped as *mut u8) };
    crate::memory::critical::drain_for_test();
}

/// The same sentinel inside the region takes a record and goes home at the
/// close, which is the return the window exists to delay: a block handed
/// back while a row addressed one of its survivors would lose every row
/// address in it.
#[test]
fn the_whole_block_sentinel_inside_the_region_waits_for_the_close() {
    let _guard = test_guard();
    let (_arena, holder, _survivor, block) = unsafe { retained_survivor() };

    unsafe { crate::memory::retained::pin(block as usize) };
    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    let window = ActiveTrace::open().expect("the pool funds the trace window");
    assert!(
        unsafe { crate::memory::retained::hold_released(block as usize) },
        "the pin was the last thing holding the block"
    );
    unsafe { crate::memory::stdapi::ll_free(block as *mut u8) };

    assert_eq!(
        deferred_slot_count(),
        1,
        "the sentinel was returned rather than recorded"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_RETAINED,
        "and the block went home under the window"
    );

    drop(window);
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the close replayed the sentinel"
    );
}

/// A block foreign when one of its slots was stacked, and this thread's by
/// the time the next slot of it dies, returns each of the two exactly once.
///
/// `Heap::adopt` runs on the ordinary refill path, so ownership moves inside
/// an open window and one block ends up with a slot on the window's stack and
/// its own place on the window's list. The stack walk runs before the block
/// walk for exactly this reason: a block walk that met a stacked slot still
/// marked would free it, and the stack walk would then read a free-list link
/// where it had written its own and walk the block's free list into the
/// allocator a second time.
#[test]
fn a_block_adopted_after_a_slot_of_it_was_stacked_returns_each_slot_once() {
    const CLASS: usize = ENTITY_SIZE * 5;

    let _guard = test_guard();

    // Two live occupants of one abandoned block, in a class nothing else
    // fills, so this thread's first allocation of that class adopts it.
    let pair = std::thread::spawn(|| {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the second thread"
        );
        let first = unsafe { crate::memory::heap::entity_alloc(CLASS) };
        let second = unsafe { crate::memory::heap::entity_alloc(CLASS) };
        assert!(!first.is_null() && !second.is_null());
        let pair = [
            unsafe { live_entity(first, 1) } as usize,
            unsafe { live_entity(second, 1) } as usize,
        ];
        crate::memory::heap::ll_thread_exit();
        pair
    })
    .join()
    .expect("the second thread finished");

    let stacked = pair[0] as *mut RcHeader;
    let listed = pair[1] as *mut RcHeader;
    let block = crate::memory::block_pool::BlockHeader::of_ptr(stacked as *const u8) as *mut u8;
    assert_eq!(
        block,
        crate::memory::block_pool::BlockHeader::of_ptr(listed as *const u8) as *mut u8,
        "the two occupants share one block"
    );

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), stacked, 1) };
    assert!(
        !unsafe { crate::memory::heap::block_shadow(block) }.is_null(),
        "the row stamped the block both deaths stand in"
    );

    // The fill takes a class of its own, so it neither adopts this block nor
    // allocates out of it.
    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 2) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    unsafe { crate::refcount::set_header_refcount(stacked, 0) };
    unsafe { crate::memory::stdapi::ll_free(stacked as *mut u8) };
    assert!(
        unsafe { &*crate::memory::heap::marked_link(block) }
            .load(Ordering::Relaxed)
            .is_null(),
        "the block was listed while it was another thread's"
    );

    // The adoption: this thread holds no block of the class, and the
    // abandoned one is already carved for it
    // (`crate::memory::heap::Heap::adopt`).
    let keeper = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert!(!keeper.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    assert_eq!(
        crate::memory::block_pool::BlockHeader::of_ptr(keeper as *const u8) as *mut u8,
        block,
        "the refill carved a fresh block rather than adopting the abandoned one"
    );
    assert!(
        unsafe { crate::memory::heap::block_is_owned_by_this_thread(block) },
        "the adoption did not move the owner word"
    );

    unsafe { crate::refcount::set_header_refcount(listed, 0) };
    unsafe { crate::memory::stdapi::ll_free(listed as *mut u8) };
    assert!(
        !unsafe { &*crate::memory::heap::marked_link(block) }
            .load(Ordering::Relaxed)
            .is_null(),
        "the second death did not list the block this thread now owns"
    );

    let occupancy_before = unsafe { crate::memory::heap::block_occupancy(block) };
    drop(window);

    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupancy_before - 2,
        "the close did not return the two marked slots exactly once between them"
    );
    for (slot, name) in [(stacked, "stacked"), (listed, "listed")] {
        assert_eq!(
            unsafe { crate::refcount::slot_state(slot) },
            crate::refcount::SlotState::Free,
            "the close left the {name} slot marked"
        );
    }

    // The free list of a block whose slots were returned twice hands one
    // address out twice, or closes into a cycle.
    let first = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    let second = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert!(!first.is_null() && !second.is_null());
    assert_ne!(first, second, "one slot reached the free list twice");

    for slot in [first, second] {
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// A panic raised inside the arena's hand-back of its own blocks finds every
/// marked slot already returned.
///
/// This is the panic the close is built to survive, and the order is what
/// survives it: the sweep runs first, then the returns, and only then does the
/// arena give its blocks back. A close that gave them back first would reach
/// the chain's drop with the rows still standing, and the marks would be
/// abandoned rather than returned — which is the disposition
/// `DeferredReturnChain::swept` decides.
///
/// The panic is injected, the reset's own sites being an underflowed ledger
/// and a poisoned pool mutex: a test can raise neither without taking the
/// tests after it down with it (`crate::cycle::arena::InjectedResetFailure`).
///
/// A live entity keeps the victim's block off the pool, so the block's own
/// words can be read after the unwind.
#[test]
fn a_panic_in_the_reset_returns_every_marked_slot() {
    let _guard = test_guard();
    let held_before = gc_blocks();

    let keeper = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert!(!keeper.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    let victim = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert!(!victim.is_null());
    let victim = unsafe { live_entity(victim, 1) };
    let block = (victim as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8;
    assert_eq!(
        (keeper as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8,
        block,
        "the keeper stands in the victim's own block, which is what keeps that \
         block off the pool once the victim goes back"
    );

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), victim, 1) };

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };
    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was marked rather than recorded"
    );

    let armed = crate::cycle::arena::InjectedResetFailure::arm();
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(window);
    }));
    assert!(refused.is_err(), "the reset was expected to raise");
    drop(armed);

    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::Free,
        "the return was made before the blocks went back"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before - 1,
        "through the owner's `used`, as an ordered close does"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and the arena's own blocks went back at its drop, the reset the panic \
         interrupted being idempotent"
    );

    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// A panic inside the walk of one entity block returns the marks that walk had
/// not reached.
///
/// The walk takes its block off the list before it touches a slot, so between
/// those two instants the block is named by `DeferredReturnChain::walking` and
/// by nothing else; without that word the marks below the raising one would be
/// named by nothing, and `WithheldReturns::drop` could not find them either.
///
/// The panic is injected at the first of the block's two marked slots. The
/// lever the other panic cases use cannot reach a marked slot at all —
/// `is_marked` reads the count before the flags, so a slot whose count was
/// raised is skipped rather than raised on — and the injection stands where
/// `ll_free`'s own refusal would leave the walk: the mark already off, the
/// return not made (`InjectedDisposalFailure`).
///
/// That first slot is the panic's own leak and the case returns it by hand,
/// with no window open, so the class is left as it was found.
#[test]
fn a_panic_inside_a_block_walk_returns_the_marks_below_it() {
    let _guard = test_guard();

    let keeper = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert!(!keeper.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    let first = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    let second = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert!(!first.is_null() && !second.is_null());
    assert!(first < second, "the walk reads the block by rising address");
    let first = unsafe { live_entity(first, 1) };
    let second = unsafe { live_entity(second, 1) };
    let block = (first as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8;
    assert_eq!(
        (second as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8,
        block,
        "both marks stand in one block, which is what makes this one walk"
    );

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), first, 1) };

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };
    for entity in [first, second] {
        unsafe { crate::refcount::set_header_refcount(entity, 0) };
        unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
        assert_eq!(
            unsafe { crate::refcount::slot_state(entity) },
            crate::refcount::SlotState::DeadInPlace,
            "both deaths were marked rather than recorded"
        );
    }

    // The replay returns its records through `ll_free` rather than through a
    // disposal, so the first disposal of the close is the block walk's first
    // mark.
    let armed = InjectedDisposalFailure::arm(1);
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(window);
    }));
    assert!(refused.is_err(), "the walk was expected to raise");
    drop(armed);

    assert_eq!(
        unsafe { crate::refcount::slot_state(second) },
        crate::refcount::SlotState::Free,
        "the mark below the raising one was returned by the drop's own pass"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before - 1,
        "one return was made, and the raising slot's is the one it could not make"
    );

    unsafe { crate::memory::stdapi::ll_free(first as *mut u8) };
    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// A panic inside the walk of a retained block returns the marks below it and
/// spends the hold that walk took.
///
/// The hold is what keeps the block standing while the walk reads its survivor
/// list, and an unwind that left it standing would keep the block off the pool
/// for the life of the process. Nothing reads the hold directly here: the
/// block reaches the pool only when its last occupant dies, so a hold left
/// standing shows as a block that never goes home.
#[test]
fn a_panic_inside_a_retained_walk_spends_the_hold_it_took() {
    let _guard = test_guard();
    let (_arena, holders, survivors, block) = unsafe { retained_survivor_pair() };
    let held_before = gc_blocks();

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    for survivor in survivors {
        unsafe { ensure_row(window.arena(), survivor, 1) };
    }

    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    // The holders take no row, so their own slots stand in a block this
    // collection never met and their deaths are returned at once. That leaves
    // the retained block the one block this window lists, and the walk's first
    // disposal the one the injection names.
    for holder in holders {
        unsafe {
            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }
    }

    for survivor in survivors {
        assert_eq!(
            unsafe { crate::refcount::slot_state(survivor) },
            crate::refcount::SlotState::DeadInPlace,
            "both deaths were marked rather than recorded"
        );
    }

    let armed = InjectedDisposalFailure::arm(1);
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(window);
    }));
    assert!(refused.is_err(), "the walk was expected to raise");
    drop(armed);

    // The walk reads the survivor list, which `promote::register` sorts
    // ascending, so the injection's first disposal is the lower-addressed
    // survivor. Asserted rather than assumed: with the pair the other way
    // round the return below would be a second free of an occupant the drop's
    // pass already gave back.
    assert!(
        survivors[0] < survivors[1],
        "the walk reaches the survivors by rising address"
    );
    let raised = survivors[0];
    assert_eq!(
        unsafe { crate::memory::retained::live_occupant_count(block as usize) },
        1,
        "the mark below the raising one was returned by the drop's own pass"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_RETAINED,
        "and the block stands, the raising survivor still being an occupant"
    );

    // The raising survivor is the panic's own leak, and its return is what
    // empties the block — which a hold the unwind failed to spend would
    // prevent.
    unsafe { crate::memory::stdapi::ll_free(raised as *mut u8) };
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the unwind spent the hold its walk took"
    );
    assert_eq!(gc_blocks(), held_before, "and drew nothing to do any of it");
}
