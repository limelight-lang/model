//! Limelight runtime data model: classes, memory manager, GC.
//!
//! Implements the design from the `rfc` repository (`model/`, `runtime/`).
//! This crate contains runtime *mechanics* only — no PHP standard library
//! functions live here.

pub mod class;
pub mod gc;
pub mod intern;
pub mod memory;
pub mod object;
pub mod promote;
pub mod refcount;
pub mod value;
pub mod walk;

pub use class::{Class, ClassBuilder};
pub use intern::{LLString, intern, intern_str};
pub use memory::{Arena, BlockPool, Heap, LLContext, LimelightAlloc};
pub use object::Object;
pub use refcount::{MemoryCategory, RcHeader};
pub use value::{Tag, Value};
