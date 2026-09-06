//! The exact test: one component validated against its members' current
//! fields, on the thread that owns them.
//!
//! A trace answers with a shortlist rather than a verdict. It may read
//! counts that have changed since it read them, so what it proposes can
//! be wrong in exactly one way — staleness (`rfc/model/gc/rc-cycle.md`,
//! "Speculative tracing and exact validation"). The validation is made here
//! instead: every member's refcount is read again and matched against
//! the edges the members themselves hold, and a component whose counts
//! are accounted for that way is held by nothing outside it. The reading
//! cannot go stale, because the thread that performs it is the thread
//! that changes the counts.
//!
//! # The rows are not read here
//!
//! The trace token covers the mark, the scan and the rows they write,
//! and it is released before the exact test of any component — with the
//! arena and every row in it (`rfc/model/gc/rc-cycle.md`, "Concurrency").
//! A component therefore arrives as a
//! member list of its own, and the in-degree this file needs is computed
//! from the heap.
//!
//! # The sum stands for the per-member identity
//!
//! The design states the identity per member — `RC(m) = IN(m) + guard` — and
//! what [`validate_component`] compares is the two sums over the whole
//! component. The two answer the same question, because `RC(m) >= IN(m) +
//! guard` holds for each member on its own: every in-component edge into `m` is
//! a counted reference a member holds, and the guard is one more. For the sums
//! to meet while one member stands above the identity, another would have to
//! stand below it, and none can. What the sum buys is memory — a release build
//! stores no per-member in-degree, and the arena that would have funded one has
//! gone back at the token's release; the debug premise check below keeps one
//! for its own pass.
//!
//! # It allocates nothing in a release build, and it cannot be refused
//!
//! Every input is already in hand — the member list is the caller's and the
//! counts are the heap's — so this module holds no memory, asks for none, and
//! has no failure of its own to report. The one exception is the premise
//! check's in-degree array, which a debug build alone allocates. What it
//! answers is a [`ValidationResult`], and `Unreachable` is a reading rather
//! than permission: the finalization protocol that acts on it is `PLAN.md`
//! S36.3 onward, and it runs after this call rather than inside it.
//!
//! **The premise is checked rather than argued**: a debug build runs it
//! member by member, because the sum cannot see a defect that invents an
//! in-edge into one member and loses a real one in another. It holds
//! while a count is exact, and a count at the `2^32` bound is not — the
//! `checked-refcount` build freezes it there and the ordinary build
//! wraps (`crate::refcount::ll_retain`), which is the corruption every
//! count-based decision in this crate already stands on.

use crate::cells::{self, PlainCells};
use crate::object::header_category;
use crate::refcount::{MemoryCategory, RcHeader, header_refcount};

/// What the exact test answered about one component.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ValidationResult {
    /// No reference to any member exists outside the component, so the
    /// teardown may proceed.
    Unreachable,
    /// A member reads count zero: it died ordinarily since it was
    /// proposed, its fields are teardown residue, and the component is
    /// dropped whole (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation",
    /// step 1).
    ZeroCountMember,
    /// A member is held by a reference the component does not contain,
    /// so the component survives this collection.
    ExternallyReferenced,
}

/// Validate one component against its members' current fields.
///
/// `members` is the component's whole membership and nothing besides,
/// each member once. **The slice is sorted by address in place**, which
/// is how a traced child is tested for membership, so the caller's own
/// order is gone when this returns: nothing may be held parallel to it
/// by index. A queue entry array indexed after the call names a
/// different member, and clearing a registration bit through it is the
/// permanent miss of `rfc/model/gc/cycle/questions.md`, Y6.
///
/// `guard_refs_per_member` is the teardown guard outstanding on every member:
/// zero before the guards are taken, one for the re-verify a destructor forces.
/// Without it the guards would leave every component externally referenced and
/// nothing is ever freed (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation", step
/// 5).
///
/// Answers [`ValidationResult::ZeroCountMember`] from a pass of its own, before
/// any field of any member is read, and only while no guard is outstanding — a
/// guarded member cannot read zero.
///
/// **Nothing is written**, neither an entity nor a shadow row, so a
/// component this refuses costs the caller nothing to undo. The guard is
/// `PLAN.md` S36.3's, the destructors S36.4's, the sever and the free
/// S36.5's.
///
/// # Safety
/// Every member is an entity header of this thread's GC heap whose
/// slot is still its own. A member that died ordinarily reads count zero and
/// its header, which is what the zero-count rule reads, and the withholding
/// that keeps that header readable while an entry names the slot is `PLAN.md`
/// S36.2's. The validation runs on the owning thread with no mutator beside it,
/// which is the condition `cells::trace_cells` reads an entity's cells plainly
/// under.
pub(crate) unsafe fn validate_component(
    members: &mut [*mut RcHeader],
    guard_refs_per_member: u32,
) -> ValidationResult {
    members.sort_unstable();
    debug_assert!(!members.is_empty(), "a component has a member");
    debug_assert!(
        members.windows(2).all(|pair| pair[0] != pair[1]),
        "a member stands in its component once: twice counts one refcount twice \
         and its in-edges once"
    );
    debug_assert!(
        members
            .iter()
            .all(|&m| unsafe { header_category(m) } == MemoryCategory::GcHeap),
        "a member outside the GC heap carries a count no store barrier maintains, \
         so no identity holds over it"
    );

    if guard_refs_per_member == 0 {
        if members.iter().any(|&m| unsafe { header_refcount(m) } == 0) {
            return ValidationResult::ZeroCountMember;
        }
    } else {
        debug_assert!(
            members.iter().all(|&m| unsafe { header_refcount(m) } > 0),
            "a guarded member cannot read zero: the guard is a reference of its own"
        );
    }

    let mut total_refcount = 0u64;
    let mut internal_edges = 0u64;
    for &member in members.iter() {
        total_refcount += u64::from(unsafe { header_refcount(member) });
        // The kind is loaded here and passed down rather than read inside
        // the tracer, which is the contract `trace_cells` states.
        let kind = unsafe { cells::entity_kind(member) };
        unsafe {
            cells::trace_cells::<PlainCells>(member, kind, |cell| {
                if members.binary_search(&cell.child).is_ok() {
                    internal_edges += 1;
                }
            });
        }
    }

    debug_assert!(
        unsafe { member_counts_cover_internal_edges(members, guard_refs_per_member) },
        "an in-component edge is a counted reference, so no member can carry \
         fewer references than the component holds of it"
    );

    let guard_refcount = u64::from(guard_refs_per_member) * members.len() as u64;
    if total_refcount == internal_edges + guard_refcount {
        ValidationResult::Unreachable
    } else {
        ValidationResult::ExternallyReferenced
    }
}

/// The premise the sum in [`validate_component`] stands on, taken member by
/// member: `RC(m) >= IN(m) + guard_refs_per_member` for every member.
///
/// Run in a debug build alone — `debug_assert!` keeps its argument
/// compiled in every build and executes it in none but that one. Every
/// member's cells are walked once, and an in-degree array indexed by the
/// member's position in the sorted slice takes the count; it is the one
/// allocation of this module, and a debug build makes it. The check is
/// here because the sum cannot check its own premise: a defect that
/// invents an in-edge into one member and loses a real one in another
/// meets the sum exactly, and frees a component a live reference holds.
///
/// # Safety
/// As [`validate_component`], with `members` already sorted.
unsafe fn member_counts_cover_internal_edges(
    members: &[*mut RcHeader],
    guard_refs_per_member: u32,
) -> bool {
    let mut in_degrees = vec![0u64; members.len()];
    for &holder in members {
        note_premise_walk();
        let kind = unsafe { cells::entity_kind(holder) };
        unsafe {
            cells::trace_cells::<PlainCells>(holder, kind, |cell| {
                if let Ok(position) = members.binary_search(&cell.child) {
                    in_degrees[position] += 1;
                }
            });
        }
    }

    members
        .iter()
        .zip(&in_degrees)
        .all(|(&member, &in_degree)| {
            u64::from(unsafe { header_refcount(member) })
                >= in_degree + u64::from(guard_refs_per_member)
        })
}

#[cfg(test)]
thread_local! {
    static PREMISE_CELL_WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Count one member's cells walked by the premise check, and nothing at
/// all without `cfg(test)`.
#[inline]
fn note_premise_walk() {
    #[cfg(test)]
    PREMISE_CELL_WALKS.with(|walks| walks.set(walks.get() + 1));
}

/// How many members' cells the premise check has walked on this thread.
#[cfg(all(test, debug_assertions))]
pub(crate) fn premise_cell_walks() -> usize {
    PREMISE_CELL_WALKS.with(std::cell::Cell::get)
}

#[cfg(test)]
mod tests;
