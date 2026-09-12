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
//! # No row is written here, and the counts come from the heap
//!
//! The trace token covers the mark, the scan and the rows they write, and it
//! is released before the exact test of any component
//! (`rfc/model/gc/rc-cycle.md`, "Concurrency"). What the release ends is the
//! right to trace rather than the life of the rows: a collection off the poll
//! keeps its arena through the teardown and its membership **is** those rows,
//! while a collection under pressure has given the blocks back and holds a
//! harvested list. Either way the in-degree this file needs is computed from
//! the heap, and the membership answers only which entities the component
//! holds ([`Membership`]).
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
//! stores no per-member in-degree, and on the pressure path the arena that
//! would have funded one has gone back before this runs; the debug premise
//! check below keeps one for its own pass.
//!
//! # It allocates nothing in a release build, and it cannot be refused
//!
//! Every input is already in hand — the membership is the caller's and the
//! counts are the heap's — so this module holds no memory, asks for none, and
//! has no failure of its own to report. The exceptions are the two debug
//! checks, which a release build does not run at all: each materialises the
//! membership as a sorted list, because a check indexed by member is what
//! neither form hands out, and the premise check allocates its in-degree array
//! beside it. What this module answers is a [`ValidationResult`], and
//! `Unreachable` is a reading rather than permission: the finalization
//! protocol that acts on it runs after this call rather than inside it.
//!
//! **The premise is checked rather than argued**: a debug build runs it
//! member by member, because the sum cannot see a defect that invents an
//! in-edge into one member and loses a real one in another. It holds
//! while a count is exact, and a count at the `2^32` bound is not — the
//! `checked-refcount` build freezes it there and the ordinary build
//! wraps (`crate::refcount::ll_retain`), which is the corruption every
//! count-based decision in this crate already stands on.

use crate::cells::{self, PlainCells};
use crate::cycle::membership::Membership;
use crate::object::header_category;
use crate::refcount::{MemoryCategory, RcHeader, header_refcount};

/// What the exact test answered about one component.
///
/// **The answer is the one refusal the finalization protocol has**, so a
/// caller that drops it tears down whatever it validated: a component this
/// refuses is live, or holds a member somebody else is already tearing down.
#[must_use = "an unread answer tears down a component the exact test may have refused"]
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
/// `members` is the component's whole membership and nothing besides, each
/// member once, in whichever of the two forms the path that produced it holds
/// ([`Membership`]). What this file asks of it is a walk and a membership
/// test, and the two forms answer both.
///
/// `guard_refs_per_member` is the teardown guard outstanding on every member:
/// zero before the guards are taken, one for the re-verify a destructor forces.
/// Without it the guards would leave every component externally referenced and
/// nothing is ever freed (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation", step
/// 5).
///
/// Answers [`ValidationResult::ZeroCountMember`] from a pass of its own, before
/// any field of any member is read, and only while no guard is outstanding — a
/// guarded member reading zero is a defect and refused as one.
///
/// **It walks the membership twice**, once for the counts and once for the
/// edges, which is what that order costs. On the listed form the second walk
/// is a second read of the same slice; on the row form it is a second walk of
/// the touched list.
///
/// **Nothing is written**, neither an entity nor a shadow row, so a
/// component this refuses costs the caller nothing to undo. The guard is
/// [`crate::cycle::finalization`]'s, and the sever and the free
/// [`crate::cycle::reclamation`]'s.
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
    members: &Membership<'_>,
    guard_refs_per_member: u32,
) -> ValidationResult {
    debug_assert!(members.len() > 0, "a component has a member");
    note_validation(members.len());
    debug_assert!(
        unsafe { every_member_is_a_gc_heap_entity_once(members) },
        "a member stands in its component once and inside the GC heap: twice counts \
         one refcount twice and its in-edges once, and a count outside the heap is \
         one no store barrier maintains"
    );

    // A pass of its own, and it is ahead of every field read below rather than
    // folded into it: a member at count zero holds teardown residue in its
    // cells, and the zero-count rule drops the component before anything reads
    // one (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation",
    // step 1).
    let mut total_refcount = 0u64;
    let mut zero_count_member = false;
    unsafe {
        members.for_each(|member| {
            let refcount = header_refcount(member);
            zero_count_member |= refcount == 0;
            total_refcount += u64::from(refcount);
        })
    };

    if zero_count_member {
        // The reading a guard makes impossible, so it is an answer on one arm
        // and a defect on the other.
        assert_eq!(
            guard_refs_per_member, 0,
            "a guarded member cannot read zero: the guard is a reference of its own"
        );
        return ValidationResult::ZeroCountMember;
    }

    let mut internal_edges = 0u64;
    unsafe {
        members.for_each(|member| {
            // The kind is loaded here and passed down rather than read inside
            // the tracer, which is the contract `trace_cells` states.
            let kind = cells::entity_kind(member);
            cells::trace_cells::<PlainCells>(member, kind, |cell| {
                if members.contains(cell.child) {
                    internal_edges += 1;
                }
            });
        })
    };

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
/// member's position in the sorted slice takes the count. That array and the
/// sorted list it indexes are two of the three global allocations a debug
/// build makes per call, the third being the list
/// [`every_member_is_a_gc_heap_entity_once`] builds for the check that runs
/// ahead of this one; each is freed before the call returns. The check is
/// here because the sum cannot check its own premise: a defect that
/// invents an in-edge into one member and loses a real one in another
/// meets the sum exactly, and frees a component a live reference holds.
///
/// # Safety
/// As [`validate_component`], with `members` already sorted.
unsafe fn member_counts_cover_internal_edges(
    members: &Membership<'_>,
    guard_refs_per_member: u32,
) -> bool {
    let listed = unsafe { members_in_address_order(members) };
    let mut in_degrees = vec![0u64; listed.len()];
    for &holder in &listed {
        note_premise_walk();
        let kind = unsafe { cells::entity_kind(holder) };
        unsafe {
            cells::trace_cells::<PlainCells>(holder, kind, |cell| {
                if let Ok(position) = listed.binary_search(&cell.child) {
                    in_degrees[position] += 1;
                }
            });
        }
    }

    listed.iter().zip(&in_degrees).all(|(&member, &in_degree)| {
        u64::from(unsafe { header_refcount(member) })
            >= in_degree + u64::from(guard_refs_per_member)
    })
}

/// Whether every member stands in `members` once and inside the GC heap.
///
/// Debug builds alone, and it materialises the membership to say so: the row
/// form answers a membership test and not a duplicate one, a row being one
/// entity's ([`Membership`]).
///
/// # Safety
/// As [`validate_component`].
unsafe fn every_member_is_a_gc_heap_entity_once(members: &Membership<'_>) -> bool {
    let listed = unsafe { members_in_address_order(members) };
    listed.windows(2).all(|pair| pair[0] != pair[1])
        && listed
            .iter()
            .all(|&m| unsafe { header_category(m) } == MemoryCategory::GcHeap)
}

/// The membership as a sorted list, which is what a check indexed by member
/// needs and what neither form hands out.
///
/// The allocation is a debug build's: both callers stand inside a
/// `debug_assert!`, whose argument is compiled in every build and executed in
/// none but that one.
///
/// # Safety
/// As [`validate_component`].
unsafe fn members_in_address_order(members: &Membership<'_>) -> Vec<*mut RcHeader> {
    let mut listed = Vec::with_capacity(members.len());
    unsafe { members.for_each(|member| listed.push(member)) };
    listed.sort_unstable();
    listed
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

/// How many members' cells the premise check has walked on this thread. Zero
/// in a release test build, where the check itself does not run.
#[cfg(test)]
pub(crate) fn premise_cell_walks() -> usize {
    PREMISE_CELL_WALKS.with(std::cell::Cell::get)
}

/// Global allocations one [`validate_component`] makes **past its zero-count
/// return**: three in a debug build and none in a release one, each of them
/// inside a `debug_assert!` and each freed before the call returns.
///
/// The arm that answers [`ValidationResult::ZeroCountMember`] costs one rather
/// than three, its return standing between the membership check and the premise
/// check, and it records no premise walk — so a bracket that contains one reads
/// this figure as zero and is short by that one. No case of the deny run reaches
/// it, and a case that does owes a calibration of its own.
///
/// A case that asks what a collection asks the allocator subtracts this rather
/// than asserting a bare zero, the premise check being the one site of the
/// collection path the GC-memory contract exempts (`PLAN.md` S36.9). What pins
/// the figure is
/// `validation::tests::what_the_premise_check_costs::a_validation_allocates_what_its_debug_checks_allocate`,
/// which reads it off the call rather than deriving it.
#[cfg(test)]
pub(crate) const EXEMPT_ALLOCATIONS_PER_VALIDATION: usize =
    if cfg!(debug_assertions) { 3 } else { 0 };

/// Count one exact validation over `members` for the census, and nothing at
/// all without `cfg(test)`.
#[inline]
fn note_validation(_members: usize) {
    #[cfg(test)]
    crate::cycle::census::note_validation(_members);
}

#[cfg(test)]
mod tests;
