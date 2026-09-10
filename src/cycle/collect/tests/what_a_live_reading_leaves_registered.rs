//! What a collection leaves registered when its trace reads every candidate's
//! component as live, and what a later collection reaches through it.
//!
//! A ring held from outside is coloured live by the scan, so the sweep writes
//! no unreachable row and the commit reads a membership of length zero. Such a
//! collection disposes of no component: it takes the close's ordinary path,
//! which merges the detached batch back into the active lane. Every token
//! survives that, the candidate bits stand, and no slot is returned — only the
//! owner reduces state, and only through a token that names a dead entity
//! (`rfc/model/gc/cycle/questions.md`, Y12 clauses 4 and 5).
//!
//! The case below is what that law buys. Registration is edge-triggered, so a
//! decrement meeting a standing bit registers nothing, and a ring whose keeper
//! goes after such a collection has no route into a lane except the tokens
//! that collection kept. A pass that dropped one would leave its members
//! carrying a bit no entry names, which is the permanent miss of
//! `rfc/model/gc/cycle/questions.md`, Y6.
//!
//! **Two rings, and tokens rather than a count.** A component is reached whole
//! from either of its roots, so the entities a second collection frees cannot
//! see one lost token inside one ring; a second ring makes a lost token cost
//! two entities. The tokens themselves are read through
//! [`collect_lane_tokens`], a count of one lane being unable to state either
//! half of the rule — a bit standing over no record, or one entity recorded
//! twice.
//!
//! **A token's lane is asserted separately from its identity.**
//! [`collect_lane_tokens`] concatenates the active chain, the deferred lane
//! and the overflow buffer, so the multiset says which entities are recorded
//! and never where. [`candidate_count`] answers the active chain alone, and
//! the two together pin all four tokens to it: a deferral that took half of
//! them would keep the multiset whole while `retire_candidates`, which walks
//! the active lane only, left two slots withheld for the life of the thread.
//!
//! **The collection is proved to have traced.** Zero is the answer to a
//! refused workspace, an empty lane and a trace that met a refused allocation
//! as much as to a component read live, and each of those leaves the tokens
//! standing for a reason this file is not about. The mark phase's own
//! dispatch count separates them
//! ([`take_dispatches_in_mark_phase`]): four roots and their four counted
//! children resolve eight rows.
//!
//! **What the lane reading can and cannot attribute.** It is taken after the
//! collection has also run its own retirement pass, so a token seen here
//! survived both the close's merge and that pass; a bit cleared at one of them
//! and restored at the other would be invisible. Retirement removes completed
//! deaths alone, and every member here is live, so what the reading states is
//! that neither pass touched a live registration.
//!
//! `queue/tests/the_batch_a_collection_detaches.rs` proves the merge itself at
//! token identity. What this file adds is the consequence the step asks for:
//! the entities a later collection frees, which is the only assertion that
//! shows a kept token was worth keeping.
//!
//! The exact validation's own reading of `ExternallyReferenced` is the other
//! disposition a live component can reach, and its records take the deferred
//! lane instead of the active one (`when_the_turnover_reoffers`).

use super::*;
use crate::cycle::queue::{candidate_count, collect_lane_tokens, release_queue_segments};
use crate::cycle::row::take_dispatches_in_mark_phase;
use crate::refcount::{CANDIDATE_BIT, mutator_flags, take_admissions};

/// A class with one counted Box property, through which a case holds a ring
/// member from outside the component.
fn keeper_class(name: &str) -> *const Class {
    ClassBuilder::new(name).prop("held", true).build()
}

/// Every token this thread's queue holds, sorted, so that a case compares
/// multisets rather than the order two lanes happen to yield.
fn lane_tokens() -> Vec<*mut RcHeader> {
    let mut tokens = Vec::new();
    collect_lane_tokens(&mut tokens);
    tokens.sort_unstable();
    tokens
}

/// `members` as the tokens a queue holding exactly them would yield.
fn as_tokens(members: &[*mut Object]) -> Vec<*mut RcHeader> {
    let mut tokens: Vec<*mut RcHeader> = members.iter().map(|&m| m as *mut RcHeader).collect();
    tokens.sort_unstable();
    tokens
}

/// Members of `ring` that still carry [`CANDIDATE_BIT`].
///
/// # Safety
/// Every member is a live entity of this thread's GC heap.
unsafe fn still_registered(ring: &[*mut Object]) -> usize {
    ring.iter()
        .filter(|&&member| unsafe { mutator_flags(member as *const RcHeader) } & CANDIDATE_BIT != 0)
        .count()
}

/// The tokens a live reading keeps are what a later collection reaches the
/// rings by: each keeper's death decrements a member that already carries the
/// bit, so no second registration follows and the kept tokens are the whole
/// route back.
#[test]
fn two_rings_read_live_are_collected_once_their_keepers_go() {
    let _g = test_guard();
    release_queue_segments();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);

    let node = node_class("LiveReadNode", counting_destructor as *const ());
    let mut arena = Arena::new();

    // The keepers are built before the rings. `ring` spends every creation
    // reference, so a ring is garbage from the moment it is built until a
    // keeper's edge holds it, and an allocation inside that window can collect
    // it: `memory::heap::entity_alloc` starts a collection on a refusal.
    let keepers = {
        let mut context = LLContext { arena: &mut arena };
        let class = keeper_class("LiveReadKeeper");
        unsafe {
            [
                new_constructed(&mut context, class, MemoryCategory::GcHeap),
                new_constructed(&mut context, class, MemoryCategory::GcHeap),
            ]
        }
    };

    take_admissions();
    let rings = unsafe {
        [
            ring(&mut arena, [node, node]),
            ring(&mut arena, [node, node]),
        ]
    };
    let members: Vec<*mut Object> = rings.iter().flatten().copied().collect();
    assert_eq!(
        take_admissions(),
        4,
        "each ring's own release admitted both members through the gate"
    );
    unsafe {
        for (keeper, ring) in keepers.iter().zip(&rings) {
            store_prop(&mut arena, *keeper, prop_offset(0), ring[1]);
        }
    }
    assert_eq!(lane_tokens(), as_tokens(&members));
    assert_eq!(
        candidate_count(),
        4,
        "and holds them all in the active lane"
    );

    take_dispatches_in_mark_phase();
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        0,
        "each keeper's reference holds its ring"
    );
    assert_eq!(
        take_dispatches_in_mark_phase(),
        8,
        "the mark completed over four roots and their four children"
    );
    assert_eq!(
        lane_tokens(),
        as_tokens(&members),
        "the live reading dropped no token and wrote none twice"
    );
    assert_eq!(candidate_count(), 4, "and left every one of them offerable");
    assert_eq!(
        unsafe { still_registered(&members) },
        4,
        "and cleared no bit"
    );

    unsafe {
        for keeper in keepers {
            assert!(ll_release(keeper as *mut RcHeader));
            ll_object_die(keeper);
        }
    }
    assert_eq!(
        take_admissions(),
        0,
        "each decrement met the standing bit and registered nothing"
    );
    assert_eq!(lane_tokens(), as_tokens(&members));
    assert_eq!(candidate_count(), 4);

    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        4,
        "the tokens the reading kept reach both rings whole"
    );
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 4);
    assert!(
        lane_tokens().is_empty(),
        "the retirement consumed every token"
    );
    assert_eq!(
        unsafe { still_registered(&members) },
        0,
        "and took every bit down with it, which is what releases the withheld slot"
    );
}
