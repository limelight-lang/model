//! The collection epoch: the mutator's epoch clock, which the collector keeps,
//! and the two-bit stamp a maturation carries.
//!
//! A maturation stamp says that the collection of one epoch read a component
//! as held from outside — its exact validation reading it as externally
//! referenced, or its scan proving it live — and the epoch is what retires it: past the
//! turnover a stamp of an earlier epoch reads as no stamp at all, so a
//! component that lost its last external reference while it was mature is
//! traced again instead of being pruned for ever
//! (`rfc/model/gc/rc-cycle.md`, "Decision summary", the age-based pruning
//! bullet; the re-offer is `crate::cycle::queue::reoffer_deferred_if_epoch_moved`).
//!
//! **The clock is the collector's** (Edmond, 2026-09-23; `dev/DECISIONS.md`,
//! "the collector finds and the mutator judges, and a recall of the token
//! bounds the mutator's wait instead of the budget"): a full-width count of
//! turnovers in the mutator's record (`crate::cycle::mutator_record`, the hold
//! line), written by the collector the record is named to and by nobody else.
//! It advances at the first of [`BATCHES_PER_EPOCH`] batches that collector
//! made for the mutator or X of the collector's own clock since the last
//! advance (`crate::cycle::worker`, "The epoch clock"), so a thread's stamps
//! age whether or not it collects, and the mutator stores nothing for it.
//! Every collection over a mutator's graph reads the cell once, at its
//! arena's open, and prunes, stamps and records the deferred lane's mirror
//! against that one reading; the entities of one mutator are that mutator's
//! — no thread points into another thread's blocks
//! (`rfc/model/gc/rc-cycle.md`, the disjointness the token's proof assumes)
//! — so a collector thread tracing for a mutator reads the cell out of that
//! mutator's record ([`of_record`]). The width is full because the deferred
//! lane's mirror compares turnovers to see one the thread slept through,
//! which two wrapped bits cannot answer; what the header carries is the low
//! two bits, because that is what byte 6 can spare
//! (`crate::refcount::MATURATION_EPOCH_MASK`).
//!
//! **A reading that missed an advance is conservative.** The stamp is read
//! in one place, the mark's test for an opaque live external
//! (`crate::cycle::mark`), and all it changes is that an edge into the target
//! reads as an external live reference: rows only grow. A collection that
//! read the cell before an advance prunes against an epoch that has ended,
//! which costs recall until the next advance, and an advance between a
//! collection's open and its stamps leaves those stamps dead at birth, which
//! costs a descent.
//!
//! # A record's next life
//!
//! The registry leaves the cell where the last life moved it and notes the new
//! life; the collector's next visit advances the cell once and clears the
//! note (`crate::cycle::worker`, "The epoch clock"), so that no stamp the old
//! life wrote reads fresh against the new one's clock — a thread that adopts
//! the dead thread's blocks would otherwise prune those entities. The window
//! is one round at most, and a collection of the new life inside it prunes
//! against the old epoch, which is recall.

use crate::cycle::mutator_record::{MutatorRecord, this_thread_record};
use crate::refcount::MATURATION_EPOCH_MASK;

/// Batches a collector makes for one mutator before it advances that
/// mutator's epoch, if X has not come first.
///
/// 64, YRC's published value for the collections one epoch spans
/// (`rfc/model/gc/cycle/questions.md`, Y9), which no reading has replaced: the
/// lane re-offers `rate × N` records per turnover and a component that dies
/// behind a deferral waits up to `N` batches, so a reading gives an exchange
/// rate and the side to pay is a ruling (`dev/BENCHMARKS.md`, "S37.5 what a
/// turnover re-offers, and what a deferral costs").
pub(crate) const BATCHES_PER_EPOCH: u8 = 64;

/// Epochs the header's field tells apart, past which the count wraps.
const EPOCHS: u64 = 4;

const _: () = assert!(
    EPOCHS == (MATURATION_EPOCH_MASK >> MATURATION_EPOCH_MASK.trailing_zeros()) as u64 + 1,
    "the epoch wraps at what the stamp's field holds"
);

/// The epoch a collection on this thread opening now would prune and stamp
/// against, tests only: this thread's reading of its own cell, or the case's
/// pin. The arena takes the same reading at its open
/// (`crate::cycle::arena::TraceScratchArena::open`).
///
/// Two collections of one epoch are what an age counts, so a component read as
/// live by both carries age 2; a collection past the turnover reads every
/// earlier stamp as unstamped.
#[cfg(test)]
pub(crate) fn current() -> u32 {
    epoch_this_thread_reads(this_threads_turnovers())
}

/// The epoch this thread prunes and stamps against for a reading of
/// `turnovers`: its two low bits, or the case's pin.
pub(crate) fn epoch_this_thread_reads(turnovers: u64) -> u32 {
    #[cfg(test)]
    if let Some(pinned) = pinned() {
        return pinned;
    }

    epoch_of(turnovers)
}

/// This thread's epoch cell, full width; zero for a thread with no record,
/// which no collector keeps a clock for.
pub(crate) fn this_threads_turnovers() -> u64 {
    let record = this_thread_record();
    if record.is_null() {
        return 0;
    }

    unsafe { (*record).turnovers() }
}

/// The epoch cell of `record`'s mutator, full width, for a collector thread
/// tracing that mutator's graph: the owner's clock, since the stamps are the
/// owner's (module doc).
///
/// # Safety
/// `record` is a live record, held for the length of the call by the trace
/// token or by the registry's hold.
pub(crate) unsafe fn of_record(record: *const MutatorRecord) -> u64 {
    unsafe { (*record).turnovers() }
}

/// Which epoch a count of turnovers stands in.
pub(crate) fn epoch_of(turnovers: u64) -> u32 {
    (turnovers % EPOCHS) as u32
}

/// Advance `record`'s cell by one turnover as its collector would, tests
/// only: the harness thread stands in for the collector, which in a process
/// with no collector born it is. The instant the next advance is measured
/// from is left where it stood.
///
/// # Safety
/// `record` is a live record.
#[cfg(test)]
pub(crate) unsafe fn turn_the_cell_of(record: *const MutatorRecord) {
    let record = unsafe { &*record };
    record.advance_the_epoch(record.advanced_at());
}

/// Advance this thread's cell by one turnover ([`turn_the_cell_of`]), tests
/// only; a thread with no record has no cell.
#[cfg(test)]
pub(crate) fn turn_this_threads_cell() {
    let record = this_thread_record();
    assert!(
        !record.is_null(),
        "a case turns the cell of a registered thread"
    );
    unsafe { turn_the_cell_of(record) };
}

/// Turn this thread's cell once, and on until its epoch is not zero, tests
/// only.
///
/// A case that stamps across several collections takes this: a stamp of the
/// epoch before a turnover reads as no stamp after it. Epoch zero is passed
/// over because a record the registry has just carved reads zero, so a case
/// that tells two clocks apart by the difference would tell nothing there.
/// The epoch always moves, so the caller can compare against the one it read
/// before the call.
#[cfg(test)]
pub(crate) fn turn_to_a_nonzero_epoch() {
    turn_this_threads_cell();
    while epoch_of(this_threads_turnovers()) == 0 {
        turn_this_threads_cell();
    }
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
/// The pin answers for this thread's readings alone ([`current`],
/// [`epoch_this_thread_reads`]); a collector thread reading this thread's
/// record ([`of_record`]) sees the cell, so a case that drives a collector
/// arranges the epochs through the cell rather than through a pin.
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
