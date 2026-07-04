//! Limelight runtime data model: classes, memory manager, GC.
//!
//! Implements the design from the `rfc` repository (`model/`, `runtime/`).
//! This crate contains runtime *mechanics* only — no PHP standard library
//! functions live here.

pub mod memory;
pub mod refcount;

pub use memory::{Arena, BlockPool, Heap, LLContext};
pub use refcount::{MemoryCategory, RcHeader};
