//! The collector's Δ-test of the set its scan proved, by the window tags
//! (`dev/design/recycler-over-counts.md`, §5f): one acquire fence after the
//! trace, then one byte read a member against the frame of the offer the
//! collector took.
//!
//! **What the Δ-test proves.** The take acquired the offer, whose release
//! followed every store the mutator made before the frame, so every value
//! the trace read is the frame's or a later one. A value written after the
//! frame is stored with a release after its entity's tag, a count after the
//! entity's own and a slot after its holder's, so the fence after the trace
//! synchronises with every such store the trace read, and makes every tag
//! stored before it visible here (release cumulativity). A set none of whose
//! reads saw a store after the frame was read at the frame, a poll where
//! every reference the mutator holds is counted; the scan over the record
//! coloured a member potentially unreachable only where every recorded edge
//! into it came from another member; so at the frame nothing outside the set
//! referred into it, and garbage stays garbage. A set any of whose reads saw
//! a later store carries the frame's number on some member: its row is
//! marked, and what its recorded edges reach is refused (U,
//! `crate::cycle::split`) while the rest of the set stays proved, a touched
//! member being made live only through a tagged write.
//!
//! **Stale tags are cleared as they are read.** Garbage is never touched, so
//! a member keeps the number of the last frame that wrote it, and with eight
//! bits a set grown across many offers would carry the frame's number
//! somewhere at every attempt. Every tag that is neither 0 nor the frame's
//! predates the frame and is cleared by a one-byte swap
//! ([`crate::refcount::clear_a_stale_window_tag`]); a stale number equal to
//! the frame's refuses this attempt and is cleared at the next. The batch's
//! release of the byte follows the last clear, and the next offer's swap
//! acquires it, so no clear lands over the next frame's tags.
//!
//! **What follows the test** is the split of the set it proved
//! (`crate::cycle::split`) and the collector's own free of the part it can
//! free (`crate::cycle::collector_frees`).

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::cycle::arena::TraceScratchArena;
use crate::cycle::mutator_record::MutatorRecord;
use crate::cycle::row;

/// What the Δ-test of one set answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TagReading {
    /// No member carries the frame's number: garbage at the frame.
    Garbage,
    /// A member carries it, or a member's address could not be recovered
    /// to read it: touched since the frame, for all the test can tell.
    /// Every such member's row carries the split's mark
    /// (`crate::cycle::shadow::SPLIT_MARK`), the seeds of U
    /// (`crate::cycle::split`); the rest of the set is garbage at the frame.
    Touched,
}

/// The Δ-test's answer, and whether a member has weak references: an upgrade
/// after the frame can make it live again, which the tags cannot see (§4.8,
/// "weak cells aside"), so a set holding one is never marked proved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct TagTest {
    pub(crate) reading: TagReading,
    pub(crate) weakly_held: bool,
    /// Whether a member's address could not be recovered to read its tag:
    /// a refusal no write proves.
    pub(crate) unreadable: bool,
}

/// Sets read untouched, read touched in some member, and holding a
/// weakly-held member, since the process started. Read by the runs of S68.8
/// (`tag_reading_counts`).
static PROVED: AtomicUsize = AtomicUsize::new(0);
static TOUCHED: AtomicUsize = AtomicUsize::new(0);
static WEAKLY_HELD: AtomicUsize = AtomicUsize::new(0);

/// The counts [`test_the_set_by_its_tags`] keeps.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct TagReadingCounts {
    pub(crate) proved: usize,
    pub(crate) touched: usize,
    pub(crate) weakly_held: usize,
}

/// The counts since the process started; the runs' probes read them.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn tag_reading_counts() -> TagReadingCounts {
    TagReadingCounts {
        proved: PROVED.load(Ordering::Relaxed),
        touched: TOUCHED.load(Ordering::Relaxed),
        weakly_held: WEAKLY_HELD.load(Ordering::Relaxed),
    }
}

/// Test the set the batch's scan over its record left potentially
/// unreachable: fence, then read the tag of every member against the frame
/// of the offer this grant took, and clear the stale ones.
///
/// # Safety
/// The calling thread holds `mutator`'s grant, taken from an offer, and
/// `arena`'s rows are those of a completed mark's completed scan over its
/// record (`crate::cycle::scan::scan_the_recorded_edges`), still standing:
/// every member's slot is withheld under the grant, so its header is mapped.
pub(crate) unsafe fn test_the_set_by_its_tags(
    mutator: &MutatorRecord,
    arena: &mut TraceScratchArena,
) -> TagTest {
    let token = &mutator.token;
    // After every read of the trace and before every tag read: the fence
    // synchronises with each released store the trace read, and so orders
    // every tag the mutator stored before it ahead of the reads below
    // (module doc). The trace's reads stay as they are: counts and flags
    // relaxed, every load that yields an address an acquire.
    std::sync::atomic::fence(Ordering::Acquire);
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
                    #[cfg(test)]
                    crate::cycle::worker::testing::read_live::note_a_stale_tag(
                        ((u16::from(window) + 255 - u16::from(tag)) % 255) as u8,
                    );
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
