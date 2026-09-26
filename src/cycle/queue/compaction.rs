//! The mutator's retirement pass in ring form: one in-place compaction of R
//! where the pass is asked to read it, the overflow buffer and, when asked,
//! the deferred lane, drawing nothing; and over P, the disposition of a
//! batch's prefix of verdicts or the in-place retirement of the completed
//! deaths standing anywhere in it. The close of a collection over P reads
//! of R only the run of completed deaths at its front, and the entry that
//! stops the run ([`Lanes::Overflow`], [`free_the_front_run`]).
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

/// Which of the mutator's own lanes a pass reads beside P.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Lanes {
    /// R whole and the overflow buffer: every pass but the close of a
    /// collection over P.
    RingAndOverflow,
    /// The overflow buffer, and of R the run of completed deaths at its front:
    /// the close of a collection over P, whose batch holds no entry of R. A
    /// completed death standing in R behind an entry that is not one is
    /// retired by the collector's batch that takes it, through P, by the
    /// free path's count below the threshold, or by a collection over R
    /// whole (`rfc/model/gc/rc-cycle.md`, "The mutator's disposition").
    Overflow,
}

/// Compact this thread's queue in place: retire every completed death, move
/// every marked entry the deferred lane can take, keep the rest in order.
///
/// `lanes` says whether R is read whole; a pass that reads it zeroes the free
/// path's count of withheld deaths, and every retirement lowers the count by
/// one ([`free`]), so the count a close leaves is about the deaths it left
/// standing — one high after an unwind inside a free, lower where the count
/// was zeroed with deaths still standing.
///
/// `deferred_at` is the epoch cell a deferred lane going from empty to
/// occupied records as its mirror, and `None` where this pass has no marks to read — every
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
pub(super) fn compact(
    lanes: Lanes,
    deferred_at: Option<u64>,
    sweep_deferred: bool,
    verdicts: Option<usize>,
) {
    let state = mutator_state();
    if state.is_null() {
        return;
    }
    let mutator_state = unsafe { mutator_state_ref(state) };
    note_queue_work(1, 0, 0);
    checkpoint(0);
    let ring = match lanes {
        Lanes::RingAndOverflow => {
            // The pass reads R whole and retires what the free path counted.
            mutator_state.candidate_deaths.set(0);
            candidate_ring()
        }
        Lanes::Overflow => None,
    };

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

    if let Some(ring) = ring {
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
    // A drop that unwinds disposes of P, which the release to `FREE` needs,
    // and leaves the run, which nothing needs at once, to the next close, so
    // that the unwind meets no more frees than it must.
    if lanes == Lanes::Overflow && !std::thread::panicking() {
        free_the_front_run();
    }
}

/// Free the run of completed deaths standing at R's front, one entry at a
/// time, and stop at the first entry that is not one — a live entry, or a
/// zero count whose teardown has not ended: every ending of a collection over
/// P, under `MUTATOR`, where the mutator is R's one consumer
/// (`dev/DECISIONS.md`, 2026-09-26, "the close of a collection over P frees
/// the run of completed deaths at R's front"). It reads one entry more than
/// it frees, so its reading of R is bounded by the slots it returns. Each
/// entry is consumed before its slot is freed, so no freed slot is named by R
/// at any instant; an unwind inside a free leaves the front past that entry
/// and the rest of the run to the next close. A marked entry is freed like
/// any other, `Free` outranking `Deferred`.
fn free_the_front_run() {
    let record = mutator_record::this_thread_record();
    if record.is_null() {
        return;
    }

    // SAFETY: the close holds the token at `MUTATOR`, so this thread is R's
    // one consumer; the Overflow arm opened no pass over R, and a collector's
    // reading before its claim consumes nothing.
    let reader = unsafe { ring::Reader::new((*record).candidate_ring()) };
    let mut one = [0usize; 1];
    loop {
        let peeked = reader.peek(&mut one);
        if peeked.len() == 0 {
            return;
        }

        note_queue_work(0, 1, 0);
        let entity = entry_entity(one[0]);
        if !completed_death(entity) {
            return;
        }

        reader.commit(peeked);
        free(entity);
        checkpoint(FRONT_RUN_CHECKPOINT);
    }
}

/// The pass over P. With `prefix` the batch's count of P's entries, dispose
/// of each of them: a completed death is retired, a root marked or read live
/// goes to the deferred lane, and everything else — a component refused or
/// resurrected, a resurrected zero count, a root the lane had no block
/// for, an unwalked root the collection over P did not trace — is written
/// back into R as a registration, before P's front advances past the whole
/// prefix. Without a prefix, every completed death
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

        // Read without the batch's form, which a close that ended before its
        // own disposition does not have: the mark is written over a root of
        // the marking batch alone, so an unwalked entry of a collection over P
        // carries one only where a collection over R whole marked it and
        // unwound before disposing of it, and that reading defers it.
        let deferrable = verdicts::entry_verdict(entry) == verdicts::Verdict::ReadLive
            || entry & verdicts::VERDICT_DEFER_MARK != 0;
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
        #[cfg(test)]
        crate::cycle::worker::testing::note_written_back();
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
        let mutator_state = unsafe { mutator_state_ref(state) };
        let retired = &mutator_state.retired_by_the_close;
        retired.set(retired.get().saturating_add(1));
        // A retired death leaves the free path's count, which a pass that
        // read none of R would otherwise keep (S65.20's Critic, finding 2).
        let deaths = &mutator_state.candidate_deaths;
        deaths.set(deaths.get().saturating_sub(1));
    }
    #[cfg(test)]
    note_a_withheld_death_retired();
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
/// entry of P's prefix is answered for and before the advance, 8 after an
/// entry of R's front run is freed. A free that raises inside P's pass or the
/// front run is point 2, the same as inside the ring's.
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

/// The checkpoint after each free of R's front run, past P's advance.
pub(super) const FRONT_RUN_CHECKPOINT: usize = 8;

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
