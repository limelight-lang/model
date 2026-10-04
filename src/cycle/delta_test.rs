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
//! member carrying the window's number was touched, and the set is refused.
//!
//! **Stale tags are cleared as they are read.** Garbage is never touched, so
//! a member keeps the number of the last window that wrote it, and with eight
//! bits a set grown across many consents would carry the open window's number
//! somewhere at every attempt. Every tag that is neither 0 nor the window's
//! predates the consent and is cleared by a one-byte swap
//! ([`crate::refcount::clear_a_stale_window_tag`]); a stale number equal to
//! the window's refuses this attempt and is cleared at the next.
//!
//! **What the Δ-test does not do yet** (S68.5): the set it proves still
//! goes to the owner's exact validation, which reads the mark beside it and,
//! in a debug build, asserts that no member of a set so proved reads live
//! (`crate::cycle::trace::trace_within_the_set`). The collector's own free of
//! a set so proved is S68.6.
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

/// Spins before the wait yields its core.
const SPINS: u32 = 2_000;

/// What the Δ-test of one set answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TagReading {
    /// No member carries the window's number: garbage at T.
    Garbage,
    /// A member carries it, or a member's address could not be recovered
    /// to read it: touched since the consent, for all the test can tell.
    Touched,
    /// A member has weak references: an upgrade after T can make it live
    /// again, which the tags cannot see (§4.8, "weak cells aside"). S68.6
    /// takes such members and what they reach the exact way.
    WeaklyHeld,
    /// The mutator reached no checkpoint before its recall or the bound.
    NoCheckpoint,
}

/// Sets proved garbage, refused as touched, and given up for want of a
/// checkpoint, since the process started; and the nanoseconds the collector
/// waited for checkpoints, in all and at the longest. Read by the runs of
/// S68.8 (`tag_reading_counts`).
static PROVED: AtomicUsize = AtomicUsize::new(0);
static TOUCHED: AtomicUsize = AtomicUsize::new(0);
static WEAKLY_HELD: AtomicUsize = AtomicUsize::new(0);
static NO_CHECKPOINT: AtomicUsize = AtomicUsize::new(0);
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
) -> TagReading {
    let token = &mutator.token;
    let from = Instant::now();
    token.ask_for_the_checkpoint();
    let reached = wait_for_the_checkpoint(token, from, || arena.read_the_recall_now().is_break());
    token.withdraw_the_checkpoint();
    let waited = from.elapsed().as_nanos() as u64;
    WAITED_NANOS.fetch_add(waited, Ordering::Relaxed);
    LONGEST_WAIT_NANOS.fetch_max(waited, Ordering::Relaxed);
    if !reached {
        NO_CHECKPOINT.fetch_add(1, Ordering::Relaxed);
        return TagReading::NoCheckpoint;
    }

    let window = token.window();
    let mut touched = false;
    let mut weakly_held = false;
    let mut array = arena.touched_head();
    while !array.is_null() {
        let (block, population) = unsafe { ((*array).block, (*array).population) };
        let _ = unsafe {
            row::for_each_proposable_met(array, block, population, |index| {
                let Some(member) = row::entity_at(block, population, index) else {
                    // A member whose tag cannot be read is not one the test
                    // may pass over.
                    touched = true;
                    return std::ops::ControlFlow::Continue(());
                };
                let tag = crate::refcount::window_tag(member);
                if tag == window {
                    touched = true;
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

    if touched {
        TOUCHED.fetch_add(1, Ordering::Relaxed);
        TagReading::Touched
    } else if weakly_held {
        WEAKLY_HELD.fetch_add(1, Ordering::Relaxed);
        TagReading::WeaklyHeld
    } else {
        PROVED.fetch_add(1, Ordering::Relaxed);
        TagReading::Garbage
    }
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

        if recalled() || from.elapsed() >= CHECKPOINT_WAIT {
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
