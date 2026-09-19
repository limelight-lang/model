//! The collection epoch: how many commits the collecting thread has closed,
//! and the two-bit stamp a maturation carries.
//!
//! A maturation stamp says that the collection of one epoch read a component
//! as held from outside — its exact validation reading it as externally
//! referenced, or its scan proving it live — and the epoch is what retires it: past the
//! turnover a stamp of an earlier epoch reads as no stamp at all, so a
//! component that lost its last external reference while it was mature is
//! traced again instead of being pruned for ever
//! (`rfc/model/gc/rc-cycle.md`, "Decision summary", the age-based pruning
//! bullet, and `PLAN.md` S37.4, which re-offers the roots the turnover frees).
//!
//! **The clock is the collecting thread's own** (Edmond, 2026-09-19): the
//! counter is a full-width word in the mutator's record
//! (`crate::cycle::mutator_record`, the writer line), counted up by that
//! thread's commits alone. A thread's stamps therefore age at the rate that
//! thread collects at, where a process-global word would hand the rate to the
//! busiest thread in the process and leave a thread that collects rarely
//! reading every stamp of its own as stale. The entities of one mutator are
//! that mutator's — no thread points into another thread's blocks
//! (`rfc/model/gc/rc-cycle.md`, the disjointness the token's proof assumes) —
//! so a collector thread tracing for a mutator reads the epoch out of that
//! mutator's record and never out of its own thread's
//! ([`of_record`]). The width is full because a per-thread mirror compares
//! turnovers against it to see one it slept through, which two wrapped bits
//! cannot answer; what the header carries is the low two bits, because that is
//! what byte 6 can spare (`crate::refcount::MATURATION_EPOCH_MASK`).

use crate::cycle::mutator_record::{MutatorRecord, this_thread_record};
use crate::refcount::MATURATION_EPOCH_MASK;

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

    epoch_of(commits())
}

/// The epoch the entities of `record`'s mutator were stamped in, for a
/// collector thread tracing that mutator's graph: the owner's clock, since the
/// stamps are the owner's commits' (module doc).
///
/// # Safety
/// `record` is a live record, held for the length of the call by the trace
/// token or by the registry's hold.
pub(crate) unsafe fn of_record(record: *const MutatorRecord) -> u32 {
    epoch_of(unsafe { (*record).commits() })
}

/// Count one commit: a collection has read every component its trace proposed,
/// and the stamps of the ones it proved live are written.
///
/// The caller is the close of the commit itself
/// (`crate::cycle::finalization::Revalidation::close`), so a trace that
/// proposed nothing and a collection that aborted before its finalization
/// count nothing. It counts into the record of the thread that closed the
/// commit, and a thread with no record — one that ran no `ll_thread_init`,
/// and therefore no collection — counts nowhere.
pub(crate) fn commit_closed() {
    let record = this_thread_record();
    debug_assert!(
        !record.is_null(),
        "a commit is closed by a collection, which a registered thread runs"
    );
    if record.is_null() {
        return;
    }

    unsafe { (*record).note_commit() };
}

/// The count a record's next life starts from: one turnover past the life
/// that just ended, so that no stamp the old life wrote reads fresh against
/// the new one's clock. Zero would not do it — zero is epoch 0, which every
/// stamp written in the old life's first epoch carries, and a thread that
/// adopts the dead thread's blocks would prune those entities at its first
/// collection (`crate::memory::heap`, thread exit and adoption).
///
/// The counter is monotone across the record's lives for that reason, and
/// wraps with the epoch as any stamp four turnovers old does.
pub(crate) fn a_new_lifes_count(ended_at: u64) -> u64 {
    ended_at.wrapping_add(COMMITS_PER_EPOCH)
}

/// Which epoch a given number of closed commits stands in.
fn epoch_of(commits: u64) -> u32 {
    ((commits / COMMITS_PER_EPOCH) % EPOCHS) as u32
}

/// Commits this thread has closed, and zero for a thread with no record,
/// which has closed none.
///
/// The mutator queue compares this full-width value with its private mirror at
/// a safepoint. The low two epoch bits in a header cannot answer whether four
/// turns elapsed while that mutator was asleep.
pub(crate) fn commits() -> u64 {
    let record = this_thread_record();
    if record.is_null() {
        return 0;
    }

    unsafe { (*record).commits() }
}

/// How many turnovers `commits` closed commits stand past process start.
///
/// The mutator queue compares this rather than [`current`]: the epoch itself
/// wraps at four, and a lane whose mutator slept through four turnovers would
/// read as one that slept through none.
pub(crate) fn turnovers_of(commits: u64) -> u64 {
    commits / COMMITS_PER_EPOCH
}

/// A commit count inside the same turnover as `commits`, and one past that
/// turnover's first commit.
///
/// A case that probes with `commits + 1` is asking a question about the
/// counter rather than about the queue: at 63 commits past a turnover the
/// increment crosses one.
#[cfg(test)]
pub(crate) fn one_commit_inside_the_turnover_of(commits: u64) -> u64 {
    commits - commits % COMMITS_PER_EPOCH + 1
}

/// The commit count one turnover past `commits`, which a case passes to the
/// mutator poll in place of the counter.
///
/// Driving 64 real collections is what this spares the case; the counter
/// itself is left where it stands, so a case that reads a stamp beside this
/// one reads its own thread's clock unchanged.
#[cfg(test)]
pub(crate) fn one_turnover_past(commits: u64) -> u64 {
    commits + COMMITS_PER_EPOCH
}

/// Close as many commits as one epoch spans, tests only: a case that needs
/// two epochs of one thread's clock drives the counter rather than 64
/// collections.
#[cfg(test)]
pub(crate) fn close_a_turnover_of_commits() {
    for _ in 0..COMMITS_PER_EPOCH {
        commit_closed();
    }
}

/// Put this thread's clock at the first commit of a later turnover whose
/// epoch is not zero, tests only.
///
/// A case that stamps across several collections takes this: a stamp of the
/// epoch before a turnover reads as no stamp after it, and where the harness
/// thread's counter stands when the case starts is that thread's own history.
/// Epoch zero is passed over because a record the registry has just handed out
/// reads zero, so a case that tells two clocks apart by the difference would
/// tell nothing there. The epoch always moves, so the caller can compare
/// against the one it read before the call.
#[cfg(test)]
pub(crate) fn stand_at_the_start_of_a_nonzero_epoch() {
    loop {
        commit_closed();
        while commits() % COMMITS_PER_EPOCH != 0 {
            commit_closed();
        }

        if epoch_of(commits()) != 0 {
            return;
        }
    }
}

/// Close commits until this thread stands one short of its next turnover, so
/// that the next commit closed crosses it.
///
/// A case about which reading of the counter a collection records takes the
/// crossing: the reading before the collection's own commit and the one after
/// it stand in two epochs there, and in the same epoch everywhere else.
#[cfg(test)]
pub(crate) fn close_commits_to_one_short_of_the_turnover() {
    while commits() % COMMITS_PER_EPOCH != COMMITS_PER_EPOCH - 1 {
        commit_closed();
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
/// The pin answers for [`current`] alone, which is this thread's reading; a
/// collector thread reading this thread's record ([`of_record`]) sees the
/// counter, so a case that drives a collector arranges the epochs through the
/// counter rather than through a pin.
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
