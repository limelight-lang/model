//! A chain of fixed-size records over segments its owner supplies.
//!
//! One collection structure holds an unbounded number of small records and
//! knows a bound on neither: the trace's worklist, whose depth is the traced
//! subgraph's. A fixed array would abort a collection the memory could serve,
//! and a growing vector would own an allocation this crate refuses the
//! collector (`PLAN.md`, S36.11). So the records are held in segments, each
//! one a header line and the records behind it, threaded both ways.
//!
//! **The chain allocates nothing.** A caller hands it a region and the
//! capacity that region holds, and where the region came from is that caller's
//! subject: the worklist's base is the thread's workspace and its overflow is
//! the collection arena's bump ([`crate::cycle::arena`]). That is what lets
//! one chain serve users whose memory comes from different places, and it is
//! why [`RecordChain::push`] reports a full append position rather than
//! growing.
//!
//! **Segments differ in capacity and each one carries its own**, because a
//! base region sized to the common case sits beside overflow segments sized to
//! the block that funds them. A boundary crossing reads that number rather
//! than a constant, so a chain of unequal segments hands its records back
//! exactly.
//!
//! [`RecordChain::pop`] takes the newest record, which is the order a descent
//! needs. A second order, the oldest-first read a replay needs, is what
//! S36.11 adds when the withheld returns move onto this chain.
//!
//! **The records are `Copy` and no drop glue runs over them.** A segment is
//! raw memory the owner rewinds or releases whole, so a record whose death
//! meant something would die unobserved.

use std::cell::Cell;

/// Bytes a segment spends on its header before its first record.
///
/// A whole line, so that the record count of a segment funded by one 64 KiB
/// block divides evenly and the fixed regions of the workspace can be laid out
/// in lines (`crate::cycle::arena`). The header is written when the segment is
/// attached and read at every boundary crossing.
pub(crate) const SEGMENT_HEADER_BYTES: usize = 64;

/// The header of one segment: its place in the chain, and how many records
/// follow it.
///
/// `previous` is null in the base segment and `next` in the newest one. Both
/// are [`Cell`]s because the chain appends through a shared reference: the
/// withheld returns reach theirs from a raw pointer on the free path
/// (`crate::cycle::deferred_slot_reuse`).
#[repr(C)]
struct Segment {
    previous: Cell<*mut Segment>,
    next: Cell<*mut Segment>,
    /// Records the region behind this header holds. Fixed when the segment is
    /// attached, and what the pop and the walk read to size it.
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
        let segment = unsafe { Segment::write_header(region, capacity, current) };
        unsafe { (*current).next.set(segment) };
        self.open(segment);
    }

    /// Whether the chain holds no record.
    pub(crate) fn is_empty(&self) -> bool {
        self.current.get() == self.base
            && self.cursor.get() == unsafe { Segment::records::<T>(self.base) }
    }

    /// Empty the chain and forget every segment past the base, which the owner
    /// owes the instant those segments' memory goes back.
    ///
    /// Nothing is freed here and nothing can be: every region is the owner's.
    /// What this undoes is the chain's own belief that it has segments to
    /// advance into.
    pub(crate) fn rewind(&self) {
        unsafe { (*self.base).next.set(std::ptr::null_mut()) };
        self.open(self.base);
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
