//! A stand-in for the collector's batch, for the cases of P: on the calling
//! thread — a test's second thread — claim the owner's token, take up to `k`
//! entries from the front of the owner's R clamped to P's room, post one
//! verdict per entry in R's order, and advance R's front past exactly them
//! through the reader's peek/commit pair. What the real batch adds is the
//! trace between the take and the post (`PLAN.md` S49.5).

use super::*;

use crate::ring::Reader;

/// What one stand-in batch did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Posted {
    /// The owner, or another holder, has the token.
    TokenHeld,
    /// The batch was made: this many verdicts were posted, and R's front
    /// moved past as many entries.
    Batch(usize),
}

/// One stand-in batch over `record`'s owner, `verdict_for` answering the verdict
/// for each root in R's order.
///
/// # Safety
/// `record` is a live record of the registry's and the calling thread is
/// not its owner.
pub(crate) unsafe fn post_batch(
    record: *mut OwnerRecord,
    k: usize,
    mut verdict_for: impl FnMut(*mut RcHeader) -> Verdict,
) -> Posted {
    let record = unsafe { &*record };
    if !record.token.try_take() {
        return Posted::TokenHeld;
    }

    struct ReleaseOnDrop<'a>(&'a crate::cycle::token::TraceToken);
    impl Drop for ReleaseOnDrop<'_> {
        fn drop(&mut self) {
            crate::cycle::token::note_traced_owner(std::ptr::null_mut());
            self.0.release();
        }
    }
    let _held = ReleaseOnDrop(&record.token);
    crate::cycle::token::note_traced_owner(std::ptr::from_ref(record).cast_mut());

    let verdicts = unsafe { VerdictWriter::open(record) };
    let reader = unsafe { Reader::new(record.candidate_ring()) };
    let mut out = vec![0usize; k.min(verdicts.room())];
    let peeked = reader.peek(&mut out);
    for &entry in &out[..peeked.len()] {
        let entity = entry_entity(entry);
        verdicts
            .post(entity, verdict_for(entity))
            .expect("the batch was clamped to P's room");
    }

    reader.commit(peeked);
    Posted::Batch(peeked.len())
}
