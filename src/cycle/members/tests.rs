use super::*;

use crate::class::ClassBuilder;
use crate::cycle::deferred_slot_reuse::ActiveTrace;
use crate::cycle::testing::open_arena;
use crate::cycle::trace::{TraceOutcome, trace_batch};
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, ll_release, ll_retain};
use crate::test_support::{POOLED_FILLERS, prop_offset, store_prop, wide_class};

/// Two garbage rings of one entity each, registered as candidates: `alpha`
/// holds itself and `beta` holds itself, so a trace over both leaves two rows
/// at zero and two entities unreachable.
///
/// **Two populations and two blocks**, which is what makes a case about the
/// sweep's walk a case at all: `alpha` is a slot of an ordinary entity block
/// and its row stands in that block's array, `beta` fills a pooled
/// large-entity block whose one row is a word of its own header. So the
/// touched list carries two arrays, both arms of the harvest run, and a claim
/// about what the sweep still owes past a refusal has a second block to be
/// made over.
struct TwoRings {
    arena: Arena,
    alpha: *mut Object,
    beta: *mut Object,
}

/// Give every root the queue holds back, so the two registrations below are
/// the only ones in the lane: the harness reuses threads, and a queue is per
/// thread.
fn two_rings() -> TwoRings {
    crate::cycle::queue::release_queue_segments();
    crate::memory::critical::drain_for_test();
    assert!(
        crate::cycle::queue::refill_spares(),
        "the growth path is funded"
    );

    let slotted = ClassBuilder::new("MemberRing").prop("next", true).build();
    let wide = wide_class("MemberWideRing", POOLED_FILLERS, None);
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let alpha = unsafe { new_constructed(&mut context, slotted, MemoryCategory::GcHeap) };
    let beta = unsafe { new_constructed(&mut context, wide, MemoryCategory::GcHeap) };

    unsafe {
        store_prop(&mut arena, alpha, prop_offset(0), alpha);
        store_prop(&mut arena, beta, prop_offset(0), beta);
        // Non-final both, so each release registers its entity.
        assert!(!ll_release(alpha as *mut RcHeader));
        assert!(!ll_release(beta as *mut RcHeader));
    }

    TwoRings { arena, alpha, beta }
}

/// Break both rings and free everything, which is the only way out: each
/// entity holds itself and nothing else does.
///
/// **The candidate bit is cleared by hand first**, as every case of this crate
/// that frees a registered entity does: `ll_free`'s candidate arm withholds the
/// slot of anything whose bit still stands, and nothing in production retires a
/// record yet (`PLAN.md` S36.5 and S39.1).
fn tear_down(rings: TwoRings) {
    let TwoRings {
        mut arena,
        alpha,
        beta,
    } = rings;
    unsafe {
        for entity in [alpha, beta] {
            ll_retain(entity as *mut RcHeader);
            store_prop(&mut arena, entity, prop_offset(0), std::ptr::null_mut());
            crate::refcount::clear_candidate_bit(entity as *mut RcHeader);
            assert!(ll_release(entity as *mut RcHeader));
            ll_object_die(entity);
        }
    }
}

/// Trace both rings under an open window and close it, arming a harvest of
/// `capacity` first where one is asked for. The window's drop is what sweeps,
/// so the list stands only after this returns.
fn trace_and_close(capacity: Option<u32>) {
    let mut active = ActiveTrace::open().expect("the guard drew this thread's workspace");
    active.detach_candidates();
    let outcome = {
        let (arena, batch) = active.rows_and_roots();
        unsafe { trace_batch(arena, batch) }
    };
    assert_eq!(outcome, TraceOutcome::Complete);

    if let Some(capacity) = capacity {
        assert!(
            active.arm_harvest(capacity),
            "no list stands on this thread"
        );
    }
}

mod what_a_pressure_close_takes;
mod what_the_region_holds;
