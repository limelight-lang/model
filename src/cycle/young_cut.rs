//! The young cut (`dev/design/the-general-algorithm.md`, "The young cut";
//! `dev/BENCHMARKS.md`, "an arm that offers only R's entries older than
//! 100 ms"): under `gc-window` the offer reads R as its entries older than the
//! cut, so that a root registered moments ago, which mostly dies by its count
//! before a batch could read it, is not traced (Edmond, 2026-10-09: 100 ms,
//! the embedder's to set).
//!
//! **The clock of R's appends.** Each thread counts the entries that reach
//! R's tail — a registration that lands, a write-back, a lane spliced back —
//! and keeps a ring of [`BUCKETS`] buckets, each the instant it opened on the
//! serve clock and the count then. A bucket opens at most once a sixteenth
//! of the cut, when the count grew since the newest: at the offer's poll,
//! and on the append path when the count crosses a multiple of
//! [`TICK_EVERY`]. The entries younger than the cut are at most the count
//! less that of the newest bucket opened a cut ago or earlier, and every
//! append where no bucket is that old.
//!
//! **The bound.** A bucket's count is the count at an instant at least the
//! cut ago, so every entry appended since, the young among them, reads
//! young: no entry younger than the cut is offered, but in the batch after
//! a lane's merge, which reads R whole. An entry R loses — a batch's
//! advance, a compaction's discard — still counts, which reads R younger and
//! delays an offer, never hastens one. An entry reads old a cut after the
//! first bucket opened past it: the next tick a sixteenth of the cut after
//! the bucket before, so on a thread that polls the delay past the cut is
//! a sixteenth of it or the gap between two polls. Seventeen buckets a sixteenth of the
//! cut apart span the cut, so once the ring wrapped, its oldest bucket is a
//! cut old. The clock runs whatever the cut, so that a cut set mid-run reads
//! no entry of before as old.
//!
//! **No allocation.** The ring and the count are const thread-locals with no
//! drop glue, as every thread-local the exit reaches.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// The buckets of the clock's ring: one more than the sixteen a cut spans.
const BUCKETS: usize = 17;

/// The appends between two readings of the clock on the append path.
const TICK_EVERY: u64 = 64;

/// The crate's cut: 100 ms (Edmond, 2026-10-09).
const YOUNG_CUT: Duration = Duration::from_millis(100);

/// The embedder's cut in nanoseconds, or zero for the crate's
/// ([`set_young_cut`]).
static EMBEDDERS_CUT_NANOS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// Every entry appended to this thread's R since its start.
    static APPENDS: Cell<u64> = const { Cell::new(0) };
    /// The instant each bucket opened, on the serve clock; zero for one
    /// never opened.
    static OPENED: [Cell<u64>; BUCKETS] = const { [const { Cell::new(0) }; BUCKETS] };
    /// The count of appends at each bucket's opening.
    static COUNTS: [Cell<u64>; BUCKETS] = const { [const { Cell::new(0) }; BUCKETS] };
    /// The newest bucket's index.
    static NEWEST: Cell<usize> = const { Cell::new(0) };
}

/// Set the embedder's cut; zero restores the crate's.
pub(crate) fn set_young_cut(cut: Duration) {
    let nanos = u64::try_from(cut.as_nanos()).unwrap_or(u64::MAX);
    EMBEDDERS_CUT_NANOS.store(nanos, Ordering::Relaxed);
}

/// The cut in force: a case's, the embedder's, or the crate's.
pub(crate) fn cut() -> Duration {
    #[cfg(test)]
    if let Some(cut) = testing::cut() {
        return cut;
    }

    match EMBEDDERS_CUT_NANOS.load(Ordering::Relaxed) {
        0 => crates_cut(),
        nanos => Duration::from_nanos(nanos),
    }
}

/// The crate's cut, [`YOUNG_CUT`]; none under the unit cases, which read
/// the collector's work over R whole, the cut's own cases and the rig
/// setting one.
fn crates_cut() -> Duration {
    #[cfg(test)]
    return Duration::ZERO;
    #[cfg(not(test))]
    return YOUNG_CUT;
}

/// The cut the crate ships, for the rig, which runs as a case and measures
/// the shipped rule.
#[cfg(test)]
pub(crate) const fn shipped_cut() -> Duration {
    YOUNG_CUT
}

/// Count `entries` appended to R's tail, on the mutator's thread, and read
/// the clock where the count crossed a multiple of [`TICK_EVERY`] with a cut
/// in force.
#[inline]
pub(crate) fn note_appends(entries: u64) {
    let before = APPENDS.with(Cell::get);
    let after = before.wrapping_add(entries);
    APPENDS.with(|appends| appends.set(after));
    if before / TICK_EVERY != after / TICK_EVERY {
        let cut = cut();
        if !cut.is_zero() {
            tick(instant(), cut);
        }
    }
}

/// The clock's reading: the serve clock, and under a case the time it set
/// the clock ahead by.
#[inline]
pub(crate) fn instant() -> u64 {
    let instant = crate::cycle::worker::serve_clock_now();
    #[cfg(test)]
    let instant = instant + testing::ahead();
    instant
}

/// [`tick`] at the clock's reading, read only where the count grew
/// since the newest bucket: the poll's tick.
#[inline]
pub(crate) fn tick_if_appended(cut: Duration) {
    let count = APPENDS.with(Cell::get);
    let newest = NEWEST.with(Cell::get);
    if count != COUNTS.with(|counts| counts[newest].get()) {
        tick(instant(), cut);
    }
}

/// Open a bucket at `instant` if the count grew since the newest and the
/// newest is a sixteenth of `cut` old.
pub(crate) fn tick(instant: u64, cut: Duration) {
    let count = APPENDS.with(Cell::get);
    let newest = NEWEST.with(Cell::get);
    let opened = OPENED.with(|opened| opened[newest].get());
    let counted = COUNTS.with(|counts| counts[newest].get());
    let width = (cut.as_nanos() / 16) as u64;
    if opened != 0 && (counted == count || instant.saturating_sub(opened) < width) {
        return;
    }
    let next = if opened == 0 {
        newest
    } else {
        (newest + 1) % BUCKETS
    };
    OPENED.with(|opened| opened[next].set(instant));
    COUNTS.with(|counts| counts[next].set(count));
    NEWEST.with(|at| at.set(next));
}

/// The entries appended to R within `cut` of `instant`, at most: the count
/// less that of the newest bucket opened at or before `instant - cut`, or
/// every append where no bucket is that old.
pub(crate) fn young(instant: u64, cut: Duration) -> u64 {
    let count = APPENDS.with(Cell::get);
    let Some(limit) = instant.checked_sub(cut.as_nanos() as u64) else {
        return count;
    };
    let mut base = None::<(u64, u64)>;
    OPENED.with(|opened| {
        COUNTS.with(|counts| {
            for (at, counted) in opened.iter().zip(counts) {
                let at = at.get();
                if at != 0 && at <= limit && base.is_none_or(|(newest, _)| at > newest) {
                    base = Some((at, counted.get()));
                }
            }
        });
    });
    base.map_or(count, |(_, counted)| count.wrapping_sub(counted))
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    thread_local! {
        /// A case's cut on this thread, or `None` for the embedder's and the
        /// crate's: the offer reads the cut on the mutator's own thread.
        static CUT: Cell<Option<Duration>> = const { Cell::new(None) };
    }

    /// Read R older than `cut` on this thread for the case, or the
    /// embedder's and the crate's again for `None`.
    pub(crate) fn cut_at(cut: Option<Duration>) {
        CUT.with(|at| at.set(cut));
    }

    pub(super) fn cut() -> Option<Duration> {
        CUT.with(Cell::get)
    }

    thread_local! {
        /// The nanoseconds a case set this thread's clock ahead by.
        static AHEAD: Cell<u64> = const { Cell::new(0) };
    }

    /// Set this thread's clock `by` further ahead, as if that much time had
    /// passed.
    pub(crate) fn set_the_clock_ahead(by: Duration) {
        AHEAD.with(|ahead| ahead.set(ahead.get() + by.as_nanos() as u64));
    }

    pub(super) fn ahead() -> u64 {
        AHEAD.with(Cell::get)
    }

    /// Forget this thread's clock: a case's fresh start.
    pub(crate) fn reset_the_clock() {
        APPENDS.with(|appends| appends.set(0));
        OPENED.with(|opened| opened.iter().for_each(|at| at.set(0)));
        COUNTS.with(|counts| counts.iter().for_each(|count| count.set(0)));
        NEWEST.with(|at| at.set(0));
        AHEAD.with(|ahead| ahead.set(0));
    }

    /// Count `entries` appended without reading the clock.
    pub(crate) fn append_quietly(entries: u64) {
        APPENDS.with(|appends| appends.set(appends.get() + entries));
    }
}

#[cfg(test)]
mod tests;
