//! The verdict ring P: what the collector says about the roots it took from
//! R, and what the mutator does with each verdict.
//!
//! P is a ring of `crate::ring`'s form with the roles swapped — the
//! collector writes, the mutator reads — and it never grows: one block per
//! thread, the mutator's memory, drawn with the record
//! (`crate::cycle::mutator_record`). The collector reads P's room before it
//! traces and clamps its batch to it, so no link is ever written into P and
//! no block passes from the mutator to the collector; a P that is full is a
//! mutator that has not polled (`rfc/dev/DECISIONS.md`, "the candidate queue is
//! read behind its writer, and the collector's verdicts come back by a
//! second ring", "The ring P and the verdicts"; `rfc/model/gc/rc-cycle.md`,
//! "Worker-to-owner handoff").
//!
//! # What an entry says
//!
//! An entry is the entity's address with one of four verdicts in its low
//! two bits ([`Verdict`]): the collector's reading of the root, made on a
//! copy of the graph and under a budget, so none of the four is a fact the
//! mutator acts on without reading again. The mutator is the one party that
//! changes state (`rfc/model/gc/rc-cycle.md`, "The mutator's disposition").
//! Bit 2 is the mutator's: the mark a close writes over a root it read live
//! ([`VERDICT_DEFER_MARK`]). An entry whose address is null is one the mutator
//! has answered for in place and the next advance of P's front drops.
//!
//! # Who writes P's slots
//!
//! The collector, between `tail` and `front`. The mutator writes into the
//! slots it has read and not yet advanced past — the mark, and the null of
//! a disposed entry — only under its token or its collecting word, which
//! keep the collector out of P altogether: the two never touch a slot at
//! once, and the collector's next write into that slot follows the mutator's
//! advance of `front` through the ring's own release/acquire pair.
//!
//! # When the mutator reads P
//!
//! **Every in-line collection reads P into its batch** — the fire, the
//! pressure path, the exit — so that a proposal never stands through a
//! collection short of memory: the proposed and unwalked entries standing
//! at the batch's reading are roots of that batch and are traced from P's
//! slots, with no entry of R written for them
//! (`crate::cycle::queue::Batch`). At the close the batch's prefix of P is
//! disposed of ([`crate::cycle::queue::compaction`]): a root whose death
//! completed is retired, a root read live — by the close's own reading or by
//! the collector's — goes to the deferred lane, a zero-count verdict is
//! retired only on the completed-free bit re-read there, and every entry
//! the close cannot dispose of — a component refused or resurrected, a
//! resurrected zero count, a root the deferred lane had no block for — is
//! written back into R as a registration is, its candidate bit still set;
//! then P's `front` advances by the whole prefix.
//!
//! **The open-gate poll reads P's prefix** ([`dispose_prefix_at_the_poll`]):
//! it retires the completed deaths and defers the roots read live from the
//! front, and stops at the first proposed or unwalked root, which arms the
//! collection this same poll fires — the collection is what reads it. A
//! closed-gate poll reads no verdict, as it fires nothing. The poll writes
//! into R only for a zero-count verdict the re-reading refuted
//! ([`PrefixReading::kept`]), through the registration path with its
//! growth and its arming; every other disposition writes nothing into R.
//! What stands behind the first proposal waits for the collection, which
//! reads the whole prefix.
//!
//! **A retirement outside a collection** — inside a teardown, and between
//! the pressure path's rounds — retires the completed deaths standing
//! anywhere in P in place, under the token, and advances nothing
//! (`crate::cycle::queue::retire_candidates`).

use super::*;

use crate::ring::{NoBlock, Reader};

/// What the collector read about one root of R.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub(crate) enum Verdict {
    /// The root's row read potentially unreachable: a component the mutator's
    /// exact validation may confirm.
    Proposed = 0,
    /// The root read live, held from outside.
    ReadLive = 1,
    /// The count read zero: a death the mutator may find completed.
    ZeroCount = 2,
    /// The trace never walked the root — its block budget was met, or an
    /// allocation refused — posted so that no root blocks the ring behind
    /// it.
    Unwalked = 3,
}

impl Verdict {
    /// Whether an entry under this verdict is a root of the mutator's batch:
    /// one the mutator traces and validates exactly.
    fn is_root(self) -> bool {
        matches!(self, Self::Proposed | Self::Unwalked)
    }
}

/// The bits of an entry that carry the verdict.
const VERDICT_BITS: usize = 3;

/// The mutator's mark over a root of the batch that a close read live, written
/// in place by [`crate::cycle::queue::Batch::mark_for_deferral`] and read
/// once by the pass that disposes of the batch. Bit 2: the third of the
/// three bits a fixture's eight-byte header alignment frees
/// (`crate::cycle::queue`, [`ENTRY_MARK_BITS`]).
pub(super) const VERDICT_DEFER_MARK: usize = 4;

/// Every low bit an entry of P may carry.
const LOW_BITS: usize = VERDICT_BITS | VERDICT_DEFER_MARK;

const _: () = assert!(Verdict::Unwalked as usize <= VERDICT_BITS);

/// P's entry for `entity` under `verdict`.
#[inline]
fn verdict_entry(entity: *mut RcHeader, verdict: Verdict) -> usize {
    entity_entry(entity) | verdict as usize
}

/// The verdict an entry of P carries.
#[inline]
pub(super) fn entry_verdict(entry: usize) -> Verdict {
    match entry & VERDICT_BITS {
        0 => Verdict::Proposed,
        1 => Verdict::ReadLive,
        2 => Verdict::ZeroCount,
        _ => Verdict::Unwalked,
    }
}

/// The entity an entry of P names, its low bits taken off; null for an
/// entry the mutator has answered for.
#[inline]
pub(super) fn verdict_entity(entry: usize) -> *mut RcHeader {
    std::ptr::with_exposed_provenance_mut(entry & !LOW_BITS)
}

/// Whether the mutator has answered for this entry in place.
#[inline]
pub(super) fn is_disposed(entry: usize) -> bool {
    entry & !LOW_BITS == 0
}

/// Whether an entry of P is a root of the mutator's batch.
#[inline]
pub(super) fn is_batch_root(entry: usize) -> bool {
    !is_disposed(entry) && entry_verdict(entry).is_root()
}

/// The collector's handle over one mutator's P: how much room it has, and the
/// post of one verdict (`crate::cycle::worker`, the batch).
pub(crate) struct VerdictWriter<'a>(ring::Writer<'a>);

impl<'a> VerdictWriter<'a> {
    /// The writer over `record`'s P.
    ///
    /// # Safety
    /// The calling thread holds `record`'s token, which is what makes it P's
    /// one producer for the handle's life.
    pub(crate) unsafe fn open(record: &'a MutatorRecord) -> Self {
        Self(unsafe { ring::Writer::new(record.verdict_ring()) })
    }

    /// Verdicts P takes before it is full, which is what a batch is clamped
    /// to before its roots are taken from R. Under the token.
    pub(crate) fn room(&self) -> usize {
        self.0.room_in_tail_block()
    }

    /// The same room by loads alone, for the idle test the collector makes
    /// ahead of its claim: no store into P's block under no claim.
    pub(crate) fn room_by_loads(&self) -> usize {
        self.0.room_in_tail_block_by_loads()
    }

    /// Post `verdict` about `entity`. [`NoBlock`] is a full P, which a batch
    /// clamped to [`VerdictWriter::room`] never meets.
    pub(crate) fn post(&self, entity: *mut RcHeader, verdict: Verdict) -> Result<(), NoBlock> {
        self.0
            .push(verdict_entry(entity, verdict), std::ptr::null_mut)
            .map(|_| ())
    }
}

/// The mutator's handle over P while the collector is kept out: under the
/// mutator's token, or under its collecting word.
///
/// `None` for a thread with no record, which holds no verdict.
pub(super) fn verdict_ring<'a>() -> Option<Quiescent<'a>> {
    let record = mutator_record::this_thread_record();
    if record.is_null() {
        return None;
    }

    Some(unsafe { Quiescent::new((*record).verdict_ring()) })
}

/// Note on this thread's record that a disposition at its poll freed
/// something ([`MutatorRecord::note_freeing_disposition`]); nothing for a
/// thread with no record, which holds no verdict.
pub(crate) fn note_freeing_disposition() {
    let record = mutator_record::this_thread_record();
    if !record.is_null() {
        unsafe { &*record }.note_freeing_disposition();
    }
}

/// What the poll's reading of P's prefix did, by count.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct PrefixReading {
    /// Completed deaths retired, their slots returned.
    pub(crate) retired: usize,
    /// Roots read live that went to the deferred lane.
    pub(crate) deferred: usize,
    /// Zero-count verdicts the re-reading refuted — a resurrection — and
    /// written back into R.
    pub(crate) kept: usize,
    /// Whether the reading stopped at a proposed or unwalked root: the
    /// collection the poll fires is what reads it.
    pub(crate) proposal_stands: bool,
}

/// Read P from its front and dispose of each verdict up to the first root
/// of a batch, advancing P past an entry only once the entry's disposition
/// holds. `at_commits` is the poll's own commit count, the mirror a deferred
/// root waits against. A root read live the deferred lane has no block for
/// stops the reading as a proposal does: it stands for the collection,
/// which writes back what it cannot dispose of, rather than being written
/// into R here.
///
/// A thread with no record, or no base block, has registered nothing and
/// holds no verdict; the reading is empty.
pub(crate) fn dispose_prefix_at_the_poll(at_commits: u64) -> PrefixReading {
    let mut reading = PrefixReading::default();
    let state = mutator_state();
    if state.is_null() {
        return reading;
    }

    let mutator_state = unsafe { mutator_state_ref(state) };
    let record = mutator_record::this_thread_record();
    if record.is_null() {
        return reading;
    }

    // The mutator is P's one consumer.
    let reader = unsafe { Reader::new((*record).verdict_ring()) };
    let mut one = [0usize; 1];
    loop {
        let peeked = reader.peek(&mut one);
        if peeked.len() == 0 {
            return reading;
        }

        let entry = one[0];
        if is_disposed(entry) {
            reader.commit(peeked);
            continue;
        }

        let entity = verdict_entity(entry);
        match entry_verdict(entry) {
            Verdict::Proposed | Verdict::Unwalked => {
                reading.proposal_stands = true;
                return reading;
            }
            Verdict::ReadLive if !compaction::completed_death(entity) => {
                if defer_entry(mutator_state, entity, Some(at_commits)).is_err() {
                    reading.proposal_stands = true;
                    return reading;
                }

                reading.deferred += 1;
            }
            // A root read live whose death has since completed is a
            // completed death, whatever the verdict: retired, as the close
            // retires it, rather than left in a lane no retirement sweeps.
            Verdict::ReadLive | Verdict::ZeroCount => {
                if compaction::completed_death(entity) {
                    // P advances before the free: a free that raises has
                    // half-returned the slot and cannot be retried, so its
                    // entry is dropped as `ll_free`'s, the rule the
                    // compaction keeps (`dev/DECISIONS.md`, "corrupt queue
                    // entries remain outside the cleanup recovery
                    // contract").
                    reader.commit(peeked);
                    compaction::free(entity);
                    reading.retired += 1;
                    continue;
                }

                // A count read zero that is not a completed death is a
                // resurrection: registered again, its bit still set.
                unsafe { append_entry(state, entity) };
                reading.kept += 1;
            }
        }

        reader.commit(peeked);
    }
}

/// Entries standing in this thread's P by its indices, the disposed ones
/// among them.
#[cfg(test)]
pub(crate) fn verdict_count() -> usize {
    let record = mutator_record::this_thread_record();
    if record.is_null() {
        return 0;
    }

    unsafe { Reader::new((*record).verdict_ring()) }.unread()
}

/// Drop every verdict standing in this thread's P, as the mutator: a case
/// leaves P as it found it, the harness thread and its record outliving
/// the case.
#[cfg(test)]
pub(crate) fn discard_standing_verdicts() {
    let record = mutator_record::this_thread_record();
    if record.is_null() {
        return;
    }

    let reader = unsafe { Reader::new((*record).verdict_ring()) };
    reader.advance(reader.unread());
}

/// Every verdict this thread's P holds and the mutator has not answered for,
/// oldest first, with its entity.
#[cfg(test)]
pub(crate) fn standing_verdicts() -> Vec<(*mut RcHeader, Verdict)> {
    let mut verdicts = Vec::new();
    if let Some(ring) = verdict_ring() {
        ring.walk(|entry| {
            if !is_disposed(entry) {
                verdicts.push((verdict_entity(entry), entry_verdict(entry)));
            }
            true
        });
    }
    verdicts
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests;
