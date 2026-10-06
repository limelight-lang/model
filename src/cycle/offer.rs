//! The mutator's offer of a batch of R (`dev/design/recycler-over-counts.md`,
//! §5f): under `recycler-over-counts` the frame a batch is tested against is
//! a poll of the mutator's, where every reference it holds is counted, and the
//! mutator decides when to offer one. The collector takes what is offered and
//! asks for nothing ([`crate::cycle::token::TraceToken::take_the_offer`]).
//!
//! **When.** At a poll with the gate open, never on the slot-free path, whose
//! reading may hold temporaries the compiler did not count (§4.7), and only
//! with the byte at `FREE`, the last batch tested and disposed of. Then on
//! the round's three branches over R, read off its front block as the
//! collector read them before ([`crate::cycle::worker`], "The thread, and the
//! round over the records"): R at the threshold; R standing non-empty below
//! it for the standing interval since the last release of the byte; or a
//! deferred lane merged into R since the merges were last accounted for. The
//! interval is measured on the collector's clock as its last round read it
//! ([`crate::cycle::worker::round_clock`]), so the poll reads no clock: a
//! ring stands at most one fallback interval past its own.
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
    unsafe { offer(worker::threshold_for_offers(), true) }
}

/// [`offer_if_due`] with R due at `threshold`, for a case whose collector
/// is a thread of its own, or one the case wakes itself: the offer stands
/// whoever stands to take it, and wakes no one.
///
/// # Safety
/// As [`offer_if_due`].
#[cfg(test)]
pub(crate) unsafe fn offer_at(threshold: usize) -> bool {
    unsafe { offer(threshold, false) }
}

/// The offer at `threshold`; `from_the_poll`, made only where a collector
/// stands to take it, and that collector woken.
unsafe fn offer(threshold: usize, from_the_poll: bool) -> bool {
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
    if !is_due(mutator, reader.front_block_reading(), merges, threshold) {
        return false;
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
    if from_the_poll {
        worker::wake_a_taker(mutator.collector());
    }
    true
}

/// The round's three branches over R as the offer reads them, off the poll's
/// reading `ring` and the merge count `merges` loaded before it, with the
/// record's standing instant stamped or cleared on the way. The instant and
/// the merges seen are this thread's words between the releases of its
/// byte, which the collector's release restamps
/// ([`MutatorRecord::note_standing_since`]).
fn is_due(
    mutator: &MutatorRecord,
    ring: Option<FrontBlockReading>,
    merges: u32,
    threshold: usize,
) -> bool {
    let Some(ring) = ring.filter(|ring| ring.holds_at_least(1)) else {
        mutator.note_standing_since(0);
        mutator.note_merges_seen(merges);
        return false;
    };

    if ring.holds_at_least(threshold) {
        mutator.note_standing_since(0);
        return true;
    }

    if merges != mutator.merges_seen() {
        return true;
    }

    let now = worker::round_clock();
    match mutator.standing_since() {
        // No round has read the clock yet: no collector stands to take a
        // standing ring, and the instant waits for the first.
        _ if now == 0 => false,
        0 => {
            mutator.note_standing_since(now);
            false
        }
        since => now.saturating_sub(since) >= worker::standing_interval_nanos(),
    }
}
