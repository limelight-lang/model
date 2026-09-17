//! The mutator's retirement pass in ring form: one in-place compaction of R,
//! the overflow buffer and, when asked, the deferred lane, drawing nothing;
//! and over P, the disposition of a batch's prefix of verdicts or the
//! in-place retirement of the completed deaths standing anywhere in it.
//!
//! An entry whose entity completed its death in place is retired — its slot
//! goes back through `ll_free` — and an entry the close marked goes to the
//! deferred lane when a spare block can be had for it; every other entry is
//! kept in order, packed from the front, and the blocks' `tail` indices and
//! the tail block are lowered by the ring's own quiet pass
//! (`crate::ring::Quiescent::rewrite`). Nothing is merged back, because
//! nothing was taken out (`rfc/dev/DECISIONS.md`, "the candidate queue is
//! read behind its writer, and the collector's verdicts come back by a
//! second ring", "The in-line collection over R").
//!
//! **An unwind inside the pass leaves every lane whole.** The ring's pass
//! and the chain's finish themselves on the unwind, keeping every entry not
//! yet answered for; the overflow pass is a frame of its own with the same
//! drop. An entry whose free raised is the one exception: its slot is
//! half-returned and cannot be retried, so its entry is dropped as
//! `ll_free`'s (`dev/DECISIONS.md`, "corrupt queue entries remain outside
//! the cleanup recovery contract").

use super::*;

/// Where a staged entry goes, and the order the three answers rank in.
///
/// [`Destination::Free`] outranks [`Destination::Deferred`]: a root marked for
/// the deferred lane that then died inside step 4 is a torn-down entity, and
/// its slot belongs to the retirement whatever the mark says.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Destination {
    /// The lane the entry stands in, which cannot refuse.
    Keep,
    /// The deferred lane, if a block can be had for it.
    Deferred,
    /// `ll_free`, which returns the slot the record was withholding.
    Free,
}

/// Compact this thread's queue in place: retire every completed death, move
/// every marked entry the deferred lane can take, keep the rest in order.
///
/// `deferred_at` is the commit count a deferred lane going from empty to
/// occupied records, and `None` where this pass has no marks to read — every
/// caller but the close's own ([`crate::cycle::queue::dispose_candidates`]
/// and [`crate::cycle::queue::defer_candidates`]). A pass given `None` over a
/// marked entry keeps it in the ring with its mark taken off, which is the
/// fallback rather than a defect.
///
/// `sweep_deferred` asks for the deferred lane's own entries to be read for
/// completed deaths too, ahead of the ring's marked entries joining it.
///
/// `verdicts` is the batch's prefix of P, disposed of whole and advanced
/// past ([`dispose_verdicts`]); `None` retires P's completed deaths in
/// place and advances nothing.
pub(super) fn compact(deferred_at: Option<u64>, sweep_deferred: bool, verdicts: Option<usize>) {
    let state = mutator_state();
    if state.is_null() {
        return;
    }
    let mutator_state = unsafe { mutator_state_ref(state) };
    note_queue_work(1, 0, 0);
    checkpoint(0);

    if sweep_deferred {
        mutator_state.deferred().retain(
            |entry| {
                note_queue_work(0, 1, 0);
                let entity = entry_entity(entry);
                if !completed_death(entity) {
                    return true;
                }

                free(entity);
                false
            },
            |block| {
                discharge_block();
                return_surplus_block(mutator_state, block);
            },
        );
    }

    if let Some(ring) = candidate_ring() {
        let mut pass = ring.packing();
        while let Some(entry) = pass.read() {
            note_queue_work(0, 1, 0);
            let marked = entry & DEFERRED_MARK != 0;
            let entity = entry_entity(entry);
            let destination = if completed_death(entity) {
                Destination::Free
            } else if marked && deferred_at.is_some() {
                Destination::Deferred
            } else {
                Destination::Keep
            };
            // Before the disposition acts: an unwind here keeps the entry
            // as it stood, mark and all.
            checkpoint(1);

            match destination {
                Destination::Free => {
                    pass.discard();
                    free(entity);
                }
                Destination::Deferred => {
                    // Out of the ring before it is in the lane, so that no
                    // unwind between the two finds it in both.
                    pass.discard();
                    let deferred = defer_entry(mutator_state, entity, deferred_at);
                    note_queue_work(0, 0, 1);
                    if deferred.is_err() {
                        // Both cells empty: the root stays in the ring and
                        // is offered to the next collection rather than to
                        // the turnover.
                        pass.write(entity_entry(entity));
                        continue;
                    }

                    checkpoint(3);
                }
                Destination::Keep => {
                    note_queue_work(0, 0, 1);
                    pass.write(entity_entry(entity));
                }
            }
        }
    }

    checkpoint(4);
    OverflowPass::open(state).run();
    checkpoint(6);
    dispose_verdicts(state, verdicts, deferred_at);
}

/// The pass over P. With `prefix` the batch's count of P's entries, dispose
/// of each of them: a completed death is retired, a root marked or read live
/// goes to the deferred lane, and everything else — a component refused or
/// resurrected, a resurrected zero count, a root the lane had no block
/// for — is written back into R as a registration, before P's front
/// advances past the whole prefix. Without a prefix, every completed death
/// standing in P is retired in place, its entry nulled, and P's front stays.
///
/// The writes into P's slots are the mutator's under its exclusion of the
/// collector (`crate::cycle::queue::verdicts`, "Who writes P's slots"). An
/// entry is nulled as soon as it is answered for, so an unwind out of a
/// free or a write-back leaves no entry that a later reading would answer
/// for twice; the advance is the pass's last act, and a prefix an unwind
/// left unadvanced is read again by the next batch, its disposed entries
/// skipped.
fn dispose_verdicts(
    state: *mut MutatorCycleState,
    prefix: Option<usize>,
    deferred_at: Option<u64>,
) {
    let Some(ring) = verdicts::verdict_ring() else {
        return;
    };
    let mutator_state = unsafe { mutator_state_ref(state) };
    let count = prefix.unwrap_or_else(|| ring.count());
    ring.map_prefix_in_place(count, |slot| {
        let entry = *slot;
        if verdicts::is_disposed(entry) {
            return;
        }

        note_queue_work(0, 1, 0);
        let entity = verdicts::verdict_entity(entry);
        if completed_death(entity) {
            // Nulled before the free, as the ring's pass discards before
            // it frees.
            *slot = 0;
            free(entity);
            return;
        }

        if prefix.is_none() {
            return;
        }

        let verdict = verdicts::entry_verdict(entry);
        let deferrable = verdict == verdicts::Verdict::ReadLive
            || (verdicts::is_batch_root(entry) && entry & verdicts::VERDICT_DEFER_MARK != 0);
        // Nulled before the move, so that no unwind between the two finds
        // the entity in P and in a lane.
        *slot = 0;
        if deferrable && defer_entry(mutator_state, entity, deferred_at).is_ok() {
            note_queue_work(0, 0, 1);
            return;
        }

        // Written back into R as a registration is, its candidate bit
        // still set (`rfc/model/gc/rc-cycle.md`, "The mutator's
        // disposition").
        unsafe { append_entry(state, entity) };
        note_queue_work(0, 0, 1);
    });

    if prefix.is_some() {
        checkpoint(7);
        let record = mutator_record::this_thread_record();
        unsafe { ring::Reader::new((*record).verdict_ring()) }.advance(count);
    }
}

/// Whether `entity`'s death completed in place, which is the one state a
/// retirement acts on: a zero count whose teardown has not ended is left
/// registered (`crate::refcount::SlotStateReading`).
pub(super) fn completed_death(entity: *mut RcHeader) -> bool {
    matches!(
        unsafe { crate::refcount::slot_state_with_flags(entity) },
        crate::refcount::SlotStateReading::DeadInPlace { .. }
    )
}

/// Return the slot a retired entry was withholding.
///
/// The two slot bits come off first, and the pointer stays this frame's
/// through the checkpoint between the two: an unwind there frees on the
/// way out, since the entry is already dropped from its lane. Ownership
/// crosses to `ll_free` at the call, and a panic inside the allocator cannot
/// be retried — it may already have returned the slot or unmapped the whole
/// run.
pub(super) fn free(entity: *mut RcHeader) {
    struct PendingFree(*mut RcHeader);
    impl Drop for PendingFree {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { crate::memory::stdapi::ll_free(self.0.cast()) };
            }
        }
    }

    unsafe {
        crate::refcount::update_header_flags(entity, |flags| {
            flags & !(crate::refcount::CANDIDATE_BIT | crate::refcount::DEAD_IN_PLACE)
        });
    }
    let mut pending = PendingFree(entity);
    checkpoint(2);
    let entity = std::mem::replace(&mut pending.0, std::ptr::null_mut());
    unsafe { crate::memory::stdapi::ll_free(entity.cast()) };
    let state = mutator_state();
    if !state.is_null() {
        let retired = &unsafe { mutator_state_ref(state) }.retired_by_the_close;
        retired.set(retired.get().saturating_add(1));
    }
}

/// The overflow buffer's pass: retire completed deaths and pack the rest
/// from the buffer's start. The drop finishes it, so an unwind out of a free
/// leaves the buffer packed and its count right.
struct OverflowPass {
    state: *mut MutatorCycleState,
    read: usize,
    write: usize,
    bound: usize,
}

impl OverflowPass {
    fn open(state: *mut MutatorCycleState) -> Self {
        let mutator_state = unsafe { mutator_state_ref(state) };
        Self {
            state,
            read: 0,
            write: 0,
            bound: usize::from(mutator_state.overflow_len.get()),
        }
    }

    fn run(&mut self) {
        while self.read < self.bound {
            let entity = unsafe { overflow_entries(self.state).add(self.read).read() };
            self.read += 1;
            note_queue_work(0, 1, 0);
            if completed_death(entity) {
                free(entity);
                continue;
            }

            self.keep(entity);
            checkpoint(5);
        }
    }

    fn keep(&mut self, entity: *mut RcHeader) {
        unsafe { overflow_entries(self.state).add(self.write).write(entity) };
        note_queue_work(0, 0, 1);
        self.write += 1;
    }
}

impl Drop for OverflowPass {
    fn drop(&mut self) {
        // The unwind's case, and a no-op on the return: what was not read is
        // kept.
        while self.read < self.bound {
            let entity = unsafe { overflow_entries(self.state).add(self.read).read() };
            self.read += 1;
            self.keep(entity);
        }

        let mutator_state = unsafe { mutator_state_ref(self.state) };
        mutator_state.overflow_len.set(stored_len(self.write));
        // The entries that left took their pointers with them; the ledger
        // follows in one step rather than per free, the bytes being
        // released in the same breath.
        gc_metadata::discharge((self.bound - self.write) * size_of::<*mut RcHeader>());
    }
}

#[cfg(test)]
thread_local! {
    static FAIL_AT: Cell<Option<usize>> = const { Cell::new(None) };
}

/// A point the pass passes through, where a case may raise an unwind
/// (`inject`): 0 before anything moves, 1 after an entry of the ring is
/// read and before its disposition acts, 2 between a retired entry's flag
/// clear and its free, 3 after an entry joined the deferred lane, 4 between
/// the ring's pass and the overflow buffer's, 5 after an overflow entry is
/// kept, 6 after the overflow buffer's pass and before P's, 7 after every
/// entry of P's prefix is answered for and before the advance. A free that
/// raises inside P's pass is point 2, the same as inside the ring's.
#[inline]
fn checkpoint(_point: usize) {
    #[cfg(test)]
    if FAIL_AT.with(|point| {
        if point.get() == Some(_point) {
            point.set(None);
            true
        } else {
            false
        }
    }) {
        panic!("injected queue compaction unwind at {_point}");
    }
}

/// The last checkpoint of the passes over R and the overflow buffer; the
/// one after it is P's ([`VERDICT_ADVANCE_CHECKPOINT`]).
#[cfg(test)]
pub(super) const LAST_CHECKPOINT: usize = 6;

/// The checkpoint before P's advance.
#[cfg(test)]
pub(super) const VERDICT_ADVANCE_CHECKPOINT: usize = 7;

/// Arm one unwind at `point` for this thread's next pass.
#[cfg(test)]
pub(super) fn inject(point: usize) -> Injection {
    FAIL_AT.with(|slot| slot.set(Some(point)));
    Injection
}

#[cfg(test)]
pub(super) struct Injection;

#[cfg(test)]
impl Drop for Injection {
    fn drop(&mut self) {
        FAIL_AT.with(|slot| slot.set(None));
    }
}
