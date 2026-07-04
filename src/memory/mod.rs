//! Memory manager: block pool, arenas, buffers.
//!
//! Implements `docs/memory-manager.md`. Design rationale lives in the
//! `rfc` repository (`model/memory/*`).

pub mod arena;
pub mod block_pool;
pub mod buffer;
pub mod context;

pub use arena::Arena;
pub use block_pool::{BLOCK_PAYLOAD, BLOCK_SIZE, BlockPool, LINE_SIZE};
pub use buffer::Buffer;
pub use context::LLContext;
