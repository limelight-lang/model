//! Limelight runtime data model: classes, memory manager, GC.
//!
//! Implements the design from the `rfc` repository (`model/`, `runtime/`).
//! This crate contains runtime *mechanics* only — no PHP standard library
//! functions live here.
//!
//! # Where the old collectors are
//!
//! Three cycle collectors were deleted on 2026-08-26 — `rc-walk`, the
//! barrier-free concurrent walk that was the default build; `rc-trace`, the
//! stop-the-thread candidate-buffer tracer; and `rc-satb`, designed and never
//! built. The one design in force is `rc-cycle`
//! (`rfc/model/gc/rc-cycle.md`), and it is not built yet either, so this
//! crate collects no cycles at all between stages S30 and S36 of `PLAN.md`.
//!
//! **Every line of the deleted code, and every document that described it, is
//! on the branch `archive/pre-rc-cycle`** — in this repository and in `rfc`,
//! on `origin` as well as locally, at the commit before the first deletion.
//! Read it there when a mechanism has to be recovered rather than
//! re-derived; `git show archive/pre-rc-cycle:src/walk.rs` and its siblings
//! `src/gc.rs`, `src/collector.rs`, `src/epoch.rs` and
//! `src/memory/deferred_free.rs` are where the substance was. Why each thing
//! went, and what replaces it, is `dev/DECISIONS.md` under 2026-08-26.
//!
//! Nothing is copied back from that branch without an entry there: the
//! deletion happened because a superseded mechanism left in the tree is read
//! as the design in force.

pub mod array;
pub(crate) mod cells;
pub mod class;
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
pub mod weak;

pub use class::{Class, ClassBuilder};
pub use intern::{intern, intern_str};
pub use memory::{Arena, BlockPool, Heap, LLContext, LimelightAlloc};
pub use object::Object;
pub use refcount::{MemoryCategory, RcHeader};
pub use string::LLString;
pub use value::{Tag, Value};
pub use weak::LLWeakRef;
