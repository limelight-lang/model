//! The `array` entity and the two storage representations it holds: the
//! mixed vector, strategy 2 of `rfc/model/arrays.md`, and the ordered
//! hash, strategy 3, designed in `rfc/model/arrays-hashtable.md`.
//!
//! A fresh array is the vector — the key is the position, and no key is
//! stored anywhere. The first key a dense range cannot hold migrates it
//! to the hash: one allocation of `u32` index slots followed by a dense
//! array of entries in insertion order, where the index slots are the
//! hashtable and the entry array is the order, so iteration is a stride
//! over it and reads no index at all. Which of the two an array has is
//! the tag in its [`head::StorageHead`], the one word the mutator and a
//! concurrent walker both read.

pub mod element;
pub mod entity;
pub mod entry;
// As public as the representations are: since the head left them, it
// appears in the signature of every table and vector operation that
// touches a walker-visible word, and a module less visible than its
// customers cannot be named by their callers.
pub mod head;
pub mod table;
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
