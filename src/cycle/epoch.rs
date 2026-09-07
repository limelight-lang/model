//! The collection epoch: how many commits this process has closed, and the
//! two-bit stamp a maturation carries.
//!
//! A maturation stamp says that the collection of one epoch read a component
//! as externally referenced, and the epoch is what retires it: past the
//! turnover a stamp of an earlier epoch reads as no stamp at all, so a
//! component that lost its last external reference while it was mature is
//! traced again instead of being pruned for ever
//! (`rfc/model/gc/rc-cycle.md`, "Decision summary", the age-based pruning
//! bullet, and `PLAN.md` S37.4, which re-offers the roots the turnover frees).
//!
//! **The counter is one process-global full-width word rather than a
//! per-thread one.** Two threads that collect in the same wall-clock minute
//! stamp with the same epoch, so a component split across their heaps matures
//! at one rate; and the full width is what a per-thread mirror compares
//! against to see a turnover it slept through, which two wrapped bits cannot
//! answer (`rfc/dev/DECISIONS.md`, closing Y12 clause 8). What the header
//! carries is the low two bits, because that is what byte 6 can spare
//! (`crate::refcount::MATURATION_EPOCH_MASK`).

use std::sync::atomic::{AtomicU64, Ordering};

use crate::refcount::MATURATION_EPOCH_MASK;

/// Commits closed since process start, counted by [`commit_closed`].
///
/// Relaxed throughout: the value orders nothing, and a collection that reads
/// it one commit late stamps with the epoch of a moment that has just passed —
/// which costs recall on a component whose stamp then reads stale a turnover
/// early, and cannot make a live component look mature.
static COMMITS: AtomicU64 = AtomicU64::new(0);

/// Commits one epoch spans.
///
/// 64, provisional after YRC's only published value; what a real workload
/// wants is `PLAN.md` S37.5's measurement, which replaces this number or
/// records it as confirmed.
const COMMITS_PER_EPOCH: u64 = 64;

/// Epochs the header's field tells apart, past which the count wraps.
const EPOCHS: u64 = 4;

const _: () = assert!(
    EPOCHS == (MATURATION_EPOCH_MASK >> MATURATION_EPOCH_MASK.trailing_zeros()) as u64 + 1,
    "the epoch wraps at what the stamp's field holds"
);

/// The epoch a collection starting now stamps its proven-live components with.
///
/// Two collections of one epoch are what an age counts, so a component read as
/// live by both carries age 2; a collection past the turnover reads every
/// earlier stamp as unstamped.
pub(crate) fn current() -> u32 {
    #[cfg(test)]
    if let Some(pinned) = pinned() {
        return pinned;
    }

    epoch_of(COMMITS.load(Ordering::Relaxed))
}

/// Count one commit: a collection has read every component its trace proposed,
/// and the stamps of the ones it proved live are written.
///
/// The caller is the close of the commit itself
/// (`crate::cycle::finalization::Revalidation::close`), so a trace that
/// proposed nothing and a collection that aborted before its finalization
/// count nothing.
pub(crate) fn commit_closed() {
    COMMITS.fetch_add(1, Ordering::Relaxed);
}

/// Which epoch a given number of closed commits stands in.
fn epoch_of(commits: u64) -> u32 {
    ((commits / COMMITS_PER_EPOCH) % EPOCHS) as u32
}

/// Commits closed process-wide, which a case reads to see that a commit of its
/// own was counted. The epoch itself moves once in 64, so it answers nothing
/// about a single commit.
#[cfg(test)]
pub(crate) fn commits() -> u64 {
    COMMITS.load(Ordering::Relaxed)
}

/// This thread's pinned epoch, or `None` when it reads the counter.
#[cfg(test)]
fn pinned() -> Option<u32> {
    PINNED.with(std::cell::Cell::get)
}

#[cfg(test)]
thread_local! {
    /// Set by [`pin`] and read by [`current`]; no drop glue, so it is legal on
    /// every path a thread's exit reaches (`dev/INDEX.md`, thread exit).
    static PINNED: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// Hold this thread's reading of the epoch at `epoch` until the guard is
/// dropped, so that a case can stamp in a chosen epoch and in the next one.
///
/// A case cannot get there by driving commits: the counter is process-global
/// and 64 commits move every other thread's epoch under it, which is a flake
/// in whichever case was reading a stamp at the time. The pin is this thread's
/// alone and leaves the counter where it stands.
#[cfg(test)]
pub(crate) fn pin(epoch: u32) -> EpochPin {
    let restored = PINNED.with(|cell| cell.replace(Some(epoch)));
    EpochPin { restored }
}

/// The pin [`pin`] opened, which puts back what this thread read before it.
#[cfg(test)]
pub(crate) struct EpochPin {
    restored: Option<u32>,
}

#[cfg(test)]
impl Drop for EpochPin {
    fn drop(&mut self) {
        PINNED.with(|cell| cell.set(self.restored));
    }
}

#[cfg(test)]
mod tests;
