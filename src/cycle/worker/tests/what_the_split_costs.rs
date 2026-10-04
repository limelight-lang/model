//! The same garbage ring collected two ways, read on one box in one run: by
//! this thread alone, in line over R whole (`ll_gc_collect_cycles`), and split
//! — a collector's batch on a thread of its own finds the ring and posts it,
//! and this thread's collection over P frees it (`ll_gc_maybe_collect`). The
//! collector's part is its serve's wall, read on its thread; the owner's part is
//! the poll's pause.

use super::*;
use crate::class::ClassBuilder;
use crate::cycle::testing::move_prop;
use crate::gc::{ll_gc_collect_cycles, ll_gc_maybe_collect};
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::{Object, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, ll_release, ll_retain};
use crate::test_support::prop_offset;
use std::time::{Duration, Instant};

/// A garbage ring of `members` objects of one counted property and no
/// destructor, one root registered: each member's creation reference goes to
/// its predecessor, so no decrement registers a candidate but the root's.
unsafe fn a_garbage_ring(arena: &mut Arena, members: usize) {
    let class = ClassBuilder::new("SplitRingNode")
        .prop("next", true)
        .build();
    let ring: Vec<*mut Object> = (0..members)
        .map(|_| {
            let mut context = LLContext { arena: &mut *arena };
            unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) }
        })
        .collect();
    for index in 0..members {
        unsafe { move_prop(ring[index], prop_offset(0), ring[(index + 1) % members]) };
    }
    unsafe {
        ll_retain(ring[0] as *mut RcHeader);
        assert!(
            !ll_release(ring[0] as *mut RcHeader),
            "the root is registered"
        );
    }
}

/// A garbage ring of `members` arrays, each a mixed vector holding two
/// integers and the next array, one root registered: the array-bearing form
/// of [`a_garbage_ring`], whose members each carry a body.
unsafe fn a_garbage_ring_of_arrays(members: usize) {
    use crate::value::{Tag, Value};
    let ring: Vec<*mut crate::array::entity::LLArray> = (0..members)
        .map(|_| unsafe { crate::array::entity::ll_array_new(MemoryCategory::GcHeap) })
        .collect();
    for index in 0..members {
        let array = ring[index];
        unsafe {
            assert!(crate::array::testing::push(array, Value::int(1)));
            assert!(crate::array::testing::push(array, Value::int(2)));
            // The next array's creation reference, moved into the slot.
            assert!(crate::array::testing::push(
                array,
                Value::entity(Tag::Array, ring[(index + 1) % members] as *mut RcHeader)
            ));
        }
    }
    unsafe {
        ll_retain(ring[0] as *mut RcHeader);
        assert!(
            !ll_release(ring[0] as *mut RcHeader),
            "the root is registered"
        );
    }
}

unsafe extern "C" fn a_leaf_destructor(_object: *mut Object) {}

/// A garbage ring of `members / 2` clean objects, each holding one leaf of a
/// class with a destructor, one root registered: the split's form, whose
/// ring the collector frees and whose leaves reach the owner proved, the
/// ring's edge into each held.
unsafe fn a_clean_ring_holding_destructed_leaves(arena: &mut Arena, members: usize) {
    let ring_class = ClassBuilder::new("SplitHoldingNode")
        .prop("next", true)
        .prop("held", true)
        .build();
    let leaf_class = ClassBuilder::new("SplitDestructedLeaf")
        .destructor(a_leaf_destructor as *const ())
        .build();
    let new = |arena: &mut Arena, class| {
        let mut context = LLContext { arena: &mut *arena };
        unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) }
    };
    let ring: Vec<*mut Object> = (0..members / 2).map(|_| new(arena, ring_class)).collect();
    for index in 0..ring.len() {
        let leaf = new(arena, leaf_class);
        unsafe {
            move_prop(ring[index], prop_offset(0), ring[(index + 1) % ring.len()]);
            move_prop(ring[index], prop_offset(1), leaf);
        }
    }
    unsafe {
        ll_retain(ring[0] as *mut RcHeader);
        assert!(
            !ll_release(ring[0] as *mut RcHeader),
            "the root is registered"
        );
    }
}

/// The ring a round builds: `LL_PROBE_SHAPE` names it — `objects`, the
/// default, `arrays`, or `held` — and answers the members the owner's poll
/// counts as freed where the collector frees C. In `held` that is the ring
/// alone: the batch's root stands in C, so no root lands in S, which is not
/// posted, and the leaves die by counting at the application of C's drops,
/// which no poll counts.
unsafe fn a_ring_of_the_probes_shape(arena: &mut Arena, members: usize) -> usize {
    match std::env::var("LL_PROBE_SHAPE").as_deref() {
        Ok("arrays") => unsafe { a_garbage_ring_of_arrays(members) },
        Ok("held") => {
            unsafe { a_clean_ring_holding_destructed_leaves(arena, members) };
            return members / 2;
        }
        Ok("objects") | Err(_) => unsafe { a_garbage_ring(arena, members) },
        Ok(other) => panic!("no probe shape {other}"),
    }
    members
}

/// The milliseconds of `wall`.
fn ms(wall: Duration) -> f64 {
    wall.as_secs_f64() * 1e3
}

/// One row a size: the median of three rounds of each way, in ms. The
/// collector's serve includes its request and this thread's consent, which a
/// harness spinning on its byte answers at once.
#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn measure_one_thread_against_the_split() {
    const ROUNDS: usize = 3;
    let _g = test_guard();
    reset_lanes();
    // Under `recycler-over-counts`, `LL_PROBE_MEMBER_CAP` sets the largest set
    // the collector frees itself (`crate::cycle::collector_frees`).
    #[cfg(feature = "recycler-over-counts")]
    if let Ok(cap) = std::env::var("LL_PROBE_MEMBER_CAP") {
        let cap = cap.parse().expect("a member cap");
        let _ = crate::cycle::collector_frees::set_member_cap_for_test(cap);
        eprintln!("member cap {cap}");
    }
    eprintln!(
        "shape {}",
        std::env::var("LL_PROBE_SHAPE").unwrap_or_else(|_| "objects".into())
    );
    eprintln!("members | one thread | collector | owner's pause | collector + owner");
    for members in [4_000, 40_000, 400_000] {
        let mut alone = Vec::new();
        let mut collector = Vec::new();
        let mut owner = Vec::new();
        let mut phases = Vec::new();
        for _ in 0..ROUNDS {
            let mut arena = Arena::new();
            let _ = unsafe { a_ring_of_the_probes_shape(&mut arena, members) };
            let start = Instant::now();
            let freed = unsafe { ll_gc_collect_cycles() };
            alone.push(start.elapsed());
            assert_eq!(freed, members, "one thread freed the ring");

            let mut arena = Arena::new();
            let counted = unsafe { a_ring_of_the_probes_shape(&mut arena, members) };
            unsafe { &*record() }.set_batch_size(1);
            let _ = crate::cycle::trace::take_sets_garbage_whole();
            let (served, wall) = timed_serve();
            assert!(matches!(
                served,
                Served::Batch {
                    roots: 1,
                    complete: true,
                    ..
                }
            ));
            collector.push(wall);
            let _ = testing::take_verdict_collections();
            let start = Instant::now();
            let freed = unsafe { ll_gc_maybe_collect() };
            owner.push(start.elapsed());
            phases.push((start.elapsed(), testing::take_verdict_collections().phases));
            // A C past the cap drops the split, and the owner frees W whole.
            assert!(
                freed == counted || freed == members,
                "the owner freed the ring: {freed} of {members}"
            );
            // The fast path of a set the drain found garbage whole, or under
            // `recycler-over-counts` that of a set proved by its tags, which
            // reads no member's cells.
            #[cfg(not(feature = "recycler-over-counts"))]
            assert_eq!(
                crate::cycle::trace::take_sets_garbage_whole(),
                1,
                "the set took the fast path"
            );
            #[cfg(feature = "recycler-over-counts")]
            assert!(
                crate::cycle::trace::take_sets_proved_by_tags_validated() == 1
                    || crate::cycle::collector_frees::frees_counts().sets > 0,
                "the set took the proved set's path, or the collector freed it"
            );
        }
        let median = |walls: &mut Vec<Duration>| {
            walls.sort_unstable();
            walls[ROUNDS / 2]
        };
        let (alone, collector, owner) = (
            median(&mut alone),
            median(&mut collector),
            median(&mut owner),
        );
        eprintln!(
            "{members} | {:.2} | {:.2} | {:.2} | {:.2}",
            ms(alone),
            ms(collector),
            ms(owner),
            ms(collector + owner),
        );
        // The owner's pause of the median round, by phase: the trace within
        // the set (mark, scan), the membership, the commit's first reading,
        // destructors, second reading with the teardown, drops.
        phases.sort_unstable_by_key(|(wall, _)| *wall);
        let (_, split) = phases[ROUNDS / 2];
        eprintln!(
            "{members} owner by phase: {}",
            split
                .iter()
                .map(|phase| format!("{:.2}", ms(*phase)))
                .collect::<Vec<_>>()
                .join(" | ")
        );
    }
}

/// `the_batch::served_by_a_collector`'s serve with its wall, read on the collector's
/// thread from before the serve to after it.
fn timed_serve() -> (Served, Duration) {
    unsafe { &*record() }.clear_posted_for_test();
    let sent = crate::cycle::testing::Sent(record());
    testing::consent_while(std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the collector thread"
        );
        let start = Instant::now();
        let served = unsafe { testing::serve_alone(sent.into_inner()) };
        (served, start.elapsed())
    }))
}
