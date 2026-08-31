use super::*;

use crate::cycle::arena::ShadowArena;
use crate::cycle::row::{Edge, edge_to};
use crate::cycle::shadow;
use crate::memory::Arena;
use crate::memory::block_pool::{BLOCK_KIND_FREE, BLOCK_KIND_RETAINED, BlockHeader, test_guard};
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

fn met(answer: crate::cycle::arena::Met) -> *mut u32 {
    match answer {
        crate::cycle::arena::Met::Row { row, .. } => row,
        other => panic!("the arena refused a row: {other:?}"),
    }
}

unsafe fn meet(arena: &mut ShadowArena, entity: *mut RcHeader, count: u32) -> *mut u32 {
    let Edge::Interior(row) = (unsafe { edge_to(entity) }) else {
        panic!("the entity heap did not resolve to a shadow row");
    };
    met(unsafe { arena.meet(row, count) })
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

/// Red without the trace window: the free-list is LIFO, so the allocation
/// below receives `dead` and `meet` returns the row already initialised from
/// the dead occupant's count zero.
#[test]
fn a_reused_slot_cannot_inherit_the_dead_occupants_row() {
    let _guard = test_guard();
    let dead = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!dead.is_null());
    let dead = unsafe { dead_entity(dead) };

    let mut window = TraceWindow::open();
    let row = unsafe { meet(window.arena(), dead, 0) };
    assert!(!row.is_null());
    assert_eq!(unsafe { shadow::count(*row) }, 0);

    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    assert_eq!(parked_count(), 1);

    let fresh = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!fresh.is_null());
    assert_ne!(
        fresh, dead as *mut u8,
        "the traced slot was reused mid-window"
    );
    let fresh = unsafe { live_entity(fresh, 7) };
    let fresh_row = unsafe { meet(window.arena(), fresh, 7) };
    assert_eq!(unsafe { shadow::count(*fresh_row) }, 7);

    unsafe { crate::refcount::set_header_refcount(fresh, 0) };
    unsafe { crate::memory::stdapi::ll_free(fresh as *mut u8) };
    drop(window);
    assert_eq!(parked_count(), 0);
}

#[test]
fn the_queue_window_may_close_before_the_trace_window() {
    let _guard = test_guard();
    let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!slot.is_null());
    let header = unsafe { dead_entity(slot) };
    unsafe {
        crate::refcount::update_header_flags(header, |flags| flags | crate::refcount::ENROLLED)
    };

    let window = TraceWindow::open();
    unsafe { crate::memory::stdapi::ll_free(slot) };
    assert_eq!(parked_count(), 0, "the queue entry is the first record");

    unsafe { crate::refcount::clear_enrolled(header) };
    unsafe { crate::memory::stdapi::ll_free(slot) };
    assert_eq!(parked_count(), 1, "the collection takes over the return");

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
        crate::refcount::update_header_flags(header, |flags| flags | crate::refcount::ENROLLED)
    };

    let window = TraceWindow::open();
    unsafe { crate::memory::stdapi::ll_free(slot) };
    assert_eq!(parked_count(), 0, "the queue entry is the first record");
    drop(window);
    assert_eq!(
        parked_count(),
        0,
        "the trace close leaves the queue entry standing"
    );

    let other = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert_ne!(other, slot, "one closed window released through the other");

    unsafe { crate::refcount::clear_enrolled(header) };
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
    let mut window = TraceWindow::open();
    let row = unsafe { meet(window.arena(), survivor, 1) };
    assert_eq!(unsafe { shadow::count(*row) }, 1);

    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
    assert_eq!(unsafe { crate::refcount::header_refcount(survivor) }, 0);
    assert_eq!(
        parked_count(),
        2,
        "the retained survivor and its heap holder both park"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_RETAINED,
        "the last occupant returned the whole block under its trace row"
    );

    drop(window);
    assert_eq!(parked_count(), 0);
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
        crate::refcount::update_header_flags(survivor, |flags| flags | crate::refcount::ENROLLED);
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_RETAINED,
        "the entry let the last occupant return its whole block"
    );

    unsafe {
        crate::refcount::clear_enrolled(survivor);
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

    let mut window = TraceWindow::open();
    unsafe { meet(window.arena(), entity, 0) };
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

    let mut window = TraceWindow::open();
    unsafe { meet(window.arena(), entity, 0) };
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
