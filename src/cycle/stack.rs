//! The trace's worklist: entities met but not yet expanded, over
//! segments drawn from the collection's own arena.
//!
//! Recursion would put the closure's depth on the native stack, and the
//! closure is not small: the subgraph reachable from a median candidate
//! root measures at the whole object population, 381 of 381
//! (`rfc/model/gc/rc-cycle.md`, "What it is"). So the descent carries a
//! stack of its own, drawn from the arena the rows come from — one
//! refusal point for both, and a refused segment aborts the collection
//! exactly as a refused row array does.
//!
//! A segment is **kept when it empties** rather than abandoned. The
//! arena is a bump with no free, so a trace whose depth crosses a
//! segment boundary repeatedly would take a fresh segment at every
//! crossing.
//!
//! One worklist serves both phases of a trace, `crate::cycle::mark` and
//! `crate::cycle::scan`, and the segments a deep mark drew are what the
//! scan's own depth reuses.

use crate::cycle::arena::TraceScratchArena;
use crate::refcount::RcHeader;

/// Entries one stack segment holds: 512 pointers, one page of worklist
/// behind the two links.
///
/// It is a trade between two costs the arena pays. Smaller, and a deep
/// trace crosses a boundary often; larger, and a shallow trace's first
/// push reserves memory the collection never uses — against a row array
/// of up to 16 408 bytes that a block's first touch reserves anyway
/// (`crate::cycle::shadow::bytes_for`), a page is the smaller of the
/// two claims.
const SEGMENT_ENTRIES: usize = 512;

/// Bytes one segment takes out of the arena. Named here because the
/// mark's abort tests price a collection's memory to the byte and the
/// segment's layout is this module's.
#[cfg(test)]
pub(crate) const SEGMENT_BYTES: usize = size_of::<StackSegment>();

/// One segment of the descent's worklist, allocated from the arena.
///
/// The two links are the segment's own, so the stack needs no vector and
/// no second allocation: `previous` is the chain the depth came up, `next`
/// the segment kept for the next crossing of this boundary.
#[repr(C)]
struct StackSegment {
    previous: *mut StackSegment,
    next: *mut StackSegment,
    entries: [*mut RcHeader; SEGMENT_ENTRIES],
}

/// The descent's worklist: entities met but not yet expanded.
///
/// Built on the collecting thread's stack and spent by one collection,
/// like the arena it draws its segments from — and freed with that
/// arena, which is why it has no [`Drop`] of its own.
///
/// **Every segment is the arena's memory, so a worklist does not outlive
/// an arena reset.** `TraceScratchArena::reset` hands those blocks to the pool
/// and to the critical reserve, and a stack used after it would climb
/// into a block another thread has since recommissioned, writing an
/// entity pointer into someone else's rows. A collection that resets and
/// traces again — the retry after an abort is exactly that collection —
/// calls [`TraceStack::reset`] in the same breath.
pub(crate) struct TraceStack {
    /// The segment the next push writes into, or null until the first
    /// push has drawn one.
    current: *mut StackSegment,
    /// Entries used in `current`, which is [`SEGMENT_ENTRIES`] when the
    /// segment is full and the next push has to advance.
    current_len: usize,
}

impl TraceStack {
    /// An empty worklist. Allocates nothing: a root whose entity has no
    /// counted children pays for no segment.
    pub(crate) fn new() -> Self {
        Self {
            current: std::ptr::null_mut(),
            current_len: 0,
        }
    }

    /// Queue `entity` for expansion, or answer **false** when both
    /// memory doors refused — which is the caller's signal to abort the
    /// collection, and the only way this can fail.
    pub(crate) fn push(&mut self, arena: &mut TraceScratchArena, entity: *mut RcHeader) -> bool {
        if self.current.is_null() || self.current_len == SEGMENT_ENTRIES {
            if !self.advance_segment(arena) {
                return false;
            }
        }

        unsafe { entries_of(self.current).add(self.current_len).write(entity) };
        self.current_len += 1;
        true
    }

    /// The next entity to expand, or `None` when the closure is
    /// exhausted.
    pub(crate) fn pop(&mut self) -> Option<*mut RcHeader> {
        if self.current_len == 0 {
            let previous = if self.current.is_null() {
                std::ptr::null_mut()
            } else {
                unsafe { (*self.current).previous }
            };

            if previous.is_null() {
                return None;
            }

            self.current = previous;
            self.current_len = SEGMENT_ENTRIES;
        }

        self.current_len -= 1;
        Some(unsafe { entries_of(self.current).add(self.current_len).read() })
    }

    /// Move onto the segment above the current one, reusing the one an
    /// earlier crossing left there or drawing a new one from `arena`.
    /// False when the arena refused.
    fn advance_segment(&mut self, arena: &mut TraceScratchArena) -> bool {
        let kept = if self.current.is_null() {
            std::ptr::null_mut()
        } else {
            unsafe { (*self.current).next }
        };

        if !kept.is_null() {
            self.current = kept;
            self.current_len = 0;
            return true;
        }

        let segment = arena.alloc(size_of::<StackSegment>()) as *mut StackSegment;
        if segment.is_null() {
            return false;
        }

        // Field by field and written rather than assigned: the arena
        // bumps memory with no value in it, so an assignment would drop a
        // `StackSegment` that was never constructed. The entries stay as
        // the bump handed them over, `current_len` being what says which of
        // them have meaning.
        unsafe {
            (&raw mut (*segment).previous).write(self.current);
            (&raw mut (*segment).next).write(std::ptr::null_mut());
            if !self.current.is_null() {
                (&raw mut (*self.current).next).write(segment);
            }
        }

        self.current = segment;
        self.current_len = 0;
        true
    }

    /// Forget every segment, which the caller owes the instant its arena
    /// gives the blocks back.
    ///
    /// Nothing is freed here and nothing can be: the memory is the
    /// arena's and goes back with it. What this undoes is the stack's
    /// own belief that it has segments to climb into, which after
    /// `TraceScratchArena::reset` names memory the pool has taken back.
    pub(crate) fn reset(&mut self) {
        self.current = std::ptr::null_mut();
        self.current_len = 0;
    }

    /// Segments drawn from the arena, emptied ones included. Tests only,
    /// and the instrument for the one defect the entries cannot show: a
    /// stack that abandoned an emptied segment answers every push and
    /// pop correctly while spending a page per boundary crossing.
    #[cfg(test)]
    pub(crate) fn segment_count(&self) -> usize {
        if self.current.is_null() {
            return 0;
        }

        let mut bottom = self.current;
        while !unsafe { (*bottom).previous }.is_null() {
            bottom = unsafe { (*bottom).previous };
        }

        let mut count = 0;
        while !bottom.is_null() {
            count += 1;
            bottom = unsafe { (*bottom).next };
        }

        count
    }
}

/// The entry array of `segment`, which follows its two links.
///
/// # Safety
/// `segment` is a segment a [`TraceStack`] drew, hence non-null.
#[inline]
unsafe fn entries_of(segment: *mut StackSegment) -> *mut *mut RcHeader {
    (unsafe { &raw mut (*segment).entries }) as *mut *mut RcHeader
}

#[cfg(test)]
mod tests;
