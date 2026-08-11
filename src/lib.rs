//! Limelight runtime data model: classes, memory manager, GC.
//!
//! Implements the design from the `rfc` repository (`model/`, `runtime/`).
//! This crate contains runtime *mechanics* only — no PHP standard library
//! functions live here.

pub mod array;
pub mod class;
#[cfg(feature = "rc-walk")]
pub(crate) mod collector;
#[cfg(feature = "rc-walk")]
pub(crate) mod epoch;
pub mod gc;
pub mod hash;
pub mod intern;
pub mod journal;
pub mod memory;
pub mod object;
pub mod promote;
pub mod refcount;
pub mod reference;
pub mod static_block;
pub mod string;
pub mod template;
#[cfg(test)]
mod test_support;
pub mod value;
pub mod walk;
pub mod weak;

pub use class::{Class, ClassBuilder};
pub use intern::{intern, intern_str};
pub use memory::{Arena, BlockPool, Heap, LLContext, LimelightAlloc};
pub use object::Object;
pub use refcount::{MemoryCategory, RcHeader};
pub use string::LLString;
pub use value::{Tag, Value};
pub use weak::LLWeakRef;
