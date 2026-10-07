//! The passes over the thread's owned entity blocks: the census (the fill
//! and the subtract in one walk), the mark, and the harvest of the garbage,
//! which clears the live
//! (`dev/design/the-general-algorithm.md`, "The backup trace: the build",
//! "The trace", steps 1-3 and 5).
//!
//! Each pass is a walk of the owned lists
//! (`crate::memory::heap::for_each_owned_entity_slot`) rather than of a
//! snapshot, so the passes hold no index of their own; the one structure
//! beside the headers is the mark's stack. The edges are the crate's edge
//! walker's (`crate::cells::trace_cells`, the stride trial deletion reads),
//! so a cell it yields no child for — a Box payload, a weak cell — is a
//! reference from outside, and its target reads as a root.

use crate::cells::{PlainCells, entity_kind, trace_cells};
use crate::memory::heap::for_each_owned_entity_slot;
use crate::refcount::{RcHeader, header_refcount, trace_field_load, trace_field_store};

use super::field::{self, MARKED, SATURATED, WALKED};

/// Entries the mark's stack holds at most, eight bytes each. A root's
/// closure past it is not lost: a mark that overflows reads every marked
/// entity's children again until a round pushes nothing it could not hold
/// ([`mark`]).
const MARK_STACK_ENTRIES: usize = 1 << 20;

/// The bound a case sets on the mark stack in place of
/// [`MARK_STACK_ENTRIES`], zero for none. Process-wide, and read only under the
/// memory tests' guard, which every case that traces holds.
#[cfg(test)]
static MARK_STACK_BOUND: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A case's bound on the mark stack, lifted when it drops.
#[cfg(test)]
pub(crate) struct BoundedStack;

/// Bound the mark stack at `entries` until the answer drops.
#[cfg(test)]
pub(crate) fn bound_the_mark_stack(entries: usize) -> BoundedStack {
    MARK_STACK_BOUND.store(entries, std::sync::atomic::Ordering::Relaxed);
    BoundedStack
}

#[cfg(test)]
impl Drop for BoundedStack {
    fn drop(&mut self) {
        MARK_STACK_BOUND.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

fn mark_stack_entries() -> usize {
    #[cfg(test)]
    {
        let bound = MARK_STACK_BOUND.load(std::sync::atomic::Ordering::Relaxed);
        if bound != 0 {
            return bound;
        }
    }

    MARK_STACK_ENTRIES
}

/// Every child `entity`'s counted cells name, in cell order.
///
/// # Safety
/// `entity` is a live entity of this thread whose cells are readable.
#[inline]
unsafe fn for_each_child(entity: *mut RcHeader, mut visit: impl FnMut(*mut RcHeader)) {
    let kind = unsafe { entity_kind(entity) };
    unsafe { trace_cells::<PlainCells>(entity, kind, |cell| visit(cell.child)) };
}

/// Steps 1 and 2 in one walk: every live entity a ring can pass through takes
/// its count as a side count, the walked bit beside it, and every edge from a
/// walked entity to a walked entity of this thread takes one off the target's.
/// An edge target the walk has not reached yet is filled where the edge meets
/// it, which needs the field zero outside a trace ([`harvest`]). Answers how
/// many entities it filled.
///
/// # Safety
/// On the owning thread, at a poll, its collecting word raised, every field
/// zero.
pub(crate) unsafe fn census() -> usize {
    let mut walked = 0;
    unsafe {
        for_each_owned_entity_slot(|entity, _| {
            if !field::may_be_walked(entity) {
                return;
            }

            if trace_field_load(entity) & WALKED == 0 {
                trace_field_store(entity, field::filled(header_refcount(entity)));
                walked += 1;
            }

            for_each_child(entity, |child| {
                if !field::walkable_here(child) {
                    return;
                }

                if trace_field_load(child) & WALKED == 0 {
                    trace_field_store(child, field::filled(header_refcount(child)));
                    walked += 1;
                }

                field::subtract_one(child);
            });
        })
    };
    walked
}

/// Step 3: mark every walked entity a root reaches, a root being a walked
/// entity whose side count stayed above zero — something outside the walked
/// set holds it: a local, a global, another thread, an unwalked entity.
///
/// The roots are seeded in a pass of their own, after the subtract, so no
/// count is read half subtracted. The stack is bounded; a push it cannot hold
/// leaves its entity marked and unexpanded, and a round then reads the
/// children of every marked entity again, until a round overflows no more.
///
/// # Safety
/// As [`census`], which ran.
pub(crate) unsafe fn mark() {
    let capacity = mark_stack_entries();
    let mut stack: Vec<*mut RcHeader> = Vec::with_capacity(capacity.min(1 << 16));
    let mut overflowed = false;

    unsafe {
        for_each_owned_entity_slot(|entity, _| {
            let read = trace_field_load(entity);
            if read & WALKED == 0 || read & SATURATED == 0 {
                return;
            }

            if field::mark(entity) {
                push_or_overflow(&mut stack, capacity, &mut overflowed, entity);
            }

            if stack.len() == capacity {
                drain(&mut stack, capacity, &mut overflowed);
            }
        })
    };
    unsafe { drain(&mut stack, capacity, &mut overflowed) };

    while overflowed {
        overflowed = false;
        unsafe {
            for_each_owned_entity_slot(|entity, _| {
                if trace_field_load(entity) & (WALKED | MARKED) != WALKED | MARKED {
                    return;
                }

                expand(&mut stack, capacity, &mut overflowed, entity);
                drain(&mut stack, capacity, &mut overflowed);
            })
        };
    }
}

fn push_or_overflow(
    stack: &mut Vec<*mut RcHeader>,
    capacity: usize,
    overflowed: &mut bool,
    entity: *mut RcHeader,
) {
    if stack.len() < capacity {
        stack.push(entity);
    } else {
        *overflowed = true;
    }
}

/// Mark every walked child of `entity` not marked yet, and push it.
///
/// # Safety
/// `entity` is a walked entity of this trace.
unsafe fn expand(
    stack: &mut Vec<*mut RcHeader>,
    capacity: usize,
    overflowed: &mut bool,
    entity: *mut RcHeader,
) {
    unsafe {
        for_each_child(entity, |child| {
            if field::walked_here(child) && field::mark(child) {
                push_or_overflow(stack, capacity, overflowed, child);
            }
        })
    };
}

/// # Safety
/// Every entity on `stack` is a walked entity of this trace.
unsafe fn drain(stack: &mut Vec<*mut RcHeader>, capacity: usize, overflowed: &mut bool) {
    while let Some(entity) = stack.pop() {
        unsafe { expand(stack, capacity, overflowed, entity) };
    }
}

/// The garbage: every walked entity the mark did not reach, in no order, and
/// the bytes of the slots they stand in. The same walk clears the field of
/// every marked entity, so no pass after the trace clears the live; the
/// garbage keeps the walked bit alone, its side count zero.
///
/// # Safety
/// As [`census`], [`mark`] having run.
pub(crate) unsafe fn harvest() -> (Vec<*mut RcHeader>, usize) {
    let mut garbage = Vec::new();
    let mut bytes = 0;
    unsafe {
        for_each_owned_entity_slot(|entity, slot_bytes| {
            let read = trace_field_load(entity);
            if read & WALKED == 0 {
                return;
            }

            if read & MARKED != 0 {
                trace_field_store(entity, 0);
            } else {
                trace_field_store(entity, WALKED);
                garbage.push(entity);
                bytes += slot_bytes;
            }
        })
    };
    (garbage, bytes)
}
