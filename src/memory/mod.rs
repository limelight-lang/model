//! Memory manager: block pool, arenas, buffers.
//!
//! Implements `docs/memory-manager.md`. Design rationale lives in the
//! `rfc` repository (`model/memory/*`).

pub mod arena;
pub mod barrier;
pub mod block_pool;
pub mod buffer;
pub mod buffer_arena;
pub mod context;
pub(crate) mod critical;
pub mod heap;
pub mod immortal;
pub(crate) mod large_entity;
pub(crate) mod reserve;
pub(crate) mod reset_window;
pub(crate) mod retained;
pub(crate) mod routing;
pub mod stats;
pub mod stdapi;

pub use arena::Arena;
pub use block_pool::{BLOCK_PAYLOAD, BLOCK_SIZE, BlockPool, LINE_SIZE};
pub use buffer::{Buffer, PressureMode};
pub use buffer_arena::BufferArena;
pub use context::LLContext;
pub use heap::Heap;
pub use stdapi::LimelightAlloc;
