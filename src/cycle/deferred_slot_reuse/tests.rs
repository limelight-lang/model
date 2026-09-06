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
    // The withheld returns' stack writes the survivor's byte 8, so a survivor
    // is at least its header and that word
    // (`crate::cycle::deferred_slot_reuse::withheld_link`). A slotted entity
    // has the smallest size class behind it; a promoted survivor takes its
    // bytes from the class instead, so the class is asked.
    assert!(
        unsafe { (*survivor_class).object_size } as usize
            >= crate::memory::heap::FREE_LIST_LINK_OFFSET + size_of::<*mut u8>(),
        "a survivor of this class has no room for the stack's link"
    );
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

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    // The row is what makes this collection one that has met the slot's block;
    // a death in memory it never met is returned at once and would leave the
    // two windows with nothing to hand over.
    assert!(!unsafe { ensure_row(window.arena(), header, 0) }.is_null());
    unsafe { crate::memory::stdapi::ll_free(slot) };
    assert_eq!(
        deferred_slot_count(),
        0,
        "the queue entry is what withholds it"
    );

    // The retirement hands the slot back before it offers it again: the free
    // above took it and the candidate arm then refused the return, so a free
    // without this clear reads as a repeat (`crate::refcount::DEAD_IN_PLACE`).
    unsafe { crate::refcount::clear_dead_in_place(header) };
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
        "the queue entry keeps the slot, so this window withholds nothing"
    );
    drop(window);
    assert_eq!(
        deferred_slot_count(),
        0,
        "the trace close leaves the queue entry standing"
    );

    let other = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert_ne!(other, slot, "one closed window released through the other");

    unsafe { crate::refcount::clear_dead_in_place(header) };
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
    // The holder stands in a heap block of its own, and its death is withheld
    // only where this collection has met that block.
    assert!(!unsafe { ensure_row(window.arena(), holder as *mut RcHeader, 1) }.is_null());

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
        "the row sweep precedes the last occupant's withheld return"
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
        crate::refcount::clear_dead_in_place(survivor);
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
        "closing the trace made the OS-direct return"
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
        "the trace window reached the global allocator: open, withhold \
         and close are all manager-backed"
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
    let mut window = ActiveTrace::open().expect("this thread's workspace is in hand");
    let (heap, pool) = crate::test_support::allocation_probe::take_allocations();
    assert_eq!(
        (heap, pool),
        (0, 0),
        "the open stands on the workspace and asks no allocation path"
    );
    assert_eq!(gc_blocks(), held_before);

    // Stamped before the reading below opens, so what that reading covers is
    // the withheld return and not the row this collection needs to have met
    // the block at all.
    assert!(!unsafe { ensure_row(window.arena(), dead, 0) }.is_null());
    let _ = crate::test_support::allocation_probe::take_allocations();

    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    let (heap, pool) = crate::test_support::allocation_probe::take_allocations();
    assert_eq!(
        (heap, pool),
        (0, 0),
        "and the withheld return is threaded through the dead entity itself"
    );
    assert_eq!(deferred_slot_count(), 1);

    drop(window);
    assert_eq!(gc_blocks(), held_before, "the close drew a block");
}

#[test]
fn an_aborted_window_returns_what_it_withheld_with_both_allocation_paths_refusing() {
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
        "the abort drew nothing: both returns stood in the dying entities themselves"
    );

    let reused = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(
        reused == first as *mut u8 || reused == second as *mut u8,
        "the abort lost a physical return"
    );
    unsafe { dead_entity(reused) };
    unsafe { crate::memory::stdapi::ll_free(reused) };
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
    let mut window = ActiveTrace::open().expect("the workspace was in hand before the refusal");
    assert!(!unsafe { ensure_row(window.arena(), dead, 0) }.is_null());
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
        "and enters none either: a control line that never left the workspace's \
         own region has no residue of its own to enter"
    );
}

/// The reserve exists for the pressure collection, where a refused pool is
/// what started the collection at all. A window that drew a block at its open
/// would spend the reserve on every such collection before a single return was
/// withheld; standing in the workspace, it spends none — and no death spends
/// any either, each being held in the dying entity's own memory
/// (`neither_the_window_nor_the_withheld_return_draws_a_manager_block`).
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

/// A death in a block the trace has stamped is held in the slot itself: no
/// block is drawn and nothing is recorded.
///
/// This is the whole of the withholding path, with no process end anywhere on
/// it and no memory asked of anyone (`dev/DECISIONS.md`, "one stack through
/// the dead entity holds every withheld return").
///
/// **The header says nothing about the withholding.** `ll_free` marks a
/// withheld death and a returned one alike, so what pins this path is the walk
/// and the allocation below: the census passes over the slot and the class
/// cannot hand it out. What returns it is the close, which is
/// `the_close_returns_a_marked_slot`'s subject.
#[test]
fn a_stamped_slot_is_marked_and_stacked() {
    let _guard = test_guard();
    let held_before = gc_blocks();

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

    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };

    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };

    assert_eq!(
        deferred_slot_count(),
        1,
        "the stack holds the withheld return"
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

    // The allocator is the witness, not the state word: the return re-enters
    // `ll_free`, which takes the slot again on its way to the free list, so the
    // word reads alike on both sides of the close
    // (`crate::refcount::DEAD_IN_PLACE`).
    let after_close = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert_eq!(
        after_close, victim as *mut u8,
        "the close returned the slot the mark held, and the class hands it out \
         again — the return itself is `the_close_returns_a_marked_slot`'s subject"
    );

    unsafe { dead_entity(served) };
    unsafe { crate::memory::stdapi::ll_free(served) };
    unsafe { dead_entity(after_close) };
    unsafe { crate::memory::stdapi::ll_free(after_close) };
}

/// The stack is threaded through byte 8 of each dead entity: the newest
/// withheld slot holds the address of the one below it there, and the oldest
/// holds null.
///
/// The case reads that word off the offset `memory::heap` names rather than
/// through the module's own helper, because the claim is about the two links
/// sharing one word: the return of a slotted death writes its free-list link
/// exactly there, which is why the pop reads the link before it hands the slot
/// over.
#[test]
fn a_withheld_return_names_the_one_below_it_through_byte_eight() {
    let _guard = test_guard();

    let keeper = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 4) };
    let first = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 4) };
    let second = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 4) };
    assert!(!keeper.is_null() && !first.is_null() && !second.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    let first = unsafe { live_entity(first, 1) };
    let second = unsafe { live_entity(second, 1) };
    let block = (first as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8;
    assert_eq!(
        (second as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8,
        block,
        "one row stamps the block both deaths stand in"
    );

    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), first, 1) };

    for entity in [first, second] {
        unsafe { crate::refcount::set_header_refcount(entity, 0) };
        unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
    }
    assert_eq!(deferred_slot_count(), 2, "both deaths were withheld");

    let link_of = |entity: *mut RcHeader| unsafe {
        (entity as *mut u8)
            .add(crate::memory::heap::FREE_LIST_LINK_OFFSET)
            .cast::<*mut u8>()
            .read()
    };
    assert_eq!(
        link_of(second),
        first as *mut u8,
        "the newest withheld slot names the one below it"
    );
    assert!(
        link_of(first).is_null(),
        "and the oldest names null, which is what ends the pop"
    );

    drop(window);

    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before - 2,
        "the close returned both, which the block's own count is what shows: each \
         return re-enters `ll_free` and is taken again, so the state word reads \
         alike on both sides of the close"
    );

    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// The retained population takes the same word: a survivor withheld after
/// another names it through byte 8 of its own header.
///
/// A retained survivor has no size class behind it — its bytes are the
/// object's own, rounded to eight by the arena it was promoted out of — so
/// what says the link fits is the class's own size, asserted in
/// `retained_survivor_pair` before the promotion. This case reads the link
/// back; the room is the fixture's assertion, an overrun into the next
/// survivor's header reading the same value here.
#[test]
fn a_withheld_retained_survivor_names_the_one_below_it_through_byte_eight() {
    let _guard = test_guard();
    let (_arena, holders, survivors, block) = unsafe { retained_survivor_pair() };

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    for survivor in survivors {
        unsafe { ensure_row(window.arena(), survivor, 1) };
    }

    // The holders stand in a block no row addresses, so their own deaths are
    // returned at once and the stack holds the two survivors alone.
    for holder in holders {
        unsafe {
            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }
    }
    assert_eq!(deferred_slot_count(), 2, "both survivors were withheld");

    let link_of = |entity: *mut RcHeader| unsafe {
        (entity as *mut u8)
            .add(crate::memory::heap::FREE_LIST_LINK_OFFSET)
            .cast::<*mut u8>()
            .read()
    };
    assert_eq!(
        link_of(survivors[1]),
        survivors[0] as *mut u8,
        "the survivor withheld second names the one withheld first"
    );
    assert!(
        link_of(survivors[0]).is_null(),
        "and the first names null, which is what ends the pop"
    );

    drop(window);
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the close returned both survivors, which emptied their block"
    );
}

/// A collection that withheld nothing reads no slot at its close.
///
/// What the probe answers is the size of the close, and this is its zero: the
/// stack is popped once per withheld death and a collection that withheld none
/// touches no slot at all. A close that walked a block it had stamped would
/// read one here.
#[test]
fn a_collection_that_withheld_nothing_reads_no_slot() {
    let _guard = test_guard();

    // Two classes, so the row and the death stand in blocks of their own: the
    // collection meets one block and the death happens in the other.
    let met = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    let victim = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 2) };
    assert!(!met.is_null() && !victim.is_null());
    let met = unsafe { live_entity(met, 1) };
    let victim = unsafe { dead_entity(victim) };
    let victim_block = (victim as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8;

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), met, 1) };
    assert!(
        unsafe { crate::memory::heap::block_shadow(victim_block) }.is_null(),
        "the row stamped the other class's block, not this one"
    );

    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(
        deferred_slot_count(),
        0,
        "the death stands in a block this collection never met"
    );

    let _ = take_marked_slots_visited();
    drop(window);
    assert_eq!(
        take_marked_slots_visited(),
        0,
        "a collection that withheld nothing reads no slot at its close"
    );

    unsafe { crate::refcount::set_header_refcount(met, 0) };
    unsafe { crate::memory::stdapi::ll_free(met as *mut u8) };
}

/// A death in a block no row of this collection addresses is returned at
/// once.
///
/// The block's shadow pointer is the whole test, and this is the case that
/// justifies the load: a block this collection never touched carries no row
/// for any of its slots, so a new occupant of this one inherits nothing and
/// the window has nothing to withhold.
#[test]
fn an_unstamped_block_is_returned_at_once() {
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
        0,
        "a death in memory this collection never met is returned at once"
    );
    assert_eq!(gc_blocks(), held_before, "and a block was drawn to hold it");
    assert_eq!(
        unsafe { crate::refcount::slot_state(over_capacity as *const RcHeader) },
        crate::refcount::SlotState::DeadInPlace,
        "the slot reads as one `ll_free` holds, the return itself being what the \
         block's count below shows"
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

/// A death in a stamped block **this thread does not own** is stacked like
/// every other: the close returns it through the block's own stack of
/// cross-thread frees, and walks no slot of the block, such a walk being
/// bounded by a cursor its owner moves.
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

    assert!(
        !unsafe { crate::memory::heap::block_is_owned_by_this_thread(block) },
        "the fill left the foreign block unadopted"
    );

    let occupancy_before = unsafe { crate::memory::heap::block_occupancy(block) };
    unsafe { crate::refcount::set_header_refcount(foreign, 0) };
    unsafe { crate::memory::stdapi::ll_free(foreign as *mut u8) };

    assert_eq!(
        deferred_slot_count(),
        1,
        "the return was withheld rather than made"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(foreign) },
        crate::refcount::SlotState::DeadInPlace,
        "the slot carries the mark"
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
        crate::refcount::SlotState::DeadInPlace,
        "the slot is neither live nor free, which is the whole of what the word \
         says on either side of a return; the return itself is the adoption \
         below"
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

/// A retained survivor dying in a block the trace has stamped carries the mark
/// in its own header, and its block still counts it: the count is what the
/// withheld return owes, so the block cannot go home while the mark stands.
///
/// The holder is given a row of its own, so that its own death is withheld
/// beside the survivor's rather than returned at once.
#[test]
fn a_stamped_retained_survivor_is_marked_and_stacked() {
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

    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    assert_eq!(
        deferred_slot_count(),
        2,
        "the survivor and its holder are both on the stack"
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

/// A pooled large entity dying under a row of its own carries the mark in the
/// entity header its block holds, and the block stays out of the pool until
/// the withheld return is made.
#[test]
fn a_stamped_pooled_large_entity_is_marked_and_stacked() {
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

    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };

    assert_eq!(
        deferred_slot_count(),
        1,
        "the stack holds the withheld return"
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

/// An OS-direct run dying under a row of its own carries the mark in the
/// entity header its run holds, and the mapping stands until the withheld
/// return is made.
#[test]
fn a_stamped_run_is_marked_and_stacked() {
    let _guard = test_guard();
    let entity = crate::memory::large_entity::alloc(crate::memory::block_pool::BLOCK_PAYLOAD + 1);
    assert!(!entity.is_null());
    let entity = unsafe { dead_entity(entity) };
    let block = BlockHeader::of_ptr(entity as *const u8) as usize;
    assert!(crate::memory::large_entity::snapshot().contains(&block));

    let held_before = gc_blocks();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), entity, 0) };

    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };

    assert_eq!(
        deferred_slot_count(),
        1,
        "the stack holds the withheld return"
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

/// A large entity dying in a block no trace has met is unmapped at once.
///
/// Its own block header word is the stamp, and this is the case that gives
/// that word its work: an untouched row is one no new occupant could inherit,
/// there being no new occupant of a run at all, so the window has nothing to
/// hold the run for.
#[test]
fn an_unmet_large_entity_is_returned_at_once() {
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

    // With both allocation paths refusing, so that the answer is read as the
    // one that asks for nothing rather than as the one that got lucky.
    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    unsafe { crate::memory::stdapi::ll_free(unmet as *mut u8) };
    drop(oom);

    assert_eq!(
        deferred_slot_count(),
        0,
        "a death in memory this collection never met is returned at once"
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

/// A retained survivor dying in a block no trace has stamped is returned at
/// once — and the return empties its block, which goes home inside the window.
#[test]
fn an_unstamped_retained_survivor_is_returned_at_once() {
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

    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    assert_eq!(
        deferred_slot_count(),
        0,
        "neither block carries a row, so both deaths are returned at once"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and a block was drawn to hold the pair"
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

    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };

    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was withheld"
    );

    let _ = take_marked_slots_visited();
    drop(window);

    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the slot is neither live nor free, which is the whole of what the word \
         says on either side of a return; the return itself is the block's \
         count below"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before - 1,
        "the return went through the owner's `used`, which is what retires a block"
    );
    assert_eq!(
        take_marked_slots_visited(),
        1,
        "the close read the one slot it withheld and no other"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and the close that made it drew nothing"
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

    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    assert_eq!(
        unsafe { crate::refcount::slot_state(survivor) },
        crate::refcount::SlotState::DeadInPlace,
        "the survivor's slot is neither live nor free"
    );
    assert_eq!(
        deferred_slot_count(),
        2,
        "the stack holds the survivor and the holder, which is what says both \
         deaths were withheld: the header reads the same for a return this \
         window never made"
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

    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(entity) },
        crate::refcount::SlotState::DeadInPlace,
        "the entity's slot is neither live nor free"
    );
    assert_eq!(
        deferred_slot_count(),
        1,
        "the stack holds it, which is what says the death was withheld: the \
         header reads the same for a return this window never made"
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

    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    assert_eq!(
        unsafe { crate::refcount::slot_state(survivor) },
        crate::refcount::SlotState::DeadInPlace,
        "the survivor's slot is neither live nor free"
    );
    assert_eq!(
        deferred_slot_count(),
        2,
        "the stack holds the survivor and the holder, which is what says both \
         deaths were withheld: the header reads the same for a return this \
         window never made"
    );

    drop(window);

    assert_eq!(
        unsafe { crate::refcount::slot_state(holder as *const RcHeader) },
        crate::refcount::SlotState::DeadInPlace,
        "the close returned the holder's own slot, whose block it did not retire; \
         the state word reads alike on both sides of that return, `ll_free` \
         taking the slot again as it makes it"
    );
    // The survivor's own header is not read back: its block reached the pool
    // at the return, and reading a slot of a returned block is what the close
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
        "and the close that made both returns drew nothing"
    );

    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// A retained block holding two marked survivors comes back once: the close
/// returns both, and the second return spends the block's last occupant
/// count, which is what hands the block to the pool.
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
            "both deaths were withheld"
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
        "the close returned both survivors and gave the block back once"
    );
    assert_eq!(gc_blocks(), held_before, "and drew nothing to do it");
}

/// The close returns a marked pooled large entity: its block goes back to the
/// pool at the close rather than at the next collection.
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

    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(entity) },
        crate::refcount::SlotState::DeadInPlace,
        "the entity's slot is neither live nor free"
    );
    assert_eq!(
        deferred_slot_count(),
        1,
        "the stack holds it, which is what says the death was withheld: the \
         header reads the same for a return this window never made"
    );

    drop(window);

    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the close returned the block the marked entity held"
    );
    assert_eq!(gc_blocks(), held_before, "and the close drew nothing");
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

    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(entity) },
        crate::refcount::SlotState::DeadInPlace,
        "the entity's slot is neither live nor free"
    );
    assert_eq!(
        deferred_slot_count(),
        1,
        "the stack holds it, which is what says the death was withheld: the \
         header reads the same for a return this window never made, and past \
         the close the run's memory is the operating system's and cannot be \
         read at all"
    );

    drop(window);

    assert!(
        !crate::memory::large_entity::snapshot().contains(&block),
        "the close unmapped the run the marked entity held"
    );
    assert_eq!(gc_blocks(), held_before, "and the close drew nothing");
}

/// The abort path returns marks as the ordered close does, and it does so
/// with both allocation paths refusing: the close asks no allocation path,
/// which is what lets a collection that ran out of memory give its marks
/// back.
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

    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };
    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was withheld"
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
        crate::refcount::SlotState::DeadInPlace,
        "the abort returned the marked slot, which the block's count below shows"
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
        crate::refcount::SlotState::DeadInPlace,
        "and made the return, which the owner's own allocation is what shows"
    );

    to_owner.send(()).expect("the owner is waiting");
    owner.join().expect("the owner thread finished");
}

/// The unwind's half for a slot of another thread's block: the drop clears the
/// mark and makes the return through the block's own stack of cross-thread
/// frees.
///
/// The panic is staged off `ll_free`'s own refusal rather than off the
/// injection: a withheld slot whose refcount is raised while the window is
/// open is one that entry point refuses in a test build. The sweep has run by
/// then, so the disposition is the returning one.
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

    let raiser = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 2) };
    assert!(!raiser.is_null());
    let raiser = unsafe { dead_entity(raiser) };

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), foreign, 1) };
    unsafe { ensure_row(window.arena(), raiser, 0) };

    // The foreign slot dies first and the raiser second, so the raiser is the
    // head: the close raises on its first pop, and the foreign slot's return is
    // the drop's own pass — which is the half this case is about.
    unsafe { crate::refcount::set_header_refcount(foreign, 0) };
    unsafe { crate::memory::stdapi::ll_free(foreign as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(foreign) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was withheld"
    );
    unsafe { crate::memory::stdapi::ll_free(raiser as *mut u8) };

    // The pop will reach this slot and find it reading live, which is the free
    // `ll_free` refuses.
    unsafe { crate::refcount::set_header_refcount(raiser, 1) };
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(window);
    }));
    assert!(refused.is_err(), "the pop was expected to refuse");

    assert_eq!(
        unsafe { crate::refcount::slot_state(foreign) },
        crate::refcount::SlotState::DeadInPlace,
        "the unwind made the return, which the adoption below is what shows"
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

    // The slot whose refusal raised is the one return the unwind lost: it
    // reached no free list, so this is its first return rather than a second.
    unsafe { crate::refcount::set_header_refcount(raiser, 0) };
    unsafe { crate::memory::stdapi::ll_free(raiser as *mut u8) };
}

/// The reset's whole-block sentinel — a return whose address is the block
/// header rather than an entity — is returned at once.
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
fn the_whole_block_sentinel_is_returned_at_once() {
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
        0,
        "the sentinel addresses a block no row of this collection names"
    );
    assert_eq!(gc_blocks(), held_before, "and a block was drawn to hold it");
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

/// The sentinel arm and the unstamped arm answer alike, and this is what
/// separates them: a whole-block sentinel arriving on a block a collection has
/// stamped fails a test build.
///
/// The state is built by hand and production cannot reach it — the sentinel is
/// issued by `promote::arena_reset_full` between `retain_block`'s clearing of
/// the collector line and the block's return, with no trace step in between,
/// which is why the arm carries an assertion rather than an answer. What the
/// case pins is that the arm is read at all: without it a sentinel would fall
/// to the arm below, whose `block_shadow` test would send the stack's link into
/// the block's own header.
///
/// Under `debug_assertions` alone: a release build carries no assertion, takes
/// the arm below and returns at once.
#[cfg(debug_assertions)]
#[test]
fn a_sentinel_on_a_stamped_block_fails_a_test_build() {
    let _guard = test_guard();
    let (_arena, holder, survivor, block) = unsafe { retained_survivor() };
    let held_before = gc_blocks();

    // The reset's own hold, so the block outlives its last occupant and the
    // sentinel free below is the one that returns it.
    unsafe { crate::memory::retained::pin(block as usize) };
    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), survivor, 0) };
    assert!(
        !unsafe { crate::memory::heap::block_shadow(block as *mut u8) }.is_null(),
        "the row stamped the block the sentinel is about to name"
    );
    assert!(
        unsafe { crate::memory::retained::hold_released(block as usize) },
        "the pin was the last thing holding the block"
    );

    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        crate::memory::stdapi::ll_free(block as *mut u8);
    }));
    let raised = refused.expect_err("the sentinel arm was expected to refuse");
    // Both payload shapes, because an assertion with no argument raises a
    // `&'static str` where one with arguments raises a `String`.
    let message = raised
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| raised.downcast_ref::<&'static str>().copied())
        .unwrap_or_default();
    assert!(
        message.contains("the reset's whole-block sentinel reached a stamped block"),
        "and it says which rule it refused on: {message}"
    );

    // The refusal read the block and changed nothing of it, so the close and
    // the return below are the ones an unstamped sentinel would have made.
    drop(window);
    unsafe { crate::memory::stdapi::ll_free(block as *mut u8) };
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the block went home once the stamp was gone"
    );
    assert_eq!(gc_blocks(), held_before, "and nothing was drawn to do it");
}

/// A block another thread's when one of its slots died, and this thread's by
/// the time the next slot of it dies, returns each of the two exactly once.
///
/// `Heap::adopt` runs on the ordinary refill path, so ownership moves inside
/// an open window and one block ends up with two slots on the window's stack,
/// pushed under different owners. What the case pins is that the owner decides
/// nothing here: the close pops both through `ll_free`, which reads the owner
/// word itself and sends each return down the path that word names.
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

    let before_adoption = pair[0] as *mut RcHeader;
    let after_adoption = pair[1] as *mut RcHeader;
    let block =
        crate::memory::block_pool::BlockHeader::of_ptr(before_adoption as *const u8) as *mut u8;
    assert_eq!(
        block,
        crate::memory::block_pool::BlockHeader::of_ptr(after_adoption as *const u8) as *mut u8,
        "the two occupants share one block"
    );

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), before_adoption, 1) };
    assert!(
        !unsafe { crate::memory::heap::block_shadow(block) }.is_null(),
        "the row stamped the block both deaths stand in"
    );

    unsafe { crate::refcount::set_header_refcount(before_adoption, 0) };
    unsafe { crate::memory::stdapi::ll_free(before_adoption as *mut u8) };

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

    unsafe { crate::refcount::set_header_refcount(after_adoption, 0) };
    unsafe { crate::memory::stdapi::ll_free(after_adoption as *mut u8) };
    assert_eq!(
        deferred_slot_count(),
        2,
        "one stack holds both slots of the block, whoever owned it at each death"
    );

    let occupancy_before = unsafe { crate::memory::heap::block_occupancy(block) };
    drop(window);

    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupancy_before - 2,
        "the close did not return the two marked slots exactly once between them"
    );
    for (slot, name) in [
        (before_adoption, "pre-adoption"),
        (after_adoption, "post-adoption"),
    ] {
        assert_eq!(
            unsafe { crate::refcount::slot_state(slot) },
            crate::refcount::SlotState::DeadInPlace,
            "the close left the {name} slot as `ll_free` holds it"
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
/// the window's drop with the rows still standing, and the marks would be
/// abandoned rather than returned — which is the disposition
/// `WindowControl::swept` decides.
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

    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };
    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was withheld"
    );

    let armed = crate::cycle::arena::InjectedResetFailure::arm();
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(window);
    }));
    assert!(refused.is_err(), "the reset was expected to raise");
    drop(armed);

    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the return was made before the blocks went back, which the block's count \
         below shows"
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

/// A window dropped before its rows are gone abandons what it withheld: no
/// memory goes back, a slot handed back under a row that names it being the
/// reuse the window exists to prevent. The abandoned slot keeps the bit
/// `ll_free` took, which is true of it — this window hands nothing back
/// ([`crate::refcount::DEAD_IN_PLACE`]).
///
/// Read at the flag rather than behind a panic. Since S44.6 the close sweeps
/// before anything that can raise, so no panic site in the crate reaches this
/// arm and `WindowControl::swept` is what says which disposition the
/// drop takes — the fact read rather than inferred from where an unwind came
/// from (`dev/DECISIONS.md`, "the row sweep runs ahead of the candidate
/// restore"). The case therefore opens the window's own structure over an
/// arena's region and drops it without telling it the rows are gone.
#[test]
fn a_window_dropped_before_its_rows_are_gone_abandons_what_it_withheld() {
    const CLASS: usize = ENTITY_SIZE * 9;

    let _guard = test_guard();
    let keeper = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    let victim = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert!(!keeper.is_null() && !victim.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    let victim = unsafe { live_entity(victim, 1) };
    let block = (victim as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8;

    let mut arena = TraceScratchArena::open().expect("this thread's workspace is in hand");
    // Safety: the region is the arena's own, and the window below dies before
    // the arena does, which is the order `ActiveTrace` gives by field order.
    let returns = unsafe { WithheldReturns::open(arena.withheld_returns_region()) };
    DEFERRED_RETURNS.with(|control| control.set(returns.control));

    unsafe { ensure_row(&mut arena, victim, 1) };
    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };
    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(deferred_slot_count(), 1, "the death was withheld");

    // No `rows_are_gone`, which is the whole of the case: the rows still stand
    // over the block when the window falls.
    drop(returns);

    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the slot is neither live nor free, which a returned slot reads too; \
         what says this window returned nothing is the block's count below"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before,
        "and no return was made"
    );
    assert_eq!(
        deferred_slot_count(),
        0,
        "the window is closed, which is what a count of zero reads after a drop"
    );

    drop(arena);
    let served = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert_ne!(
        served, victim as *mut u8,
        "the abandoned slot reached no free list"
    );

    // The abandoned slot is out of circulation by design: nothing handed it
    // back, so `ll_free` refuses it and the case has to hand it back itself
    // before it can leave the class as it found it
    // (`crate::refcount::DEAD_IN_PLACE`).
    unsafe { crate::refcount::clear_dead_in_place(victim) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    unsafe { dead_entity(served) };
    unsafe { crate::memory::stdapi::ll_free(served) };
    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// An unwind out of the candidate restore returns what the window withheld:
/// the sweep has run by then, so the rows that would make a return a reuse are
/// gone and the drop's own pass gives every stacked slot back.
///
/// This is what S44.6's order buys, and the restore's refusal is the only
/// panic site the drop has ahead of its own returns — a candidate registered
/// while the batch is detached (`queue::restore_candidates`). Before the sweep
/// was hoisted this same unwind abandoned the slot and the block holding it
/// for the life of the process.
///
/// In a child process, as the queue's own cases of that refusal are: the
/// refused restore leaves the batch's chain owned by nothing, and a test that
/// unwound through it in this process would hand every case after it a pool
/// two blocks short.
#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn an_unwind_out_of_the_candidate_restore_returns_what_was_withheld() {
    const CHILD: &str = "LL_TRACE_ABANDON_CHILD";
    const CLASS: usize = ENTITY_SIZE * 8;

    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(
                "cycle::deferred_slot_reuse::tests::\
                 an_unwind_out_of_the_candidate_restore_returns_what_was_withheld",
            )
            .arg("--nocapture")
            .env(CHILD, "1")
            .output()
            .expect("the child runs this case again");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "the child read the returning close: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        // A child that matched no test name also exits zero, so the count is
        // read rather than the status alone: a rename that left this literal
        // behind would otherwise leave the case passing over nothing.
        assert!(
            stdout.contains("1 passed"),
            "the child ran this case rather than an empty filter: {stdout}"
        );
        return;
    }

    let _guard = test_guard();

    // A candidate before the window, so the batch the trace detaches holds a
    // chain: a restore of an empty batch returns without reaching the refusal.
    let root = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!root.is_null());
    let root = unsafe { live_entity(root, 2) };
    assert!(
        !unsafe { crate::refcount::ll_release(root) },
        "the non-final decrement registered a candidate"
    );

    let keeper = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    let victim = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert!(!keeper.is_null() && !victim.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    let victim = unsafe { live_entity(victim, 1) };
    let block = (victim as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8;

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    window.detach_candidates();
    unsafe { ensure_row(window.arena(), victim, 1) };

    // The refusal the drop raises on: a lane refilled while the batch is out.
    let later = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!later.is_null());
    let later = unsafe { live_entity(later, 2) };
    assert!(!unsafe { crate::refcount::ll_release(later) });

    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };
    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was withheld"
    );

    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(window);
    }));
    assert!(refused.is_err(), "the restore was expected to refuse");

    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the slot reads as one `ll_free` holds, on both sides of the return"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before - 1,
        "and the return was made, the sweep having run before the refusal"
    );

    let served = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert_eq!(served, victim as *mut u8, "the slot is the class's again");

    unsafe { dead_entity(served) };
    unsafe { crate::memory::stdapi::ll_free(served) };
    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// A panic inside one return leaves the slots below it standing on the stack,
/// and the drop's own pass gives them back.
///
/// The pop takes the head off the stack before it hands the slot over, so the
/// slot whose return raises is named by nothing and the slots behind it are
/// still named by the head. A pop that moved the head after the return would
/// leave the head naming a slot the free list has taken back.
///
/// The panic is staged off `ll_free`'s own refusal, on the close's first
/// return, which is the newest withheld slot: a slot whose refcount is raised
/// while the window is open is one that entry point refuses at its head, with
/// the mark already off and the return unmade.
///
/// **Which return was lost is read off the free list**, the headers saying
/// nothing about it: a slot whose return was made and one whose return was
/// interrupted both read `DeadInPlace`, `ll_free` having taken the first again
/// at its return and never handed the second back. The block's free list is
/// LIFO and holds exactly what this close gave back, so the allocation at the
/// end names that slot.
///
/// The newest slot is the panic's own leak and the case returns it by hand,
/// with no window open, so the class is left as it was found.
#[test]
fn a_panic_inside_one_return_gives_back_the_slots_below_it() {
    let _guard = test_guard();

    let keeper = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert!(!keeper.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    let mut dying = [std::ptr::null_mut(); 3];
    for slot in &mut dying {
        let entity = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
        assert!(!entity.is_null());
        *slot = unsafe { live_entity(entity, 1) };
    }
    let [first, second, third] = dying;
    let block = (first as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8;
    for entity in [second, third] {
        assert_eq!(
            (entity as usize & !crate::memory::block_pool::BLOCK_MASK) as *mut u8,
            block,
            "the three deaths stand in one block, whose occupancy counts the returns made"
        );
    }

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), first, 1) };

    // `third` dies last, so it is the head of the stack and the close's first
    // return is its own. Three rather than two, so that a pass which gave one
    // slot back and stopped is not the same reading as one that gave back
    // every slot behind the raising return.
    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };
    for entity in dying {
        unsafe { crate::refcount::set_header_refcount(entity, 0) };
        unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
        assert_eq!(
            unsafe { crate::refcount::slot_state(entity) },
            crate::refcount::SlotState::DeadInPlace,
            "all three deaths were withheld"
        );
    }

    // The pop will reach `third` first and find it reading live, which is the
    // free `ll_free` refuses at its head — the mark already off, the return
    // unmade.
    unsafe { crate::refcount::set_header_refcount(third, 1) };
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(window);
    }));
    assert!(refused.is_err(), "the close was expected to raise");

    assert_eq!(
        unsafe { crate::refcount::slot_state(third) },
        crate::refcount::SlotState::Live,
        "the unwind took the mark off the slot whose return it raised inside, \
         and the raised count is what that return refused on"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before - 2,
        "two returns were made, and the raising slot's is the one they could not make"
    );

    // The free list is LIFO and holds what this close gave back: `second` went
    // on it first and `first` after it, so the block hands them out in that
    // order reversed.
    let served = [
        unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) },
        unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) },
    ];
    assert_eq!(
        served,
        [first as *mut u8, second as *mut u8],
        "the drop's own pass gave back every slot below the raising return"
    );

    // The raising slot reached no free list, so this is its first return
    // rather than a second.
    unsafe { crate::refcount::set_header_refcount(third, 0) };
    unsafe { crate::memory::stdapi::ll_free(third as *mut u8) };
    for slot in served {
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// A panic inside the return of one retained survivor gives back the survivor
/// below it, and gives its block back with it.
///
/// The two survivors stand in retained blocks of their own, which is what says
/// *which* return the unwind made: a retained survivor's return is an atomic
/// decrement of its block's occupant count, so the block whose only occupant
/// went back reaches the pool and the other one stands.
///
/// The survivor whose return raised is still an occupant of its block, and the
/// case returns it by hand afterwards — with no window open, so the block goes
/// home the way an ordered close would have sent it.
#[test]
fn a_panic_inside_a_retained_survivors_return_gives_back_the_one_below_it() {
    let _guard = test_guard();
    let (_first_arena, first_holder, first_survivor, first_block) = unsafe { retained_survivor() };
    let (_second_arena, second_holder, second_survivor, second_block) =
        unsafe { retained_survivor() };
    assert_ne!(
        first_block, second_block,
        "each survivor stands in a retained block of its own, which is what \
         tells the two returns apart"
    );
    let held_before = gc_blocks();

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    for survivor in [first_survivor, second_survivor] {
        unsafe { ensure_row(window.arena(), survivor, 1) };
    }

    // The holders take no row, so their own slots stand in a block this
    // collection never met and their deaths are returned at once. The second
    // survivor dies last, which puts it at the head of the stack and makes its
    // return the one the close raises inside.
    for holder in [first_holder, second_holder] {
        unsafe {
            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }
    }

    for survivor in [first_survivor, second_survivor] {
        assert_eq!(
            unsafe { crate::refcount::slot_state(survivor) },
            crate::refcount::SlotState::DeadInPlace,
            "both deaths were withheld"
        );
    }

    // As the slotted case stages it: a raised count is the free `ll_free`
    // refuses at its head, with the mark already off and the return unmade.
    unsafe { crate::refcount::set_header_refcount(second_survivor, 1) };
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(window);
    }));
    assert!(refused.is_err(), "the close was expected to raise");

    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*first_block).kind) },
        BLOCK_KIND_FREE,
        "the drop's own pass returned the survivor below the raising one, and \
         that emptied its block"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*second_block).kind) },
        BLOCK_KIND_RETAINED,
        "and the raising survivor is still its own block's occupant"
    );
    assert_eq!(
        unsafe { crate::memory::retained::live_occupant_count(second_block as usize) },
        1
    );

    // The raising survivor is the panic's own leak, and its return is what
    // empties the block it stands in.
    unsafe { crate::refcount::set_header_refcount(second_survivor, 0) };
    unsafe { crate::memory::stdapi::ll_free(second_survivor as *mut u8) };
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*second_block).kind) },
        BLOCK_KIND_FREE,
        "the return the unwind lost is the one this makes"
    );
    assert_eq!(gc_blocks(), held_before, "and drew nothing to do any of it");
}
