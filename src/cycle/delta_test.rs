//! The collector's Δ-test of the set its scan proved, by the window tags
//! (`dev/design/recycler-over-counts.md`, §4): a handshake at the mutator's
//! next safepoint checkpoint, the cut-off T, then one byte read a member.
//!
//! **What the Δ-test proves.** An entity untagged at T had no count write
//! and no write into its slots since the consent, so its count is the one the
//! mark read and every edge the mark recorded out of it stands at T. The scan
//! over the record coloured a member potentially unreachable only where every
//! recorded edge into it came from another member; at a checkpoint every
//! reference is counted, locals included; so at T nothing outside the set
//! refers into it, and garbage stays garbage (the Sage's proof, §4.8). A
//! member carrying the window's number was touched: its row is marked, and
//! what its recorded edges reach is refused (U, `crate::cycle::split`) while
//! the rest of the set stays proved, a touched member being made live only
//! through a tagged write.
//!
//! **Stale tags are cleared as they are read.** Garbage is never touched, so
//! a member keeps the number of the last window that wrote it, and with eight
//! bits a set grown across many consents would carry the open window's number
//! somewhere at every attempt. Every tag that is neither 0 nor the window's
//! predates the consent and is cleared by a one-byte swap
//! ([`crate::refcount::clear_a_stale_window_tag`]); a stale number equal to
//! the window's refuses this attempt and is cleared at the next.
//!
//! **What follows the test** is the split of the set it proved
//! (`crate::cycle::split`) and the collector's own free of the part it can
//! free (`crate::cycle::collector_frees`).
//!
//! **The wait for T is bounded**, by the mutator's recall and by
//! [`CHECKPOINT_WAIT`]: a mutator that polls no more within it — asleep, or in
//! a long stretch of native work — costs the set its Δ-test and nothing
//! else, the set going the exact way as a refused one does.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::cycle::arena::TraceScratchArena;
use crate::cycle::mutator_record::MutatorRecord;
use crate::cycle::row;

/// How long the collector waits for the mutator's checkpoint before it gives
/// the Δ-test up. A placeholder, read by S68.8's runs: a web load polls
/// far more often than this.
pub(crate) const CHECKPOINT_WAIT: Duration = Duration::from_millis(2);

/// The bound in force, in nanoseconds: [`CHECKPOINT_WAIT`], or what a
/// measurement set.
static WAIT_NANOS: AtomicU64 = AtomicU64::new(CHECKPOINT_WAIT.as_nanos() as u64);

/// Set the bound a measurement reads the Δ-test under, and answer the one it
/// replaces.
#[cfg(test)]
pub(crate) fn set_checkpoint_wait_for_test(wait: Duration) -> Duration {
    Duration::from_nanos(WAIT_NANOS.swap(wait.as_nanos() as u64, Ordering::Relaxed))
}

/// Spins before the wait yields its core.
const SPINS: u32 = 2_000;

/// What the Δ-test of one set answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TagReading {
    /// No member carries the window's number: garbage at T.
    Garbage,
    /// A member carries it, or a member's address could not be recovered
    /// to read it: touched since the consent, for all the test can tell.
    /// Every such member's row carries the split's mark
    /// (`crate::cycle::shadow::SPLIT_MARK`), the seeds of U
    /// (`crate::cycle::split`); the rest of the set is garbage at T.
    Touched,
    /// The mutator reached no checkpoint before its recall or the bound.
    NoCheckpoint,
}

/// The Δ-test's answer, and whether a member has weak references: an upgrade
/// after T can make it live again, which the tags cannot see (§4.8, "weak
/// cells aside"), so a set holding one is never marked proved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct TagTest {
    pub(crate) reading: TagReading,
    pub(crate) weakly_held: bool,
    /// Whether a member's address could not be recovered to read its tag:
    /// a refusal no write proves.
    pub(crate) unreadable: bool,
}

/// Sets read untouched, read touched in some member, holding a weakly-held
/// member, and given up for want of a checkpoint, since the process started; and the nanoseconds the collector
/// waited for checkpoints, in all and at the longest. Read by the runs of
/// S68.8 (`tag_reading_counts`).
static PROVED: AtomicUsize = AtomicUsize::new(0);
static TOUCHED: AtomicUsize = AtomicUsize::new(0);
static WEAKLY_HELD: AtomicUsize = AtomicUsize::new(0);
static NO_CHECKPOINT: AtomicUsize = AtomicUsize::new(0);
static ASKS_A_BLOCKING_ANSWERED: AtomicUsize = AtomicUsize::new(0);
static WAITED_NANOS: AtomicU64 = AtomicU64::new(0);
static LONGEST_WAIT_NANOS: AtomicU64 = AtomicU64::new(0);

/// The counts [`test_the_set_by_its_tags`] keeps.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct TagReadingCounts {
    pub(crate) proved: usize,
    pub(crate) touched: usize,
    pub(crate) weakly_held: usize,
    pub(crate) no_checkpoint: usize,
    /// Asks a standing blocking stretch answered at once.
    pub(crate) blocking: usize,
    pub(crate) waited: Duration,
    pub(crate) longest_wait: Duration,
}

/// The counts since the process started; the runs' probes read them.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn tag_reading_counts() -> TagReadingCounts {
    TagReadingCounts {
        proved: PROVED.load(Ordering::Relaxed),
        touched: TOUCHED.load(Ordering::Relaxed),
        weakly_held: WEAKLY_HELD.load(Ordering::Relaxed),
        no_checkpoint: NO_CHECKPOINT.load(Ordering::Relaxed),
        blocking: ASKS_A_BLOCKING_ANSWERED.load(Ordering::Relaxed),
        waited: Duration::from_nanos(WAITED_NANOS.load(Ordering::Relaxed)),
        longest_wait: Duration::from_nanos(LONGEST_WAIT_NANOS.load(Ordering::Relaxed)),
    }
}

/// Test the set the batch's scan over its record left potentially
/// unreachable: ask for the mutator's checkpoint, wait for it, then read the
/// tag of every member and clear the stale ones.
///
/// # Safety
/// The calling thread holds `mutator`'s grant, and `arena`'s rows are those
/// of a completed mark's completed scan over its record
/// (`crate::cycle::scan::scan_the_recorded_edges`), still standing: every
/// member's slot is withheld under the grant, so its header is mapped.
pub(crate) unsafe fn test_the_set_by_its_tags(
    mutator: &MutatorRecord,
    arena: &mut TraceScratchArena,
) -> TagTest {
    let token = &mutator.token;
    let from = Instant::now();
    // A blocking stretch the ask found is the checkpoint, whatever the byte
    // reads after: the stretch may end before the wait's first reading, and
    // its end erases the ask (`checkpoint_model`, "an ask a stretch answers,
    // the stretch left before the first reading").
    let mut recalled = false;
    let reached = if token.ask_for_the_checkpoint() {
        ASKS_A_BLOCKING_ANSWERED.fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        crate::cycle::worker::testing::note_an_ask_a_stretch_answered(mutator);
        true
    } else {
        wait_for_the_checkpoint(token, from, || {
            recalled = arena.read_the_recall_now().is_break();
            recalled
        })
    };
    token.withdraw_the_checkpoint();
    let waited = from.elapsed().as_nanos() as u64;
    WAITED_NANOS.fetch_add(waited, Ordering::Relaxed);
    LONGEST_WAIT_NANOS.fetch_max(waited, Ordering::Relaxed);
    if !reached {
        NO_CHECKPOINT.fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        crate::cycle::worker::testing::note_a_checkpoint_missed(mutator, recalled);
        #[cfg(not(test))]
        let _ = recalled;
        return TagTest {
            reading: TagReading::NoCheckpoint,
            weakly_held: false,
            unreadable: false,
        };
    }

    let window = token.window();
    let mut touched = false;
    let mut weakly_held = false;
    let mut unreadable = false;
    let mut array = arena.touched_head();
    while !array.is_null() {
        let (block, population) = unsafe { ((*array).block, (*array).population) };
        let _ = unsafe {
            row::for_each_proposable_met(array, block, population, |index| {
                let row = row::row_at(array, block, population, index);
                let Some(member) = row::entity_at(block, population, index) else {
                    // A member whose tag cannot be read is not one the test
                    // may pass over.
                    touched = true;
                    unreadable = true;
                    crate::cycle::split::mark(row);
                    return std::ops::ControlFlow::Continue(());
                };
                let tag = crate::refcount::window_tag(member);
                if tag == window {
                    touched = true;
                    crate::cycle::split::mark(row);
                } else if tag != 0 {
                    crate::refcount::clear_a_stale_window_tag(member, tag);
                }
                if crate::refcount::mutator_flags(member) & crate::refcount::HAS_WEAK_REFERENCES
                    != 0
                {
                    weakly_held = true;
                }
                std::ops::ControlFlow::Continue(())
            })
        };
        array = unsafe { (*array).next };
    }

    if weakly_held {
        WEAKLY_HELD.fetch_add(1, Ordering::Relaxed);
    }
    let reading = if touched {
        TOUCHED.fetch_add(1, Ordering::Relaxed);
        TagReading::Touched
    } else {
        PROVED.fetch_add(1, Ordering::Relaxed);
        TagReading::Garbage
    };
    TagTest {
        reading,
        weakly_held,
        unreadable,
    }
}

/// The edges the mark recorded between two members of the set its scan
/// proved: what the counts of a garbage set sum to, every reference to a
/// member coming from another (`crate::cycle::finalization`'s sum, which the
/// owner reads in place of its own trace of a set proved by its tags).
///
/// # Safety
/// As [`test_the_set_by_its_tags`], after it: the rows stand, a run's header
/// row holding its index, a potentially unreachable row a member.
pub(crate) unsafe fn internal_edges_of_the_set(arena: &TraceScratchArena) -> usize {
    use crate::cycle::recorded_edges::RUN;
    use crate::cycle::shadow::{Color, color};

    let record = arena.recorded_edges();
    let mut edges = 0;
    let mut from_a_member = false;
    for index in 0..record.len() {
        let entry = unsafe { record.entry(index) };
        let row = (entry & !RUN) as *const u32;
        let member = color(unsafe { *row }) == Color::PotentiallyUnreachable;
        if entry & RUN != 0 {
            from_a_member = member;
        } else if from_a_member && member {
            edges += 1;
        }
    }
    edges
}

/// Wait for the mutator's answer: true once it passed a checkpoint, false
/// where `recalled` reads the recall at the stop level, or past
/// [`CHECKPOINT_WAIT`] from `from`. A recall at the wind-down level does not
/// end the wait, as it does not end the scan whose set this is (design §2.1);
/// `recalled` is the arena's reading, which releases the grants held behind
/// this one at each call as a reading at the stride does.
fn wait_for_the_checkpoint(
    token: &crate::cycle::token::TraceToken,
    from: Instant,
    mut recalled: impl FnMut() -> bool,
) -> bool {
    let mut spins = 0;
    loop {
        if token.checkpoint_reached() {
            return true;
        }

        if recalled() || from.elapsed() >= Duration::from_nanos(WAIT_NANOS.load(Ordering::Relaxed))
        {
            return false;
        }

        if spins < SPINS {
            spins += 1;
            std::hint::spin_loop();
        } else {
            std::thread::yield_now();
        }
    }
}

#[cfg(test)]
mod tests;
