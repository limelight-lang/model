//! When the poll runs a trace: once the bytes of the slots in use in the
//! thread's entity blocks pass a ratio of what the last trace left live, and
//! never under a minimum (`dev/design/the-general-algorithm.md`, "The Sage on
//! the revised build", which reads the existing test counter at a fixed ratio).
//!
//! The bytes are `Heap::bytes_in_owned_blocks`, slots in use rather than
//! blocks held, so a fragmented heap does not keep a trace due, at the price of
//! a counter at slot grain on the allocation path, which this arm accepts and
//! the full design does not. What a trace left live is the bytes at its start
//! less the garbage slots it found, set before the destructors run: a slot a
//! destructor allocates counts toward the next trace, and a teardown's
//! unwalked children (strings, Boxes), freed by the sever, are left in the
//! baseline, which only delays the next trace.
//!
//! The baseline stands in the thread's entity heap
//! (`crate::memory::heap::entity_heap_left_live`), so a heap a thread starts
//! anew starts at zero.

use std::sync::OnceLock;

use crate::memory::heap::{entity_heap_left_live, set_entity_heap_left_live};

/// No trace below this many bytes, whatever the ratio reads.
pub(crate) const MINIMUM_BYTES: usize = 4 << 20;

/// The ratio when `LL_TB_RATIO` is unset or does not parse as a positive
/// number.
const DEFAULT_RATIO: f64 = 2.0;

/// The ratio of held to live bytes a trace waits for, read from `LL_TB_RATIO`
/// once per process.
pub(crate) fn ratio() -> f64 {
    static RATIO: OnceLock<f64> = OnceLock::new();
    *RATIO.get_or_init(|| {
        std::env::var("LL_TB_RATIO")
            .ok()
            .and_then(|text| text.trim().parse::<f64>().ok())
            .filter(|ratio| ratio.is_finite() && *ratio > 0.0)
            .unwrap_or(DEFAULT_RATIO)
    })
}

/// Whether `held` bytes call for a trace on this thread.
pub(crate) fn is_due(held: usize) -> bool {
    let bound = (entity_heap_left_live() as f64 * ratio()).max(MINIMUM_BYTES as f64);
    held as f64 > bound
}

/// Take `live` as what this thread's trace leaves live.
pub(crate) fn reset_baseline(live: usize) {
    set_entity_heap_left_live(live);
}
