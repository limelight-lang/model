//! The region's own contract, without a trace: what it reserves, what it takes
//! before it refuses, and what a second collection finds while a list stands.
//!
//! Each case arms through the arena, which is where a collection asks, and
//! lets the arena's own close end the harvest — the order production runs in,
//! the sweep standing between the arming and the driver's read.

use super::*;

use crate::cycle::arena::{WORKSPACE_BUMP_BYTES, WORKSPACE_PREFIX_BYTES};

/// One entity address, standing for a member: the region stores what it is
/// given and reads nothing through it, so a case about the region needs no
/// entity at all.
fn address(word: usize) -> *mut RcHeader {
    word as *mut RcHeader
}

/// The bytes the region takes and the bytes it leaves, asserted as the figures
/// the design names rather than as the arithmetic that produces them: an
/// expression here would agree with any capacity at all.
///
/// The workspace is one 64 KiB block, so what the prefix takes the rows do not
/// get, and the pair is what the choice of 1,024 records was made against
/// (`PLAN.md` S36.12).
#[test]
fn the_region_takes_eight_kilobytes_and_leaves_the_rest_to_the_rows() {
    assert_eq!(MEMBER_CAPACITY, 1_024);
    assert_eq!(size_of::<MemberControl>(), 64);
    assert_eq!(MEMBERS_BASE_BYTES, 8_256);
    assert_eq!(WORKSPACE_PREFIX_BYTES, 8_320);
    assert_eq!(WORKSPACE_BUMP_BYTES, 56_960);
}

/// The list a harvest fills is the list the driver reads, in the order the
/// sweep wrote it, and the region is free again once the driver drops it.
#[test]
fn the_records_stand_until_the_driver_drops_them() {
    let _g = test_guard();
    let members = [address(0x1000), address(0x2000), address(0x3000)];

    let mut arena = open_arena();
    assert!(arena.arm_harvest(MEMBER_CAPACITY));
    for entity in members {
        assert!(unsafe { push(entity) });
    }
    drop(arena);

    let standing = take_standing().expect("a harvest was armed");
    assert_eq!(standing.entities(), members);
    assert!(!standing.overflowed());
    assert!(
        take_standing().is_none(),
        "one reader of one list, so a second take answers nothing"
    );

    drop(standing);
    let mut next = open_arena();
    assert!(
        next.arm_harvest(MEMBER_CAPACITY),
        "the region is free for the next collection"
    );
    drop(next);
    take_standing();
}

/// The nesting rule: a collection a destructor of a teardown starts is refused
/// an arming, rather than writing over the list the outer driver is reading.
#[test]
fn a_second_arming_is_refused_while_a_list_stands() {
    let _g = test_guard();

    let mut arena = open_arena();
    assert!(arena.arm_harvest(MEMBER_CAPACITY));
    assert!(unsafe { push(address(0x1000)) });
    assert!(
        !arena.arm_harvest(MEMBER_CAPACITY),
        "armed and not yet read"
    );
    drop(arena);

    let standing = take_standing().expect("a harvest was armed");
    let mut nested = open_arena();
    assert!(
        !nested.arm_harvest(MEMBER_CAPACITY),
        "and standing, which is the case the teardown runs in"
    );
    drop(nested);
    assert_eq!(standing.entities().len(), 1);

    drop(standing);
    let mut after = open_arena();
    assert!(after.arm_harvest(MEMBER_CAPACITY));
    drop(after);
    take_standing();
}

/// A trace's set fits or none of it does: the record past the capacity is
/// refused, and the end of the harvest empties what did fit, so no driver can
/// tear down a part of a set whose remaining members still name it.
#[test]
fn an_overflow_empties_the_list_and_says_so() {
    let _g = test_guard();

    let mut arena = open_arena();
    assert!(arena.arm_harvest(2));
    assert!(unsafe { push(address(0x1000)) });
    assert!(unsafe { push(address(0x2000)) });
    assert!(
        !unsafe { push(address(0x3000)) },
        "the third is past the capacity"
    );
    drop(arena);

    let standing = take_standing().expect("a harvest was armed");
    assert!(standing.overflowed());
    assert_eq!(
        standing.entities(),
        [] as [*mut RcHeader; 0],
        "what fitted is not a set the teardown may take"
    );
}

/// The two answers an empty list can carry, told apart by the word that exists
/// for it: a trace that met nothing unreachable and one that met more than the
/// region holds both read as empty.
#[test]
fn an_empty_list_says_whether_it_overflowed() {
    let _g = test_guard();

    let mut arena = open_arena();
    assert!(arena.arm_harvest(MEMBER_CAPACITY));
    drop(arena);

    let standing = take_standing().expect("a harvest was armed");
    assert!(standing.entities().is_empty());
    assert!(!standing.overflowed(), "nothing was refused");
}

/// A row the dispatch cannot place gives the whole harvest up, and the driver
/// reads that refusal the way it reads an overflow: the set is closed under
/// its in-edges, so a list with one member missing is not a list a teardown
/// may take.
#[test]
fn a_given_up_harvest_reads_as_an_overflow() {
    let _g = test_guard();

    let mut arena = open_arena();
    assert!(arena.arm_harvest(MEMBER_CAPACITY));
    assert!(unsafe { push(address(0x1000)) });
    unsafe { abandon() };
    drop(arena);

    let standing = take_standing().expect("a harvest was armed");
    assert!(standing.overflowed());
    assert!(standing.entities().is_empty());
}

/// A capacity above the region's own is a caller error rather than a clamp:
/// clamping would write records the driver never asked for into memory the
/// next collection bumps.
#[test]
#[should_panic(expected = "more records than the region holds")]
fn a_capacity_past_the_region_is_refused() {
    let _g = test_guard();
    let mut arena = open_arena();
    let _ = arena.arm_harvest(MEMBER_CAPACITY + 1);
}
