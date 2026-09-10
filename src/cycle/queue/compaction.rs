//! Bounded, allocation-free combination and owner retirement.
//!
//! The input links remain intact while entries move towards their front.
//! Both original partial heads keep their own bounds. Once no reader remains,
//! the occupied prefix is reversed, making its last partial segment the head.
//! The fixed cleanup frame owns every cursor and pending return across unwind.

use super::*;

/// Where a staged entry goes, and the order the three answers rank in.
///
/// [`Destination::Free`] outranks [`Destination::Deferred`]: a root marked for
/// the deferred lane that then died inside step 4 is a torn-down entity, and
/// its slot belongs to the retirement whatever the mark says.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Destination {
    /// The in-place output, which is the active lane and cannot refuse.
    Active,
    /// The deferred lane, if a segment can be had for it.
    Deferred,
    /// `ll_free`, which returns the slot the record was withholding.
    Free,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Records,
    Overflow,
    Reverse,
    Publish,
    ReturnSegments,
    Done,
}

struct Compaction {
    state: *mut OwnerCycleState,
    partial_heads: [*mut BlockHeader; 2],
    bounds: [usize; 2],
    read: *mut BlockHeader,
    read_index: usize,
    write: *mut BlockHeader,
    write_fill: usize,
    head: *mut BlockHeader,
    kept_segments: usize,
    charged_segments: usize,
    overflow_read: usize,
    overflow_write: usize,
    overflow_bound: usize,
    pending: *mut RcHeader,
    /// Where [`Compaction::pending`] goes, or [`Destination::Active`] when the
    /// frame holds none.
    pending_to: Destination,
    /// The commit count a lane going from empty to occupied records, or `None`
    /// where this pass defers nothing.
    deferred_at: Option<u64>,
    /// Deferred heads this pass made interior, which the ledger charges once
    /// at [`Phase::Publish`] rather than at each append.
    deferred_heads: usize,
    reversed_head: *mut BlockHeader,
    surplus: *mut BlockHeader,
    retire: bool,
    phase: Phase,
}

/// Combine the active lane and `batch` into one chain, retiring what died and
/// sending what the close marked to the deferred lane.
///
/// `deferred_at` is the commit count a deferred lane going from empty to
/// occupied records, and `None` where this pass has no marks to read — every
/// caller but the close's own ([`crate::cycle::queue::dispose_candidates`]).
/// A pass given `None` over a marked batch would put a marked root in the
/// active lane, which is the fallback rather than a defect, and the mark comes
/// off either way.
pub(super) fn finish(mut batch: InFlightBatch, retire: bool, deferred_at: Option<u64>) {
    let state = owner_state();
    if state.is_null() {
        assert!(
            batch.is_empty(),
            "the queue base block left with a batch out"
        );
        return;
    }
    let owner_state = unsafe { owner_state_ref(state) };
    let active = owner_state.write_segment.get();
    if !retire && active.is_null() {
        owner_state.write_segment.set(batch.head);
        owner_state.write_len.set(stored_len(batch.fill));
        batch.head = std::ptr::null_mut();
        return;
    }
    let mut pass = Compaction {
        state,
        partial_heads: [active, batch.head],
        bounds: [usize::from(owner_state.write_len.get()), batch.fill],
        read: active,
        read_index: 0,
        write: active,
        write_fill: 0,
        head: active,
        kept_segments: 0,
        charged_segments: 0,
        overflow_read: 0,
        overflow_write: 0,
        overflow_bound: usize::from(owner_state.overflow_len.get()),
        pending: std::ptr::null_mut(),
        pending_to: Destination::Active,
        deferred_at,
        deferred_heads: 0,
        reversed_head: std::ptr::null_mut(),
        surplus: std::ptr::null_mut(),
        retire,
        phase: Phase::Records,
    };

    // No fallible operation separates acquisition from joining the chains.
    // The frame owns both original bounds before either head becomes interior.
    for head in pass.partial_heads {
        let mut segment = head;
        while !segment.is_null() {
            if segment != head {
                pass.charged_segments += 1;
            }
            segment = unsafe { (*segment).next };
        }
    }
    if active.is_null() {
        pass.head = batch.head;
    } else {
        let mut tail = active;
        while !unsafe { (*tail).next }.is_null() {
            tail = unsafe { (*tail).next };
        }
        unsafe { (*tail).next = batch.head };
    }
    pass.read = pass.head;
    pass.write = pass.head;
    batch.head = std::ptr::null_mut();
    owner_state.write_segment.set(std::ptr::null_mut());
    owner_state.write_len.set(0);
    // The temporary queue is private until publication, and the overflow
    // bounds are owned here too.
    owner_state.overflow_len.set(0);
    note_queue_work(1, 0, 0);
    gc_metadata::mark_peak((pass.bounds[0] + pass.bounds[1]) * size_of::<*mut RcHeader>());
    checkpoint(0);
    pass.run(true);
}

impl Compaction {
    fn bound(&self) -> usize {
        for index in 0..2 {
            if self.read == self.partial_heads[index] {
                return self.bounds[index];
            }
        }
        SEGMENT_CAPACITY
    }

    fn run(&mut self, inject: bool) {
        while self.phase != Phase::Done {
            if !self.pending.is_null() {
                if self.pending_to == Destination::Deferred && !self.append_to_deferred_lane() {
                    // Both spare cells were empty. The record takes the one
                    // destination that cannot refuse, and its root is offered
                    // to the next collection rather than to the turnover.
                    self.pending_to = Destination::Active;
                }

                if self.pending_to == Destination::Deferred {
                    self.pending = std::ptr::null_mut();
                    self.pending_to = Destination::Active;
                    if inject {
                        checkpoint(9);
                    }
                    continue;
                }

                if self.pending_to == Destination::Free {
                    // No checkpoint lies inside ll_free. The pointer remains
                    // owned here through the pre-return checkpoints, including
                    // the interval after its two slot bits have been cleared.
                    unsafe {
                        crate::refcount::update_header_flags(self.pending, |flags| {
                            flags
                                & !(crate::refcount::CANDIDATE_BIT | crate::refcount::DEAD_IN_PLACE)
                        });
                    }
                    if inject {
                        checkpoint(2);
                    }
                    // Ownership crosses to ll_free at this call. A panic
                    // inside the allocator cannot be retried: it may already
                    // have returned the slot or unmapped the whole run.
                    let entity = self.pending;
                    self.pending = std::ptr::null_mut();
                    unsafe { crate::memory::stdapi::ll_free(entity.cast()) };
                } else if self.phase == Phase::Records {
                    if self.write_fill == SEGMENT_CAPACITY {
                        self.write = unsafe { (*self.write).next };
                        self.write_fill = 0;
                    }
                    if self.write_fill == 0 {
                        self.kept_segments += 1;
                    }
                    unsafe {
                        segment_entries(self.write)
                            .add(self.write_fill)
                            .write(self.pending)
                    };
                    note_queue_work(0, 0, 1);
                    self.write_fill += 1;
                } else {
                    unsafe {
                        overflow_entries(self.state)
                            .add(self.overflow_write)
                            .write(self.pending);
                    }
                    note_queue_work(0, 0, 1);
                    self.overflow_write += 1;
                }
                self.pending = std::ptr::null_mut();
                self.pending_to = Destination::Active;
                if inject {
                    checkpoint(3);
                }
                continue;
            }

            match self.phase {
                Phase::Records if !self.read.is_null() => {
                    if self.read_index == self.bound() {
                        self.read = unsafe { (*self.read).next };
                        self.read_index = 0;
                        continue;
                    }
                    let entry = unsafe { segment_entries(self.read).add(self.read_index).read() };
                    self.read_index += 1;
                    note_queue_work(0, 1, 0);
                    self.stage_entry(entry);
                    if inject {
                        checkpoint(1);
                    }
                }
                Phase::Records => self.phase = Phase::Overflow,
                Phase::Overflow if self.overflow_read < self.overflow_bound => {
                    let entry =
                        unsafe { overflow_entries(self.state).add(self.overflow_read).read() };
                    self.overflow_read += 1;
                    note_queue_work(0, 1, 0);
                    self.stage_entry(entry);
                    if inject {
                        checkpoint(1);
                    }
                }
                Phase::Overflow => {
                    if self.kept_segments == 0 {
                        self.surplus = self.head;
                        self.head = std::ptr::null_mut();
                    } else {
                        self.surplus = unsafe { (*self.write).next };
                        unsafe { (*self.write).next = std::ptr::null_mut() };
                    }
                    self.phase = Phase::Reverse;
                    if inject {
                        checkpoint(4);
                    }
                }
                Phase::Reverse if !self.head.is_null() => {
                    let segment = self.head;
                    self.head = unsafe { (*segment).next };
                    unsafe { (*segment).next = self.reversed_head };
                    self.reversed_head = segment;
                    if inject {
                        checkpoint(5);
                    }
                }
                Phase::Reverse => self.phase = Phase::Publish,
                Phase::Publish => {
                    // Nothing below may be repeated by this frame's drop. The
                    // queue is visible before the ledger updates, so a corrupt
                    // ledger reports once without losing the chain on unwind.
                    self.phase = Phase::ReturnSegments;
                    let owner_state = unsafe { owner_state_ref(self.state) };
                    owner_state.write_segment.set(self.reversed_head);
                    owner_state.write_len.set(stored_len(self.write_fill));
                    owner_state
                        .overflow_len
                        .set(stored_len(self.overflow_write));
                    self.reversed_head = std::ptr::null_mut();

                    // Each original interior was charged once; neither input
                    // head was charged. The output charges every kept segment
                    // except its final head, including exact-capacity output.
                    let charged_after = self.kept_segments.saturating_sub(1) + self.deferred_heads;
                    if charged_after < self.charged_segments {
                        gc_metadata::discharge(
                            (self.charged_segments - charged_after) * BLOCK_PAYLOAD,
                        );
                    } else {
                        gc_metadata::charge(
                            (charged_after - self.charged_segments) * BLOCK_PAYLOAD,
                        );
                    }
                    if inject {
                        checkpoint(8);
                    }
                    gc_metadata::discharge(
                        (self.overflow_bound - self.overflow_write) * size_of::<*mut RcHeader>(),
                    );
                    if inject {
                        checkpoint(6);
                    }
                }
                Phase::ReturnSegments if !self.surplus.is_null() => {
                    let segment = self.surplus;
                    self.surplus = unsafe { (*segment).next };
                    unsafe { (*segment).next = std::ptr::null_mut() };
                    let owner_state = unsafe { owner_state_ref(self.state) };
                    let count = owner_state.spare_count.get();
                    if usize::from(count) < SPARE_SEGMENTS {
                        owner_state.spares[usize::from(count)].set(segment);
                        owner_state.spare_count.set(count + 1);
                    } else {
                        gc_metadata::release_to_critical(segment);
                    }
                    if inject {
                        checkpoint(7);
                    }
                }
                Phase::ReturnSegments => self.phase = Phase::Done,
                Phase::Done => {}
            }
        }
    }

    /// Take one entry off the input and decide where it goes.
    ///
    /// **The mark comes off here and travels in the frame**, so nothing below
    /// this line reads a tagged pointer: the header the slot state is read
    /// through, the entry written into either lane, and the pointer handed to
    /// `ll_free` are all the entity's own address
    /// (`crate::cycle::queue::DEFERRED_MARK`).
    fn stage_entry(&mut self, entry: *mut RcHeader) {
        let marked = entry.addr() & DEFERRED_MARK != 0;
        let entity = entry.map_addr(|address| address & !DEFERRED_MARK);
        self.pending = entity;
        self.pending_to = if self.retire
            && matches!(
                unsafe { crate::refcount::slot_state_with_flags(entity) },
                crate::refcount::SlotStateReading::DeadInPlace { .. }
            ) {
            Destination::Free
        } else if marked {
            Destination::Deferred
        } else {
            Destination::Active
        };
    }

    /// Append the pending entry to the deferred lane, or answer **false**
    /// where no segment can be had for it and the active lane takes it
    /// instead.
    ///
    /// The head grows by one spare and the old head becomes interior, which
    /// keeps the invariant every reader of a lane rests on: the head carries
    /// the fill and every segment behind it is full. The reserve is not drawn
    /// here — a segment the deferred lane keeps is one the reserve does not
    /// get back, and a draw inside this frame's re-run would be the second
    /// panic the cleanup contract excludes (`rfc/model/gc/cycle/questions.md`,
    /// Y12 clause 8).
    fn append_to_deferred_lane(&mut self) -> bool {
        let owner_state = unsafe { owner_state_ref(self.state) };
        let mut head = owner_state.deferred_segment.get();
        let mut fill = usize::from(owner_state.deferred_len.get());
        if head.is_null() || fill == SEGMENT_CAPACITY {
            let fresh = take_spare(owner_state);
            if fresh.is_null() {
                return false;
            }

            if head.is_null() {
                // The oldest deferred record is what the re-offer's mirror is
                // about, so the count is taken where the lane starts.
                if let Some(at_commits) = self.deferred_at {
                    owner_state.turnover_mirror.set(at_commits);
                }
            } else {
                self.deferred_heads += 1;
            }

            unsafe { (*fresh).next = head };
            head = fresh;
            fill = 0;
            owner_state.deferred_segment.set(head);
        }

        unsafe { segment_entries(head).add(fill).write(self.pending) };
        note_queue_work(0, 0, 1);
        owner_state.deferred_len.set(stored_len(fill + 1));
        true
    }
}

impl Drop for Compaction {
    fn drop(&mut self) {
        // A valid queue makes this continuation non-panicking: the ledger
        // phase has advanced already, and ownership crosses to `ll_free`
        // before that call. What a corrupt one costs, and why the second panic
        // is not caught, is `dev/DECISIONS.md`, "corrupt queue entries remain
        // outside the cleanup recovery contract".
        self.run(false);
    }
}

#[cfg(test)]
thread_local! {
    static FAIL_AT: Cell<Option<usize>> = const { Cell::new(None) };
}

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
