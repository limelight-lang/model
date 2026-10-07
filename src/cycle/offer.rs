//! The mutator's offer of a batch of R (`dev/design/recycler-over-counts.md`,
//! §5f): under `recycler-over-counts` the frame a batch is tested against is
//! a poll of the mutator's, where every reference it holds is counted, and the
//! mutator decides when to offer one. The collector takes what is offered and
//! asks for nothing ([`crate::cycle::token::TraceToken::take_the_offer`]).
//!
//! **When.** At a poll with the gate open, never on the slot-free path, whose
//! reading may hold temporaries the compiler did not count (§4.7), and only
//! with the byte at `FREE`, the last batch tested and disposed of. Then on
//! the branches over R, read off its front block as the collector read them
//! before ([`crate::cycle::worker`], "The thread, and the round over the
//! records"), with the bound above them: R at the batch's bound; R at the
//! threshold for the short interval on this thread's own clock; R standing
//! non-empty for the standing interval since the last release of the byte;
//! or a deferred lane merged into R since the merges were last accounted for
//! (`dev/design/the-general-algorithm.md`, "The offer at the bound,
//! repaired"; Edmond, 2026-10-07). The short interval is read off [`Instant`]
//! only after the byte, the ring and the threshold have answered, and while
//! R holds the threshold the elder is started as an offer starts it. The
//! standing interval is measured on the collector's clock as its last round
//! read it ([`crate::cycle::worker::round_clock`]): a ring stands at most one
//! fallback interval past its own.
//!
//! **What.** The window turned to the next frame, R's count up to the batch's
//! bound as the batch's ceiling, and the recall at the level the withheld
//! stacks hold, all published by the offer's release swap; then the wake of
//! the collector the record names, or the elder where that slot holds no
//! thread ([`worker::wake_a_taker`]). Before the offer, the elder is born
//! where none stands, and where its birth is refused no offer is made
//! ([`worker::a_taker_stands`]).
//!
//! **After a mark.** A stack of returns withheld under an offer that reaches
//! its mark withdraws the offer, and the returns go back
//! ([`crate::cycle::token::recall_this_threads_token`]). The next offer waits
//! for the collector's next round, so that an offer no collector is free to
//! take is not made and withdrawn at every poll (Critic, 2026-10-05, token
//! states, finding 2).

use std::cell::Cell;
use std::time::{Duration, Instant};

use crate::cycle::mutator_record::{self, MutatorRecord};
use crate::cycle::token::{FREE, state};
use crate::cycle::worker;
use crate::ring::{FrontBlockReading, Reader};

thread_local! {
    /// The collector rounds begun when this thread's last offer was withdrawn
    /// at a mark, or `u64::MAX` for none since its last offer: no offer is
    /// made until a round begins after it ([`worker::rounds_begun`]). No drop
    /// glue, as every thread-local the exit reaches.
    static WITHDRAWN_AT_ROUND: Cell<u64> = const { Cell::new(u64::MAX) };

    /// The instant this thread's poll first read R at the threshold and
    /// below the bound, or `None` while R is not there or since the last
    /// offer.
    static AT_THE_THRESHOLD_SINCE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Note that a stack's mark withdrew this thread's offer, for the pacing of
/// the next ([`offer_if_due`]).
pub(crate) fn note_a_withdrawal_at_a_mark() {
    WITHDRAWN_AT_ROUND.with(|at| at.set(worker::rounds_begun()));
}

/// Offer a batch of R where the poll's reading calls for one (module doc), and
/// answer whether it was made. None is made where no collector stands to
/// take it, the elder unborn and its birth refused.
///
/// # Safety
/// The caller is the poll, with the gate open: every reference this thread
/// holds is counted.
pub(crate) unsafe fn offer_if_due() -> bool {
    unsafe {
        offer(
            worker::threshold_for_short_offers(),
            worker::threshold_for_offers(),
            worker::SHORT_STANDING_INTERVAL,
            true,
        )
    }
}

/// [`offer_if_due`] with R due at `threshold`, for a case whose collector
/// is a thread of its own, or one the case wakes itself: the offer stands
/// whoever stands to take it, and wakes no one.
///
/// # Safety
/// As [`offer_if_due`].
#[cfg(test)]
pub(crate) unsafe fn offer_at(threshold: usize) -> bool {
    unsafe { offer(threshold, threshold, Duration::ZERO, false) }
}

/// [`offer_at`] with the short interval: R offered at `bound`, or at
/// `threshold` once it has held it `short` on this thread's own clock.
///
/// # Safety
/// As [`offer_if_due`].
#[cfg(test)]
pub(crate) unsafe fn offer_between(threshold: usize, bound: usize, short: Duration) -> bool {
    unsafe { offer(threshold, bound, short, false) }
}

/// The offer at `bound`, or at `threshold` held `short`; `from_the_poll`,
/// made only where a collector stands to take it, and that collector woken.
unsafe fn offer(threshold: usize, bound: usize, short: Duration, from_the_poll: bool) -> bool {
    let record = mutator_record::this_thread_record();
    if record.is_null() {
        return false;
    }

    let mutator = unsafe { &*record };
    if state(mutator.token.read()) != FREE {
        return false;
    }

    let withdrawn_at = WITHDRAWN_AT_ROUND.with(Cell::get);
    if withdrawn_at != u64::MAX && worker::rounds_begun() == withdrawn_at {
        return false;
    }

    // The merges before the ring, as the collector read them: a merge between
    // the two loads is read in the ring and missed by the count, which offers
    // the ring a poll later; the other order would account for a merge its
    // reading never saw.
    let merges = mutator.merges();
    let reader = unsafe { Reader::new(mutator.candidate_ring()) };
    match is_due(
        mutator,
        reader.front_block_reading(),
        merges,
        threshold,
        bound,
        short,
    ) {
        Due::Now => {}
        Due::NotYet => return false,
        // The elder born while R holds the threshold, as an offer at the
        // threshold starts it, so that the offer finds a taker standing.
        Due::AtTheThreshold => {
            if from_the_poll {
                let _ = worker::a_taker_stands(mutator.collector());
            }
            return false;
        }
    }

    // No offer where no thread stands to take it, the elder's birth
    // refused: it would withhold every return until a stack's mark, and
    // spend a frame's number, for a take that cannot come.
    if from_the_poll && !worker::a_taker_stands(mutator.collector()) {
        return false;
    }

    // The ring's owner, with no holder of its token, walks the chain.
    let ceiling = reader.unread_up_to(worker::BATCH_BOUND);
    let window = crate::refcount::the_next_window();
    let level = crate::cycle::deferred_slot_reuse::withheld_recall();
    if mutator.token.offer(window, ceiling, level).is_err() {
        return false;
    }

    // Opened once the swap landed, and before any count write after it.
    crate::refcount::set_window(window);
    crate::cycle::deferred_slot_reuse::note_an_offer();
    #[cfg(test)]
    worker::testing::wave_three::note_an_offer(
        mutator.token.address(),
        ceiling,
        worker::BATCH_BOUND,
    );
    WITHDRAWN_AT_ROUND.with(|at| at.set(u64::MAX));
    AT_THE_THRESHOLD_SINCE.with(|since| since.set(None));
    if from_the_poll {
        worker::wake_a_taker(mutator.collector());
    }
    true
}

/// Whether the poll offers R: at once, not yet, or not yet with R at the
/// threshold, where the elder is started.
enum Due {
    Now,
    NotYet,
    AtTheThreshold,
}

/// The branches over R as the offer reads them (module doc), off the poll's
/// reading `ring` and the merge count `merges` loaded before it, with the
/// record's standing instant and this thread's instant at the threshold
/// stamped or cleared on the way. The instant and
/// the merges seen are this thread's words between the releases of its
/// byte, which the collector's release restamps
/// ([`MutatorRecord::note_standing_since`]).
fn is_due(
    mutator: &MutatorRecord,
    ring: Option<FrontBlockReading>,
    merges: u32,
    threshold: usize,
    bound: usize,
    short: Duration,
) -> Due {
    let Some(ring) = ring.filter(|ring| ring.holds_at_least(1)) else {
        mutator.note_standing_since(0);
        mutator.note_merges_seen(merges);
        AT_THE_THRESHOLD_SINCE.with(|since| since.set(None));
        return Due::NotYet;
    };

    if ring.holds_at_least(bound) {
        mutator.note_standing_since(0);
        return Due::Now;
    }

    if merges != mutator.merges_seen() {
        return Due::Now;
    }

    let at_the_threshold = ring.holds_at_least(threshold);
    if at_the_threshold {
        let now = Instant::now();
        match AT_THE_THRESHOLD_SINCE.with(Cell::get) {
            None => AT_THE_THRESHOLD_SINCE.with(|since| since.set(Some(now))),
            Some(since) if now.duration_since(since) >= short => {
                mutator.note_standing_since(0);
                return Due::Now;
            }
            Some(_) => {}
        }
    } else {
        AT_THE_THRESHOLD_SINCE.with(|since| since.set(None));
    }

    let now = worker::round_clock();
    let stood = match mutator.standing_since() {
        // No round has read the clock yet: no collector stands to take a
        // standing ring, and the instant waits for the first.
        _ if now == 0 => false,
        0 => {
            mutator.note_standing_since(now);
            false
        }
        since => now.saturating_sub(since) >= worker::standing_interval_nanos(),
    };
    match (stood, at_the_threshold) {
        (true, _) => Due::Now,
        (false, true) => Due::AtTheThreshold,
        (false, false) => Due::NotYet,
    }
}
