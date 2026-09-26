//! The collector's chain of the roots it read live: variant C of `PLAN.md`
//! S65.24, built behind the feature `collector-chain` to be measured against
//! today's code and form D (`dev/design/the-collector-keeps-the-live-roots.md`).
//!
//! **What changes.** A root the collector reads live under its grant is not
//! posted into P: it goes into the chain's waiting part on the mutator's
//! record, and a batch that posted nothing into P releases the token to
//! `FREE`, so the mutator does nothing for it. A block of the waiting part
//! moves whole to the ready part once the record's epoch has passed the stamp
//! it took at its first entry ([`expire`]), and the next batch reads the
//! ready part beside R, the clamp split between the two. A root the trace did
//! not reach goes to the ready part's tail, behind the retries already
//! waiting. The candidate bit stays set on every root the chain holds, so a
//! chained root that dies keeps its slot: each grant reads up to
//! [`DEATH_CHECK_BUDGET`] headers of the waiting part past a cursor kept in
//! each block ([`check_the_deaths`]), and a completed death it finds is taken
//! out of the chain and posted into P as `ZeroCount`, which the mutator's
//! disposition of P frees.
//!
//! **What stays with the mutator.** Its own collections are form D's: they
//! defer the roots they read live into the deferred lane. Before them the
//! chain comes back into R, R's one writer being the mutator: whole before
//! each round of the exit and before a collection under pressure
//! ([`splice_the_whole_chain_into_r`]), and its ready part, the blocks the
//! epoch passed moved into it first, before a collection over R whole off the
//! poll, by the explicit call or on the elder's ask
//! ([`splice_the_ready_part_into_r`]).
//!
//! **Who touches it.** Every operation here is the token holder's: the
//! collector under `COLLECTOR|s`, or the mutator under `MUTATOR`. The round
//! reads the two counts, the oldest stamp and the check's instant under its
//! reading hold as atomic loads that read no block ([`is_due`]).

use crate::cycle::mutator_record::MutatorRecord;
use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockHeader};
use crate::memory::gc_metadata;
use crate::refcount::RcHeader;
use crate::ring::{ChainPeek, Checked, RecordChain};

/// Headers the death check reads of the waiting part per grant: a grant
/// already reads up to [`crate::cycle::worker`]'s `BATCH_BOUND` roots' headers
/// on its first pass, so the check adds no more cold reads than the batch's
/// own (the Sage, 2026-09-26).
pub(crate) const DEATH_CHECK_BUDGET: usize = 1024;

/// Completed deaths the check must find before it posts them: one, so every
/// death found is posted in the grant that found it. The Sage's 32 stays
/// named for the arm that would batch them (2026-09-26).
pub(crate) const DEATHS_TO_POST: usize = 1;

const _: () = assert!(
    DEATHS_TO_POST == 1,
    "a floor above one needs its accumulation"
);

/// A block for the chain, drawn on the collector's thread and charged whole
/// to the GC ledger as a lane's block is; null when the pool refuses.
fn fresh_block() -> *mut BlockHeader {
    #[cfg(test)]
    if testing::REFUSE_BLOCKS.load(std::sync::atomic::Ordering::Relaxed) {
        crate::cycle::worker::testing::note_chain_refusal();
        return std::ptr::null_mut();
    }

    let block = gc_metadata::acquire();
    if block.is_null() {
        #[cfg(test)]
        crate::cycle::worker::testing::note_chain_refusal();
    } else {
        gc_metadata::charge(BLOCK_PAYLOAD);
        #[cfg(test)]
        crate::cycle::worker::testing::note_chain_block(1);
    }
    block
}

/// Give back a block the chain consumed or packed away empty.
fn give_back(block: *mut BlockHeader) {
    gc_metadata::discharge(BLOCK_PAYLOAD);
    gc_metadata::release(block);
    #[cfg(test)]
    crate::cycle::worker::testing::note_chain_block(-1);
}

/// Whether `record`'s chain owes a grant: a ready part standing, or a
/// waiting part whose oldest block the epoch has passed or whose last death
/// check is `term` old at `now`. Atomic loads of the record alone.
pub(crate) fn is_due(record: &MutatorRecord, now: u64, term: u64) -> bool {
    if record.chain_ready().len() > 0 {
        return true;
    }

    let waiting = record.chain_waiting();
    waiting.len() > 0
        && (record.turnovers() > waiting.oldest_stamp()
            || now.saturating_sub(record.chain_checked_at()) >= term)
}

/// Move the waiting part's blocks whose stamp the record's epoch has passed
/// to the ready part's tail, whole and in order. `stop` is read before each
/// block, the recall's reading, and a true answer leaves the rest for the
/// next grant.
///
/// # Safety
/// The caller holds `record`'s token.
pub(crate) unsafe fn expire(record: &MutatorRecord, mut stop: impl FnMut() -> bool) {
    let epoch = record.turnovers();
    if let Some(due) = unsafe {
        record
            .chain_waiting()
            .detach_while(|stamp| stamp < epoch, &mut stop)
    } {
        unsafe { record.chain_ready().append(due) };
    }
    // A ready block the death check left with tombstones alone holds no
    // root, and no peek walks it: it goes back here.
    unsafe {
        record
            .chain_ready()
            .give_back_leading_empty_blocks(give_back, stop)
    };
}

/// Read up to [`DEATH_CHECK_BUDGET`] headers of the waiting part past its
/// cursor and hand every completed death found to `post`, which answers
/// whether P took it; a death P has no room for stays in the chain with the
/// cursor on it, and the check ends there, so the next check posts it first.
/// `stop` is read before each header, the recall's reading, and ends the
/// check with the cursor kept. Notes the check's instant. Answers the deaths
/// posted.
///
/// A completed death is final: its count reached zero and its teardown
/// ended, and its slot stays withheld by the candidate bit until the
/// mutator retires it, so the reading holds whatever the mutator does
/// meanwhile, as the grant's withholding holds every other death.
///
/// # Safety
/// The caller holds `record`'s token as a collector's grant, which withholds
/// every return of the mutator's.
pub(crate) unsafe fn check_the_deaths(
    record: &MutatorRecord,
    now: u64,
    mut stop: impl FnMut() -> bool,
    mut post: impl FnMut(*mut RcHeader) -> bool,
) -> usize {
    record.note_chain_checked(now);
    let mut posted = 0;
    let read = unsafe {
        record
            .chain_waiting()
            .check(DEATH_CHECK_BUDGET, stop, |entry| {
                let entity = crate::cycle::queue::entry_root(entry);
                if !is_a_completed_death(entity) {
                    return Checked::Keep;
                }

                if !post(entity) {
                    return Checked::KeepAndStop;
                }

                posted += 1;
                Checked::Take
            })
    };
    #[cfg(test)]
    crate::cycle::worker::testing::note_chain_check(read, posted);
    #[cfg(not(test))]
    let _ = read;
    posted
}

/// Whether `entity`'s death has completed in place, the one state the
/// mutator's retirement acts on (`crate::cycle::queue`).
fn is_a_completed_death(entity: *mut RcHeader) -> bool {
    matches!(
        unsafe { crate::refcount::slot_state_with_flags(entity) },
        crate::refcount::SlotStateReading::DeadInPlace { .. }
    )
}

/// Copy up to `out.len()` roots of the ready part into `out`, oldest first;
/// they stay in the chain until [`commit_the_ready_part`].
///
/// # Safety
/// The caller holds `record`'s token.
pub(crate) unsafe fn peek_the_ready_part(record: &MutatorRecord, out: &mut [usize]) -> ChainPeek {
    unsafe { record.chain_ready().peek(out) }
}

/// Take what `peek` copied out of the ready part, every block it consumed
/// whole given back.
///
/// # Safety
/// The caller holds `record`'s token, and `peek` is the ready part's last.
pub(crate) unsafe fn commit_the_ready_part(record: &MutatorRecord, peek: ChainPeek) {
    unsafe { record.chain_ready().commit(peek, give_back) };
}

/// Put `root`, read live, in the waiting part, its block stamped with the
/// record's epoch; false when the pool refused a block and the root is not
/// in the chain.
///
/// # Safety
/// The caller holds `record`'s token as a collector's grant.
pub(crate) unsafe fn keep_read_live(record: &MutatorRecord, root: *mut RcHeader, now: u64) -> bool {
    let waiting = record.chain_waiting();
    if !unsafe { waiting.has_a_block() } {
        // The term of the death check runs from the first root of a chain
        // that held none.
        record.note_chain_checked(now);
    }

    let kept =
        unsafe { waiting.push(root.expose_provenance(), record.turnovers(), fresh_block) }.is_ok();
    #[cfg(test)]
    if kept {
        crate::cycle::worker::testing::note_chain_push(false);
    }
    kept
}

/// Put `root`, which the trace did not reach, at the ready part's tail;
/// false when the pool refused a block.
///
/// # Safety
/// The caller holds `record`'s token as a collector's grant.
pub(crate) unsafe fn keep_unwalked(record: &MutatorRecord, root: *mut RcHeader) -> bool {
    let kept = unsafe {
        record
            .chain_ready()
            .push(root.expose_provenance(), record.turnovers(), fresh_block)
    }
    .is_ok();
    #[cfg(test)]
    if kept {
        crate::cycle::worker::testing::note_chain_push(true);
    }
    kept
}

/// Entries the chain holds, both parts: the exit's count of registrations
/// (`crate::cycle::queue::registered_by_lane`).
pub(crate) fn len(record: &MutatorRecord) -> usize {
    record.chain_waiting().len() + record.chain_ready().len()
}

/// Splice the whole chain into R after its tail, packed: the exit before each
/// round and the collection under pressure, both of which read every root a
/// registration holds.
///
/// # Safety
/// The caller is `record`'s mutator and holds its own claim.
pub(crate) unsafe fn splice_the_whole_chain_into_r(record: &MutatorRecord) {
    unsafe {
        splice(record, record.chain_ready());
        splice(record, record.chain_waiting());
    }
}

/// Splice the ready part into R after its tail, packed, the waiting part's
/// blocks the epoch passed moved into it first: a collection over R whole,
/// which reads what the collector would have read next.
///
/// # Safety
/// As [`splice_the_whole_chain_into_r`].
pub(crate) unsafe fn splice_the_ready_part_into_r(record: &MutatorRecord) {
    unsafe {
        expire(record, || false);
        splice(record, record.chain_ready());
    }
}

/// This thread's chain into its R, `whole` or its ready part; nothing for a
/// thread with no record.
///
/// # Safety
/// The calling thread holds its own claim.
pub(crate) unsafe fn splice_this_threads_chain_into_r(whole: bool) {
    let record = crate::cycle::mutator_record::this_thread_record();
    if record.is_null() {
        return;
    }

    let record = unsafe { &*record };
    unsafe {
        if whole {
            splice_the_whole_chain_into_r(record);
        } else {
            splice_the_ready_part_into_r(record);
        }
    }
}

/// Splice `part` into R after its tail and count the merge, so that the
/// collector's next round takes the merged ring as it takes a re-offered
/// lane.
///
/// # Safety
/// As [`splice_the_whole_chain_into_r`].
unsafe fn splice(record: &MutatorRecord, part: &RecordChain) {
    let Some((first, last)) = (unsafe { part.take_compacted(give_back) }) else {
        return;
    };

    // The blocks change owner from the chain to R: R's blocks are charged
    // whole from their link to their unlink as the chain's are, so the
    // ledger moves nothing.
    #[cfg(test)]
    {
        let mut blocks = 0;
        let mut block = first;
        loop {
            blocks += 1;
            if block == last {
                break;
            }
            block = unsafe { crate::ring::next_block(block) };
        }
        crate::cycle::worker::testing::note_chain_block(-blocks);
    }
    let writer = unsafe { crate::ring::Writer::new(record.candidate_ring()) };
    unsafe { writer.splice_after_tail(first, last) };
    record.note_a_merge();
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// Whether the chain's draws are refused, for a case of the pool's
    /// refusal that leaves the collector's own workspace served.
    pub(crate) static REFUSE_BLOCKS: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    /// Give every block of this thread's chain back, entries and all: the
    /// reset a case makes of what an earlier one left.
    pub(crate) fn dismantle_this_threads() {
        let record = crate::cycle::mutator_record::this_thread_record();
        if record.is_null() {
            return;
        }

        let record = unsafe { &*record };
        for part in [record.chain_ready(), record.chain_waiting()] {
            if let Some((first, last)) = unsafe { part.take_compacted(give_back) } {
                let mut block = first;
                loop {
                    let next = unsafe { crate::ring::next_block(block) };
                    give_back(block);
                    if block == last {
                        break;
                    }
                    block = next;
                }
            }
        }
    }

    /// The roots this thread's chain holds, ready part first.
    pub(crate) fn roots_of_this_threads() -> (Vec<*mut RcHeader>, Vec<*mut RcHeader>) {
        let record = unsafe { &*crate::cycle::mutator_record::this_thread_record() };
        let read = |part: &RecordChain| {
            let mut roots = Vec::new();
            unsafe { part.walk(|entry| roots.push(crate::cycle::queue::entry_root(entry))) };
            roots
        };
        (read(record.chain_ready()), read(record.chain_waiting()))
    }
}
