//! The record's own arithmetic: where an append needs a page or a segment,
//! that entries read back across both boundaries, and where it refuses.

use super::*;

/// Grants for a record that crosses a segment boundary, out of test memory
/// standing in for the arena's bump.
struct Bump {
    top: Vec<Box<[*mut *mut u64]>>,
    pages: Vec<Box<[*mut u64]>>,
    segments: Vec<Box<[u64]>>,
}

impl Bump {
    fn append(&mut self, record: &mut RecordedEdges, entry: u64) {
        match record.room_for_the_next() {
            Room::Ready => {}
            Room::TopPageAndSegment { capacity } => {
                self.top
                    .push(vec![std::ptr::null_mut(); capacity].into_boxed_slice());
                unsafe { record.attach_top(self.top.last_mut().unwrap().as_mut_ptr(), capacity) };
                self.page(record);
            }
            Room::PageAndSegment => self.page(record),
            Room::Segment => self.segment(record),
            Room::Full => panic!("the record refused below its bound"),
        }
        assert!(record.push_into_current(entry));
    }

    fn page(&mut self, record: &mut RecordedEdges) {
        self.pages
            .push(vec![std::ptr::null_mut(); PAGE_SEGMENTS].into_boxed_slice());
        unsafe { record.attach_page(self.pages.last_mut().unwrap().as_mut_ptr()) };
        self.segment(record);
    }

    fn segment(&mut self, record: &mut RecordedEdges) {
        self.segments
            .push(vec![0; SEGMENT_ENTRIES].into_boxed_slice());
        unsafe { record.attach_segment(self.segments.last_mut().unwrap().as_mut_ptr()) };
    }
}

/// The first append asks for the top table, a page and a segment, the next
/// ones for nothing until the segment is full, a full page for a page, and a
/// full top table for a wider one.
#[test]
fn an_append_asks_for_a_page_and_a_segment_at_their_boundaries() {
    let mut record = RecordedEdges::new();
    assert_eq!(
        record.room_for_the_next(),
        Room::TopPageAndSegment {
            capacity: FIRST_PAGES
        }
    );

    record.top_capacity = FIRST_PAGES;
    record.len = 1;
    assert_eq!(record.room_for_the_next(), Room::Ready);
    record.len = SEGMENT_ENTRIES;
    assert_eq!(record.room_for_the_next(), Room::Segment);
    record.len = SEGMENT_ENTRIES * PAGE_SEGMENTS;
    assert_eq!(record.room_for_the_next(), Room::PageAndSegment);
    record.len = SEGMENT_ENTRIES * PAGE_SEGMENTS * FIRST_PAGES;
    assert_eq!(
        record.room_for_the_next(),
        Room::TopPageAndSegment {
            capacity: 8 * FIRST_PAGES
        },
        "a full top table is granted again, wider"
    );
    record.len = MAX_ENTRIES;
    assert_eq!(
        record.room_for_the_next(),
        Room::Full,
        "the record refuses where a row's thirty bits could not index it"
    );
}

/// Entries read back in order across every boundary: segments, a page, and
/// the top table granted again wider with the first one's pages copied in.
#[test]
fn entries_read_back_across_every_boundary() {
    let mut record = RecordedEdges::new();
    let mut bump = Bump {
        top: Vec::new(),
        pages: Vec::new(),
        segments: Vec::new(),
    };
    let entries = FIRST_PAGES * PAGE_SEGMENTS * SEGMENT_ENTRIES + SEGMENT_ENTRIES + 3;
    for index in 0..entries {
        bump.append(&mut record, (index as u64) << 2 | (index % 5 == 0) as u64);
    }

    assert_eq!(record.len(), entries);
    assert_eq!(bump.segments.len(), FIRST_PAGES * PAGE_SEGMENTS + 2);
    assert_eq!(bump.top.len(), 2, "the top table was granted again once");
    for index in 0..entries {
        assert_eq!(
            unsafe { record.entry(index) },
            (index as u64) << 2 | (index % 5 == 0) as u64
        );
    }

    record.clear();
    assert_eq!(record.len(), 0);
    assert_eq!(
        record.room_for_the_next(),
        Room::TopPageAndSegment {
            capacity: FIRST_PAGES
        }
    );
}

/// An append into the segment attached last stops at the record's bound and
/// not only at the segment's end: the last segment holds fewer entries than
/// a segment's room, and the one past the bound refuses mid-segment, as the
/// arithmetic would.
#[test]
fn an_append_into_the_current_segment_stops_at_the_bound() {
    let mut record = RecordedEdges::new();
    let mut top: Vec<*mut *mut u64> = vec![std::ptr::null_mut(); MAX_PAGES];
    let mut page: Vec<*mut u64> = vec![std::ptr::null_mut(); PAGE_SEGMENTS];
    let mut segment = vec![0u64; SEGMENT_ENTRIES];
    let last = MAX_ENTRIES / SEGMENT_ENTRIES * SEGMENT_ENTRIES;
    assert!(last < MAX_ENTRIES, "the bound falls inside a segment");
    record.len = last;
    top[last / SEGMENT_ENTRIES / PAGE_SEGMENTS] = page.as_mut_ptr();
    unsafe { record.attach_top(top.as_mut_ptr(), MAX_PAGES) };
    assert_eq!(record.room_for_the_next(), Room::Segment);
    unsafe { record.attach_segment(segment.as_mut_ptr()) };

    for entry in last..MAX_ENTRIES {
        assert!(record.push_into_current((entry as u64) << 2));
    }
    assert!(
        !record.push_into_current(0),
        "the record took an entry past its bound"
    );
    assert_eq!(record.room_for_the_next(), Room::Full);
    assert_eq!(record.len(), MAX_ENTRIES);
    assert_eq!(
        unsafe { record.entry(MAX_ENTRIES - 1) },
        ((MAX_ENTRIES - 1) as u64) << 2
    );
}

/// Before any segment and at a segment's end the fast append refuses, so the
/// arena's slow path asks for the grant the arithmetic names.
#[test]
fn an_append_into_the_current_segment_refuses_at_its_end() {
    let mut record = RecordedEdges::new();
    assert!(!record.push_into_current(0));
    let mut bump = Bump {
        top: Vec::new(),
        pages: Vec::new(),
        segments: Vec::new(),
    };
    for index in 0..SEGMENT_ENTRIES as u64 {
        bump.append(&mut record, index << 2);
    }
    assert!(!record.push_into_current(0));
    assert_eq!(record.room_for_the_next(), Room::Segment);
    record.clear();
    assert!(!record.push_into_current(0));
}
