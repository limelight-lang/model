//! The two forms asked the same three questions over the same graph.
//!
//! The listed form is the one every other test of the teardown builds, so what
//! these cases are for is the row form and the agreement between the two: a
//! membership read out of the rows names the entities a harvest would have
//! listed, tests the same children for membership, and counts the same
//! members. A disagreement here is a teardown that severs one set and frees
//! another.

use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::mark::{MarkResult, mark};
use crate::cycle::scan::{ScanResult, scan};
use crate::cycle::shadow::Color;
use crate::cycle::testing::{dismantle_ring, open_arena, ring, row_color, traced_unreachable_from};
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::memory::context::LLContext;
use crate::object::{ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, ll_release};
use crate::test_support::{POOLED_FILLERS, prop_offset, store_prop, wide_class};

/// A class with two counted Box properties: `prop_offset(0)`, which is what
/// [`ring`] links its members through, and one a case hangs a child of its own
/// on.
fn node_class(name: &str) -> *const Class {
    ClassBuilder::new(name)
        .prop("next", true)
        .prop("child", true)
        .build()
}

/// Every member the row form names, sorted, so that two readings of a set can
/// be compared without the walk's own order being a contract.
///
/// # Safety
/// As [`Membership::for_each`].
unsafe fn walked(membership: &Membership<'_>) -> Vec<*mut RcHeader> {
    let mut members = Vec::new();
    unsafe { membership.for_each(|member| members.push(member)) };
    members.sort_unstable();
    members
}

/// The agreement, over a ring of three: same count, same members, and the same
/// answer about a child of each kind — a member, and an entity of a touched
/// block the scan left alone.
#[test]
fn the_two_forms_name_the_same_members() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let class = node_class("MembershipNode");
    let members = unsafe { ring(&mut arena, [class, class, class]) };

    // A child of the ring the trace meets and the scan leaves live, because
    // this frame holds a reference the ring does not. Its row is therefore
    // initialised and carries a colour that is not a verdict, which is what
    // makes the membership test read the colour rather than the row's
    // presence.
    let mut context = LLContext { arena: &mut arena };
    let outsider = unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) };
    unsafe { store_prop(&mut arena, members[0], prop_offset(1), outsider) };

    let mut scratch = unsafe { traced_unreachable_from(members[0], &members) };
    assert_eq!(
        unsafe { row_color(outsider as *mut RcHeader) },
        Color::Live,
        "the control the membership test has to answer no about is a row the \
         trace met"
    );
    let rows = unsafe { Membership::rows(scratch.touched_head()) }
        .expect("every unreachable row of the fixture names its entity");

    let mut expected: Vec<*mut RcHeader> = members.iter().map(|&m| m as *mut RcHeader).collect();
    expected.sort_unstable();

    assert_eq!(rows.len(), expected.len(), "the row form counts the ring");
    assert_eq!(
        unsafe { walked(&rows) },
        expected,
        "and names its members and nothing else"
    );

    let mut listed_members = expected.clone();
    let listed = Membership::listed(&mut listed_members);
    assert_eq!(listed.len(), rows.len());
    assert_eq!(unsafe { walked(&listed) }, expected);

    for &member in &expected {
        assert!(
            unsafe { rows.contains(member) },
            "the row form holds every member"
        );
        assert!(unsafe { listed.contains(member) });
    }

    assert!(
        !unsafe { rows.contains(outsider as *mut RcHeader) },
        "and holds no entity the scan left standing, row or no row"
    );
    assert!(!unsafe { listed.contains(outsider as *mut RcHeader) });

    scratch.reset();
    // The ring's death releases the cell naming the child, so the reference
    // this frame holds is the last one.
    unsafe { dismantle_ring(&mut arena, members) };
    unsafe {
        assert!(ll_release(outsider as *mut RcHeader));
        ll_object_die(outsider);
    }
}

/// The population whose row is not in its array: a large entity's colour is a
/// word of its own block header, so a walk that read the array alone would
/// leave it out of the membership and a teardown would free its holders around
/// it.
#[test]
fn the_row_form_names_a_large_member() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let small = node_class("MembershipSmallNode");
    let wide = wide_class("MembershipWideNode", POOLED_FILLERS, None);
    let members = unsafe { ring(&mut arena, [small, wide]) };

    // The phases are run here rather than through `traced_unreachable_from`,
    // whose per-entity reading goes through the block's shadow pointer: a
    // large entity's row is a word of its own block header and no array holds
    // it, which is the very asymmetry this case is about
    // (`crate::cycle::testing::row_word`).
    let mut scratch = open_arena();
    assert_eq!(
        unsafe { mark(&mut scratch, members[0] as *mut RcHeader) },
        MarkResult::Complete
    );
    assert_eq!(
        unsafe { scan(&mut scratch, members[0] as *mut RcHeader) },
        ScanResult::Complete
    );

    let rows = unsafe { Membership::rows(scratch.touched_head()) }
        .expect("every unreachable row of the fixture names its entity");

    let mut expected: Vec<*mut RcHeader> = members.iter().map(|&m| m as *mut RcHeader).collect();
    expected.sort_unstable();
    assert_eq!(rows.len(), 2, "the block with one occupant is counted too");
    assert_eq!(unsafe { walked(&rows) }, expected);
    assert!(
        unsafe { rows.contains(members[1] as *mut RcHeader) },
        "and the large member answers the membership test"
    );

    scratch.reset();
    unsafe { dismantle_ring(&mut arena, members) };
}

/// A trace that met nothing: the list is empty, and the membership over it has
/// no members rather than no answer.
#[test]
fn an_empty_touched_list_is_a_membership_of_no_members() {
    let _g = test_guard();
    let membership = unsafe { Membership::rows(std::ptr::null_mut()) }
        .expect("an empty list places every row it has");

    assert_eq!(membership.len(), 0);
    assert_eq!(unsafe { walked(&membership) }, Vec::new());
}
