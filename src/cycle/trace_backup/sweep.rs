//! The direct sweep: the garbage no destructor and no weak cell can reach is
//! severed and freed without the finalization chain, and only the rest is split
//! into components and finalized (`dev/design/the-general-algorithm.md`, the
//! backup trace's second arm).
//!
//! # Which garbage is swept
//!
//! A garbage entity that owes a destructor or is named by a weak cell seeds the
//! finalized part, and so does every garbage entity reachable from a seed: a
//! destructor can read whatever its object reaches, and a weak load can hand
//! user code the object it names. The rest is the swept part, and no edge runs
//! from the finalized part into it, by the closure. An edge from the swept part
//! into the finalized part is held as a drop until the chain is done, so a
//! component it enters reads as externally referenced and stands; the drain
//! lets go of it, and the next trace finds it closed under its in-edges.
//!
//! The finalized part is told apart by its field: the harvest leaves every
//! garbage entity at the walked bit alone, and the closure writes [`DIRTY`],
//! the walked bit under a saturated count, which no garbage field carries
//! otherwise.
//!
//! # No user code runs inside the sweep
//!
//! The sweep's members take a guard each, their cells are severed, and each is
//! freed by the ordinary death path, which finds no destructor owed, every cell
//! null and no weak cell. A severed child of the swept part takes the narrow
//! decrement, which never acts at zero; any other child, whose release can run
//! a destructor, is held in [`Drops`] and dropped after the finalized part is
//! torn down, so no destructor it runs can load a weak cell the chain has not
//! nulled yet.

use crate::cells::{PlainCells, entity_kind, sever_cells, trace_cells};
#[cfg(debug_assertions)]
use crate::cycle::membership::Membership;
use crate::memory::barrier::drop_ref;
use crate::refcount::{
    DESTRUCTOR_PENDING, DESTRUCTOR_RAN, HAS_WEAK_REFERENCES, MemoryCategory, RcHeader, ll_release,
    mutator_flags, mutator_guard_retain, severed_edge_release, trace_field_load, trace_field_store,
};

use super::field::{self, SATURATED, WALKED};

/// The field of a garbage entity in the finalized part.
pub(crate) const DIRTY: u16 = WALKED | SATURATED;

/// Whether a garbage entity seeds the finalized part: it owes a destructor, or
/// a weak cell names it.
///
/// # Safety
/// `entity` is a live entity header.
#[inline]
unsafe fn seeds_the_finalized_part(entity: *const RcHeader) -> bool {
    let flags = unsafe { mutator_flags(entity) };
    flags & HAS_WEAK_REFERENCES != 0
        || flags & (DESTRUCTOR_PENDING | DESTRUCTOR_RAN) == DESTRUCTOR_PENDING
}

/// Split the garbage into the swept part and the finalized part, in that
/// order, the finalized part's fields at [`DIRTY`].
///
/// # Safety
/// Every entry is a garbage entity of this thread's trace, its field at the
/// walked bit alone, and no other walked entity's field has the walked bit.
pub(crate) unsafe fn partition(
    garbage: Vec<*mut RcHeader>,
) -> (Vec<*mut RcHeader>, Vec<*mut RcHeader>) {
    let mut stack = Vec::new();
    for &entity in &garbage {
        if unsafe { seeds_the_finalized_part(entity) } {
            unsafe { trace_field_store(entity, DIRTY) };
            stack.push(entity);
        }
    }

    while let Some(entity) = stack.pop() {
        let kind = unsafe { entity_kind(entity) };
        unsafe {
            trace_cells::<PlainCells>(entity, kind, |cell| {
                let child = cell.child;
                if field::walked_here(child) && trace_field_load(child) == WALKED {
                    trace_field_store(child, DIRTY);
                    stack.push(child);
                }
            })
        };
    }

    garbage
        .into_iter()
        .partition(|&entity| unsafe { trace_field_load(entity) } != DIRTY)
}

/// The counted references the sweep took off its members' cells to entities
/// outside the garbage, owed a release.
#[must_use = "the severed children still carry counted references"]
pub(crate) struct Drops(Vec<*mut RcHeader>);

impl Drops {
    /// Release every child, which is where user code runs.
    pub(crate) fn drain(self) {
        for child in self.0 {
            unsafe { drop_ref(MemoryCategory::GcHeap, child) };
        }
    }
}

/// Sever and free every member of the swept part, and answer the children it
/// held outside the garbage.
///
/// # Safety
/// `swept` is the swept part [`partition`] answered, every member live, the
/// finalized part's fields still at [`DIRTY`]; on the owning thread, its
/// collecting word raised.
pub(crate) unsafe fn sweep(swept: &[*mut RcHeader]) -> Drops {
    // The chain's own check on a sever, in a debug build: it displaces the
    // children a walk of the same cells counts (`crate::cycle::reclamation`).
    #[cfg(debug_assertions)]
    let outside = {
        let mut sorted = swept.to_vec();
        sorted.sort_unstable();
        unsafe { Membership::Listed(&sorted).external_children() }
    };

    let mut drops = Vec::new();
    for &member in swept {
        unsafe { mutator_guard_retain(member) };
    }

    for &member in swept {
        let kind = unsafe { entity_kind(member) };
        unsafe {
            sever_cells(member, kind, |child| {
                // A swept child stands at its guard. A finalized one keeps the
                // reference until the drain, so no count in the finalized
                // part reads zero, which the chain's validation refuses.
                if field::walked_here(child) && trace_field_load(child) != DIRTY {
                    severed_edge_release(child);
                } else {
                    drops.push(child);
                }
            })
        };
    }

    #[cfg(debug_assertions)]
    debug_assert_eq!(
        drops.len(),
        outside,
        "the sever displaces every child the walk ahead of it counted"
    );

    for &member in swept {
        unsafe { trace_field_store(member, 0) };
        let last = unsafe { ll_release(member) };
        debug_assert!(last, "a swept member's guard is its last reference");
        if last {
            unsafe { crate::object::ll_entity_die(member) };
        }
    }

    Drops(drops)
}

/// Zero the field of every member of the finalized part, which the chain
/// reads no further.
///
/// # Safety
/// Every entry is a live entity of this thread's trace.
pub(crate) unsafe fn clear(finalized: &[*mut RcHeader]) {
    for &entity in finalized {
        unsafe { trace_field_store(entity, 0) };
    }
}
