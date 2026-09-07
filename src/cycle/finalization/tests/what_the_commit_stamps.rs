//! The maturation stamp a commit leaves in the entities it proved live, and
//! the components it leaves unstamped.
//!
//! The stamp is what the next collection's descent reads to stop at an edge
//! rather than follow it (`PLAN.md` S37.1), so the cases here are about which
//! components earn one and what age it carries. Every one of them drives the
//! finalization chain by hand, as S36.7's driver will: the stamp is written at
//! the reading that proves the component live, and the commit is counted at
//! the close.
//!
//! The epoch is pinned rather than driven. The counter is process-global, so a
//! case that closed 64 commits to reach a turnover would move the epoch under
//! whatever other case was reading a stamp at the time
//! (`crate::cycle::epoch::pin`).

use super::*;
use crate::cycle::epoch;
use crate::refcount::{MaturationStamp, read_maturation_stamp};

/// A ring in the GC heap, each member naming the next, with the fixture's own
/// reference to every member released — so the members are held by their own
/// edges and by whatever the case adds.
///
/// The trace is what the caller's component list stands on: it reads every
/// member as potentially unreachable, which is the proposal an exact
/// validation is asked about.
///
/// # Safety
/// The caller runs on a quiescent heap and takes the ring apart afterwards.
unsafe fn ring<const MEMBERS: usize>(
    arena: &mut Arena,
    classes: [*const crate::class::Class; MEMBERS],
) -> [*mut Object; MEMBERS] {
    let mut context = LLContext { arena: &mut *arena };
    let ring = classes
        .map(|class| unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) });
    unsafe {
        for (index, &member) in ring.iter().enumerate() {
            store_prop(arena, member, prop_offset(0), ring[(index + 1) % MEMBERS]);
        }

        for &member in &ring {
            assert!(!ll_release(member as *mut RcHeader));
        }
    }

    let expected: Vec<*mut Object> = ring.to_vec();
    let mut shadow_arena = unsafe { traced_unreachable_from(ring[0], &expected) };
    shadow_arena.reset();
    ring
}

/// The stamp each member carries, in the caller's own order.
///
/// # Safety
/// Every member is a live entity of this thread's GC heap.
unsafe fn stamps(members: &[*mut Object]) -> Vec<MaturationStamp> {
    members
        .iter()
        .map(|&member| unsafe { read_maturation_stamp(member as *mut RcHeader) })
        .collect()
}

/// The stamp an unstamped entity reads, which is what a component read as
/// unreachable keeps and what a member born this epoch carries.
const UNSTAMPED: MaturationStamp = MaturationStamp { epoch: 0, age: 0 };

/// The stamp a member carries after a commit of `epoch` read its component
/// live for the `age`-th consecutive time.
fn stamped(epoch: u32, age: u32) -> MaturationStamp {
    MaturationStamp { epoch, age }
}

/// One collection's commit over a component the exact validation reads as
/// externally referenced: no member is guarded, no destructor runs, and the
/// close counts the commit.
///
/// # Safety
/// As [`Finalization::confirm`]: the members are a component of this thread's
/// GC heap, each named once.
unsafe fn commit_reading_live(members: &mut [*mut RcHeader]) {
    let mut finalization = Finalization::begin();
    assert_eq!(
        unsafe { finalization.confirm(members) },
        ValidationResult::ExternallyReferenced,
        "the fixture holds this component from outside"
    );

    finalization.seal().destructors().close().close();
}

/// A destructor that keeps `$this` alive past its own component's teardown,
/// which is what the second reading is taken for. The reference is a retain
/// rather than a store, because what the reading answers to is the count.
unsafe extern "C" fn resurrecting_destructor(obj: *mut Object) {
    unsafe { ll_retain(obj as *mut RcHeader) };
}

/// An age counts consecutive collections of one epoch, and stops at the two
/// bits the field has. The fourth commit is the case's own control: an age
/// that kept counting would wrap to 0 and read as unstamped, which is a
/// component the descent follows again.
#[test]
fn a_component_read_live_ages_by_one_at_every_collection() {
    let _g = test_guard();
    let _epoch = epoch::pin(1);
    let node = ClassBuilder::new("StampedRingNode")
        .prop("next", true)
        .build();
    let holder = ClassBuilder::new("StampedRingHolder")
        .prop("held", true)
        .build();

    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, [node, node]) };
    let mut context = LLContext { arena: &mut arena };
    let keeper = unsafe { new_constructed(&mut context, holder, MemoryCategory::GcHeap) };
    unsafe { store_prop(&mut arena, keeper, prop_offset(0), members[0]) };

    assert_eq!(
        unsafe { stamps(&members) },
        vec![UNSTAMPED; 2],
        "the ring is born unstamped"
    );

    let mut headers = members.map(|member| member as *mut RcHeader);
    for age in [1, 2, 3, 3] {
        unsafe { commit_reading_live(&mut headers) };
        assert_eq!(unsafe { stamps(&members) }, vec![stamped(1, age); 2]);
    }

    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
        dismantle_ring(&mut arena, members);
    }
}

/// The commit that writes the stamps is what carries the process toward the
/// next epoch, so a collection counts once however many components it read.
///
/// The count is process-global and the assertion is an inequality for that
/// reason: another thread's case may close a commit between the two readings.
/// Three commits are driven rather than one so that the reading is not an
/// inequality another thread can satisfy on this one's behalf.
#[test]
fn every_closed_commit_is_counted_toward_the_turnover() {
    let _g = test_guard();
    let _epoch = epoch::pin(1);
    let node = ClassBuilder::new("StampedCountedNode")
        .prop("next", true)
        .build();
    let holder = ClassBuilder::new("StampedCountedHolder")
        .prop("held", true)
        .build();

    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, [node, node]) };
    let mut context = LLContext { arena: &mut arena };
    let keeper = unsafe { new_constructed(&mut context, holder, MemoryCategory::GcHeap) };
    unsafe { store_prop(&mut arena, keeper, prop_offset(0), members[0]) };

    let mut headers = members.map(|member| member as *mut RcHeader);
    let before = epoch::commits();
    for _ in 0..3 {
        unsafe { commit_reading_live(&mut headers) };
    }

    assert!(
        epoch::commits() >= before + 3,
        "each commit closed is one commit counted"
    );

    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
        dismantle_ring(&mut arena, members);
    }
}

/// A stamp of an earlier epoch is no stamp: the age beside it is read against
/// the epoch it was written in, so the first collection past a turnover starts
/// the component at one again. Nothing clears the old stamp, which is what
/// makes the turnover free.
#[test]
fn a_stamp_of_another_epoch_starts_the_age_again() {
    let _g = test_guard();
    let node = ClassBuilder::new("StampedTurnoverNode")
        .prop("next", true)
        .build();
    let holder = ClassBuilder::new("StampedTurnoverHolder")
        .prop("held", true)
        .build();

    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, [node, node]) };
    let mut context = LLContext { arena: &mut arena };
    let keeper = unsafe { new_constructed(&mut context, holder, MemoryCategory::GcHeap) };
    unsafe { store_prop(&mut arena, keeper, prop_offset(0), members[0]) };

    let mut headers = members.map(|member| member as *mut RcHeader);
    {
        let _epoch = epoch::pin(1);
        unsafe { commit_reading_live(&mut headers) };
        unsafe { commit_reading_live(&mut headers) };
        assert_eq!(unsafe { stamps(&members) }, vec![stamped(1, 2); 2]);
    }

    let _epoch = epoch::pin(2);
    unsafe { commit_reading_live(&mut headers) };
    assert_eq!(
        unsafe { stamps(&members) },
        vec![stamped(2, 1); 2],
        "the age of the epoch before it counts for nothing"
    );

    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
        dismantle_ring(&mut arena, members);
    }
}

/// A component ages as a unit and at the rate of its youngest member: two
/// members carrying age 2 are joined by a third carrying none, and the whole
/// component is stamped 1. The descent stops at a mature edge target, so a
/// component whose oldest member decided its age would be pruned around a
/// member no collection has read twice.
#[test]
fn the_youngest_member_decides_the_component_age() {
    let _g = test_guard();
    let _epoch = epoch::pin(1);
    let node = ClassBuilder::new("StampedYoungestNode")
        .prop("next", true)
        .build();
    let holder = ClassBuilder::new("StampedYoungestHolder")
        .prop("held", true)
        .build();

    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, [node, node, node]) };
    let [first, second, third] = members;

    // The third member's edge into the ring is what holds the pair from
    // outside, so the pair alone reads as externally referenced.
    let mut pair = [first as *mut RcHeader, second as *mut RcHeader];
    unsafe { commit_reading_live(&mut pair) };
    unsafe { commit_reading_live(&mut pair) };
    assert_eq!(
        unsafe { stamps(&[first, second, third]) },
        vec![stamped(1, 2), stamped(1, 2), UNSTAMPED]
    );

    let mut context = LLContext { arena: &mut arena };
    let keeper = unsafe { new_constructed(&mut context, holder, MemoryCategory::GcHeap) };
    unsafe { store_prop(&mut arena, keeper, prop_offset(0), first) };

    let mut whole = members.map(|member| member as *mut RcHeader);
    unsafe { commit_reading_live(&mut whole) };
    assert_eq!(
        unsafe { stamps(&members) },
        vec![stamped(1, 1); 3],
        "the member that joined this epoch decides for all three"
    );

    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
        dismantle_ring(&mut arena, members);
    }
}

/// A destructor that does nothing, which is what puts the second reading on
/// the exact validation's road rather than on the skip a commit with no user
/// code takes.
unsafe extern "C" fn inert_destructor(_obj: *mut Object) {}

/// A component read as unreachable carries no stamp: nothing read it live, and
/// the commit that tears it down writes nothing into it.
///
/// Both roads to that answer are taken, because they are different code: a
/// commit no destructor ran in skips the second exact validation, and one that
/// ran user code takes it.
#[test]
fn a_component_read_unreachable_is_never_stamped() {
    let _g = test_guard();
    let _epoch = epoch::pin(1);
    let plain = ClassBuilder::new("StampedCondemnedNode")
        .prop("next", true)
        .build();
    let with_destructor = ClassBuilder::new("StampedCondemnedNodeWithDestructor")
        .prop("next", true)
        .destructor(inert_destructor as *const ())
        .build();

    unsafe {
        a_component_read_unreachable_takes_no_stamp([plain, plain]);
        a_component_read_unreachable_takes_no_stamp([plain, with_destructor]);
    }
}

/// Drive one commit over a ring nothing outside it names, and read the members
/// while their guards are still on — the last instant every one of them is
/// addressable.
///
/// # Safety
/// The caller runs on a quiescent heap under `memory::block_pool::test_guard`.
unsafe fn a_component_read_unreachable_takes_no_stamp(classes: [*const crate::class::Class; 2]) {
    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, classes) };
    let mut headers = members.map(|member| member as *mut RcHeader);

    let mut finalization = Finalization::begin();
    assert_eq!(
        unsafe { finalization.confirm(&mut headers) },
        ValidationResult::Unreachable
    );

    let mut pass = finalization.seal().destructors();
    unsafe { pass.run(&headers) };
    let mut revalidation = pass.close();
    let Revalidated::Unreachable(component) = (unsafe { revalidation.revalidate(&mut headers) })
    else {
        panic!("nothing took a reference to this component");
    };

    assert_eq!(
        unsafe { stamps(&members) },
        vec![UNSTAMPED; 2],
        "the component the teardown is about takes no stamp"
    );

    // The teardown itself is `cycle::reclamation`'s; what this case owes the
    // finalization is the guards, and the release is the discharge that tears
    // nothing down.
    unsafe { component.release(&headers) };
    revalidation.close();

    unsafe { dismantle_ring(&mut arena, members) };
}

/// A component user code resurrected is stamped at the second reading, and at
/// the first it carries nothing: until the destructors have run, the exact
/// validation reads it as unreachable. The stamp is written before the guards
/// come off, which is where every member is still addressable.
#[test]
fn a_resurrected_component_is_stamped_at_the_second_reading() {
    let _g = test_guard();
    let _epoch = epoch::pin(3);
    let plain = ClassBuilder::new("StampedResurrectedPeer")
        .prop("next", true)
        .build();
    let keeper = ClassBuilder::new("StampedResurrectingNode")
        .prop("next", true)
        .destructor(resurrecting_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, [plain, keeper]) };
    let mut headers = members.map(|member| member as *mut RcHeader);

    let mut finalization = Finalization::begin();
    assert_eq!(
        unsafe { finalization.confirm(&mut headers) },
        ValidationResult::Unreachable
    );
    assert_eq!(
        unsafe { stamps(&members) },
        vec![UNSTAMPED; 2],
        "the first reading condemned the component"
    );

    let mut pass = finalization.seal().destructors();
    unsafe { pass.run(&headers) };
    let mut revalidation = pass.close();
    assert!(
        matches!(
            unsafe { revalidation.revalidate(&mut headers) },
            Revalidated::ExternallyReferenced
        ),
        "the destructor kept a reference the component does not contain"
    );
    revalidation.close();

    assert_eq!(
        unsafe { stamps(&members) },
        vec![stamped(3, 1); 2],
        "a component proved live at step 5 is stamped like one proved live at \
         step 2"
    );

    unsafe {
        // What the destructor kept, given back before the ring is taken apart.
        assert!(!ll_release(members[1] as *mut RcHeader));
        dismantle_ring(&mut arena, members);
    }
}
