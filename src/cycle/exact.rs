//! The exact test: one component judged against its members' current
//! fields, on the thread that owns them.
//!
//! A trace answers with a shortlist rather than a verdict. It may read
//! counts that have changed since it read them, so what it proposes can
//! be wrong in exactly one way — staleness (`rfc/model/gc/rc-cycle.md`,
//! "Who judges, and what a trace is worth"). The judgement is made here
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
//! arena and every row in it (`rfc/model/gc/rc-cycle.md`, "The release
//! obliges a readership rule"). A component therefore arrives as a
//! member list of its own, and the in-degree this file needs is computed
//! from the heap.
//!
//! # The sum stands for the per-member identity
//!
//! The design states the identity per member — `RC(m) = IN(m) + guard` —
//! and what [`judge`] compares is the two sums over the whole component.
//! The two answer the same question, because `RC(m) >= IN(m) + guard`
//! holds for each member on its own: every in-component edge into `m` is
//! a counted reference a member holds, and the guard is one more. For
//! the sums to meet while one member stands above the identity, another
//! would have to stand below it, and none can. What the sum buys is
//! memory — no per-member in-degree is stored anywhere, and the arena
//! that would have funded one has gone back at the token's release.
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
pub(crate) enum Judged {
    /// No reference to any member exists outside the component, so the
    /// teardown may proceed.
    Condemned,
    /// A member reads count zero: it died ordinarily since it was
    /// proposed, its fields are teardown residue, and the component is
    /// dropped whole (`rfc/model/gc/rc-cycle.md`, "Cycle teardown",
    /// step 1).
    Corpse,
    /// A member is held by a reference the component does not contain,
    /// so the component survives this collection.
    Acquitted,
}

/// Judge one component against its members' current fields.
///
/// `members` is the component's whole membership and nothing besides,
/// each member once. **The slice is sorted by address in place**, which
/// is how a traced child is tested for membership, so the caller's own
/// order is gone when this returns: nothing may be held parallel to it
/// by index. A queue entry array indexed after the call names a
/// different member, and clearing an enrolment bit through it is the
/// permanent miss of `rfc/model/gc/cycle/questions.md`, Y6.
///
/// `discount` is the teardown guard outstanding on every member: zero
/// before the guards are taken, one for the re-verify a destructor
/// forces. Without it the guards acquit every component and nothing is
/// ever freed (`rfc/model/gc/rc-cycle.md`, "Cycle teardown", step 5).
///
/// Answers [`Judged::Corpse`] from a pass of its own, before any field
/// of any member is read, and only while no guard is outstanding — a
/// guarded member cannot read zero.
///
/// **Nothing is written**, neither an entity nor a shadow row, so a
/// component this refuses costs the caller nothing to undo. The guard is
/// `PLAN.md` S36.3's, the destructors S36.4's, the sever and the free
/// S36.5's.
///
/// # Safety
/// Every member is an entity header of this thread's GC heap whose slot
/// is still its own. A member that died ordinarily reads count zero and
/// its header, which is what the corpse rule reads, and the parking that
/// keeps that header readable while an entry names the slot is `PLAN.md`
/// S36.2's. The judgement runs on the owning thread with no mutator
/// beside it, which is the condition `cells::trace_cells` reads an
/// entity's cells plainly under.
pub(crate) unsafe fn judge(members: &mut [*mut RcHeader], discount: u32) -> Judged {
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

    if discount == 0 {
        if members.iter().any(|&m| unsafe { header_refcount(m) } == 0) {
            return Judged::Corpse;
        }
    } else {
        debug_assert!(
            members.iter().all(|&m| unsafe { header_refcount(m) } > 0),
            "a guarded member cannot read zero: the guard is a reference of its own"
        );
    }

    let mut references = 0u64;
    let mut internal_edges = 0u64;
    for &member in members.iter() {
        references += u64::from(unsafe { header_refcount(member) });
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
        unsafe { every_member_holds_its_own_share(members, discount) },
        "an in-component edge is a counted reference, so no member can carry \
         fewer references than the component holds of it"
    );

    let guards = u64::from(discount) * members.len() as u64;
    if references == internal_edges + guards {
        Judged::Condemned
    } else {
        Judged::Acquitted
    }
}

/// The premise the sum in [`judge`] stands on, taken member by member:
/// `RC(m) >= IN(m) + discount` for every member.
///
/// Quadratic in the component and run in a debug build alone —
/// `debug_assert!` keeps its argument compiled in every build and
/// executes it in none but that one. It is here because the sum cannot
/// check its own premise: a defect that invents an in-edge into one
/// member and loses a real one in another meets the sum exactly, and
/// frees a component a live reference holds.
///
/// # Safety
/// As [`judge`], with `members` already sorted.
unsafe fn every_member_holds_its_own_share(members: &[*mut RcHeader], discount: u32) -> bool {
    members.iter().all(|&member| {
        let mut in_edges = 0u64;
        for &holder in members {
            let kind = unsafe { cells::entity_kind(holder) };
            unsafe {
                cells::trace_cells::<PlainCells>(holder, kind, |cell| {
                    if cell.child == member {
                        in_edges += 1;
                    }
                });
            }
        }

        u64::from(unsafe { header_refcount(member) }) >= in_edges + u64::from(discount)
    })
}

#[cfg(test)]
mod tests;
