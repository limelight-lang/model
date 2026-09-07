//! The queue a cycle teardown holds its deferred drops in: the children a
//! sever displaced out of the component, waiting for the last member's free.
//!
//! Why they wait is `crate::cycle::reclamation`'s subject — a child's release
//! runs its destructor, and between the sever and the free no user code may run
//! at all. What stands here is where they wait: the chain both collection
//! structures share ([`crate::cycle::records::LazyChain`]), over segments of
//! the collection arena's bump, emptied at the end of every component.
//!
//! The type is beside `crate::cycle::stack` rather than inside the teardown for
//! the reason the worklist is: the arena owns the queue, and an arena that
//! reached into the teardown for the type of one of its own fields would
//! depend on a phase of the collection rather than on a structure.
//!
//! **The verbs the arena reaches for are `attach`, `push_into_current` and
//! `drain`.** A reservation taken before the first cell is emptied is what
//! draws the segments, so the queue grows behind its append position rather
//! than at it, and the replay reads the records in the order the sever wrote
//! them.

use crate::cycle::records::{LazyChain, SEGMENT_HEADER_BYTES};
use crate::refcount::RcHeader;

/// Children one segment holds behind its header line.
///
/// The worklist's trade (`crate::cycle::stack`), taken again over an eight-byte
/// record: a page of children behind the line, so a component whose children
/// fit one page crosses no boundary and one whose children do not costs a page
/// at a time.
pub(crate) const SEGMENT_RECORDS: usize = 512;

/// Bytes one segment takes out of the arena.
pub(crate) const SEGMENT_BYTES: usize =
    SEGMENT_HEADER_BYTES + SEGMENT_RECORDS * size_of::<*mut RcHeader>();

// The page the trade above is against, pinned: the record count and the
// record's width are chosen together.
const _: () = assert!(SEGMENT_BYTES - SEGMENT_HEADER_BYTES == 4096);

/// The children a sever displaced out of a component, waiting for the last
/// member's free.
///
/// Held by the arena whose memory it stands on and emptied once per component.
/// **It does not outlive an arena reset**: the segments are that arena's blocks
/// and the workspace it bumps over, so a queue used after the reset would read
/// children out of memory another collection is granting.
pub(crate) type DeferredDrops = LazyChain<*mut RcHeader>;
