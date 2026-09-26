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
//! copy of the candidates and the entities the trace reaches, under a budget, so none of the four is a fact the
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
//! a disposed entry — only under its token, which keeps the collector out
//! of P altogether: the two never touch a slot at
//! once, and the collector's next write into that slot follows the mutator's
//! advance of `front` through the ring's own release/acquire pair.
//!
//! # When the mutator reads P
//!
//! **P is read under the mutator's token alone: by in-line collections, and by
//! the poll's disposition of a batch that proposed nothing.** The collector's
//! release after a batch that proposed a set writes `POSTED` into the
//! mutator's token byte, and the mutator's reading of it — on its slot free
//! entry and at its poll — arms the collection over P
//! (`crate::cycle::token::read_and_act_on_this_thread`,
//! `crate::cycle::collect::collect_over_the_verdicts`); after a batch that
//! posted and proposed nothing it writes `NOTHING_PROPOSED`, whose reading
//! arms P's disposition with no trace window
//! (`crate::cycle::collect::dispose_of_p`). In the collection over P the
//! proposed entries standing at the batch's reading are its roots and are
//! traced from P's slots, with no entry of R written for them and nothing of
//! R traced (`crate::cycle::queue::Batch`). An unwalked entry is no root of it: the
//! collector never read that root, and the collection over P validates what
//! the collector read rather than searching, so its close writes the root
//! back into R for the collector's next batch (`rfc/model/gc/rc-cycle.md`,
//! "The mutator's disposition"). The pressure path, the exit and the
//! explicit fire read P into their batch first, ahead of R, with its
//! unwalked entries among the roots, so that a proposal never stands through
//! a collection short of memory ([`BatchForm`]). The invariant the byte
//! carries: P holds an entry the mutator has not disposed of only while the
//! byte reads `POSTED` or `MUTATOR`, so a byte at `FREE` promises an empty
//! P (`rfc/dev/design/trace-token-handshake.md`, "The word").
//!
//! **At the close P is disposed of whole, on every ending of every path**
//! ([`crate::cycle::queue::compaction`];
//! `crate::cycle::queue::retire_candidates_and_dispose_of_verdicts`): a root
//! whose death completed is retired, a root read live — by the close's own
//! reading or by the collector's — goes to the deferred lane, a zero-count
//! verdict is retired only on the completed-free bit re-read there, and
//! every entry the close cannot dispose of — a component refused,
//! resurrected or never traced, a resurrected zero count, a root the
//! deferred lane had no block for — is written back into R as a
//! registration is, its candidate bit still set; then P's `front` advances
//! by the whole prefix, and the token's release to `FREE` follows.
//!
//! **A retirement outside a collection** — inside a teardown, and between
//! the pressure path's rounds — retires the completed deaths standing
//! anywhere in P in place, under the token or under `POSTED`, and advances
//! nothing (`crate::cycle::queue::retire_candidates`).

use super::*;

use crate::ring::NoBlock;
#[cfg(test)]
use crate::ring::Reader;

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
    /// The trace never walked the root — an allocation refused, or the
    /// mutator recalled its token — posted so that no root blocks the ring
    /// behind it.
    Unwalked = 3,
}

impl Verdict {
    /// Whether an entry under this verdict is a root of a batch of `form`:
    /// one the mutator traces and validates exactly. A proposal is a root of
    /// every batch, and an unwalked root of a batch over R whole alone.
    fn is_root_in(self, form: BatchForm) -> bool {
        match self {
            Self::Proposed => true,
            Self::Unwalked => form == BatchForm::AllRoots,
            Self::ReadLive | Self::ZeroCount => false,
        }
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

/// Whether an entry of P is a root of the mutator's batch of `form`.
#[inline]
pub(super) fn is_batch_root(entry: usize, form: BatchForm) -> bool {
    !is_disposed(entry) && entry_verdict(entry).is_root_in(form)
}

/// The collector's handle over one mutator's P: how much room it has, and the
/// post of one verdict (`crate::cycle::worker`, the batch).
pub(crate) struct VerdictWriter<'a>(ring::Writer<'a>);

impl<'a> VerdictWriter<'a> {
    /// The writer over `record`'s P.
    ///
    /// # Safety
    /// The calling thread holds `record`'s token, which is what makes it P's
    /// one producer for the handle's life; without it the handle is read
    /// through [`VerdictWriter::room_by_loads`] alone, which stores nothing.
    pub(crate) unsafe fn open(record: &'a MutatorRecord) -> Self {
        Self(unsafe { ring::Writer::new(record.verdict_ring()) })
    }

    /// Verdicts P takes before it is full, which is what a batch is clamped
    /// to before its roots are taken from R: exact, the writer's copy of P's
    /// front brought up to the reader's first. A push refreshes that copy
    /// only when it reads the block full, so after a batch that posted short
    /// of a full block the copy lags by every verdict the mutator has answered
    /// for since, and a batch that posts nothing never refreshes it; a room
    /// read off the lagging copy clamps a batch to what the last posting batch
    /// left, down to one root (`dev/BENCHMARKS.md`, "S65.24 A, B and C on a
    /// box with a PMU"). One load of the reader's line a call; under the
    /// token, where no other thread writes that line.
    pub(crate) fn room(&self) -> usize {
        self.0.catch_up_with_the_reader();
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
/// mutator's token.
///
/// `None` for a thread with no record, which holds no verdict.
pub(super) fn verdict_ring<'a>() -> Option<Quiescent<'a>> {
    let record = mutator_record::this_thread_record();
    if record.is_null() {
        return None;
    }

    Some(unsafe { Quiescent::new((*record).verdict_ring()) })
}

/// Note on this thread's record that the collection its poll fired freed
/// or retired something ([`MutatorRecord::note_freeing_disposition`]); nothing for a
/// thread with no record, which holds no verdict.
pub(crate) fn note_freeing_disposition() {
    let record = mutator_record::this_thread_record();
    if !record.is_null() {
        unsafe { &*record }.note_freeing_disposition();
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
    // The byte with it: a `POSTED` left standing over an empty P would make
    // the next case's first reading arm a collection over nothing.
    unsafe { &*record }.clear_posted_for_test();
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
