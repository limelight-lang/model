//! What a collection over P does with the set the collector posted beside its
//! verdicts (`crate::cycle::posted_set`): it frees what the set proves and
//! nothing the set holds live, stamps nothing, loses the set when a block a
//! member stands in goes back, and a collection over R whole gives the set
//! back unread.
//!
//! The stand-in posts the verdicts ([`stand_in_posts`]) and the case posts the
//! set beside them on its own thread (`posted_set::testing::post_for_test`).

use super::*;
use crate::cycle::posted_set::testing::{post_for_test, take_sets_dropped_at_a_return};
use crate::cycle::testing::stamp_of;
use crate::test_support::{POOLED_FILLERS, RUN_FILLERS, wide_class};

/// A destructor that does nothing, which `node_class` asks for.
unsafe extern "C" fn nothing_to_dispose(_object: *mut Object) {}

/// The headers of `objects`, as the set lists them.
fn headers(objects: &[*mut Object]) -> Vec<*mut RcHeader> {
    objects
        .iter()
        .map(|&object| object as *mut RcHeader)
        .collect()
}

/// An object of `class` the case holds by its creation reference.
unsafe fn held(arena: &mut Arena, class: *const Class) -> *mut Object {
    let mut context = LLContext { arena };
    unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) }
}

/// Give back an object [`held`] made.
unsafe fn let_go(object: *mut Object) {
    unsafe {
        assert!(
            ll_release(object as *mut RcHeader),
            "the case's reference was the last"
        );
        ll_object_die(object);
    }
}

/// Whether this thread's record holds a posted set.
fn a_set_stands() -> bool {
    !unsafe { (*crate::cycle::mutator_record::this_thread_record()).posted_set() }.is_null()
}

/// A set holding a garbage ring of three, one root of which P proposes, and a
/// live object the case holds — a reissued slot's occupant, as far as the
/// owner can tell: the ring is freed, which only the set can prove, the live
/// object is read live by the owner's own scan and left standing, and nothing
/// is stamped — the set is read alone, so a row it leaves live proves no
/// liveness. Red with the set ignored, under a validation of the set whole,
/// which the live member refuses, and with the commit's stamping.
#[test]
fn a_set_frees_its_garbage_and_leaves_a_live_member_unstamped() {
    let _g = test_guard();
    let node = node_class("PostedSetRingNode", nothing_to_dispose as *const ());
    let mut arena = Arena::new();
    let ring = unsafe { long_ring(&mut arena, node, 3) };
    let live = unsafe { held(&mut arena, keeper_class("PostedSetLive")) };
    assert_eq!(stand_in_posts(1, Verdict::Proposed), Posted::Batch(1));
    let mut set = ring.clone();
    set.push(live);
    post_for_test(&headers(&set));

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 3, "the ring is freed");
    assert!(!a_set_stands(), "the collection took the set");
    assert_eq!(
        unsafe { slot_state(live as *const RcHeader) },
        SlotState::Live
    );
    assert_eq!(
        unsafe { stamp_of(live) }.1,
        0,
        "a collection over P stamps nothing"
    );

    unsafe { let_go(live) };
}

/// A member whose block goes back under `POSTED` — a member proposed wrongly,
/// which died since — drops the set before the pool can hand the block on, in
/// the pooled form through `put` and in the run form before the unmapping; the
/// collection over P then reads the proposed root alone, which proves nothing
/// of a ring of three, and writes it back into R for the collector's next
/// batch.
#[test]
fn a_member_whose_block_goes_back_drops_the_set() {
    for (name, fillers) in [
        ("PostedSetPooledMember", POOLED_FILLERS),
        ("PostedSetRunMember", RUN_FILLERS),
    ] {
        let _g = test_guard();
        let node = node_class("PostedSetDroppedRingNode", nothing_to_dispose as *const ());
        let mut arena = Arena::new();
        let ring = unsafe { long_ring(&mut arena, node, 3) };
        let wide = unsafe { held(&mut arena, wide_class(name, fillers, None)) };
        assert_eq!(stand_in_posts(1, Verdict::Proposed), Posted::Batch(1));
        let mut set = ring.clone();
        set.push(wide);
        post_for_test(&headers(&set));
        let _ = take_sets_dropped_at_a_return();

        unsafe { let_go(wide) };
        assert_eq!(
            take_sets_dropped_at_a_return(),
            1,
            "{name}: the return dropped the set"
        );
        assert!(!a_set_stands(), "{name}");

        assert_eq!(
            unsafe { ll_gc_maybe_collect() },
            0,
            "{name}: no set, no proof"
        );
        assert_eq!(
            crate::cycle::queue::candidate_count(),
            3,
            "{name}: the root went back into R beside the two it never took"
        );
        assert_eq!(unsafe { ll_gc_collect_cycles() }, 3, "{name}");
    }
}

/// A return of a block no member stands in leaves the set standing.
#[test]
fn a_return_of_another_block_leaves_the_set() {
    let _g = test_guard();
    let node = node_class("PostedSetKeptRingNode", nothing_to_dispose as *const ());
    let mut arena = Arena::new();
    let ring = unsafe { long_ring(&mut arena, node, 2) };
    let other = unsafe {
        held(
            &mut arena,
            wide_class("PostedSetOther", POOLED_FILLERS, None),
        )
    };
    assert_eq!(stand_in_posts(2, Verdict::Proposed), Posted::Batch(2));
    post_for_test(&headers(&ring));
    let _ = take_sets_dropped_at_a_return();

    unsafe { let_go(other) };
    assert_eq!(take_sets_dropped_at_a_return(), 0);
    assert!(a_set_stands());

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2);
}

/// The explicit call under `POSTED` collects over R whole and gives the set
/// back unread, its blocks back in this thread's figures.
#[test]
fn a_collection_over_r_whole_gives_the_set_back() {
    let _g = test_guard();
    let node = node_class("PostedSetWholeRingNode", nothing_to_dispose as *const ());
    let mut arena = Arena::new();
    let ring = unsafe { long_ring(&mut arena, node, 2) };
    assert_eq!(stand_in_posts(2, Verdict::Proposed), Posted::Batch(2));
    let blocks = crate::memory::gc_metadata::thread_stats().current_blocks();
    post_for_test(&headers(&ring));
    assert!(a_set_stands());

    assert_eq!(unsafe { ll_gc_collect_cycles() }, 2);
    assert!(
        !a_set_stands(),
        "the collection over R whole gave the set back"
    );
    assert_eq!(
        crate::memory::gc_metadata::thread_stats().current_blocks(),
        blocks,
        "the set's blocks went back"
    );
}

/// A pressure collection under `POSTED` gives the set back unread.
#[test]
fn a_pressure_collection_gives_the_set_back() {
    let _g = test_guard();
    let node = node_class("PostedSetPressureRingNode", nothing_to_dispose as *const ());
    let mut arena = Arena::new();
    let ring = unsafe { long_ring(&mut arena, node, 2) };
    assert_eq!(stand_in_posts(2, Verdict::Proposed), Posted::Batch(2));
    post_for_test(&headers(&ring));

    let _ = unsafe { crate::cycle::collect::collect_under_pressure() };
    assert!(!a_set_stands(), "the pressure path gave the set back");
}

/// A block the pool refuses lists no member: the member goes after its block,
/// so a member never stands in the set without the block a return would be
/// matched against.
#[test]
fn a_refused_block_lists_no_member_without_its_block() {
    use crate::cycle::posted_set::Writer;
    use crate::cycle::posted_set::testing::{list, listed, refuse_the_next_block};

    let _g = test_guard();
    let mut arena = Arena::new();
    let member = unsafe { held(&mut arena, keeper_class("PostedSetRefusedMember")) };
    let mut set = Writer::new();
    refuse_the_next_block();
    assert!(!list(&mut set, member as *mut RcHeader, true));
    assert_eq!(listed(&set), (false, 0), "neither the block nor the member");
    drop(set);

    unsafe { let_go(member) };
}
