//! An `Unwalked` verdict names a root the collector took from R and never
//! walked. The collection over P, which `POSTED` arms, writes it back into R
//! untraced, its candidate bit still set, so that the collector's next batch
//! takes it again; every collection over R whole — the explicit fire, the
//! pressure path, the exit's rounds — reads P into its batch and traces it.
//!
//! The stand-in collector is `cycle::queue::verdicts::testing::post_batch`,
//! which claims, takes from R, posts and releases as the collector's batch
//! does, less the trace.

use super::*;
use crate::cycle::queue::verdicts::verdict_count;
use crate::cycle::queue::{candidate_count, deferred_count};
use crate::cycle::token::{FREE, POSTED, state};
use crate::refcount::{CANDIDATE_BIT, mutator_flags};

fn byte() -> u8 {
    unsafe {
        (*crate::cycle::mutator_record::this_thread_record())
            .token
            .read()
    }
}

fn reset() {
    crate::cycle::queue::verdicts::discard_standing_verdicts();
    crate::cycle::queue::release_queue_segments();
    crate::gc::disarm();
}

#[test]
fn the_collection_over_p_writes_an_unwalked_root_back_into_r_untraced() {
    let _g = test_guard();
    reset();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let node = node_class("UnwalkedWrittenBack", counting_destructor as *const ());
    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, [node, node]) };
    assert_eq!(stand_in_posts(2, Verdict::Unwalked), Posted::Batch(2));
    assert_eq!(state(byte()), POSTED);
    assert_eq!(candidate_count(), 0, "the batch took both roots out of R");

    let census = crate::cycle::census::arm();
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        0,
        "the collection over P freed nothing"
    );
    let report = crate::cycle::census::take();
    drop(census);
    let scan = report.scan.expect("the collection over P ran its trace");
    assert_eq!(scan.roots, 0, "neither unwalked root was a root of it");
    assert_eq!(scan.mark_dispatches, 0, "and no mark ran");
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 0);

    assert_eq!(state(byte()), FREE, "the close released FREE");
    assert_eq!(verdict_count(), 0, "P's front advanced past both");
    assert_eq!(deferred_count(), 0, "no reading deferred them");
    assert_eq!(
        candidate_count(),
        2,
        "both were written back into R as registrations"
    );
    for member in members {
        assert_ne!(
            unsafe { mutator_flags(member as *mut RcHeader) } & CANDIDATE_BIT,
            0,
            "with the candidate bit still set"
        );
    }

    // A collection over R whole takes them from there.
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 2);
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 2);
    reset();
}

#[test]
fn the_explicit_fire_traces_an_unwalked_root_out_of_p() {
    let _g = test_guard();
    reset();
    let node = node_class("UnwalkedFired", counting_destructor as *const ());
    let mut arena = Arena::new();
    let _members = unsafe { ring(&mut arena, [node, node]) };
    assert_eq!(stand_in_posts(2, Verdict::Unwalked), Posted::Batch(2));
    assert_eq!(candidate_count(), 0);

    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        2,
        "the fire read P into its batch and traced both"
    );
    assert_eq!(verdict_count(), 0);
    assert_eq!(candidate_count(), 0);
    reset();
}

/// Sixty-three rings of five, every member's verdict `Unwalked` and R empty:
/// the pressure path finds the rings through P alone.
#[test]
fn the_pressure_path_traces_every_unwalked_root_out_of_p() {
    const RINGS: usize = 63;
    let _g = test_guard();
    reset();
    let node = node_class("UnwalkedUnderPressure", counting_destructor as *const ());
    let mut arena = Arena::new();
    let rings: Vec<[*mut Object; 5]> = (0..RINGS)
        .map(|_| unsafe { ring(&mut arena, [node; 5]) })
        .collect();
    assert_eq!(
        stand_in_posts(RINGS * 5, Verdict::Unwalked),
        Posted::Batch(RINGS * 5)
    );
    assert_eq!(candidate_count(), 0);

    crate::cycle::collect::count_pressure_roots(true);
    let _ = crate::cycle::collect::take_pressure_roots_traced();
    let freed = unsafe { crate::cycle::collect::collect_under_pressure() };
    let traced = crate::cycle::collect::take_pressure_roots_traced();
    crate::cycle::collect::count_pressure_roots(false);
    assert_eq!(freed, RINGS * 5, "every ring was torn down");
    assert_eq!(
        traced,
        RINGS * 5,
        "every unwalked root was a root of the trace"
    );
    assert_eq!(verdict_count(), 0);
    let _ = rings;
    crate::gc::disarm();
    reset();
}

/// The exit leaves no unwalked root behind. Its rounds are collections over
/// R whole, so the first traces the root out of P; a first round that wrote
/// it back into R instead would free it in the second, and the residue reads
/// the same either way, which is why the round is pinned by the explicit
/// fire's case above rather than here.
#[test]
fn the_exit_leaves_no_unwalked_root_behind() {
    let _g = test_guard();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let class = Sent(node_class(
        "UnwalkedAtExit",
        counting_destructor as *const (),
    ));
    let residue = std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served this thread"
        );
        let class = class.into_inner();
        let mut arena = Arena::new();
        let _members = unsafe { ring(&mut arena, [class, class]) };
        drop(arena);
        assert_eq!(stand_in_posts(2, Verdict::Unwalked), Posted::Batch(2));
        assert_eq!(candidate_count(), 0, "R is empty; the roots stand in P");

        crate::memory::heap::ll_thread_exit();
        crate::cycle::collect::take_exit_residue().expect("the exit ran its collection")
    })
    .join()
    .unwrap();
    assert_eq!(residue.freed, 2, "the exit's rounds freed both");
    assert_eq!(residue.registered, 0);
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 2);
}

/// An unwalked root inside a proposal's closure is traced by the collection
/// over P all the same, as a member of that closure, and may read live there;
/// it is still no root of that collection, so the close writes it back into R
/// rather than deferring it on that reading, while the proposal it read live
/// is deferred.
#[test]
fn an_unwalked_root_a_proposal_reaches_is_written_back_rather_than_deferred() {
    let _g = test_guard();
    reset();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    assert!(crate::cycle::queue::refill_spares());
    let node = node_class("UnwalkedReached", counting_destructor as *const ());
    let mut arena = Arena::new();
    let (proposed, reached) = {
        let mut context = LLContext { arena: &mut arena };
        unsafe {
            (
                new_constructed(&mut context, node, MemoryCategory::GcHeap),
                new_constructed(&mut context, node, MemoryCategory::GcHeap),
            )
        }
    };
    unsafe {
        store_prop(&mut arena, proposed, prop_offset(0), reached);
        // Registered at its creation reference's release, `proposed` holding
        // it; then `proposed`, by a retain and a release, the case keeping
        // its creation reference.
        assert!(!ll_release(reached as *mut RcHeader));
        ll_retain(proposed as *mut RcHeader);
        assert!(!ll_release(proposed as *mut RcHeader));
    }
    assert_eq!(candidate_count(), 2);

    // R in order: `reached`, then `proposed`.
    let record = Sent(crate::cycle::mutator_record::this_thread_record());
    let posted = std::thread::spawn(move || {
        let mut verdicts = [Verdict::Unwalked, Verdict::Proposed].into_iter();
        unsafe {
            post_batch(record.into_inner(), 2, |_| {
                verdicts.next().expect("one verdict per root")
            })
        }
    })
    .join()
    .expect("the stand-in finished");
    assert_eq!(posted, Posted::Batch(2));

    assert_eq!(unsafe { ll_gc_maybe_collect() }, 0, "the case holds both");
    assert_eq!(deferred_count(), 1, "the proposal, read live, was deferred");
    assert_eq!(
        candidate_count(),
        1,
        "the unwalked root was written back into R"
    );

    unsafe {
        assert!(ll_release(proposed as *mut RcHeader));
        ll_object_die(proposed);
    }
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 2);
    reset();
}
