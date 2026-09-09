//! Bounded, allocation-free combination and owner retirement.
//!
//! The input links remain intact while entries move towards their front.
//! Both original partial heads keep their own bounds. Once no reader remains,
//! the occupied prefix is reversed, making its last partial segment the head.
//! The fixed cleanup frame owns every cursor and pending return across unwind.

use super::*;

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
    partial: [*mut BlockHeader; 2],
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
    pending_return: bool,
    reverse: *mut BlockHeader,
    surplus: *mut BlockHeader,
    retire: bool,
    phase: Phase,
}

pub(super) fn finish(mut batch: InFlightBatch, retire: bool) {
    let state = owner_state();
    if state.is_null() {
        assert!(
            batch.is_empty(),
            "the queue base block left with a batch out"
        );
        return;
    }
    let q = unsafe { owner_state_ref(state) };
    let active = q.write_segment.get();
    if !retire && active.is_null() {
        q.write_segment.set(batch.head);
        q.write_len.set(batch.fill);
        batch.head = std::ptr::null_mut();
        return;
    }
    let mut pass = Compaction {
        state,
        partial: [active, batch.head],
        bounds: [q.write_len.get(), batch.fill],
        read: active,
        read_index: 0,
        write: active,
        write_fill: 0,
        head: active,
        kept_segments: 0,
        charged_segments: 0,
        overflow_read: 0,
        overflow_write: 0,
        overflow_bound: q.overflow_len.get(),
        pending: std::ptr::null_mut(),
        pending_return: false,
        reverse: std::ptr::null_mut(),
        surplus: std::ptr::null_mut(),
        retire,
        phase: Phase::Records,
    };

    // No fallible operation separates acquisition from joining the chains.
    // The frame owns both original bounds before either head becomes interior.
    for head in pass.partial {
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
    q.write_segment.set(std::ptr::null_mut());
    q.write_len.set(0);
    // The temporary queue is private until publication; overflow bounds are
    // owned here too. No entity code or allocation callback is run by packing.
    q.overflow_len.set(0);
    note_queue_work(1, 0, 0);
    gc_metadata::mark_peak((pass.bounds[0] + pass.bounds[1]) * size_of::<*mut RcHeader>());
    checkpoint(0);
    pass.run(true);
}

impl Compaction {
    fn bound(&self) -> usize {
        for index in 0..2 {
            if self.read == self.partial[index] {
                return self.bounds[index];
            }
        }
        SEGMENT_CAPACITY
    }

    fn run(&mut self, inject: bool) {
        while self.phase != Phase::Done {
            if !self.pending.is_null() {
                if self.pending_return {
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
                self.pending_return = false;
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
                    self.take(entry);
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
                    self.take(entry);
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
                    unsafe { (*segment).next = self.reverse };
                    self.reverse = segment;
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
                    let q = unsafe { owner_state_ref(self.state) };
                    q.write_segment.set(self.reverse);
                    q.write_len.set(self.write_fill);
                    q.overflow_len.set(self.overflow_write);
                    self.reverse = std::ptr::null_mut();

                    // Each original interior was charged once; neither input
                    // head was charged. The output charges every kept segment
                    // except its final head, including exact-capacity output.
                    let charged_after = self.kept_segments.saturating_sub(1);
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
                    let q = unsafe { owner_state_ref(self.state) };
                    let count = q.spare_count.get();
                    if count < SPARE_SEGMENTS {
                        q.spares[count].set(segment);
                        q.spare_count.set(count + 1);
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

    fn take(&mut self, entry: *mut RcHeader) {
        self.pending = entry;
        self.pending_return = self.retire
            && matches!(
                unsafe { crate::refcount::slot_state_with_flags(entry) },
                crate::refcount::SlotStateReading::DeadInPlace { .. }
            );
    }
}

impl Drop for Compaction {
    fn drop(&mut self) {
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
