//! Step 4: the garbage split into its connected components, and each one
//! finalized and reclaimed by the path every collection of the crate commits
//! through (`dev/design/the-general-algorithm.md`, "The build, revised",
//! "Components").
//!
//! # Why components, and not the set whole
//!
//! The exact validation and the revalidation answer about the membership they
//! are handed, and a destructor that resurrects one member keeps every member
//! of that membership (`crate::cycle::finalization`). Handed the garbage
//! whole, one pooled object whose destructor stores `$this` would keep every
//! trace from freeing anything (the Critic's finding 6). Split, it keeps its
//! own component: the components share no edge, so a component the
//! revalidation reads as resurrected holds nothing of another.
//!
//! The split is a union-find over the garbage-to-garbage edges, the garbage
//! sorted by address so that an edge's target is found by a binary search
//! rather than a field the header has no room for. Every in-edge of a garbage
//! entity comes from a walked entity of the garbage — a marked holder would
//! have marked it, and a root's count is above zero — so a component is
//! closed under its in-edges, which is what the exact validation needs to
//! read it as unreachable.
//!
//! # One finalization over every component
//!
//! The chain's own shape for several components (`crate::cycle::finalization`):
//! each component confirmed into one [`Finalization`], every guard and every
//! nulled weak cell standing before the first destructor, the destructors of
//! every confirmed component run, and then each component read again and torn
//! down before the next one is read (`dev/DECISIONS.md`, "the revalidation of a
//! component and its teardown are adjacent").

use std::ops::Range;

use crate::cells::{PlainCells, entity_kind, trace_cells};
use crate::cycle::arena::TraceScratchArena;
use crate::cycle::finalization::{Finalization, Revalidated};
use crate::cycle::membership::Membership;
use crate::cycle::reclamation::reclaim_before_drops;
use crate::cycle::validation::ValidationResult;
use crate::refcount::RcHeader;

/// Reorder `garbage` component by component, each component's members sorted
/// by address, and answer the range each component stands in.
///
/// # Safety
/// Every entry is a garbage entity of this thread's trace, live and readable.
pub(crate) unsafe fn split(garbage: &mut Vec<*mut RcHeader>) -> Vec<Range<usize>> {
    garbage.sort_unstable();
    let count = garbage.len();
    let mut parent: Vec<u32> = (0..count as u32).collect();

    for index in 0..count {
        let entity = garbage[index];
        let kind = unsafe { entity_kind(entity) };
        unsafe {
            trace_cells::<PlainCells>(entity, kind, |cell| {
                if let Ok(target) = garbage.binary_search(&cell.child) {
                    union(&mut parent, index as u32, target as u32);
                }
            })
        };
    }

    // Grouped by the component's root, address order inside each: a sort of
    // (root, member) pairs.
    let mut grouped: Vec<(u32, *mut RcHeader)> = (0..count)
        .map(|index| (find(&mut parent, index as u32), garbage[index]))
        .collect();
    grouped.sort_unstable();

    let mut ranges = Vec::new();
    let mut start = 0;
    for index in 0..count {
        garbage[index] = grouped[index].1;
        let last = index + 1 == count || grouped[index + 1].0 != grouped[index].0;
        if last {
            ranges.push(start..index + 1);
            start = index + 1;
        }
    }

    ranges
}

/// The root of `index`'s set, halving the path on the way.
fn find(parent: &mut [u32], mut index: u32) -> u32 {
    while parent[index as usize] != index {
        let grand = parent[parent[index as usize] as usize];
        parent[index as usize] = grand;
        index = grand;
    }

    index
}

fn union(parent: &mut [u32], a: u32, b: u32) {
    let (a, b) = (find(parent, a), find(parent, b));
    if a != b {
        parent[a.max(b) as usize] = a.min(b);
    }
}

/// Finalize and reclaim every component of `members`, `components` naming
/// each one's range, and answer how many members were freed.
///
/// A component the exact validation does not read as unreachable is passed
/// over; one a destructor resurrected gets its true counts back and stands,
/// its destructors behind it; one whose teardown the arena refuses stands as
/// floating garbage for the next trace.
///
/// # Safety
/// Every member is a garbage entity of this thread's trace, named once, each
/// range a component closed under its in-edges; the call runs on the owning
/// thread, its collecting word raised, and `arena` is this thread's.
pub(crate) unsafe fn finalize_and_reclaim(
    members: &[*mut RcHeader],
    components: &[Range<usize>],
    arena: &mut TraceScratchArena,
) -> usize {
    let membership = |range: &Range<usize>| Membership::Listed(&members[range.clone()]);
    let mut finalization = Finalization::begin(arena.epoch());
    let confirmed: Vec<bool> = components
        .iter()
        .map(|range| {
            let result = unsafe { finalization.confirm(&membership(range), None, 0) };
            debug_assert_eq!(
                result,
                ValidationResult::Unreachable,
                "a component the mark left is closed under its in-edges"
            );
            result == ValidationResult::Unreachable
        })
        .collect();

    let mut pass = finalization.seal().destructors();
    for (range, &confirmed) in components.iter().zip(&confirmed) {
        if confirmed {
            unsafe { pass.run(&membership(range)) };
        }
    }

    let mut revalidation = pass.close();
    let mut freed = 0;
    for (range, &confirmed) in components.iter().zip(&confirmed) {
        if !confirmed {
            continue;
        }

        let component = membership(range);
        match unsafe { revalidation.revalidate(&component) } {
            Revalidated::Unreachable(guarded) => {
                if let Some(deferred) = unsafe { reclaim_before_drops(guarded, &component, arena) }
                {
                    deferred.drain();
                    freed += component.len();
                }
            }
            Revalidated::ExternallyReferenced => {}
        }
    }

    revalidation.close();
    freed
}
