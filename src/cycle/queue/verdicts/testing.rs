//! A stand-in for the collector's batch, for the cases of P: on the calling
//! thread — a test's second thread — claim the mutator's token, take up to `k`
//! entries from the front of the mutator's R clamped to P's room, post one
//! verdict per entry in R's order, and advance R's front past exactly them
//! through the reader's peek/commit pair. What the real batch adds is the
//! trace between the take and the post (`crate::cycle::worker`).

use super::*;

use crate::ring::Reader;

/// What one stand-in batch did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Posted {
    /// The mutator, or another holder, has the token.
    TokenHeld,
    /// The batch was made: this many verdicts were posted, and R's front
    /// moved past as many entries.
    Batch(usize),
}

/// One stand-in batch over `record`'s mutator, `verdict_for` answering the verdict
/// for each root in R's order.
///
/// # Safety
/// `record` is a live record of the registry's and the calling thread is
/// not its mutator.
///
/// The release is to `POSTED` whenever the batch posted, whatever it posted:
/// the stand-in for a batch whose verdicts owe the collection over P.
/// [`post_batch_released_as_the_collector_does`] releases a batch that
/// proposed nothing to `NOTHING_PROPOSED` instead.
pub(crate) unsafe fn post_batch(
    record: *mut MutatorRecord,
    k: usize,
    verdict_for: impl FnMut(*mut RcHeader) -> Verdict,
) -> Posted {
    unsafe { post_batch_releasing(record, k, verdict_for, false) }
}

/// [`post_batch`], released as the collector's batch is: to `POSTED` when a
/// verdict proposed a set, to `NOTHING_PROPOSED` when the batch posted and
/// none did, and to `FREE` when it posted nothing.
///
/// # Safety
/// As [`post_batch`].
pub(crate) unsafe fn post_batch_released_as_the_collector_does(
    record: *mut MutatorRecord,
    k: usize,
    verdict_for: impl FnMut(*mut RcHeader) -> Verdict,
) -> Posted {
    unsafe { post_batch_releasing(record, k, verdict_for, true) }
}

unsafe fn post_batch_releasing(
    record: *mut MutatorRecord,
    k: usize,
    mut verdict_for: impl FnMut(*mut RcHeader) -> Verdict,
    by_the_rule: bool,
) -> Posted {
    use crate::cycle::token::{FREE, NOTHING_PROPOSED, POSTED};

    let record = unsafe { &*record };
    if !record.token.claim_for_test(crate::cycle::worker::ELDER) {
        return Posted::TokenHeld;
    }

    struct ReleaseOnDrop<'a> {
        token: &'a crate::cycle::token::TraceToken,
        posted: std::cell::Cell<bool>,
        proposed: std::cell::Cell<bool>,
        by_the_rule: bool,
    }
    impl Drop for ReleaseOnDrop<'_> {
        fn drop(&mut self) {
            crate::cycle::token::note_traced_mutator(std::ptr::null_mut());
            let released = match (self.posted.get(), self.proposed.get() || !self.by_the_rule) {
                (false, _) => FREE,
                (true, true) => POSTED,
                (true, false) => NOTHING_PROPOSED,
            };
            self.token
                .release_claim_to(crate::cycle::worker::ELDER, released);
        }
    }
    let release = ReleaseOnDrop {
        token: &record.token,
        posted: std::cell::Cell::new(false),
        proposed: std::cell::Cell::new(false),
        by_the_rule,
    };
    let posted = &release.posted;
    crate::cycle::token::note_traced_mutator(std::ptr::from_ref(record).cast_mut());

    let verdicts = unsafe { VerdictWriter::open(record) };
    let reader = unsafe { Reader::new(record.candidate_ring()) };
    let mut out = vec![0usize; k.min(verdicts.room())];
    let peeked = reader.peek(&mut out);
    posted.set(peeked.len() > 0);
    for &entry in &out[..peeked.len()] {
        let entity = entry_entity(entry);
        let verdict = verdict_for(entity);
        release
            .proposed
            .set(release.proposed.get() || verdict == Verdict::Proposed);
        verdicts
            .post(entity, verdict)
            .expect("the batch was clamped to P's room");
    }

    reader.commit(peeked);
    Posted::Batch(peeked.len())
}

/// Post `count` entries naming no entity into this thread's own P, for a
/// case that wants P short of room: the mutator's reading skips them as
/// entries already answered for. Only while no collector runs, since the
/// mutator is not P's producer.
pub(crate) unsafe fn fill_for_test(count: usize) {
    let record = mutator_record::this_thread_record();
    assert!(!record.is_null(), "this thread has a record");
    let writer = unsafe { VerdictWriter::open(&*record) };
    for _ in 0..count {
        writer
            .post(std::ptr::null_mut(), Verdict::Proposed)
            .expect("P has the room the case counted");
    }
}
