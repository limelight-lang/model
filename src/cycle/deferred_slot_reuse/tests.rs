use super::*;

use crate::class::Class;
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

/// The class every fake entity of this module carries, built once.
///
/// A header alone is not enough for a fake that reads live: the crate's heap
/// census walks every slot of every carved region whose state is `Live` and
/// reads its class word, so an entity of kind `Object` with garbage there is a
/// slot the census dereferences. The fakes outlive their case — the block
/// holding them is abandoned rather than emptied when the thread that filled it
/// exits — so the census that reads them belongs to some later case
/// (`memory::heap::for_each_entity_slot`, `cells::heap_census`).
fn fake_class() -> *const Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let class = *CLASS.get_or_init(|| {
        // The build draws immortal memory, so `FORCE_OOM` refuses it and
        // answers null. A null cached here would stand for the life of the
        // process and give every later fake the empty class word this write
        // exists to prevent, so the refusal ends the case instead.
        let class = crate::class::ClassBuilder::new("DeferredSlotFake").build();
        assert!(
            !class.is_null(),
            "the fake class is built under a refused allocation; build it before the window"
        );
        class as usize
    });
    class as *const Class
}

unsafe fn dead_entity(slot: *mut u8) -> *mut RcHeader {
    let header = slot as *mut RcHeader;
    unsafe {
        header.write(RcHeader::new(
            MemoryCategory::GcHeap,
            EntityKind::Object.to_flags(),
        ));
        crate::refcount::set_header_refcount(header, 0);
        (&raw mut (*(slot as *mut Object)).class).write(fake_class());
    }
    header
}

unsafe fn live_entity(slot: *mut u8, count: u32) -> *mut RcHeader {
    let header = unsafe { dead_entity(slot) };
    unsafe { crate::refcount::set_header_refcount(header, count) };
    header
}

/// The block an entity stands in, as the `*mut u8` the heap's readers take.
fn block_of<T>(entity: *mut T) -> *mut u8 {
    BlockHeader::of_ptr(entity as *const u8) as *mut u8
}

/// The word at byte 8 of a dead entity, read off the offset `memory::heap`
/// names rather than through the module's own helper: the claim the readers
/// make is that the stack's link and the free-list link share one word, which
/// is why the pop reads the link before it hands the slot over.
unsafe fn link_of(entity: *mut RcHeader) -> *mut u8 {
    unsafe {
        (entity as *mut u8)
            .add(crate::memory::heap::FREE_LIST_LINK_OFFSET)
            .cast::<*mut u8>()
            .read()
    }
}

/// Retire a withheld candidate by hand, which is what the queue's owner does
/// in production (`cycle::queue::compaction`): the candidate bit comes off, and
/// the slot is handed back before it is offered again, because the free that
/// registered the death took it and the candidate arm then refused the return,
/// so a free without the hand-back reads as a repeat
/// (`crate::refcount::DEAD_IN_PLACE`).
unsafe fn retire_by_hand(header: *mut RcHeader) {
    unsafe { crate::refcount::clear_candidate_bit(header) };
    unsafe { crate::memory::stdapi::hand_back_and_free(header as *mut u8) };
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

/// Promote `N` arena objects in place, each held by a heap holder of its own,
/// so that one retained block counts `N` occupants and each holder owns its
/// survivor's last reference.
unsafe fn retained_survivors<const N: usize>() -> (
    Arena,
    [*mut Object; N],
    [*mut RcHeader; N],
    *mut BlockHeader,
) {
    let survivor_class = crate::class::ClassBuilder::new("RetainedSurvivor").build();
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
    let holder_class = crate::class::ClassBuilder::new("RetainedSurvivorHolder")
        .prop("member", true)
        .build();
    let mut arena = Arena::new();
    let mut holders = [std::ptr::null_mut(); N];
    let mut survivors = [std::ptr::null_mut(); N];

    // Each borrow of the arena ends before the next begins: `store_prop`
    // takes one of its own, and a context held across it is a second live
    // `&mut` to the same arena.
    for index in 0..N {
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
    for survivor in &survivors[1..] {
        assert_eq!(
            block,
            BlockHeader::of_ptr(*survivor as *const u8),
            "one arena reset promotes every survivor into one retained block"
        );
    }

    unsafe { crate::promote::arena_reset_full(&mut arena) };
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_RETAINED
    );
    assert_eq!(
        unsafe { crate::memory::retained::held_occupant_count(block as usize) },
        N as u32
    );
    (arena, holders, survivors, block)
}

/// Fill one block of `class` to capacity with live entities on this thread,
/// and hand every slot over as an address, which crosses a thread boundary
/// where a raw pointer does not.
///
/// The thread's heap holds no block of `class` yet, or the fill spans two
/// blocks, which is why every caller fills on a thread of its own.
unsafe fn fill_one_block(class: usize) -> Vec<usize> {
    let first = unsafe { crate::memory::heap::entity_alloc(class) };
    assert!(!first.is_null());
    let block = block_of(first);
    let capacity = unsafe { crate::memory::heap::collector_block_slots(block) } as usize;
    let mut held = vec![unsafe { live_entity(first, 1) } as usize];
    for _ in 1..capacity {
        let slot = unsafe { crate::memory::heap::entity_alloc(class) };
        assert!(!slot.is_null());
        assert_eq!(
            block_of(slot),
            block,
            "the class opened a second block before the first was full"
        );
        held.push(unsafe { live_entity(slot, 1) } as usize);
    }

    held
}

/// A block an exited thread abandoned with every slot of it live, which leaves
/// it owned by nobody — the shape a thread that dies inside another thread's
/// reach leaves behind: `ll_thread_exit` puts a block with a live occupant on
/// the abandoned list rather than back in the pool. Filled to capacity, so an
/// adoption of it has nowhere to draw from but the block's own stack of
/// cross-thread frees.
fn abandoned_block_of(class: usize) -> Vec<usize> {
    std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the second thread"
        );
        let held = unsafe { fill_one_block(class) };
        crate::memory::heap::ll_thread_exit();
        held
    })
    .join()
    .expect("the second thread finished")
}

/// Blocks the memory manager reports as collection's. Stable for the duration
/// of a `test_guard`, which every test asserting on it holds.
fn gc_blocks() -> usize {
    crate::memory::gc_metadata::thread_stats().current_blocks()
}

/// Bytes the memory manager reports as in use inside the blocks collection
/// holds. Stable for the duration of a `test_guard`, as `gc_blocks` is.
fn in_use_bytes() -> usize {
    crate::memory::gc_metadata::thread_stats().current_bytes_in_use()
}

mod how_each_population_is_classified;
mod what_a_window_asks_the_allocator;
mod what_an_unwind_gives_back;
mod what_the_close_and_the_abort_return;
mod which_window_withholds_a_death;
