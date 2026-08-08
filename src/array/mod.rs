//! The `array` entity's ordered hash: storage strategy 3 of
//! `rfc/model/arrays.md`, designed in `rfc/model/arrays-hashtable.md`.
//!
//! One allocation holds `u32` index slots followed by a dense array of
//! entries in insertion order. The index slots are the hashtable; the
//! entry array is the order, so iteration is a stride over it and reads
//! no index at all.

pub mod element;
pub mod entity;
pub mod entry;
pub mod table;

// A model of the table's version bracket, checked by `loom` rather than by
// the suite: it exists only under `--cfg loom`, where the dev-dependency
// exists too. How to run it, and what it demonstrated, are in the file.
#[cfg(loom)]
mod version_bracket_model;
