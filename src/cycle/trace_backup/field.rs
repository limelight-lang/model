//! The field a trace keeps in header bytes 6-7, and the two questions every
//! pass asks before it reads or writes one: may this entity be walked, and is
//! this edge target one this thread's trace walked.
//!
//! # The field
//!
//! One `u16` (`crate::refcount::trace_field_load`): bits 0-13 the side count,
//! bit 14 walked, bit 15 marked (`dev/design/the-general-algorithm.md`, "The
//! build, revised", "The field"). The count saturates at [`SATURATED`] and
//! a saturated entity reads as a root: it keeps what it reaches, which is only
//! conservative, and this arm keeps no overflow map to make it exact ("The
//! Sage on the revised build"). The mark has a bit of its own, so no count can
//! read as marked, the defect the Critic's finding 2 names.
//!
//! # Whose headers a trace writes
//!
//! Its own thread's alone. Nothing serialises two threads' traces, so a walked
//! bit another thread set says nothing to this one: an edge into an entity of
//! another thread's block is a reference from outside this heap, and
//! subtracting it would free that entity live ("The Sage on the revised
//! build", on finding 8). Every subtract and every mark therefore tests the
//! target's block owner first ([`walked_here`]).

use crate::memory::heap::entity_is_in_a_block_of_this_thread;
use crate::refcount::{
    MEMORY_CATEGORY_MASK, RING_GATE_MASK, RcHeader, mutator_flags, trace_field_load,
    trace_field_store,
};

/// The side count's bits, and the value a count saturates at.
pub(crate) const SATURATED: u16 = 0x3FFF;
/// Set by the fill on every entity this trace walks, and on no other.
pub(crate) const WALKED: u16 = 1 << 14;
/// Set by the mark on every walked entity a root reaches.
pub(crate) const MARKED: u16 = 1 << 15;

/// Whether a ring can pass through `entity`, so that the fill walks it: the
/// category is `GcHeap`, the kind is below eight and the class is not proven
/// acyclic (`crate::refcount::RING_GATE_MASK`).
///
/// # Safety
/// `entity` is a live entity header.
#[inline]
pub(crate) unsafe fn may_be_walked(entity: *const RcHeader) -> bool {
    let flags = unsafe { mutator_flags(entity) };
    flags & RING_GATE_MASK == 0
}

/// The field the fill writes for an entity counted `refcount` times.
#[inline]
pub(crate) fn filled(refcount: u32) -> u16 {
    WALKED | refcount.min(u32::from(SATURATED)) as u16
}

/// Whether the edge target `child` is an entity this thread's trace walked: a
/// GC-heap entity in a block this thread owns, carrying the walked bit.
///
/// The category is read first, from the child's own header, because only a
/// GC-heap entity stands in a block whose header the ownership test may read;
/// the owner is read before the field, so another thread's walked bit is never
/// taken for this trace's.
///
/// # Safety
/// `child` is null or a live entity header, read on the owning thread of the
/// trace.
#[inline]
pub(crate) unsafe fn walked_here(child: *mut RcHeader) -> bool {
    if child.is_null() || unsafe { mutator_flags(child) } & MEMORY_CATEGORY_MASK != 0 {
        return false;
    }

    if !unsafe { entity_is_in_a_block_of_this_thread(child) } {
        return false;
    }

    let read = unsafe { trace_field_load(child) };
    read & WALKED != 0
}

/// Whether the census walks the edge target `child`: a GC-heap entity a ring
/// can pass through, in a block this thread owns, which is what the owned
/// lists hold. The gate is read first, from the child's own header, because
/// only a GC-heap entity stands in a block whose header the ownership test may
/// read.
///
/// # Safety
/// `child` is null or a live entity header, read on the owning thread of the
/// trace.
#[inline]
pub(crate) unsafe fn walkable_here(child: *mut RcHeader) -> bool {
    !child.is_null()
        && unsafe { mutator_flags(child) } & RING_GATE_MASK == 0
        && unsafe { entity_is_in_a_block_of_this_thread(child) }
}

/// Subtract one counted edge from `child`'s side count, a saturated count
/// standing.
///
/// # Safety
/// [`walked_here`] answered true for `child` in this trace.
#[inline]
pub(crate) unsafe fn subtract_one(child: *mut RcHeader) {
    debug_assert!(
        unsafe { entity_is_in_a_block_of_this_thread(child) },
        "a trace subtracts only from its own thread's entities"
    );
    let field = unsafe { trace_field_load(child) };
    let count = field & SATURATED;
    if count == SATURATED {
        return;
    }

    // An edge between walked entities is one of the count's own references,
    // so a count at zero here is a count and an edge that disagree; it stays
    // at zero, which reads the entity as garbage only if nothing else holds it.
    debug_assert!(count > 0, "more walked in-edges than references");
    unsafe { trace_field_store(child, field - u16::from(count > 0)) };
}

/// Mark `entity` and answer whether this call did, false where it stood
/// marked.
///
/// # Safety
/// [`walked_here`] answered true for `entity` in this trace, or the fill
/// walked it.
#[inline]
pub(crate) unsafe fn mark(entity: *mut RcHeader) -> bool {
    debug_assert!(
        unsafe { entity_is_in_a_block_of_this_thread(entity) },
        "a trace marks only its own thread's entities"
    );
    let field = unsafe { trace_field_load(entity) };
    if field & MARKED != 0 {
        return false;
    }

    unsafe { trace_field_store(entity, field | MARKED) };
    true
}
