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
// As public as the representations are: since the head left them, it
// appears in the signature of every table and vector operation that
// touches a walker-visible word, and a module less visible than its
// customers cannot be named by their callers.
pub mod head;
pub mod table;
// Strategy 2 is built one step ahead of its producer: `ll_array_new`
// stamps the ordered hash until the element layer reads the tag, so until
// then the vector's own operations are reached by its tests alone.
#[allow(dead_code, reason = "the producer lands with the factory's stamp")]
pub mod vector;

// One call per operation for the tests, which cannot destructure the
// pair a production call site does. Test builds only.
#[cfg(test)]
pub(crate) mod testing;

// A model of the table's version bracket, checked by `loom` rather than by
// the suite: it exists only under `--cfg loom`, where the dev-dependency
// exists too. How to run it, and what it demonstrated, are in the file.
#[cfg(loom)]
mod version_bracket_model;
