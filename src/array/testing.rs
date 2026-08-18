//! Shorthands for reaching an array's storage from a test, one call per
//! operation.
//!
//! **Why these exist.** The storage head is the entity's
//! (`crate::array::head`), so every operation over a walker-visible word
//! takes it as a parameter, and production code derives the pair and
//! destructures it — `let (table, head) = as_table_mut(a)` — so that a
//! head belonging to another array cannot be passed by accident. An
//! assertion cannot destructure: it needs the answer inside an
//! expression. These functions do the same field-precise derivation in
//! one call, so a test that is about insertion order or about a
//! refcount stays about that rather than about how to reach a table.
//!
//! **The category is the array's own**, read through [`category_of`] at
//! the call. A test needing a different one calls the table directly.
//!
//! Nothing here is compiled into a release build, and nothing in the
//! crate outside `#[cfg(test)]` may use it: an operation worth having in
//! production is worth its own name at the element layer.

use crate::MemoryCategory;
use crate::array::entity::{
    LLArray, as_table, as_table_mut, as_vector_mut, category_of, storage_head,
};
use crate::array::entry::Entry;
use crate::array::table::{InsertKind, InsertOutcome, Key};
use crate::string::LLString;
use crate::value::Value;

/// A fresh array in the ordered hash, for a test whose subject is that
/// representation. Null when the entity allocation is refused.
///
/// The array is built by the factory and migrated, which is the route
/// production reaches strategy 3 by: `ll_array_new` stamps the mixed
/// vector, and a fresh vector migrating carries no element and allocates
/// no storage, so the migration here cannot refuse.
///
/// # Safety
/// As `ll_array_new`: the result is a fresh entity at count 1.
pub(crate) unsafe fn hash_array(category: MemoryCategory) -> *mut LLArray {
    let a = unsafe { crate::array::entity::ll_array_new(category) };
    if a.is_null() {
        return std::ptr::null_mut();
    }

    assert!(
        unsafe { crate::array::entity::migrate_to_hash(a, category) },
        "an empty vector's migration allocates nothing and cannot refuse"
    );
    a
}

/// The ordered hash alone, for the reads that need no head — `len`,
/// `is_empty`, `is_strong`, `append_key`, `salt`, `is_reseeded`.
///
/// # Safety
/// `a` is a live array whose storage is the ordered hash.
#[inline]
pub(crate) unsafe fn table<'a>(a: *mut LLArray) -> &'a crate::array::table::Table {
    unsafe { as_table(a) }.0
}

/// An admission, answered in the pair shape the assertions were written
/// against: `None` is the memory refusal, `(added, displaced)` the rest.
/// A ladder refusal panics rather than folding into `None`: a fixture
/// that trips the terminal rung must not read as out of memory. A test
/// about that refusal calls `Table::insert` raw.
///
/// # Safety
/// As [`table`].
#[inline]
pub(crate) unsafe fn insert(
    a: *mut LLArray,
    key: Key,
    value: Value,
) -> Option<(bool, Option<Value>)> {
    let category = unsafe { category_of(a) };
    let (table, head) = unsafe { as_table_mut(a) };
    match table.insert(head, category, InsertKind::Admission, key, value) {
        InsertOutcome::Added => Some((true, None)),
        InsertOutcome::Replaced(old) => Some((false, Some(old))),
        InsertOutcome::RefusedForMemory => None,
        InsertOutcome::RefusedByLadder => panic!("the ladder refused an insert in a fixture"),
    }
}

/// # Safety
/// As [`table`].
#[inline]
pub(crate) unsafe fn get(a: *mut LLArray, key: Key) -> Option<Value> {
    let (table, head) = unsafe { as_table(a) };
    table.get(head, key)
}

/// # Safety
/// As [`table`].
#[inline]
pub(crate) unsafe fn contains(a: *mut LLArray, key: Key) -> bool {
    let (table, head) = unsafe { as_table(a) };
    table.contains(head, key)
}

/// # Safety
/// As [`table`].
#[must_use = "the pair carries the table's key reference; dropping it leaks the key"]
#[inline]
pub(crate) unsafe fn remove(a: *mut LLArray, key: Key) -> Option<(Value, *mut LLString)> {
    let (table, head) = unsafe { as_table_mut(a) };
    table.remove(head, key)
}

/// # Safety
/// As [`table`].
#[inline]
pub(crate) unsafe fn compact(a: *mut LLArray) -> Option<usize> {
    let category = unsafe { category_of(a) };
    let (table, head) = unsafe { as_table_mut(a) };
    table.compact(head, category)
}

/// # Safety
/// As [`table`], and `i` is inside the entry count.
#[inline]
pub(crate) unsafe fn entry<'a>(a: *mut LLArray, i: usize) -> &'a Entry {
    let (table, head) = unsafe { as_table(a) };
    table.entry(head, i)
}

/// Entries written so far, holes included — the head's own count, which
/// both representations keep.
///
/// # Safety
/// `a` is a live array.
#[inline]
pub(crate) unsafe fn used(a: *mut LLArray) -> usize {
    unsafe { (*storage_head(a)).used() }
}

/// # Safety
/// As [`table`].
#[inline]
pub(crate) unsafe fn iter<'a>(a: *mut LLArray) -> impl Iterator<Item = &'a Entry> {
    let (table, head) = unsafe { as_table(a) };
    table.iter(head)
}

/// # Safety
/// As [`table`].
#[inline]
pub(crate) unsafe fn storage_and_capacity(a: *mut LLArray) -> (*mut u8, usize) {
    let (table, head) = unsafe { as_table(a) };
    table.storage_and_capacity(head)
}

/// Read a position of a mixed vector, `None` past the count.
///
/// # Safety
/// `a` is a live array whose storage is the mixed vector.
#[inline]
pub(crate) unsafe fn at(a: *mut LLArray, i: usize) -> Option<Value> {
    let (vector, head) = unsafe { crate::array::entity::as_vector(a) };
    vector.get(head, i)
}

/// Append to a mixed vector.
///
/// # Safety
/// `a` is a live array whose storage is the mixed vector.
#[inline]
pub(crate) unsafe fn push(a: *mut LLArray, value: Value) -> bool {
    let category = unsafe { category_of(a) };
    let (vector, head) = unsafe { as_vector_mut(a) };
    vector.push(head, category, value)
}
