//! The weak table: target address → subscriber row, open-addressed in one
//! contiguous long-lived buffer payload.
//!
//! # What it owns and for how long
//!
//! One payload per thread, drawn through
//! [`crate::memory::buffer_arena::buffer_alloc_longlived_payload`]
//! and given back through its inverse — at thread exit, and at every growth,
//! which frees the payload it has just copied out of. That is the storage
//! class an array's table storage already uses, and it is the right one here
//! because the weak table is the mutator's death-notification machinery: a
//! thread that never runs a collection still fills it, and the memory manager
//! answers whose memory a block is by its kind (`dev/DECISIONS.md`, "GC
//! memory is counted once, and the block kind is the split"). Stamping it GC
//! metadata would report mutator memory as collection's holding.
//!
//! Thread-local storage keeps one non-owning pointer to the payload; a null
//! pointer means no table, which is also the state a thread that never took a
//! weak reference stays in. The pointer has no drop glue: thread-exit order is
//! owned by `memory::heap::ll_thread_exit` (`dev/DECISIONS.md`, "thread exit
//! owns the order its per-thread state dies in").
//!
//! # The layout
//!
//! ```text
//! payload ─▶ [ count | mask | granted ]  16 bytes
//!            [ target | subscriber ]     16 bytes, `mask + 1` of them
//!            ...
//! ```
//!
//! A row's `target` is the entity's address and zero means the row is empty.
//! Its `subscriber` is tagged in the low four bits, which entity slots leave
//! free by being sixteen-byte aligned in every size class
//! (`memory::heap::SIZE_CLASSES`): [`TAG_CANONICAL_CELL`] is the only tag
//! today, and the rest are what the design's spilled subscriber list will use
//! when `WeakMap` starts subscribing (`rfc/model/weak-references.md`, "The
//! weak table: address → subscriber row" — no step of `PLAN.md` builds it
//! yet). The row is the width it will keep, so that arrival widens a tag
//! rather than rebuilding a table.
//!
//! # Capacity, and where a refusal is answered
//!
//! Capacity is a power of two of rows and the table holds at most half of
//! them, so a probe walk always meets an empty row and terminates. It starts
//! at [`INITIAL_ROWS`] and doubles, and it never falls: a thread that held
//! 2,000 weak references and now holds two keeps the payload of the two
//! thousand until it exits.
//!
//! **Past 2,048 rows the payload leaves the buffer arena.** 4,096 rows are
//! 65,552 bytes against a 65,280-byte block payload, so
//! `buffer_alloc_longlived_payload` maps the run directly and the free returns
//! it by mask. Both routes are that one call and its inverse; what changes is
//! which of them answers, and a thread needs 1,025 live weak references to
//! reach the second.
//!
//! **Every allocation this module makes happens before the caller has anything
//! in hand.** `ll_weakref_create` calls [`ensure_room_for_one_more`] first, and
//! a refusal there is a null return from an ABI entry that already answers null
//! for out of memory: no cell built, no row written, no gate bit set. The
//! insert that follows cannot fail, and removal never allocates — so the death
//! path is structurally incapable of failing rather than promising not to.
//!
//! A growth that has already taken hold when the *cell* is then refused stays
//! taken: the table is twice the size it was and the payload it copied out of
//! is back on the free list. Nothing observable to the caller changed — the
//! rows, the gate bits and the arena's weak log are what they were — and the
//! capacity is spent, not lost.

use std::cell::Cell;

use super::LLWeakRef;
use crate::memory::buffer_arena::{buffer_alloc_longlived_payload, buffer_free_longlived_payload};

/// Rows the first table holds: 64, which is 1,040 bytes with the header and
/// covers a thread holding up to 32 weakly referenced objects without a
/// growth.
const INITIAL_ROWS: usize = 64;

/// The tag of a row whose subscriber is the target's one canonical cell.
const TAG_CANONICAL_CELL: usize = 0;

/// The low bits a subscriber word carries a tag in.
const TAG_MASK: usize = 0xF;

/// The address no live entity has, and therefore the empty row.
const EMPTY: usize = 0;

/// The table's own three words, written at the payload's start.
#[repr(C)]
pub(super) struct WeakTable {
    /// Rows in use.
    count: u32,
    /// Capacity less one. Capacity is a power of two, so this is a mask.
    mask: u32,
    /// Bytes the buffer layer granted, which is what its free asks for.
    granted: usize,
}

const _: () = assert!(size_of::<WeakTable>() == 16);
// Eight, and it may not rise: the buffer layer rounds a grant to eight bytes
// from a payload start of its own, and promises nothing wider.
const _: () = assert!(align_of::<WeakTable>() == 8);

/// One target and what subscribes to its death.
#[repr(C)]
#[derive(Clone, Copy)]
struct Row {
    target: usize,
    subscriber: usize,
}

const _: () = assert!(size_of::<Row>() == 16);
const _: () = assert!(align_of::<Row>() == 8);

thread_local! {
    /// This thread's table payload, or null while it has none. Non-owning:
    /// [`dispose`] is what returns it.
    static WEAK_TABLE: Cell<*mut WeakTable> = const { Cell::new(std::ptr::null_mut()) };
}

/// The row array behind a table's header.
#[inline]
fn rows_of(table: *mut WeakTable) -> *mut Row {
    unsafe { (table as *mut u8).add(size_of::<WeakTable>()) as *mut Row }
}

/// The slot a target hashes to.
///
/// Fibonacci hashing over the address with its four dead low bits shifted
/// out, taking the high bits of the product — the low bits of a multiply
/// carry the least of the input. The shift already leaves `bits` of value, so
/// the mask is the statement that a slot is in range rather than a step of the
/// arithmetic.
#[inline]
fn slot_of(target: usize, mask: u32) -> usize {
    const KNUTH: usize = 0x9E37_79B9_7F4A_7C15;
    let bits = (mask + 1).trailing_zeros();
    ((target >> 4).wrapping_mul(KNUTH) >> (usize::BITS - bits)) as usize & mask as usize
}

/// The slot holding `target`, or the empty slot its insert would take.
///
/// # Safety
/// `rows` is a table's row array and `mask` its own, and the table holds at
/// most half its rows — which is what makes the walk terminate.
#[inline]
unsafe fn probe(rows: *mut Row, mask: u32, target: usize) -> (usize, bool) {
    let mut index = slot_of(target, mask);
    loop {
        let found = unsafe { (*rows.add(index)).target };
        if found == target {
            return (index, true);
        }

        if found == EMPTY {
            return (index, false);
        }

        index = (index + 1) & mask as usize;
    }
}

/// This thread's table, or null while it has none.
#[inline]
pub(super) fn current() -> *mut WeakTable {
    WEAK_TABLE.with(Cell::get)
}

/// The canonical cell registered for `target`, or null when no row names it.
///
/// # Safety
/// `table` is this thread's table or null.
pub(super) unsafe fn find(table: *mut WeakTable, target: usize) -> *mut LLWeakRef {
    if table.is_null() {
        return std::ptr::null_mut();
    }

    let mask = unsafe { (*table).mask };
    let rows = rows_of(table);
    let (index, hit) = unsafe { probe(rows, mask, target) };
    if !hit {
        return std::ptr::null_mut();
    }

    let subscriber = unsafe { (*rows.add(index)).subscriber };
    debug_assert_eq!(
        subscriber & TAG_MASK,
        TAG_CANONICAL_CELL,
        "the only subscriber tag in use is the canonical cell"
    );
    (subscriber & !TAG_MASK) as *mut LLWeakRef
}

/// Register `cell` as `target`'s canonical subscriber.
///
/// Cannot fail: [`ensure_room_for_one_more`] has already made room, and the
/// caller has done everything fallible before reaching here. The table is read
/// here rather than passed in, because a growth between the two calls frees
/// the payload a caller would be holding.
///
/// # Safety
/// [`ensure_room_for_one_more`] has answered since the last insert, and
/// `target` has no row.
pub(super) unsafe fn insert(target: usize, cell: *mut LLWeakRef) {
    let table = current();
    debug_assert!(!table.is_null(), "an insert with no table under it");
    debug_assert_eq!(cell as usize & TAG_MASK, 0, "an entity slot is 16-aligned");
    let mask = unsafe { (*table).mask };
    let rows = rows_of(table);
    let (index, hit) = unsafe { probe(rows, mask, target) };
    debug_assert!(!hit, "a target takes one row");

    unsafe {
        rows.add(index).write(Row {
            target,
            subscriber: cell as usize | TAG_CANONICAL_CELL,
        });
        (*table).count += 1;
    }
}

/// Drop `target`'s row and answer the cell it named, or null when no row
/// named it. Allocates nothing, which is what the death path needs.
///
/// # Safety
/// `table` is this thread's table or null.
pub(super) unsafe fn remove(table: *mut WeakTable, target: usize) -> *mut LLWeakRef {
    if table.is_null() {
        return std::ptr::null_mut();
    }

    let mask = unsafe { (*table).mask };
    let rows = rows_of(table);
    let (index, hit) = unsafe { probe(rows, mask, target) };
    if !hit {
        return std::ptr::null_mut();
    }

    let subscriber = unsafe { (*rows.add(index)).subscriber };
    unsafe {
        close_over(rows, mask, index);
        (*table).count -= 1;
    }

    (subscriber & !TAG_MASK) as *mut LLWeakRef
}

/// Close the gap a removed row leaves, so every remaining row stays reachable
/// from its own slot by a walk that stops at the first empty one.
///
/// Knuth's algorithm R: a row is pulled back into the gap unless its own slot
/// lies inside the span between them, in which case pulling it back would put
/// it before the slot its probe starts at.
///
/// # Safety
/// `rows` is a table's row array, `mask` its own, and `hole` a slot whose row
/// is being dropped.
unsafe fn close_over(rows: *mut Row, mask: u32, hole: usize) {
    let mut hole = hole;
    let mut index = hole;
    loop {
        index = (index + 1) & mask as usize;
        let candidate = unsafe { *rows.add(index) };
        if candidate.target == EMPTY {
            break;
        }

        let ideal = slot_of(candidate.target, mask);
        let stays = if hole <= index {
            hole < ideal && ideal <= index
        } else {
            hole < ideal || ideal <= index
        };
        if stays {
            continue;
        }

        unsafe { rows.add(hole).write(candidate) };
        hole = index;
    }

    unsafe {
        rows.add(hole).write(Row {
            target: EMPTY,
            subscriber: 0,
        })
    };
}

/// Draw a table of `capacity` rows with every row empty, or null when the
/// buffer layer refuses.
fn draw(capacity: usize) -> *mut WeakTable {
    let bytes = size_of::<WeakTable>() + capacity * size_of::<Row>();
    let (payload, granted) = buffer_alloc_longlived_payload(bytes);
    if payload.is_null() {
        return std::ptr::null_mut();
    }

    let table = payload as *mut WeakTable;
    unsafe {
        table.write(WeakTable {
            count: 0,
            mask: (capacity - 1) as u32,
            granted,
        });
        // The rows arrive dirty from the bump; an empty row is a zero target,
        // so the array is the one part that has to be written before a read.
        // `write_bytes` counts rows, scaling by the pointee itself.
        std::ptr::write_bytes(rows_of(table), 0, capacity);
    }

    table
}

/// This thread's table with room for one more row, or **null when neither the
/// creation nor the growth could be funded**.
///
/// Called before the caller allocates anything of its own, so a null answer
/// costs it nothing to act on.
pub(super) fn ensure_room_for_one_more() -> *mut WeakTable {
    let table = current();
    if table.is_null() {
        let fresh = draw(INITIAL_ROWS);
        WEAK_TABLE.with(|cell| cell.set(fresh));
        return fresh;
    }

    let count = unsafe { (*table).count } as usize;
    let capacity = unsafe { (*table).mask } as usize + 1;
    if (count + 1) * 2 <= capacity {
        return table;
    }

    let grown = draw(capacity * 2);
    if grown.is_null() {
        return grown;
    }

    let old_rows = rows_of(table);
    let grown_mask = unsafe { (*grown).mask };
    let grown_rows = rows_of(grown);
    for index in 0..capacity {
        let row = unsafe { *old_rows.add(index) };
        if row.target == EMPTY {
            continue;
        }

        let (slot, hit) = unsafe { probe(grown_rows, grown_mask, row.target) };
        debug_assert!(!hit, "the old table held one row per target");
        unsafe { grown_rows.add(slot).write(row) };
    }

    unsafe { (*grown).count = (*table).count };
    WEAK_TABLE.with(|cell| cell.set(grown));
    unsafe { give_back(table) };
    grown
}

/// Give one table's payload back to the buffer layer.
///
/// # Safety
/// `table` is a payload this module drew and nothing names it any more.
unsafe fn give_back(table: *mut WeakTable) {
    let granted = unsafe { (*table).granted };
    unsafe { buffer_free_longlived_payload(table as *mut u8, granted) };
}

/// Give this thread's table back. Null-tolerant and idempotent.
pub(super) fn dispose() {
    let table = WEAK_TABLE.with(|cell| cell.replace(std::ptr::null_mut()));
    if !table.is_null() {
        unsafe { give_back(table) };
    }
}

/// The payload this thread's table sits in, or null. Tests only, and what an
/// assertion about the buffer arena's free list needs to name.
#[cfg(test)]
pub(super) fn payload() -> *mut u8 {
    current() as *mut u8
}

/// Bytes the current table's payload was asked for, by the arithmetic
/// [`draw`] uses. Tests only: a test that recomputed it would be checking one
/// copy of the sum against another.
#[cfg(test)]
pub(super) fn payload_bytes() -> usize {
    let table = current();
    if table.is_null() {
        0
    } else {
        size_of::<WeakTable>() + capacity() * size_of::<Row>()
    }
}

/// Rows in use. Tests only, and the instrument for a growth that rehashed
/// wrongly: a lost row is invisible in every other reading until the target
/// dies.
#[cfg(test)]
pub(super) fn len() -> usize {
    let table = current();
    if table.is_null() {
        0
    } else {
        unsafe { (*table).count as usize }
    }
}

/// Rows the table has room for. Tests only, and what a growth is measured by.
#[cfg(test)]
pub(super) fn capacity() -> usize {
    let table = current();
    if table.is_null() {
        0
    } else {
        unsafe { (*table).mask as usize + 1 }
    }
}
