//! A chain of fixed-size records over segments its owner supplies.
//!
//! Two collection structures hold an unbounded number of small records and
//! know a bound on neither: the trace's worklist, whose depth is the traced
//! subgraph's, and the deferred drops of a teardown, whose length is the
//! component's external children. A fixed array would abort a collection the
//! memory could serve, and a growing vector would own an allocation this crate
//! refuses the collector (`PLAN.md`, S36.11). So the records are held in
//! segments, each one a header line and the records behind it, threaded both
//! ways.
//!
//! **The chain allocates nothing.** A caller hands it a region and the
//! capacity that region holds, and where the region came from is that caller's
//! subject: the worklist takes every segment out of the collection arena's
//! bump, one at the first push ([`crate::cycle::arena`],
//! [`crate::cycle::stack`]). That is what lets one chain serve users whose
//! memory comes from different places, and it is why [`RecordChain::push`]
//! reports a full append position rather than growing. A user that cannot act
//! on a refusal draws its regions ahead of the first push instead, against
//! [`RecordChain::room`] ([`crate::cycle::reclamation`]).
//!
//! **Each segment carries its own capacity**, which a boundary crossing reads
//! instead of a constant, so a chain of unequal segments hands its records
//! back exactly. Each of the two users attaches segments of one size of its
//! own, so no chain of unequal ones stands today.
//!
//! Two access orders, one per user. [`RecordChain::pop`] takes the newest
//! record, which is what a descent needs, and [`RecordChain::drain`] hands
//! every record over oldest first, which is what a replay in the order the
//! records were written needs — the deferred drops of a cycle teardown, whose
//! children are dropped in the order the sever displaced them
//! ([`crate::cycle::reclamation`]).
//!
//! **The records are `Copy` and no drop glue runs over them.** A segment is
//! raw memory the owner rewinds or releases whole, so a record whose death
//! meant something would die unobserved.

use std::cell::Cell;

/// Bytes a segment spends on its header before its first record.
///
/// A whole line, so that a page of entries fits behind it exactly: the
/// worklist's segment is 4,160 bytes and its records 4,096
/// (`crate::cycle::stack`). The header is written when the segment is attached
/// and read at every boundary crossing.
pub(crate) const SEGMENT_HEADER_BYTES: usize = 64;

/// The header of one segment: its place in the chain, and how many records
/// follow it.
///
/// `previous` is null in the base segment and `next` in the newest one. Both
/// are [`Cell`]s because the chain's own append takes `&self`, which is what
/// let a user reach one from a raw pointer without a borrow of its own.
#[repr(C)]
struct Segment {
    previous: Cell<*mut Segment>,
    next: Cell<*mut Segment>,
    /// Records the region behind this header holds. Fixed when the segment is
    /// attached, and what the pop reads to size it.
    capacity: usize,
    _line: [u8; SEGMENT_HEADER_BYTES - 3 * size_of::<usize>()],
}

const _: () = assert!(size_of::<Segment>() == SEGMENT_HEADER_BYTES);

impl Segment {
    /// Write a segment header at `region`, over `capacity` records, below
    /// `previous`.
    ///
    /// # Safety
    /// `region` addresses `SEGMENT_HEADER_BYTES + capacity * size_of::<T>()`
    /// writable bytes that no other segment claims, and it is aligned for a
    /// pointer.
    unsafe fn write_header(
        region: *mut u8,
        capacity: usize,
        previous: *mut Segment,
    ) -> *mut Segment {
        let segment = region as *mut Segment;

        // Field by field rather than by assignment: the region is memory with
        // no value in it, so an assignment would drop a `Segment` that was
        // never constructed. The padding stays as the region handed it over.
        unsafe {
            (&raw mut (*segment).previous).write(Cell::new(previous));
            (&raw mut (*segment).next).write(Cell::new(std::ptr::null_mut()));
            (&raw mut (*segment).capacity).write(capacity);
        }

        segment
    }

    /// The first record position of `segment`.
    ///
    /// # Safety
    /// `segment` is a header a chain wrote, and `T` is that chain's record.
    unsafe fn records<T>(segment: *mut Segment) -> *mut T {
        unsafe { (segment as *mut u8).add(SEGMENT_HEADER_BYTES) as *mut T }
    }
}

/// A chain of records of one collection structure.
///
/// The base segment is the chain's first and is never released by it: a chain
/// is built over a region its owner already holds, and only the segments
/// attached past that region are the owner's to give back.
///
/// **No `Drop`.** The records are `Copy` and every segment is memory the owner
/// rewinds or releases; a chain forgotten rather than dropped costs nothing.
pub(crate) struct RecordChain<T: Copy> {
    /// Where the next record goes.
    cursor: Cell<*mut T>,
    /// One past the last record position of the segment the cursor is inside,
    /// which is what the push tests against.
    limit: Cell<*mut T>,
    /// The segment the cursor is inside.
    current: Cell<*mut Segment>,
    /// The first segment, whose region is the owner's and outlives the chain.
    base: *mut Segment,
}

impl<T: Copy> RecordChain<T> {
    /// An empty chain over `region`, which holds `capacity` records behind its
    /// header line.
    ///
    /// # Safety
    /// `region` addresses `SEGMENT_HEADER_BYTES + capacity * size_of::<T>()`
    /// writable bytes, aligned for `T` and for a pointer, and stays this
    /// chain's for as long as the chain is used.
    pub(crate) unsafe fn over(region: *mut u8, capacity: usize) -> Self {
        let base = unsafe { Segment::write_header(region, capacity, std::ptr::null_mut()) };
        let records = unsafe { Segment::records::<T>(base) };

        Self {
            cursor: Cell::new(records),
            limit: Cell::new(unsafe { records.add(capacity) }),
            current: Cell::new(base),
            base,
        }
    }

    /// Add `record`, or answer **false** when the append position is full —
    /// which is the caller's signal to advance the chain and try once more.
    pub(crate) fn push(&self, record: T) -> bool {
        let cursor = self.cursor.get();
        if cursor == self.limit.get() {
            return false;
        }

        unsafe {
            cursor.write(record);
            self.cursor.set(cursor.add(1));
        }

        true
    }

    /// The newest record, or `None` when the chain is empty.
    ///
    /// A segment emptied by a pop is kept rather than dropped, so a depth that
    /// oscillates across a boundary reuses it through
    /// [`advance_to_kept`](Self::advance_to_kept).
    pub(crate) fn pop(&self) -> Option<T> {
        let current = self.current.get();
        let mut cursor = self.cursor.get();

        if cursor == unsafe { Segment::records::<T>(current) } {
            let previous = unsafe { (*current).previous.get() };
            if previous.is_null() {
                return None;
            }

            // The segment below the current one is full: a chain advances only
            // when its append position has no room left.
            cursor = unsafe { Segment::records::<T>(previous).add((*previous).capacity) };
            self.current.set(previous);
            self.limit.set(cursor);
        }

        let cursor = unsafe { cursor.sub(1) };
        self.cursor.set(cursor);
        Some(unsafe { cursor.read() })
    }

    /// Move the append position onto the segment an earlier crossing left
    /// above the current one, or answer **false** when there is none and the
    /// caller owes a region.
    pub(crate) fn advance_to_kept(&self) -> bool {
        let kept = unsafe { (*self.current.get()).next.get() };
        if kept.is_null() {
            return false;
        }

        self.open(kept);
        true
    }

    /// Attach `region` as a new segment above the current one and make it the
    /// append position.
    ///
    /// # Safety
    /// As [`over`](Self::over), and the current segment has no segment above
    /// it — which is what [`advance_to_kept`](Self::advance_to_kept) answering
    /// false reports.
    pub(crate) unsafe fn extend(&self, region: *mut u8, capacity: usize) {
        let current = self.current.get();
        // The link this overwrites would otherwise be the only path to a
        // segment an earlier crossing left above the current one: the chain
        // would answer every push and pop correctly while its owner had no
        // way left to reach that segment, which is a leaked region. A chain
        // that pops owes [`advance_to_kept`](Self::advance_to_kept) before it
        // comes here.
        debug_assert!(
            unsafe { (*current).next.get() }.is_null(),
            "a segment already stands above the one being extended"
        );
        let segment = unsafe { Segment::write_header(region, capacity, current) };
        unsafe { (*current).next.set(segment) };
        self.open(segment);
    }

    /// Records the chain takes before it owes another region: the room left in
    /// the append position, plus the whole of every segment an earlier crossing
    /// left above it.
    ///
    /// A user that measures its walk against this figure before the walk starts
    /// makes every push of that walk unfailing, which is what a caller writing
    /// into memory it cannot restore needs ([`crate::cycle::reclamation`]). It
    /// counts nothing below the append position: those segments are full, and a
    /// chain fills and empties from the same end.
    pub(crate) fn room(&self) -> usize {
        let mut room = (self.limit.get() as usize - self.cursor.get() as usize) / size_of::<T>();
        let mut segment = unsafe { (*self.current.get()).next.get() };
        while !segment.is_null() {
            room += unsafe { (*segment).capacity };
            segment = unsafe { (*segment).next.get() };
        }

        room
    }

    /// Attach `region` above the newest segment, leaving the append position
    /// where it is.
    ///
    /// [`extend`](Self::extend) is the same act for a user that has just filled
    /// its append position: it attaches above the current segment and opens it.
    /// This one serves a reservation taken before the first push, where
    /// segments an earlier walk emptied already stand above the append position
    /// and the new region belongs behind them.
    ///
    /// # Safety
    /// As [`over`](Self::over).
    pub(crate) unsafe fn attach(&self, region: *mut u8, capacity: usize) {
        let mut top = self.current.get();
        loop {
            let above = unsafe { (*top).next.get() };
            if above.is_null() {
                break;
            }

            top = above;
        }

        let segment = unsafe { Segment::write_header(region, capacity, top) };
        unsafe { (*top).next.set(segment) };
    }

    /// Hand every record to `visit` oldest first and leave the chain empty over
    /// the segments it holds.
    ///
    /// The order is the append order, which is what a user replaying an act in
    /// the order it was performed needs; [`pop`](Self::pop) is the other
    /// direction and serves a descent. Every segment below the append position
    /// is full — the chain fills one and advances — so the count each one hands
    /// over is its own capacity, and the append position hands over what stands
    /// below its cursor.
    ///
    /// **The chain is emptied before the first visit runs**, over the bounds
    /// this call read, so an unwind out of a visit leaves records nobody drops
    /// rather than records a second drain hands out again. `visit` may not
    /// reach this chain for the same reason: a push from inside would write
    /// into a record this walk has not read yet.
    pub(crate) fn drain(&self, mut visit: impl FnMut(T)) {
        let last = self.current.get();
        let filled_in_last = (self.cursor.get() as usize
            - unsafe { Segment::records::<T>(last) } as usize)
            / size_of::<T>();
        self.open(self.base);

        let mut segment = self.base;
        loop {
            let records = unsafe { Segment::records::<T>(segment) };
            let filled = if segment == last {
                filled_in_last
            } else {
                unsafe { (*segment).capacity }
            };

            for i in 0..filled {
                visit(unsafe { records.add(i).read() });
            }

            if segment == last {
                break;
            }

            segment = unsafe { (*segment).next.get() };
        }
    }

    /// Read records from the newest down, stopping where `visit` answers
    /// false, and leave the chain as it was.
    ///
    /// The reading a pop cannot give: a caller that must know something about
    /// a run of records before it takes them out — the run's minimum, its
    /// length — would otherwise pop them into a second structure and pay a
    /// region for it. **Nothing here allocates**, so a run read by this call
    /// can be popped whole afterwards with no refusal in the middle.
    ///
    /// `visit` may not reach this chain: it holds the cursor of a walk that
    /// has not finished.
    pub(crate) fn for_each_from_top(&self, mut visit: impl FnMut(&T) -> bool) {
        let mut segment = self.current.get();
        let mut cursor = self.cursor.get();
        loop {
            let records = unsafe { Segment::records::<T>(segment) };
            while cursor > records {
                cursor = unsafe { cursor.sub(1) };
                if !visit(unsafe { &*cursor }) {
                    return;
                }
            }

            // The segment below is full, as it is for the pop: a chain
            // advances only when its append position has no room left.
            let previous = unsafe { (*segment).previous.get() };
            if previous.is_null() {
                return;
            }

            segment = previous;
            cursor = unsafe { Segment::records::<T>(previous).add((*previous).capacity) };
        }
    }

    /// Whether the chain holds no record.
    pub(crate) fn is_empty(&self) -> bool {
        self.current.get() == self.base
            && self.cursor.get() == unsafe { Segment::records::<T>(self.base) }
    }

    /// Make `segment` the append position, empty.
    fn open(&self, segment: *mut Segment) {
        let records = unsafe { Segment::records::<T>(segment) };
        self.current.set(segment);
        self.cursor.set(records);
        self.limit.set(unsafe { records.add((*segment).capacity) });
    }

    /// Segments the chain holds, the base and the emptied ones included. Tests
    /// only, and the instrument for the one defect the records cannot show: a
    /// chain that abandoned an emptied segment answers every push and pop
    /// correctly while spending a region per boundary crossing.
    #[cfg(test)]
    pub(crate) fn segment_count(&self) -> usize {
        let mut count = 0;
        let mut segment = self.base;
        while !segment.is_null() {
            count += 1;
            segment = unsafe { (*segment).next.get() };
        }

        count
    }
}

/// A [`RecordChain`] whose first region is drawn at the first push rather than
/// at its birth, and whose segments are its owner's to give back.
///
/// Both users of the chain want this and neither wants it differently: a trace
/// that queues no entity and a teardown that displaces no child each pay for no
/// segment, and both hold their chain inside the arena whose bump the segments
/// come from, so [`rewind`](Self::rewind) is what the arena owes the instant it
/// hands those blocks back. What differs is only the verb each user needs — a
/// descent takes [`pop`](Self::pop) and grows through
/// [`extend`](Self::extend), a replay takes [`drain`](Self::drain) and reserves
/// through [`attach`](Self::attach) — and both verbs stand here rather than in
/// two wrappers that would each restate `new`, `is_empty` and `rewind`
/// (`crate::cycle::stack`, `crate::cycle::drops`).
pub(crate) struct LazyChain<T: Copy> {
    /// The chain, or `None` until a region has been taken.
    records: Option<RecordChain<T>>,
}

impl<T: Copy> LazyChain<T> {
    /// An empty chain holding no region.
    pub(crate) fn new() -> Self {
        Self { records: None }
    }

    /// Add `record` to the segment being filled, or answer **false** when
    /// there is no room and the caller owes an advance or a region.
    pub(crate) fn push_into_current(&mut self, record: T) -> bool {
        self.records
            .as_ref()
            .is_some_and(|chain| chain.push(record))
    }

    /// Move the append position onto a segment an earlier crossing left above
    /// the current one, or answer **false** when there is none.
    pub(crate) fn advance_to_kept(&mut self) -> bool {
        self.records
            .as_ref()
            .is_some_and(RecordChain::advance_to_kept)
    }

    /// The newest record, or `None` when the chain holds none.
    pub(crate) fn pop(&mut self) -> Option<T> {
        self.records.as_ref().and_then(RecordChain::pop)
    }

    /// Hand every record over oldest first and leave the chain empty over the
    /// segments it holds ([`RecordChain::drain`]).
    pub(crate) fn drain(&mut self, visit: impl FnMut(T)) {
        if let Some(chain) = self.records.as_ref() {
            chain.drain(visit);
        }
    }

    /// Records the chain takes before it owes another region
    /// ([`RecordChain::room`]).
    pub(crate) fn room(&self) -> usize {
        self.records.as_ref().map_or(0, RecordChain::room)
    }

    /// Take `region` as the segment being filled — the first one, or one above
    /// the current — which is what a user that has just filled its append
    /// position needs ([`RecordChain::extend`]).
    ///
    /// # Safety
    /// As [`RecordChain::over`], and the region is the owner's for as long as
    /// the chain is used.
    pub(crate) unsafe fn extend(&mut self, region: *mut u8, capacity: usize) {
        match self.records.as_ref() {
            Some(chain) => unsafe { chain.extend(region, capacity) },
            None => self.records = Some(unsafe { RecordChain::over(region, capacity) }),
        }
    }

    /// Take `region` as one more segment behind those the chain holds, leaving
    /// the append position where it is, which is what a reservation taken
    /// before the first push needs ([`RecordChain::attach`]).
    ///
    /// # Safety
    /// As [`extend`](Self::extend).
    pub(crate) unsafe fn attach(&mut self, region: *mut u8, capacity: usize) {
        match self.records.as_ref() {
            Some(chain) => unsafe { chain.attach(region, capacity) },
            None => self.records = Some(unsafe { RecordChain::over(region, capacity) }),
        }
    }

    /// Read records from the newest down without taking them out
    /// ([`RecordChain::for_each_from_top`]).
    pub(crate) fn for_each_from_top(&self, visit: impl FnMut(&T) -> bool) {
        if let Some(chain) = self.records.as_ref() {
            chain.for_each_from_top(visit);
        }
    }

    /// Whether the chain holds no record.
    pub(crate) fn is_empty(&self) -> bool {
        self.records.as_ref().is_none_or(RecordChain::is_empty)
    }

    /// Forget every segment, which the owner owes the instant it gives those
    /// blocks back. Nothing is freed here: the memory is the owner's.
    pub(crate) fn rewind(&mut self) {
        self.records = None;
    }

    /// Segments drawn, emptied ones included. Tests only, and the instrument
    /// for the one defect the records cannot show: a chain that abandoned an
    /// emptied segment answers every push and pop correctly while spending a
    /// region per boundary crossing.
    #[cfg(test)]
    pub(crate) fn segment_count(&self) -> usize {
        self.records.as_ref().map_or(0, RecordChain::segment_count)
    }
}

#[cfg(test)]
mod tests;
