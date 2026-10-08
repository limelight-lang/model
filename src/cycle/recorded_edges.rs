//! The edges a collector's mark subtracted, recorded so that its scan reads
//! them and not the heap (`dev/design/recycler-over-counts.md`, §3).
//!
//! **Why the scan may not read the heap here.** The default build's scan
//! re-reads every expanded entity's cells, and under a concurrent mutator
//! those cells are the heap of the scan's instant, not the mark's. An edge the
//! mark subtracted and the mutator then moved out of a live holder — a
//! count-free move, which tags the holder and not the entity moved — would be
//! missing from the scan's reading, and the entity it named would read
//! unreachable on a count the mark had already lowered. Under the feature a
//! set the scan colours unreachable is freed by the collector's reading
//! alone, so the scan spreads the live colour over the recorded edges only:
//! what the mark subtracted is exactly what the scan may credit back.
//!
//! **The record.** One word per entry, in the order the mark wrote them: a
//! *run header* — the expanded entity's row, its low bit set — at the start
//! of each expansion, then one *edge* — the target's row — per subtraction the
//! expansion made. Rows are four-byte aligned, so the low bit is free. A run
//! ends where the next header begins or the record does.
//!
//! **Random access by index, in segments the arena grants.** The scan reaches
//! a run from the row that heads it: after the scan's first pass the row's
//! working count has answered its only question and its thirty bits take the
//! run's index instead (`crate::cycle::scan::scan_the_recorded_edges`). An
//! index is found through three levels — a top table of directory pages, each
//! page a table of segment addresses, each segment a run of entries, every one
//! of them a grant of the arena's bump, the top table granted again wider as
//! the record grows — so no grant is larger than 32 KiB, and the record
//! refuses only where a row's thirty bits could no longer index it.
//!
//! **The record allocates nothing itself**, as [`crate::cycle::records`] does
//! not: the arena hands it each segment and page
//! ([`crate::cycle::arena::TraceScratchArena::open_run`]), and it lives
//! exactly as long as the arena's bump.

/// Entries a segment holds: 4 KiB of words. Small, so that the first run of
/// a small trace asks the bump for a little over 8 KiB in all — top table,
/// page and segment — which the thread's workspace serves without a block
/// drawn.
pub(crate) const SEGMENT_ENTRIES: usize = 512;
/// Segment addresses a directory page holds: 4 KiB of pointers.
pub(crate) const PAGE_SEGMENTS: usize = 512;
/// Directory pages the first top table holds: 64 bytes, two million entries.
/// The table is granted at the first entry like the rest, so that the arena
/// carries a pointer and not a table — an arena stands on the collector's
/// stack — and is granted again eight times wider, its pointers copied, each
/// time the record outgrows it.
pub(crate) const FIRST_PAGES: usize = 8;
/// Directory pages the widest top table holds: 32 KiB of pointers.
const MAX_PAGES: usize = 4096;
/// Entries the record holds before it refuses: the most a row's payload can
/// index below the split's mark (`crate::cycle::shadow::SPLIT_MARK`), with one
/// to spare for "no run", four gibibytes of record — the row's own bound, not
/// one of the record's.
pub(crate) const MAX_ENTRIES: usize = crate::cycle::shadow::SPLIT_MARK as usize - 2;

const _: () = assert!(MAX_PAGES * PAGE_SEGMENTS * SEGMENT_ENTRIES >= MAX_ENTRIES);

/// The low bit of an entry that heads a run.
pub(crate) const RUN: u64 = 1;

/// The record of one collector's mark.
pub(crate) struct RecordedEdges {
    /// The top table of directory pages, null until the first entry.
    pages: *mut *mut *mut u64,
    /// Pages the top table has room for.
    top_capacity: usize,
    /// Entries written.
    len: usize,
    /// Where the next entry goes in the segment attached last, null before
    /// the first.
    cursor: *mut u64,
    /// Entries the record takes at [`Self::cursor`] before an append needs a
    /// segment or refuses.
    room: usize,
}

/// What [`RecordedEdges::room_for_the_next`] says the next append needs
/// first.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Room {
    /// The current segment has room.
    Ready,
    /// A new segment, its directory page standing.
    Segment,
    /// A new directory page, then a new segment.
    PageAndSegment,
    /// A top table of `capacity` pages — the first, or one wider than the
    /// table standing — then a directory page and a segment.
    TopPageAndSegment { capacity: usize },
    /// The record is full.
    Full,
}

impl RecordedEdges {
    pub(crate) const fn new() -> Self {
        Self {
            pages: std::ptr::null_mut(),
            top_capacity: 0,
            len: 0,
            cursor: std::ptr::null_mut(),
            room: 0,
        }
    }

    /// Entries written.
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Forget every entry. The memory is the arena's, which rewinds it.
    pub(crate) fn clear(&mut self) {
        self.pages = std::ptr::null_mut();
        self.top_capacity = 0;
        self.len = 0;
        self.cursor = std::ptr::null_mut();
        self.room = 0;
    }

    /// What the next append needs before it can be written.
    #[inline]
    pub(crate) fn room_for_the_next(&self) -> Room {
        let segment = self.len / SEGMENT_ENTRIES;
        if self.len >= MAX_ENTRIES {
            Room::Full
        } else if self.len % SEGMENT_ENTRIES != 0 {
            Room::Ready
        } else if segment % PAGE_SEGMENTS != 0 {
            Room::Segment
        } else if segment / PAGE_SEGMENTS < self.top_capacity {
            Room::PageAndSegment
        } else {
            Room::TopPageAndSegment {
                capacity: (self.top_capacity * 8).clamp(FIRST_PAGES, MAX_PAGES),
            }
        }
    }

    /// Attach a top table of `capacity` pages, copying the standing one's
    /// pages into it; the old table is the arena's bump, rewound with it.
    ///
    /// # Safety
    /// `top` addresses `capacity` pointers of the arena's bump, and
    /// [`Self::room_for_the_next`] answered [`Room::TopPageAndSegment`] with
    /// that capacity.
    pub(crate) unsafe fn attach_top(&mut self, top: *mut *mut *mut u64, capacity: usize) {
        if !self.pages.is_null() {
            unsafe { std::ptr::copy_nonoverlapping(self.pages, top, self.top_capacity) };
        }
        self.pages = top;
        self.top_capacity = capacity;
    }

    /// Attach a directory page for the segments the next append opens.
    ///
    /// # Safety
    /// `page` addresses `PAGE_SEGMENTS` pointers of the arena's bump, and
    /// [`Self::room_for_the_next`] answered [`Room::PageAndSegment`] or
    /// [`Room::TopPageAndSegment`], the top table attached.
    pub(crate) unsafe fn attach_page(&mut self, page: *mut *mut u64) {
        let segment = self.len / SEGMENT_ENTRIES;
        unsafe { *self.pages.add(segment / PAGE_SEGMENTS) = page };
    }

    /// Attach the segment the next append writes into.
    ///
    /// # Safety
    /// `segment` addresses `SEGMENT_ENTRIES` words of the arena's bump, the
    /// directory page covering it stands, and the entries written fill whole
    /// segments: the next append writes the new segment's first word.
    pub(crate) unsafe fn attach_segment(&mut self, segment: *mut u64) {
        debug_assert_eq!(
            self.len % SEGMENT_ENTRIES,
            0,
            "a segment opens at its boundary"
        );
        let index = self.len / SEGMENT_ENTRIES;
        unsafe { *(*self.pages.add(index / PAGE_SEGMENTS)).add(index % PAGE_SEGMENTS) = segment };
        self.cursor = segment;
        self.room = SEGMENT_ENTRIES.min(MAX_ENTRIES - self.len);
    }

    /// Append `entry` into the segment attached last, or answer **false**
    /// when [`Self::room`] is spent. `room` is the only authority on whether
    /// an append can be written: it is above zero exactly when a segment is
    /// attached at the record's end and the bound is not reached, and
    /// [`Self::room_for_the_next`] serves only to name the grant a refused
    /// append asks for. Between [`Self::attach_segment`] and the first
    /// write the two read differently, the arithmetic still asking for the
    /// segment just attached.
    #[inline]
    pub(crate) fn push_into_current(&mut self, entry: u64) -> bool {
        if self.room == 0 {
            return false;
        }

        unsafe {
            self.cursor.write(entry);
            self.cursor = self.cursor.add(1);
        }
        self.room -= 1;
        self.len += 1;
        true
    }

    /// The entry at `index`.
    ///
    /// # Safety
    /// `index` is below [`Self::len`].
    #[inline]
    pub(crate) unsafe fn entry(&self, index: usize) -> u64 {
        debug_assert!(index < self.len);
        unsafe { self.slot(index).read() }
    }

    #[inline]
    fn slot(&self, index: usize) -> *mut u64 {
        let segment = index / SEGMENT_ENTRIES;
        unsafe {
            (*(*self.pages.add(segment / PAGE_SEGMENTS)).add(segment % PAGE_SEGMENTS))
                .add(index % SEGMENT_ENTRIES)
        }
    }
}

#[cfg(test)]
mod tests;
