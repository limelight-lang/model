//! The backup trace's measuring arm: in place of candidate registration, the
//! owner traces its whole entity heap at a poll and frees the garbage cycles it
//! finds (`dev/design/the-general-algorithm.md`, "A third collector: the backup
//! trace", and the reduced arm "The Sage on the revised build" advises).
//!
//! # What it is for
//!
//! A measurement, and no collector the runtime ships. Under `trace-backup-rig`
//! the release path registers nothing (`crate::refcount`, `release_word`), the
//! collector thread is never started (`crate::cycle::worker`), and the poll
//! runs [`trace_if_due`] (`crate::gc::ll_gc_maybe_collect`). The rig reads
//! what the traces cost from [`counts`]. What the arm leaves out is the Sage's
//! list: thread exit, the pressure path and `gc_collect_cycles()` run trial
//! deletion's paths over an empty R and free no cycle; retained former-arena
//! blocks, large entities and their rings are not walked, and a ring through
//! one of them is never freed; the overflow map is not kept.
//!
//! # The trace
//!
//! On the owner, at a poll whose gate is open, so every reference the thread
//! holds is counted (`crate::cycle::collect::may_collect`). This thread's
//! collecting word stands raised from the fill to the clear, which closes the
//! gate: a destructor's poll, allocation failure or explicit collection inside
//! the trace collects nothing, and no trace nests (the Critic's finding 1).
//!
//! 1. Fill, subtract, mark and harvest ([`census`]): every live entity a ring
//!    can pass through, of this thread's owned entity blocks, takes its count
//!    as a side count in header bytes 6-7 ([`field`]); each edge from a walked
//!    entity to a walked entity of this thread takes one off the target's;
//!    whatever stays above zero is a root, and the mark reaches out from the
//!    roots; the walked entities left unmarked are the garbage.
//! 2. The threshold's baseline is reset ([`threshold`]), before any user code
//!    runs.
//! 3. The garbage is split into connected components, and each is finalized
//!    and reclaimed by the crate's own chain ([`components`]).
//! 4. The field of every walked entity still standing is cleared ([`census`]).
//!
//! The verdict is trial deletion's with every entity a candidate: the same
//! edges subtracted from the same counts, and what the subtraction leaves above
//! zero marking what it reaches. The tests hold the two together on one heap
//! (`tests::the_verdict_against_trial_deletion`).

#[cfg(feature = "recycler-over-counts")]
compile_error!(
    "`trace-backup-rig` and `recycler-over-counts` are two cycle collectors \
     over the same header bytes 6-7; build one of them"
);

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::cycle::arena::TraceScratchArena;
use crate::cycle::mutator_record::MutatorRecord;
#[cfg(test)]
use crate::refcount::RcHeader;

pub(crate) mod census;
pub(crate) mod components;
pub(crate) mod field;
pub(crate) mod threshold;

/// What one trace read and freed.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct Trace {
    /// Entities the fill walked.
    pub(crate) walked: usize,
    /// Walked entities the mark did not reach.
    pub(crate) garbage: usize,
    /// Connected components the garbage split into.
    pub(crate) components: usize,
    /// Members the reclamation freed: the garbage less every component a
    /// destructor resurrected or whose teardown the arena refused.
    pub(crate) freed: usize,
}

/// The poll's seam: run a trace where this thread's bytes call for one, and
/// answer how many members it freed.
///
/// # Safety
/// As [`trace`].
pub(crate) unsafe fn trace_if_due() -> usize {
    if !threshold::is_due(crate::memory::heap::entity_bytes_in_owned_blocks()) {
        return 0;
    }

    unsafe { trace() }.map_or(0, |trace| trace.freed)
}

/// Run one trace at once, or answer `None` where this thread may not collect: a
/// collection, a reset or a teardown is in flight on it, or it has no record
/// for the collecting word.
///
/// # Safety
/// At a safepoint of the calling mutator: counts and edges consistent, every
/// reference the thread holds counted.
pub(crate) unsafe fn trace() -> Option<Trace> {
    let _collecting = CollectingWord::raise()?;
    let held = crate::memory::heap::entity_bytes_in_owned_blocks();
    let start = Instant::now();

    let walked = unsafe { census::fill() };
    unsafe { census::subtract() };
    let subtracted = Instant::now();
    unsafe { census::mark() };
    let marked = Instant::now();

    let (mut garbage, garbage_bytes) = unsafe { census::harvest() };
    threshold::reset_baseline(held.saturating_sub(garbage_bytes));
    let components = unsafe { components::split(&mut garbage) };
    let split = Instant::now();

    let freed = if garbage.is_empty() {
        0
    } else {
        // The workspace is the thread's, drawn at its first collection and
        // held until it exits; refused, the garbage stands for the next trace.
        match TraceScratchArena::open() {
            Some(mut arena) => {
                let freed =
                    unsafe { components::finalize_and_reclaim(&garbage, &components, &mut arena) };
                arena.reset();
                freed
            }
            None => 0,
        }
    };
    let reclaimed = Instant::now();

    unsafe { census::clear() };
    let end = Instant::now();

    note(Spent {
        field: subtracted - start,
        mark: marked - subtracted,
        components: split - marked,
        reclamation: reclaimed - split,
        total: end - start,
        walked,
        freed,
    });
    Some(Trace {
        walked,
        garbage: garbage.len(),
        components: components.len(),
        freed,
    })
}

/// The verdict alone: fill, subtract, mark and harvest, the field cleared
/// behind them and nothing freed. Answers the garbage, or `None` as [`trace`]
/// does.
///
/// # Safety
/// As [`trace`].
#[cfg(test)]
pub(crate) unsafe fn verdict() -> Option<Vec<*mut RcHeader>> {
    let _collecting = CollectingWord::raise()?;
    unsafe {
        census::fill();
        census::subtract();
        census::mark();
    }
    let (garbage, _) = unsafe { census::harvest() };
    unsafe { census::clear() };
    Some(garbage)
}

/// This thread's collecting word, raised for as long as one trace runs, and
/// lowered when this value drops.
struct CollectingWord(*mut MutatorRecord);

impl CollectingWord {
    /// Raise the word, or answer `None` where the gate is closed or the thread
    /// has no record.
    fn raise() -> Option<Self> {
        if !crate::cycle::collect::may_collect() {
            return None;
        }

        let record = crate::cycle::mutator_record::this_thread_record();
        if record.is_null() {
            return None;
        }

        unsafe { (*record).set_collecting() };
        Some(Self(record))
    }
}

impl Drop for CollectingWord {
    fn drop(&mut self) {
        unsafe { (*self.0).clear_collecting() };
    }
}

/// One trace's cost and yield, for the counters.
struct Spent {
    /// The fill and the subtract.
    field: std::time::Duration,
    mark: std::time::Duration,
    /// The harvest and the split.
    components: std::time::Duration,
    /// Finalization and reclamation, the destructors among them.
    reclamation: std::time::Duration,
    /// The whole trace, the clear included, which no part above holds.
    total: std::time::Duration,
    walked: usize,
    freed: usize,
}

/// The process's counters, every thread's traces summed, in [`counts`]'s
/// order.
static COUNTERS: [AtomicU64; COUNTS] = [const { AtomicU64::new(0) }; COUNTS];

/// The figures [`counts`] answers.
pub(crate) const COUNTS: usize = 9;

/// Where each figure stands in [`COUNTERS`] and in [`counts`]'s answer.
const AT_TRACES: usize = 0;
const AT_TOTAL_NS: usize = 1;
const AT_FIELD_NS: usize = 2;
const AT_MARK_NS: usize = 3;
const AT_COMPONENTS_NS: usize = 4;
const AT_RECLAMATION_NS: usize = 5;
const AT_LONGEST_NS: usize = 6;
const AT_WALKED: usize = 7;
const AT_FREED: usize = 8;

fn note(spent: Spent) {
    let nanos = |duration: std::time::Duration| duration.as_nanos() as u64;
    let add = |at: usize, value: u64| COUNTERS[at].fetch_add(value, Ordering::Relaxed);
    add(AT_TRACES, 1);
    add(AT_TOTAL_NS, nanos(spent.total));
    add(AT_FIELD_NS, nanos(spent.field));
    add(AT_MARK_NS, nanos(spent.mark));
    add(AT_COMPONENTS_NS, nanos(spent.components));
    add(AT_RECLAMATION_NS, nanos(spent.reclamation));
    COUNTERS[AT_LONGEST_NS].fetch_max(nanos(spent.total), Ordering::Relaxed);
    add(AT_WALKED, spent.walked as u64);
    add(AT_FREED, spent.freed as u64);
}

/// Every thread's traces since the process began: traces; nanoseconds in all,
/// then in the fill and subtract, the mark, the harvest and split, and
/// finalization with reclamation; the longest trace's nanoseconds; entities
/// walked; members freed. The clear's time is the total less the four parts.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "read by the rig, which is a test build")
)]
pub(crate) fn counts() -> [u64; COUNTS] {
    std::array::from_fn(|at| COUNTERS[at].load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests;
