//! A class whose counted cells lie in a block outside the object's own
//! body, and the five behaviours that reach them
//! (`crate::walk::OutsideCells`).
//!
//! Neither real customer is in this crate — `limelight-lang/io`'s
//! coroutine puts its wait halves in a raw block once there are more than
//! two, and `rfc/model/maps.md`'s map keeps its whole table in a chunk —
//! so the group has no producer here and this is what the tests of three
//! modules build on: the collector's, the cycle collector's and the
//! object teardown's.
//!
//! **The block is drawn under the instance's own category**, through
//! `memory::routing::body_alloc`, which is the group's contract
//! (`crate::walk::OutsideCells`) and the same door a table's storage
//! takes. For the GcHeap instances these tests build that is a
//! buffer-arena chunk, which parks during an epoch like any other body.
//!
//! **The block is replaced whole rather than written in place**, and the
//! version word beside it is the array head's bracket in miniature
//! (`crate::array::head::StorageHead::coherent`): odd while the pointer
//! moves, and read on both sides of the collector's own reading. It is
//! here because the re-check is one of the five, and Phase 3 has nothing
//! to ask about a class whose storage never moves. The window covers the
//! release as well as the move, which is `StorageHead`'s rule too: a
//! class whose block goes away while the instance lives — a coroutine
//! whose wait completed — publishes that null the same way it publishes
//! a fresh pointer.
//!
//! **Both words are declared properties here, and a real class must not
//! keep them that way.** It is a layout convenience: a scalar property
//! puts them at a known offset with no new machinery. The price is that
//! the language can name them, and a generated scalar store is a plain
//! eight-byte write beside a collector reading the same word as an
//! `AtomicU64` — the mixed-atomicity race `crate::walk::empty_cell`
//! exists to refuse. The storage pointer and its version belong in slots
//! no property store can reach.

use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering, fence};

use crate::class::{Class, ClassBuilder};
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::Object;
use crate::refcount::RcHeader;
use crate::value::Value;
use crate::walk::{Cell, CellReader, OutsideCells, PlainCells};
#[cfg(feature = "rc-walk")]
use crate::walk::{OutsideRead, RelaxedCells};

/// Cells one block holds. Two is the coroutine's own number: its block
/// exists exactly when a wait has more than two halves.
pub(crate) const CELLS: usize = 2;

/// Bytes one block takes — the cells and nothing else. A block carries no
/// header: what would be in one is in the object, because the re-check
/// asks the entity and not the storage.
pub(crate) const BLOCK_SIZE: usize = CELLS * 16;

/// The block pointer's offset in the body, the version's beside it, and
/// the capacity the buffer machinery granted after that. All three are
/// scalar slots, so none is traced: what the runs may not reach is the
/// whole subject here.
///
/// The capacity is stored because a chunk carries no metadata of its own
/// and its free takes the grant back (`buffer_arena.rs`, the zero-metadata
/// contract) — the same reason an array keeps its own
/// beside the chunk pointer.
const BLOCK_AT: usize = 16;
const VERSION_AT: usize = 24;
const CAPACITY_AT: usize = 32;

/// Readings before the racing walk gives the entity up, the bound
/// `array::head::COHERENT_READ_ATTEMPTS` sets for the same protocol: a
/// mutator that keeps moving the block wins the epoch rather than the
/// walker's stack.
#[cfg(feature = "rc-walk")]
const COHERENT_READ_ATTEMPTS: usize = 4;

/// A class of instances whose counted cells lie in a block outside the
/// body, laid out as this module's readers expect.
///
/// The name is the caller's because a descriptor is immortal and two
/// tests that share one share its instances' history.
pub(crate) fn class(name: &str) -> *const Class {
    let cls = ClassBuilder::new(name)
        .prop("block", false)
        .prop("version", false)
        .prop("capacity", false)
        .outside_cells(&GROUP)
        .build();
    assert!(!cls.is_null(), "the immortal region refused the class");

    // Read back rather than assumed: the layout groups the traced runs
    // first and appends scalars in declaration order, so a traced
    // property declared here would move every word this module reads.
    let props = unsafe { (*cls).props() };
    assert_eq!(props[0].offset as usize, BLOCK_AT, "`block` moved");
    assert_eq!(props[1].offset as usize, VERSION_AT, "`version` moved");
    assert_eq!(props[2].offset as usize, CAPACITY_AT, "`capacity` moved");
    cls
}

/// Give `obj` a fresh block, carrying over what the old one held, and
/// free the old one — through the same category-routed door, so an epoch
/// in flight parks it instead of recycling.
///
/// Answers the new block, whose granted capacity the object keeps. The
/// cells of a fresh block are written plainly: no walker can reach a
/// block the object has not published yet, which is the same reason
/// `array::table::move_entries` fills its new chunk that way.
///
/// # Safety
/// `obj` is a live instance of a class from [`class`], `ctx` per
/// [`crate::memory::context::ll_arena_alloc`], and no other thread is
/// walking the instance.
pub(crate) unsafe fn install_block(ctx: *mut LLContext, obj: *mut Object) -> *mut u8 {
    let base = obj as *mut u8;
    let category = unsafe { (*obj).rc.memory_category() };
    let (fresh, granted) = unsafe { crate::memory::routing::body_alloc(ctx, category, BLOCK_SIZE) };
    assert!(!fresh.is_null(), "the memory manager refused a block");

    let old = unsafe { block_at::<PlainCells>(base) };
    let old_capacity = unsafe { capacity(base) };
    for i in 0..CELLS {
        let carried = if old.is_null() {
            Value::null()
        } else {
            unsafe { (old.add(i * 16) as *const Value).read() }
        };

        unsafe { (fresh.add(i * 16) as *mut Value).write(carried) };
    }

    unsafe { publish_block(base, fresh) };
    unsafe { store_capacity(base, granted) };

    if !old.is_null() {
        unsafe { crate::memory::routing::body_free(category, old, old_capacity) };
    }

    fresh
}

/// Publish `value` into cell `index` of `obj`'s block, through the store
/// barrier: a cell outside the body is a `Value` slot of a live holder
/// like any other, and the retain, the category barrier and the drop of
/// what the cell held are all owed the same way.
///
/// `old` is the entity the cell holds now, or null — the barrier's own
/// contract ([`crate::memory::barrier::ref_store`]). Answers whether the
/// store happened.
///
/// # Safety
/// `obj` is a live instance with a block, `index` is below [`CELLS`], and
/// `arena` is the context's.
pub(crate) unsafe fn store_cell(
    arena: *mut Arena,
    obj: *mut Object,
    index: usize,
    old: *mut RcHeader,
    value: Value,
) -> bool {
    assert!(index < CELLS, "the block holds {CELLS} cells");
    let block = unsafe { block_at::<PlainCells>(obj as *mut u8) };
    assert!(!block.is_null(), "the object has no block to store into");
    unsafe {
        crate::memory::barrier::ref_store(
            arena,
            obj as *mut RcHeader,
            block.add(index * 16) as *mut Value,
            old,
            value,
        )
    }
}

/// The block `obj` holds, or null before [`install_block`] and after the
/// teardown freed it.
///
/// # Safety
/// `obj` is a live instance of a class from [`class`].
pub(crate) unsafe fn block_of(obj: *mut Object) -> *mut u8 {
    unsafe { block_at::<PlainCells>(obj as *mut u8) }
}

/// The version the walk would answer with, for a test that wants to see
/// it move.
///
/// # Safety
/// As [`block_of`].
#[cfg_attr(not(feature = "rc-walk"), expect(dead_code))]
pub(crate) unsafe fn version_of(obj: *mut Object) -> usize {
    unsafe { version(obj as *mut u8) }
}

/// Open the move window and leave it open, without moving anything.
///
/// The mutator half of a race the stepped harness cannot otherwise
/// produce: one thread plays both actors there, so a walk never runs
/// while a window is genuinely open, and the branch that gives the entity
/// up would go untried. Closed by [`close_window`], which the caller owes
/// before the instance is torn down — the teardown's own members read the
/// block, and a class left mid-move is a class the walk refuses.
///
/// # Safety
/// As [`block_of`].
#[cfg(feature = "rc-walk")]
pub(crate) unsafe fn open_window(obj: *mut Object) {
    unsafe { open_move(obj as *mut u8) };
}

/// Close what [`open_window`] opened.
///
/// # Safety
/// As [`block_of`], with a window open.
#[cfg(feature = "rc-walk")]
pub(crate) unsafe fn close_window(obj: *mut Object) {
    unsafe { close_move(obj as *mut u8) };
}

/// Publish a block pointer — a fresh one, or the null a release leaves —
/// inside the window a walker validates its reading against.
///
/// One body for both writers, because the window is what a reader trusts
/// and two copies of a four-step sequence lose a step: the S18.3 review
/// found the release publishing outside it.
unsafe fn publish_block(base: *mut u8, block: *mut u8) {
    unsafe { open_move(base) };
    unsafe {
        (*(base.add(BLOCK_AT) as *const AtomicPtr<u8>)).store(block, Ordering::Relaxed);
    }

    unsafe { close_move(base) };
}

/// The version goes odd, and the fence comes **after** the store rather
/// than the store being a release: what must stay on this side is
/// everything the move writes next, which a release store would leave
/// free to become visible before the odd version — the reading a walker
/// would then accept (`array::head::StorageHead::begin_move`).
unsafe fn open_move(base: *mut u8) {
    let opened = unsafe { version(base) } + 1;
    debug_assert!(opened % 2 != 0, "the window was already open");
    unsafe { store_version(base, opened, Ordering::Relaxed) };
    fence(Ordering::Release);
}

/// Even again, by a release store: here the writes to order are the ones
/// that precede the call, which is why the asymmetry with [`open_move`]
/// is deliberate (`array::head::StorageHead::end_move`).
unsafe fn close_move(base: *mut u8) {
    let closed = unsafe { version(base) } + 1;
    debug_assert!(closed % 2 == 0, "no window was open");
    unsafe { store_version(base, closed, Ordering::Release) };
}

/// The group, whose five members are the whole of what a class owes for
/// cells the runs cannot describe.
static GROUP: OutsideCells = OutsideCells {
    walk_plain,
    #[cfg(feature = "rc-walk")]
    walk_relaxed,
    recheck,
    sever,
    free,
};

/// The quiescent walk: every cell of the block, and the version they came
/// out of. `None` for an instance with no block — there is no storage
/// behind it, so there is nothing for Phase 3 to ask about.
unsafe fn walk_plain(base: *mut u8, _: *const Class, visit: &mut dyn FnMut(Cell)) -> Option<usize> {
    let block = unsafe { block_at::<PlainCells>(base) };
    if block.is_null() {
        return None;
    }

    unsafe { yield_cells::<PlainCells>(block, visit) };
    Some(unsafe { version(base) })
}

/// The same cells under the collector's reader, with the mutator running.
///
/// The pointer is validated **before** a single cell is yielded: a cell
/// address that escapes into the epoch's edge list is one Phase 3 will
/// re-read, so a reading taken across a move may not be handed out and
/// then withdrawn.
#[cfg(feature = "rc-walk")]
unsafe fn walk_relaxed(base: *mut u8, _: *const Class, visit: &mut dyn FnMut(Cell)) -> OutsideRead {
    for _ in 0..COHERENT_READ_ATTEMPTS {
        let before = unsafe { version(base) };
        if before % 2 != 0 {
            continue;
        }

        let block = unsafe { block_at::<RelaxedCells>(base) };
        // The fence rather than an acquire load, for
        // `StorageHead::coherent`'s reason: an acquire load orders what
        // follows it, so the pointer this check validates could be read
        // past the check and validated by nothing.
        fence(Ordering::Acquire);
        if unsafe { version(base) } != before {
            continue;
        }

        if block.is_null() {
            return OutsideRead::NoStorage;
        }

        unsafe { yield_cells::<RelaxedCells>(block, visit) };
        return OutsideRead::Version(before);
    }

    OutsideRead::GaveUp
}

/// Is the block still the one the walk read? An odd reading is a move in
/// flight and equals no walked version, so it needs no case of its own.
unsafe fn recheck(base: *mut u8, _: *const Class, walked: usize) -> bool {
    unsafe { version(base) == walked }
}

/// Empty every cell and hand its former occupant back undropped, the
/// drain's contract for the group.
unsafe fn sever(entity: *mut RcHeader, displaced: &mut Vec<*mut RcHeader>) {
    let block = unsafe { block_at::<PlainCells>(entity as *mut u8) };
    if block.is_null() {
        return;
    }

    unsafe {
        yield_cells::<PlainCells>(block, &mut |cell| {
            crate::walk::empty_cell(cell);
            displaced.push(cell.child);
        })
    };
}

/// Give the block back. The slot is nulled first: this runs from the
/// teardown's last act and from the white-set free alike, and a block
/// pointer left in a slot is one a second reader would free again.
///
/// The null goes through the window like any other pointer store. Both of
/// today's callers hold an entity whose count has already changed, so a
/// walker would be acquitted by the count alone — but a class whose block
/// goes away while the instance lives has no such second answer, and then
/// an unbracketed null leaves the epoch re-reading cell addresses in a
/// freed block against a version that never moved.
unsafe fn free(entity: *mut RcHeader) {
    let base = entity as *mut u8;
    let block = unsafe { block_at::<PlainCells>(base) };
    if block.is_null() {
        return;
    }

    let opened = unsafe { version(base) } + 1;
    unsafe { store_version(base, opened, Ordering::Relaxed) };
    fence(Ordering::Release);
    unsafe {
        (*(base.add(BLOCK_AT) as *const AtomicPtr<u8>))
            .store(std::ptr::null_mut(), Ordering::Relaxed);
    }

    unsafe { store_version(base, opened + 1, Ordering::Release) };
    let category = unsafe { (*entity).memory_category() };
    unsafe { crate::memory::routing::body_free(category, block, capacity(base)) };
}

/// The block's cells, as the reader `R` sees them. A cell is a 16-byte
/// `Value`, so what decides whether it holds a counted child is the same
/// test the body's Box runs use.
unsafe fn yield_cells<R: CellReader>(block: *mut u8, visit: &mut dyn FnMut(Cell)) {
    for i in 0..CELLS {
        if let Some(cell) = unsafe { crate::walk::counted_box_cell::<R>(block.add(i * 16)) } {
            visit(cell);
        }
    }
}

/// The block pointer, read as `R` reads the entity's own memory.
#[inline]
unsafe fn block_at<R: CellReader>(base: *mut u8) -> *mut u8 {
    unsafe { R::ptr(base.add(BLOCK_AT)) }
}

/// The version word. Read atomically whoever asks: the mutator writes it
/// beside a collector that reads it, and a plain load there is a data
/// race rather than a stale number.
#[inline]
unsafe fn version(base: *mut u8) -> usize {
    unsafe { (*(base.add(VERSION_AT) as *const AtomicU64)).load(Ordering::Acquire) as usize }
}

#[inline]
unsafe fn store_version(base: *mut u8, value: usize, order: Ordering) {
    unsafe { (*(base.add(VERSION_AT) as *const AtomicU64)).store(value as u64, order) };
}

/// The capacity the buffer machinery granted for the block the object
/// holds now. Atomic because the group's `free` reads it from the
/// collector's drain while the mutator's [`install_block`] writes it, and
/// a plain access on either side of that pair is a data race.
#[inline]
unsafe fn capacity(base: *mut u8) -> usize {
    unsafe { (*(base.add(CAPACITY_AT) as *const AtomicU64)).load(Ordering::Relaxed) as usize }
}

#[inline]
unsafe fn store_capacity(base: *mut u8, value: usize) {
    unsafe {
        (*(base.add(CAPACITY_AT) as *const AtomicU64)).store(value as u64, Ordering::Relaxed)
    };
}

/// The block a test allocated and the capacity its free needs back, for a
/// probe that asks the allocator whether the chunk came home.
///
/// # Safety
/// As [`block_of`].
pub(crate) unsafe fn block_and_capacity(obj: *mut Object) -> (*mut u8, usize) {
    let base = obj as *mut u8;
    (unsafe { block_at::<PlainCells>(base) }, unsafe {
        capacity(base)
    })
}
