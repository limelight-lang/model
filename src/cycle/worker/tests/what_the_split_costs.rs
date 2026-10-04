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
    eprintln!("members | one thread | collector | owner's pause | collector + owner");
    for members in [4_000, 40_000, 400_000] {
        let mut alone = Vec::new();
        let mut collector = Vec::new();
        let mut owner = Vec::new();
        let mut phases = Vec::new();
        for _ in 0..ROUNDS {
            let mut arena = Arena::new();
            unsafe { a_garbage_ring(&mut arena, members) };
            let start = Instant::now();
            let freed = unsafe { ll_gc_collect_cycles() };
            alone.push(start.elapsed());
            assert_eq!(freed, members, "one thread freed the ring");

            let mut arena = Arena::new();
            unsafe { a_garbage_ring(&mut arena, members) };
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
            assert_eq!(freed, members, "the owner freed the ring");
            assert_eq!(
                crate::cycle::trace::take_sets_garbage_whole(),
                1,
                "the set took the fast path"
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
