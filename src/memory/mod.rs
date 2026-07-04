//! Memory manager: block pool, arenas, buffers.
//!
//! Implements `docs/memory-manager.md`. Design rationale lives in the
//! `rfc` repository (`model/memory/*`).

pub mod arena;
pub mod block_pool;
pub mod context;
pub mod heap;
pub mod stdapi;

pub use arena::Arena;
pub use block_pool::{BLOCK_PAYLOAD, BLOCK_SIZE, BlockPool, LINE_SIZE};
pub use context::LLContext;
pub use heap::Heap;
pub use stdapi::LimelightAlloc;
