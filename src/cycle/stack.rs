//! The trace's worklist: entities met but not yet expanded, over a fixed
//! region of the thread's workspace and segments the collection's arena
//! serves past it.
//!
//! Recursion would put the closure's depth on the machine stack, and the
//! closure is not small: the subgraph reachable from a median candidate
//! root measures at the whole object population, 381 of 381
//! (`rfc/model/gc/rc-cycle.md`, "Candidate registration and trial
//! deletion"). So the descent carries a
//! stack of its own.
//!
//! **The first [`WORKLIST_BASE_ENTRIES`] entries cost no allocation.** They
//! are a fixed region of the workspace the thread already holds, so a leaf
//! root and a shallow trace queue their entities without asking the memory
//! manager for anything, and a depth past that region draws segments from the
//! same arena the rows come from — one refusal point for both, and a refused
//! segment aborts the collection exactly as a refused row array does
//! ([`crate::cycle::arena`]).
//!
//! A segment is **kept when it empties** rather than abandoned. The
//! arena is a bump with no free, so a trace whose depth crosses a
//! segment boundary repeatedly would take a fresh segment at every
//! crossing.
//!
//! One worklist serves both phases of a trace, `crate::cycle::mark` and
//! `crate::cycle::scan`, and the segments a deep mark drew are what the
//! scan's own depth reuses.
//!
//! **The worklist owns no memory of its own**, which is why the arena owns the
//! worklist: the base region is the workspace the arena is bumping over and
//! every segment past it is a block that arena drew, so the arena's reset
//! frees the whole worklist at once and re-establishes the emptiness a fresh
//! trace starts from. The push is the arena's for the same reason — it is the
//! only caller that knows where a segment comes from
//! ([`TraceScratchArena::push_work`](crate::cycle::arena::TraceScratchArena::push_work)).

use crate::cycle::records::{RecordChain, SEGMENT_HEADER_BYTES};
use crate::refcount::RcHeader;

/// Entries the workspace's own worklist region holds, and entries one
/// overflow segment holds behind its header line.
///
/// It is a trade between two costs. Smaller, and a deep trace crosses a
/// boundary often; larger, and the fixed region takes workspace from the
/// bump that serves rows — against a row array of up to 16 408 bytes at the
/// smallest size class (`crate::cycle::shadow::bytes_for`), a page of entries
/// is the smaller of the two claims. The capacity the Sage gate fixed for the
/// region is in `PLAN.md`, S36.11.
pub(crate) const WORKLIST_BASE_ENTRIES: usize = 256;

/// Bytes the worklist's fixed region takes out of the workspace, and the
/// bytes one overflow segment takes out of the arena.
///
/// Named here because the mark's abort tests price a collection's memory to
/// the byte and the segment's layout is this module's.
pub(crate) const SEGMENT_BYTES: usize =
    SEGMENT_HEADER_BYTES + WORKLIST_BASE_ENTRIES * size_of::<WorklistEntry>();

// The page the comment above trades against, pinned: the entry count and the
// entry's width are chosen together, and changing either without the other
// leaves the comment stating a size the segment no longer has.
const _: () = assert!(SEGMENT_BYTES - SEGMENT_HEADER_BYTES == 4096);

/// One entity the trace has met and not yet expanded, with the shadow row
/// that meeting found for it.
///
/// The row travels beside the entity because resolving an address to a row
/// is a block dispatch — a kind load, and for a retained block a search of
/// the survivor list (`crate::cycle::row::resolve_edge_target`) — and the
/// push has already paid it. The pointer and not the colour: another path
/// into the same entity can recolour the row between the push and the pop,
/// and what decides the expansion is the colour the row holds at the pop
/// (`dev/CYCLE-COLLECTOR-REVIEW.md`, finding 2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub(crate) struct WorklistEntry {
    /// The entity whose out-edges the expansion reads.
    pub(crate) entity: *mut RcHeader,
    /// Its shadow row, which stays valid until
    /// [`TraceScratchArena::clear_touched_rows`](crate::cycle::arena::TraceScratchArena::clear_touched_rows)
    /// nulls the block's pointer at the end of scan.
    pub(crate) row: *mut u32,
}

/// The descent's worklist: entities met but not yet expanded.
///
/// Held by the arena whose memory it stands on and spent by one collection.
/// **A worklist does not outlive an arena reset**: that call hands the drawn
/// blocks to the pool and to the critical reserve and rewinds the bump over
/// the thread's workspace, so a worklist used after it would advance either
/// into a block another thread has since recommissioned or into rows the next
/// collection on this thread is granting — an entity pointer written over
/// somebody's rows either way, and the second is the quieter of the two. The
/// reset is what empties it, and the retry after an abort is the collection
/// that depends on this.
pub(crate) struct TraceStack {
    entries: RecordChain<WorklistEntry>,
}

impl TraceStack {
    /// An empty worklist over `region`, which holds [`WORKLIST_BASE_ENTRIES`]
    /// entries behind its header line.
    ///
    /// # Safety
    /// `region` addresses [`SEGMENT_BYTES`] writable bytes of the thread's
    /// workspace, and stays this worklist's until the arena holding it closes.
    pub(crate) unsafe fn over(region: *mut u8) -> Self {
        Self {
            entries: unsafe { RecordChain::over(region, WORKLIST_BASE_ENTRIES) },
        }
    }

    /// Queue `entry` in the segment the worklist is filling, or answer
    /// **false** when that segment is full and the caller owes a region.
    pub(crate) fn push_into_current(&mut self, entry: WorklistEntry) -> bool {
        self.entries.push(entry)
    }

    /// Move onto the segment an earlier crossing left above the current one,
    /// or answer **false** when there is none.
    pub(crate) fn advance_to_kept(&mut self) -> bool {
        self.entries.advance_to_kept()
    }

    /// Attach `region` as one more segment and make it the one being filled.
    ///
    /// # Safety
    /// `region` addresses [`SEGMENT_BYTES`] writable bytes of the arena that
    /// holds this worklist, and no segment stands above the current one —
    /// which is what [`advance_to_kept`](Self::advance_to_kept) answering
    /// false reports.
    pub(crate) unsafe fn extend(&mut self, region: *mut u8) {
        unsafe { self.entries.extend(region, WORKLIST_BASE_ENTRIES) };
    }

    /// The next entity to expand and the row its meeting found, or `None`
    /// when the closure is exhausted.
    pub(crate) fn pop(&mut self) -> Option<WorklistEntry> {
        self.entries.pop()
    }

    /// Whether the trace has expanded everything it queued.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Empty the worklist and forget every segment past the workspace's own
    /// region, which the arena owes the instant it gives those blocks back.
    pub(crate) fn rewind(&mut self) {
        self.entries.rewind();
    }

    /// Segments the worklist holds, the workspace's own region included.
    /// Tests only: a shallow trace holds one, and a second says the depth
    /// crossed the region's boundary.
    #[cfg(test)]
    pub(crate) fn segment_count(&self) -> usize {
        self.entries.segment_count()
    }
}

#[cfg(test)]
mod tests;
