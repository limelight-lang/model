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
