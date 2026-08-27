use super::*;

use crate::memory::block_pool::test_guard;
use crate::refcount::{ACYCLIC_GATE, ENROLLED, EntityKind, MemoryCategory, mutator_flags};

/// A header the enrolment gate admits, at count `holders`: a heap object
/// with no clause of the gate against it, so a decrement that leaves a
/// holder behind reaches the enrolment.
///
/// A bare header rather than an allocated entity, which is what the
/// gate's own tests use and for the same reason: nothing on this path
/// dereferences the entry it writes, the reader that will being S35.1's.
fn candidate(holders: u32) -> RcHeader {
    candidate_with(holders, 0)
}

/// The same, with one more flag set — a clause of the gate, for a test
/// that wants the decrement refused.
fn candidate_with(holders: u32, extra: u32) -> RcHeader {
    let mut header = RcHeader::new(
        MemoryCategory::GcHeap,
        EntityKind::Object.to_flags() | extra,
    );
    for _ in 1..holders {
        unsafe { crate::refcount::ll_retain(&raw mut header) };
    }

    header
}

/// Release once through the ABI, which is the only door enrolment has.
///
/// **A raw pointer, and the caller keeps one per header and reuses it.**
/// A fresh `&mut` per call is a Unique retag that invalidates every raw
/// pointer taken before it, so a test that read the flags back through
/// one would be reading through a dead tag — a Miri failure of the
/// fixture rather than of the runtime (`dev/WORKFLOW.md`, Miri).
///
/// # Safety
/// `entity` points at a header this thread owns and outlives the call.
unsafe fn release(entity: *mut RcHeader) -> bool {
    unsafe { crate::refcount::ll_release(entity) }
}

/// Empty the queue and the spare cells, and give every block back.
///
/// Every test here starts and ends with it, because the queue is per
/// thread and the harness reuses threads: a segment another test left
/// live would put this one's first enrolment in a half-full segment, and
/// a spare it left held would answer this one's pool arithmetic.
fn reset() {
    drain();
    crate::memory::critical::drain_for_test();
}

mod what_an_enrolment_writes;
mod what_the_poll_owes_the_queue;
mod where_a_full_segment_comes_from;
