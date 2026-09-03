use super::*;

use crate::class::ClassBuilder;
use crate::cycle::deferred_slot_reuse::ActiveTrace;
use crate::cycle::queue::candidate_count;
use crate::cycle::shadow::Color;
use crate::cycle::testing::{open_arena, row_color};
use crate::memory::arena::Arena;
use crate::memory::block_pool::{force_oom, test_guard};
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, ll_release, ll_retain};
use crate::test_support::{prop_offset, store_prop};

/// Two garbage rings, the second of which points into the first.
///
/// `alpha` holds itself; `beta` holds itself and `alpha`. Both are registered,
/// `alpha` last so that it comes first in the batch — the order in which the
/// interleaved arm below gets the wrong answer.
///
/// The count each row starts from: `alpha` at two, its own edge and `beta`'s,
/// and `beta` at one. Every one of those three edges is internal, so a trace
/// that finds them all leaves both rows at zero, and both rings are what they
/// are — unreachable.
struct TwoRings {
    arena: Arena,
    alpha: *mut Object,
    beta: *mut Object,
}

/// Give every root the queue holds back to the pool, so that the two
/// registrations below are the only ones in the lane and the batch's order is
/// the fixture's. The harness reuses threads, and a queue is per thread.
fn empty_the_queue() {
    crate::cycle::queue::release_queue_segments();
    crate::memory::critical::drain_for_test();
}

/// The same, and then the spare cells filled again: the release above empties
/// them, and a registration that finds no cell and no reserve lands in the
/// overflow buffer, which is a lane the detach does not take.
fn empty_the_queue_and_restock() {
    empty_the_queue();
    assert!(
        crate::cycle::queue::refill_spares(),
        "the growth path is funded"
    );
}

fn two_rings() -> TwoRings {
    empty_the_queue_and_restock();
    let single = ClassBuilder::new("TraceRingSingle")
        .prop("next", true)
        .build();
    let forked = ClassBuilder::new("TraceRingForked")
        .prop("self_edge", true)
        .prop("other", true)
        .build();

    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let alpha = unsafe { new_constructed(&mut context, single, MemoryCategory::GcHeap) };
    let beta = unsafe { new_constructed(&mut context, forked, MemoryCategory::GcHeap) };

    unsafe {
        store_prop(&mut arena, alpha, prop_offset(0), alpha);
        store_prop(&mut arena, beta, prop_offset(0), beta);
        store_prop(&mut arena, beta, prop_offset(1), alpha);
        // Non-final both, so each release registers its entity: `beta` first,
        // which puts `alpha` at the head of the chain and therefore first in
        // the batch.
        assert!(!ll_release(beta as *mut RcHeader));
        assert!(!ll_release(alpha as *mut RcHeader));
    }

    TwoRings { arena, alpha, beta }
}

/// Break both rings and free everything, which is the only way out: the
/// entities hold each other and nothing else does.
fn tear_down(rings: TwoRings) {
    let TwoRings {
        mut arena,
        alpha,
        beta,
    } = rings;
    unsafe {
        ll_retain(alpha as *mut RcHeader);
        ll_retain(beta as *mut RcHeader);
        store_prop(&mut arena, alpha, prop_offset(0), std::ptr::null_mut());
        store_prop(&mut arena, beta, prop_offset(0), std::ptr::null_mut());
        store_prop(&mut arena, beta, prop_offset(1), std::ptr::null_mut());
        for entity in [alpha, beta] {
            assert!(ll_release(entity as *mut RcHeader));
            ll_object_die(entity);
        }
    }
}

/// The rule the module exists for, with the defect it prevents shown first.
///
/// Interleaved — mark a root, scan it, then the next — `alpha` is read as held
/// from outside, because the edge `beta` was about to take off its row had not
/// been taken yet. Marked whole and then scanned whole, both rows read zero and
/// both rings are proposed for teardown.
#[test]
fn every_root_marks_before_any_root_scans() {
    let _g = test_guard();
    let rings = two_rings();
    let (alpha, beta) = (rings.alpha, rings.beta);
    assert_eq!(candidate_count(), 2, "both rings are registered");

    // The control arm, and it is the defect rather than the fix: one arena,
    // the two phases interleaved per root, in the batch's own order.
    let mut interleaved = open_arena();
    unsafe {
        assert_eq!(
            mark(&mut interleaved, alpha as *mut RcHeader),
            MarkResult::Complete
        );
        assert_eq!(
            scan(&mut interleaved, alpha as *mut RcHeader),
            ScanResult::Complete
        );
        assert_eq!(
            mark(&mut interleaved, beta as *mut RcHeader),
            MarkResult::Complete
        );
        assert_eq!(
            scan(&mut interleaved, beta as *mut RcHeader),
            ScanResult::Complete
        );
    }

    assert_eq!(
        unsafe { row_color(alpha as *mut RcHeader) },
        Color::Live,
        "interleaved, the first root's verdict stands on a count two edges short"
    );
    // Dropped rather than reset: the reset kills the rows and the drop is what
    // hands the workspace back, and the trace below cannot open without it.
    drop(interleaved);

    // The fix: both phases over the whole batch, in that order.
    let mut active = ActiveTrace::open().expect("the workspace is warm");
    active.detach_candidates();
    let outcome = {
        let (arena, batch) = active.rows_and_roots();
        unsafe { trace_batch(arena, batch) }
    };
    assert_eq!(outcome, TraceOutcome::Complete);
    assert_eq!(
        unsafe { row_color(alpha as *mut RcHeader) },
        Color::PotentiallyUnreachable,
        "marked whole, the ring holds nothing but itself and `beta`"
    );
    assert_eq!(
        unsafe { row_color(beta as *mut RcHeader) },
        Color::PotentiallyUnreachable
    );

    drop(active);
    assert_eq!(
        candidate_count(),
        2,
        "a trace that disposed of nothing puts both roots back"
    );

    empty_the_queue();
    tear_down(TwoRings {
        arena: rings.arena,
        alpha,
        beta,
    });
}

/// A refusal in the mark ends the trace where it stands: no root scans, no row
/// is coloured, and every root goes back to the lane it came out of.
#[test]
fn a_refusal_in_the_mark_abandons_the_whole_trace() {
    let _g = test_guard();
    let rings = two_rings();
    let (alpha, beta) = (rings.alpha, rings.beta);

    let mut active = ActiveTrace::open().expect("the workspace is warm");
    active.detach_candidates();

    // The arena is emptied to its last byte first, so the refusal lands on the
    // first row array the first root asks for rather than somewhere inside the
    // descent — what is under test here is the propagation, and the partial
    // mark itself is `cycle::mark`'s own.
    {
        let (arena, _) = active.rows_and_roots();
        let room = arena.room_left();
        assert!(!arena.alloc(room).is_null());
    }

    let oom = force_oom();
    let outcome = {
        let (arena, batch) = active.rows_and_roots();
        unsafe { trace_batch(arena, batch) }
    };
    drop(oom);

    assert_eq!(outcome, TraceOutcome::AllocationFailed);
    {
        let (arena, _) = active.rows_and_roots();
        assert_eq!(
            arena.touched_blocks(),
            0,
            "the refusal came before any block's rows were attached"
        );
    }

    drop(active);
    assert_eq!(candidate_count(), 2, "and both roots are registered again");

    empty_the_queue();
    tear_down(TwoRings {
        arena: rings.arena,
        alpha,
        beta,
    });
}
