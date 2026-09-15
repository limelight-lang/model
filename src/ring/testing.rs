//! The one hook a case sets on the reader: something to run between the
//! reader's two readings of an empty front block, so that a case can fill
//! and leave that block in the window the second reading exists for.

use std::cell::Cell;

thread_local! {
    /// What runs between the reads, or null; cleared before it runs, so a
    /// hook that reads again does not recurse into itself.
    static BETWEEN_THE_READS: Cell<Option<fn()>> = const { Cell::new(None) };
}

/// Run `hook` once, between the reader's next two readings of an empty
/// front block on this thread.
pub(crate) fn run_between_the_reads(hook: fn()) {
    BETWEEN_THE_READS.with(|cell| cell.set(Some(hook)));
}

pub(super) fn between_the_reads() {
    if let Some(hook) = BETWEEN_THE_READS.with(Cell::take) {
        hook();
    }
}
