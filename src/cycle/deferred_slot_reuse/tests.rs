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
    assert_eq!(
        gc_blocks(),
        held_before,
        "the close drew and gave back nothing"
    );
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

/// The same abort with a block under the cursor. The case above withholds two
/// returns, which the workspace's region holds, so it says nothing about the
/// blocks a grown chain owes: this one crosses the region first, and then the
/// abort has a block to give back and a replay that spans two segments.
#[test]
fn an_abort_past_the_region_gives_the_chains_block_back() {
    let _guard = test_guard();
    let held_before = gc_blocks();
    let window = ActiveTrace::open().expect("this thread's workspace is in hand");

    // One OS-direct large entity at each end of the chain, as the growth case
    // above does. Its return is the one a reader can see from outside, so a
    // replay that reads the region alone and a replay that reads the block
    // alone are each caught by the marker the other end holds.
    let region_marker = unsafe { withheld_large_entity() };
    for _ in 0..RETURNS_BASE_RECORDS - 1 {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS);
    assert_eq!(
        gc_blocks(),
        held_before,
        "the region holds its capacity and draws nothing"
    );

    let block_marker = unsafe { withheld_large_entity() };
    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS + 1);
    assert_eq!(
        gc_blocks(),
        held_before + 1,
        "the record past the region drew the chain's one block"
    );

    // The abort is a collection that gives up where memory ran out, so the
    // close runs with both allocation paths refusing: giving a block back and
    // replaying what it holds may need no memory.
    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    let _ = crate::test_support::allocation_probe::take_allocations();
    drop(window);
    let requests = crate::test_support::allocation_probe::take_allocations();
    drop(oom);

    assert_eq!(requests, (0, 0), "the abort asked an allocation path");
    assert_eq!(gc_blocks(), held_before, "the abort gave the block back");
    for (marker, segment) in [(region_marker, "region"), (block_marker, "block")] {
        assert!(
            !crate::memory::large_entity::snapshot().contains(&marker),
            "the replay left the withheld return standing in the {segment}"
        );
    }

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

#[test]
fn the_append_moves_into_a_block_when_the_workspace_region_is_full() {
    let _guard = test_guard();
    let held_before = gc_blocks();
    let bytes_before = in_use_bytes();
    crate::memory::gc_metadata::lower_thread_peak_to_current();
    let window = ActiveTrace::open().expect("the pool funds the trace window");

    // One OS-direct large entity at each end of the chain. Its return is the
    // one a reader can see from outside — the run leaves the registry and the
    // address space only when the replay reaches its record — so a replay that
    // walks one segment of two is caught whichever segment it skips.
    let first_marker = unsafe { withheld_large_entity() };
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

    let second_marker = unsafe { withheld_large_entity() };
    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS + 1);
    assert_eq!(
        gc_blocks(),
        held_before + 1,
        "the record past the capacity drew the chain's first block"
    );
    assert_eq!(
        in_use_bytes(),
        bytes_before,
        "and charged nothing: the region the append left is the thread's \
         workspace, which stands in neither figure"
    );

    drop(window);
    assert_eq!(
        deferred_slot_count(),
        0,
        "the window is closed, which is what a count of zero reads after a drop"
    );
    for marker in [first_marker, second_marker] {
        assert!(
            !crate::memory::large_entity::snapshot().contains(&marker),
            "the close left a withheld return standing in one of the two segments"
        );
    }

    assert_eq!(gc_blocks(), held_before, "the chain's block went back");
    assert_eq!(
        in_use_bytes(),
        bytes_before,
        "the close discharges exactly what the growth charged"
    );
    assert_eq!(
        crate::memory::gc_metadata::thread_stats().peak_bytes_in_use(),
        bytes_before + SEGMENT_HEADER_BYTES + size_of::<*mut u8>(),
        "the grown chain enters the block under the cursor: its segment header \
         and the one record written behind it"
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
/// withheld; standing in the workspace, it spends none. What the reserve does
/// fund is the growth past the region, which is the case below it.
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

#[test]
fn the_high_water_figure_holds_both_residues_of_one_collection() {
    let _guard = test_guard();
    let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!slot.is_null());
    let dead = unsafe { dead_entity(slot) };

    let bytes_before = in_use_bytes();
    crate::memory::gc_metadata::lower_thread_peak_to_current();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), dead, 0) };
    assert!(!row.is_null());
    let arena_residue = window.arena().residue();
    assert!(
        arena_residue > 0,
        "the trace reserved no row to residue over"
    );

    // Past the workspace's region, so the chain has a residue of its own: a
    // chain still inside that region enters nothing, and the case would then
    // be reading one residue rather than two.
    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    for _ in 0..RETURNS_BASE_RECORDS - 1 {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    // The record past the capacity comes from a block no row addresses: a
    // slot of the stamped block would be marked in place instead, and this
    // case is about the residue a record leaves (`PLAN.md` S43.2).
    let unstamped = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 2) };
    assert!(!unstamped.is_null());
    unsafe { dead_entity(unstamped) };
    unsafe { crate::memory::stdapi::ll_free(unstamped) };
    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS + 1);

    drop(window);
    assert_eq!(
        crate::memory::gc_metadata::thread_stats().peak_bytes_in_use(),
        bytes_before + arena_residue + SEGMENT_HEADER_BYTES + size_of::<*mut u8>(),
        "the rows and the withheld return stood together and were entered apart"
    );
}

#[test]
fn a_growth_the_pool_refuses_draws_the_reserve_and_gives_it_back() {
    let _guard = test_guard();
    let held_before = gc_blocks();
    let window = ActiveTrace::open().expect("the pool funds the trace window");

    // The workspace's region is filled to its capacity before the ordinary
    // allocation path starts refusing, so the refusal lands on the growth and
    // on nothing else.
    for _ in 0..RETURNS_BASE_RECORDS {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert_eq!(deferred_slot_count(), RETURNS_BASE_RECORDS);
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
    assert_eq!(gc_blocks(), held_before + 1);

    drop(window);
    assert_eq!(
        crate::memory::critical::blocks_held(),
        reserve_before,
        "a block the reserve lent went back to the pool"
    );
    assert_eq!(gc_blocks(), held_before);
}

/// A death the region cannot record, in a block the trace has stamped, is
/// marked in the slot itself: no block is drawn, no record is made, and the
/// slot reads as neither live nor free.
///
/// This is the path that replaces the process end, and it is reached here
/// with the pool healthy — the region's capacity is the whole trigger
/// (`PLAN.md` S43.2, and `dev/DECISIONS.md`, "the chain stays and the mark
/// answers its refusal").
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

/// A death the region cannot record, in a block no trace has stamped, still
/// takes a record — and therefore still draws the block the mark exists to
/// stop drawing.
///
/// The block's shadow pointer is the whole test, and this is the case that
/// justifies the load: the stamp is what says the block is this window's, and
/// a mark in a block no window of this thread is holding is one nothing
/// returns.
#[test]
fn an_unstamped_block_past_the_region_still_takes_a_record() {
    let _guard = test_guard();
    let held_before = gc_blocks();

    // A stamped block has to stand for the shadow-pointer test to be doing any
    // work: with none in the process the case passes for an implementation
    // that reads the wrong block's shadow, or that asks whether a window is
    // open at all.
    let stamped = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!stamped.is_null());
    let stamped = unsafe { live_entity(stamped, 1) };
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

    // Another size class, so the death comes from a block of its own and the
    // row above addresses none of it.
    let over_capacity = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 2) };
    assert!(!over_capacity.is_null());
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

    unsafe { dead_entity(over_capacity) };
    unsafe { crate::memory::stdapi::ll_free(over_capacity) };
    assert_eq!(
        deferred_slot_count(),
        RETURNS_BASE_RECORDS + 1,
        "the return was recorded rather than marked"
    );
    assert_eq!(
        gc_blocks(),
        held_before + 1,
        "which is the block the chain's growth drew"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(over_capacity as *const RcHeader) },
        crate::refcount::SlotState::Free,
        "and the slot carries no mark"
    );

    drop(window);
    assert_eq!(gc_blocks(), held_before, "the close gave the block back");

    unsafe { crate::refcount::set_header_refcount(stamped, 0) };
    unsafe { crate::memory::stdapi::ll_free(stamped as *mut u8) };
}

/// A death past a full region in a stamped block **this thread does not
/// own** takes a record: the mark is the owner's to make and the owner's
/// to clear, and a slot marked in a stranger's block is one this window's
/// walk cannot return: the return is the owner's free list and its `used`.
///
/// The block here is one an exited thread abandoned with a live occupant
/// still in it, which leaves it owned by nobody — the shape a thread that
/// dies inside another thread's reach leaves behind.
#[test]
fn a_stamped_block_this_thread_does_not_own_is_recorded_rather_than_marked() {
    let _guard = test_guard();

    // The entity outlives its thread: `ll_thread_exit` puts a block with a
    // live occupant on the abandoned list rather than back in the pool.
    let foreign = std::thread::spawn(|| {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the second thread"
        );
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        let header = unsafe { live_entity(slot, 1) };
        crate::memory::heap::ll_thread_exit();
        header as usize
    })
    .join()
    .expect("the second thread finished") as *mut RcHeader;

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

    unsafe { crate::refcount::set_header_refcount(foreign, 0) };
    unsafe { crate::memory::stdapi::ll_free(foreign as *mut u8) };

    assert_eq!(
        deferred_slot_count(),
        RETURNS_BASE_RECORDS + 1,
        "the return was recorded rather than marked"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(foreign) },
        crate::refcount::SlotState::Free,
        "and no mark stands in a block this thread cannot sweep"
    );

    drop(window);
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

/// A large entity dying past a full region, in a block no trace has met,
/// still takes a record — and therefore still draws the block the mark exists
/// to stop drawing.
///
/// Its own block header word is the stamp, and this is the case that gives
/// that word its work: a mark under an untouched row stands on memory no
/// window of this thread is holding, and the run would never be unmapped.
#[test]
fn an_unmet_large_entity_past_the_region_still_takes_a_record() {
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
    unsafe { crate::memory::stdapi::ll_free(unmet as *mut u8) };

    assert_eq!(
        deferred_slot_count(),
        RETURNS_BASE_RECORDS + 1,
        "the return was recorded rather than marked"
    );
    assert_eq!(
        gc_blocks(),
        held_before + 1,
        "which is the block the chain's growth drew"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(unmet) },
        crate::refcount::SlotState::Free,
        "and the run carries no mark"
    );

    drop(window);
    assert_eq!(gc_blocks(), held_before, "the close gave the block back");
    assert!(
        !crate::memory::large_entity::snapshot().contains(&unmet_block),
        "and replayed the run's unmap"
    );

    unsafe { crate::refcount::set_header_refcount(met, 0) };
    unsafe { crate::memory::stdapi::ll_free(met as *mut u8) };
}

/// A retained survivor dying past a full region, in a block no trace has
/// stamped, still takes a record. The block's own occupant count would
/// otherwise never fall, and the block would never go home.
#[test]
fn an_unstamped_retained_survivor_past_the_region_still_takes_a_record() {
    let _guard = test_guard();
    let (_stamped_arena, stamped_holder, stamped_survivor, stamped_block) =
        unsafe { retained_survivor() };
    let (_arena, holder, survivor, block) = unsafe { retained_survivor() };
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
        RETURNS_BASE_RECORDS + 2,
        "the survivor and its holder were both recorded rather than marked"
    );
    assert_eq!(
        gc_blocks(),
        held_before + 1,
        "which is the block the chain's growth drew for the pair"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(survivor) },
        crate::refcount::SlotState::Free,
        "and no mark stands on memory this window is not holding"
    );

    drop(window);
    assert_eq!(gc_blocks(), held_before, "the close gave the block back");
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "and replayed the survivor's return, which emptied its block"
    );

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

/// A panic inside the close leaves no block listed and no mark standing.
///
/// What the unwind may not leave behind is a block whose link word still
/// names a list nothing heads: membership is that word against null, so such
/// a block would refuse every later mark of itself, and a mark left standing
/// on it would reach an adopting thread through abandonment. The memory
/// itself is not returned here and leaks with the records the same unwind
/// loses (`PLAN.md` S43.6).
///
/// The panic is staged inside the replay by the assertion that guards a
/// live free: a recorded slot whose refcount is raised while the window is
/// open is one `ll_free` refuses in a test build.
#[test]
fn a_panic_in_the_close_leaves_no_block_listed() {
    let _guard = test_guard();

    let victim = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    let recorded = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!victim.is_null() && !recorded.is_null());
    let victim = unsafe { live_entity(victim, 1) };
    let recorded = unsafe { dead_entity(recorded) };
    let block = (victim as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8;

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), victim, 1) };
    unsafe { crate::memory::stdapi::ll_free(recorded as *mut u8) };

    for _ in 0..RETURNS_BASE_RECORDS - 1 {
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

    // The slot is on no free list and its block counts it, which is the leak
    // S43.6 owns; the recorded slot the panic came from is left as it stands.
    unsafe { crate::refcount::set_header_refcount(recorded, 0) };
}

/// A collection that took no mark walks no slot at its close, which is the
/// whole of what this step costs an ordinary collection: the list is empty,
/// so no block is read and `clear_touched_rows` keeps the one store per
/// touched block it makes without it.
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
        "a collection that listed no block reads no slot at its close"
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
    let (_, holder_stride, _) = unsafe { crate::memory::heap::entity_block_slots(holder_block) };
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
