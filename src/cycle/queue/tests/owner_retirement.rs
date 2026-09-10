use super::*;
use crate::memory::block_pool::{BLOCK_PAYLOAD, budget_blocks};
use crate::memory::gc_metadata::{self, thread_stats};
use crate::test_support::allocation_probe;

/// Every record owns a distinct allocator-backed object, including the fillers.
fn append_real(arena: &mut Arena, class: *const Class, overflow: bool) -> *mut RcHeader {
    let entity = unsafe { allocated_candidate(arena, class, 1) };
    unsafe {
        crate::refcount::update_header_flags(entity, |flags| flags | CANDIDATE_BIT);
        if overflow {
            append_to_overflow(owner_state(), entity);
        } else {
            if candidate_count() % SEGMENT_CAPACITY == 0 {
                assert!(refill_spares());
            }
            append_entry(owner_state(), entity);
        }
    }
    entity
}

fn held_segments(batch: Option<&InFlightBatch>) -> Vec<*mut BlockHeader> {
    let q = unsafe { owner_state_ref(owner_state()) };
    let mut blocks = Vec::new();
    for head in [
        q.write_segment.get(),
        batch.map_or(std::ptr::null_mut(), |batch| batch.head),
    ] {
        let mut segment = head;
        while !segment.is_null() {
            blocks.push(segment);
            segment = unsafe { (*segment).next };
        }
    }
    for spare in &q.spares[..usize::from(q.spare_count.get())] {
        blocks.push(spare.get());
    }
    blocks
}

fn run_shape(
    detached: usize,
    active: usize,
    overflow: usize,
    dead_stride: usize,
    fault: Option<usize>,
) {
    reset();
    let _ = take_queue_work();
    assert!(refill_spares());
    let baseline = thread_stats().current_bytes_in_use();
    let mut arena = Arena::new();
    let class = candidate_class("OwnerRetirement");
    let mut entities = Vec::new();
    for _ in 0..detached {
        entities.push(append_real(&mut arena, class, false));
    }
    let batch = detach_candidates();
    for _ in 0..active {
        entities.push(append_real(&mut arena, class, false));
    }
    for _ in 0..overflow {
        entities.push(append_real(&mut arena, class, true));
    }
    let mut expected = Vec::new();
    let mut overflow_survivors = 0;
    for (index, &entity) in entities.iter().enumerate() {
        if dead_stride != 0 && index % dead_stride == 0 {
            unsafe { dismantle_candidate(entity) };
        } else {
            expected.push(entity);
            overflow_survivors += usize::from(index >= detached + active);
        }
    }
    let chain_survivors = expected.len() - overflow_survivors;
    let skipped_ledger_bytes = if fault == Some(8) {
        (overflow - overflow_survivors) * size_of::<*mut RcHeader>()
    } else {
        0
    };
    let before_segments = held_segments(Some(&batch));
    let blocks_before = thread_stats().current_blocks();
    // Unlike FORCE_OOM, this refusal counts requests before refusing them.
    let _refused = budget_blocks(0);
    allocation_probe::take_allocations();
    if let Some(point) = fault {
        let _injection = compaction::inject(point);
        let raised = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            compaction::finish(batch, true, None);
        }));
        assert!(
            raised.is_err(),
            "the selected boundary was reached: {point}"
        );
    } else {
        compaction::finish(batch, true, None);
        assert_eq!(allocation_probe::take_allocations(), (0, 0));
        assert_eq!(
            take_queue_work(),
            QueueWork {
                record_passes: 1,
                records_read: entities.len(),
                records_moved: expected.len(),
            },
            "retirement exposes S39.4's final-only baseline"
        );
    }
    let mut actual = Vec::new();
    collect_lane_tokens(&mut actual);
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(
        actual, expected,
        "every surviving registration survives exactly once"
    );
    assert_eq!(candidate_count(), chain_survivors);
    assert_eq!(overflow_len(), overflow_survivors);
    let segments = chain_survivors.div_ceil(SEGMENT_CAPACITY);
    assert_eq!(segment_count(), segments);
    let mut after_segments = held_segments(None);
    let block_count = after_segments.len();
    after_segments.sort_unstable();
    after_segments.dedup();
    assert_eq!(
        after_segments.len(),
        block_count,
        "a segment is stored in exactly one place"
    );
    assert!(
        after_segments
            .iter()
            .all(|block| before_segments.contains(block))
    );
    assert_eq!(
        thread_stats().current_blocks(),
        blocks_before - before_segments.len() + block_count
    );
    if segments != 0 {
        let q = unsafe { owner_state_ref(owner_state()) };
        assert_eq!(
            usize::from(q.write_len.get()),
            (chain_survivors - 1) % SEGMENT_CAPACITY + 1
        );
    }
    assert_eq!(
        thread_stats().current_bytes_in_use(),
        baseline
            + segments.saturating_sub(1) * BLOCK_PAYLOAD
            + overflow_survivors * size_of::<*mut RcHeader>()
            + skipped_ledger_bytes,
        "only final interiors and surviving overflow records stay charged"
    );
    if skipped_ledger_bytes != 0 {
        assert_eq!(
            skipped_ledger_bytes,
            7 * size_of::<*mut RcHeader>(),
            "point 8 deliberately skips the second publish adjustment"
        );
        // Fault injection interrupted the second half of a non-transactional
        // instrument update. Repair that test-only residue so this thread can
        // continue to use the process ledger after proving its exact size.
        gc_metadata::discharge(skipped_ledger_bytes);
    }
    for entity in expected {
        assert_ne!(unsafe { mutator_flags(entity) } & CANDIDATE_BIT, 0);
        unsafe { dismantle_candidate(entity) };
    }
    unsafe { retire_candidates() };
    assert_eq!(candidate_count(), 0);
    assert_eq!(overflow_len(), 0);
    assert_eq!(thread_stats().current_bytes_in_use(), baseline);
    let _ = take_queue_work();
    reset();
}

#[test]
fn both_partial_heads_full_interiors_and_overflow_are_read_at_their_bounds() {
    let _g = test_guard();
    run_shape(SEGMENT_CAPACITY + 3, SEGMENT_CAPACITY + 5, 7, 3, None);
}

#[test]
fn empty_dead_live_and_exact_capacity_outputs_balance_the_ledger() {
    let _g = test_guard();
    for (detached, active, overflow, stride) in [
        (0, 0, 0, 0),
        (0, 0, 7, 2),
        (3, 5, 7, 1),
        (3, 5, 7, 0),
        (SEGMENT_CAPACITY - 3, 3, 0, 0),
        (SEGMENT_CAPACITY + 1, SEGMENT_CAPACITY + 1, 3, 1),
    ] {
        run_shape(detached, active, overflow, stride, None);
    }
}

#[test]
fn every_compaction_boundary_has_an_unwind_owner() {
    let _g = test_guard();
    for point in 0..8 {
        run_shape(3, 5, 7, 2, Some(point));
    }
}

#[test]
fn an_unwind_with_unreversed_segments_keeps_both_halves() {
    let _g = test_guard();
    run_shape(SEGMENT_CAPACITY + 3, SEGMENT_CAPACITY + 5, 7, 3, Some(5));
}

#[test]
fn an_unwind_between_publish_ledger_updates_does_not_repeat_the_first_discharge() {
    let _g = test_guard();
    run_shape(SEGMENT_CAPACITY + 3, SEGMENT_CAPACITY + 5, 7, 1, Some(8));
}

#[test]
fn a_zero_count_without_completed_teardown_is_not_retired() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());
    let mut arena = Arena::new();
    let class = candidate_class("UnfinishedCandidate");
    let entity = append_real(&mut arena, class, false);
    assert!(unsafe { ll_release(entity) });
    unsafe { retire_candidates() };
    assert_eq!(candidate_count(), 1);
    assert_ne!(unsafe { mutator_flags(entity) } & CANDIDATE_BIT, 0);
    unsafe { ll_object_die(entity.cast()) };
    unsafe { retire_candidates() };
    assert_eq!(candidate_count(), 0);
    reset();
}

#[test]
fn a_trace_close_combines_both_candidate_chains_before_retirement() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());
    let mut arena = Arena::new();
    let class = candidate_class("CloseRetirement");
    let first = append_real(&mut arena, class, false);
    let mut window = crate::cycle::deferred_slot_reuse::ActiveTrace::open().unwrap();
    window.detach_candidates();
    let second = append_real(&mut arena, class, false);
    for entity in [first, second] {
        unsafe { dismantle_candidate(entity) };
    }
    drop(window);
    let mut registrations = Vec::new();
    collect_lane_tokens(&mut registrations);
    registrations.sort_unstable();
    let mut expected = [first, second];
    expected.sort_unstable();
    assert_eq!(registrations, expected);
    unsafe { retire_candidates() };
    assert_eq!(candidate_count(), 0);
    reset();
}

#[test]
fn an_unwind_in_final_retirement_lowers_the_collection_gate() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());
    let mut arena = Arena::new();
    let class = candidate_class("RetirementGate");
    let entity = append_real(&mut arena, class, true);
    unsafe { dismantle_candidate(entity) };
    let _injection = compaction::inject(2);
    assert!(
        std::panic::catch_unwind(|| unsafe { crate::cycle::collect::collect_under_pressure() })
            .is_err()
    );
    assert_eq!(overflow_len(), 0);
    let _ = crate::cycle::collect::take_pressure_collections();
    assert_eq!(
        unsafe { crate::cycle::collect::collect_under_pressure() },
        0
    );
    assert_eq!(crate::cycle::collect::take_pressure_collections(), 1);
    reset();
}

/// The disposition that sends records to the deferred lane survives an unwind
/// at every one of its own boundaries: no record is written twice, none is
/// lost, and a record that is both marked and dead takes the retirement rather
/// than the lane.
///
/// The pass the rest of this file exercises defers nothing — it is given
/// `None` — so the arm that appends to the deferred lane, and the boundary
/// that closes it, are reached by nothing else
/// (`PLAN.md` S37.6, and the Critic round of 2026-09-10 that named the hole).
#[test]
fn a_deferring_pass_survives_an_unwind_at_each_of_its_boundaries() {
    for fault in 0..10 {
        reset();
        let mut arena = Arena::new();
        let class = candidate_class("DeferringUnwind");
        let mut entities = Vec::new();
        for _ in 0..6 {
            entities.push(append_real(&mut arena, class, false));
        }

        // Every third record dies before the pass, and every other record is
        // marked, so one record is both — the ranking the Sage's ruling put on
        // the two answers.
        let mut expected = Vec::new();
        for (index, &entity) in entities.iter().enumerate() {
            if index % 3 == 0 {
                unsafe { dismantle_candidate(entity) };
            } else {
                expected.push(entity);
            }
        }
        expected.sort_unstable();

        // The lane's head comes out of a spare cell, and the pass may need one
        // whatever the fault does.
        assert!(refill_spares());
        let mut batch = detach_candidates();
        let mut seen = 0;
        batch.mark_for_deferral(|_| {
            seen += 1;
            seen % 2 == 0
        });

        let _injection = compaction::inject(fault);
        let raised = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            compaction::finish(batch, true, Some(11));
        }));
        // Two boundaries this shape does not reach: 7 returns surplus segments
        // and 8 discharges the overflow buffer's ledger, and six records in one
        // segment leave neither with anything to do. The state below is
        // asserted whether the pass unwound or ran to its end.
        assert!(
            raised.is_err() || fault == 7 || fault == 8,
            "the boundary was reached: {fault}"
        );

        let mut actual = Vec::new();
        collect_lane_tokens(&mut actual);
        actual.sort_unstable();
        assert_eq!(
            actual, expected,
            "every surviving record stands in one lane and in one place, past \
             a fault at {fault}"
        );
        assert_eq!(
            candidate_count() + deferred_count(),
            expected.len(),
            "and the two fills add up to them, past a fault at {fault}"
        );
        assert!(
            deferred_count() > 0,
            "the pass reached its deferred arm, past a fault at {fault}"
        );

        for entity in expected {
            unsafe { dismantle_candidate(entity) };
        }
    }

    reset();
}
