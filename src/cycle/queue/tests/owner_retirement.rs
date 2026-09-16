//! The retirement pass in ring form: every completed death gives its slot
//! back, every other record stays in its lane in order, and an unwind at any
//! boundary of the pass leaves every lane whole.

use super::*;
use crate::memory::block_pool::{BLOCK_PAYLOAD, budget_blocks};
use crate::memory::gc_metadata::thread_stats;
use crate::test_support::allocation_probe;

/// Every record owns a distinct allocator-backed object, including the fillers.
fn append_real(arena: &mut Arena, class: *const Class, overflow: bool) -> *mut RcHeader {
    let entity = unsafe { allocated_candidate(arena, class, 1) };
    unsafe {
        crate::refcount::update_header_flags(entity, |flags| flags | CANDIDATE_BIT);
        if overflow {
            append_to_overflow(owner_state(), entity);
        } else {
            if spare_count() == 0 {
                assert!(refill_spares());
            }
            append_entry(owner_state(), entity);
        }
    }
    entity
}

/// The ring's records in the ring's order, without the overflow buffer's.
fn ring_tokens() -> Vec<*mut RcHeader> {
    let mut tokens = Vec::new();
    collect_lane_tokens(&mut tokens);
    tokens.truncate(tokens.len() - overflow_len());
    tokens
}

/// One shape of the pass: `read` records in the ring, `behind` registered
/// after a reading of them, `overflow` in the buffer, every `dead_stride`th
/// one dead in place before the pass, and an unwind injected at `fault`.
fn run_shape(
    read: usize,
    behind: usize,
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
    for _ in 0..read {
        entities.push(append_real(&mut arena, class, false));
    }
    drop(read_batch());
    for _ in 0..behind {
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
            overflow_survivors += usize::from(index >= read + behind);
        }
    }
    let ring_survivors = expected.len() - overflow_survivors;
    let blocks_before = segment_count();
    let spares_before = spare_count();
    let out_before = thread_stats().current_blocks();
    // Unlike FORCE_OOM, this refusal counts requests before refusing them.
    let _refused = budget_blocks(0);
    allocation_probe::take_allocations();
    if let Some(point) = fault {
        let _injection = compaction::inject(point);
        let raised = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            retire_candidates();
        }));
        assert!(
            raised.is_err(),
            "the selected boundary was reached: {point}"
        );

        // What the unwind left: every survivor once, nothing that was not
        // registered, and no record twice. The dead ones the pass had not
        // reached stand as they were, and the next pass retires them.
        let mut left = Vec::new();
        collect_lane_tokens(&mut left);
        left.sort_unstable();
        let mut deduplicated = left.clone();
        deduplicated.dedup();
        assert_eq!(
            left, deduplicated,
            "no record twice, past a fault at {point}"
        );
        for entity in &expected {
            assert!(
                left.contains(entity),
                "a survivor is gone, past a fault at {point}"
            );
        }
        for entity in &left {
            assert!(
                entities.contains(entity),
                "a record nobody registered, past a fault at {point}"
            );
        }
        unsafe { retire_candidates() };
    } else {
        unsafe { retire_candidates() };
        assert_eq!(allocation_probe::take_allocations(), (0, 0));
        assert_eq!(
            take_queue_work(),
            QueueWork {
                record_passes: 1,
                records_read: entities.len(),
                records_moved: expected.len(),
            },
            "one retirement pass reads every record and keeps the survivors"
        );
    }

    let mut actual = Vec::new();
    collect_lane_tokens(&mut actual);
    actual.sort_unstable();
    let mut sorted = expected.clone();
    sorted.sort_unstable();
    assert_eq!(
        actual, sorted,
        "every surviving registration survives exactly once"
    );
    assert_eq!(candidate_count(), ring_survivors);
    assert_eq!(overflow_len(), overflow_survivors);
    assert_eq!(
        ring_tokens(),
        expected[..ring_survivors].to_vec(),
        "and the ring keeps them in the order they were registered"
    );
    assert_eq!(segment_count(), blocks_before, "no block left the circle");
    assert_eq!(
        spare_count(),
        spares_before,
        "and none was taken from a cell"
    );
    assert_eq!(thread_stats().current_blocks(), out_before);
    assert_eq!(
        thread_stats().current_bytes_in_use(),
        baseline + blocks_before * BLOCK_PAYLOAD + overflow_survivors * size_of::<*mut RcHeader>(),
        "the ring's blocks and the surviving overflow records stay charged"
    );

    for entity in expected {
        assert_ne!(unsafe { mutator_flags(entity) } & CANDIDATE_BIT, 0);
        unsafe { dismantle_candidate(entity) };
    }
    unsafe { retire_candidates() };
    assert_eq!(candidate_count(), 0);
    assert_eq!(overflow_len(), 0);
    assert_eq!(
        thread_stats().current_bytes_in_use(),
        baseline + blocks_before * BLOCK_PAYLOAD,
        "an emptied block stays in the circle, charged"
    );
    let _ = take_queue_work();
    reset();
}

#[test]
fn records_across_two_blocks_and_the_overflow_buffer_are_read_to_their_indices() {
    let _g = test_guard();
    run_shape(BLOCK_ENTRIES + 3, BLOCK_ENTRIES + 5, 7, 3, None);
}

#[test]
fn empty_dead_live_and_exact_capacity_shapes_balance_the_ledger() {
    let _g = test_guard();
    for (read, behind, overflow, stride) in [
        (0, 0, 0, 0),
        (0, 0, 7, 2),
        (3, 5, 7, 1),
        (3, 5, 7, 0),
        (BLOCK_ENTRIES - 3, 3, 0, 0),
        (BLOCK_ENTRIES + 1, BLOCK_ENTRIES + 1, 3, 1),
    ] {
        run_shape(read, behind, overflow, stride, None);
    }
}

/// Every boundary of the pass this shape reaches: before anything, after a
/// ring entry is read, between a retired entry's flag clear and its free,
/// between the ring's pass and the overflow buffer's, after an overflow
/// entry is kept, and after everything. Point 3 is the deferred arm's, which
/// a retirement never takes ([`a_deferring_pass_survives_an_unwind_at_each_of_its_boundaries`]).
#[test]
fn every_compaction_boundary_has_an_unwind_owner() {
    let _g = test_guard();
    for point in (0..=compaction::LAST_CHECKPOINT).filter(|&point| point != 3) {
        run_shape(3, 5, 7, 2, Some(point));
    }
}

#[test]
fn an_unwind_inside_a_ring_of_two_blocks_keeps_both_blocks_packed() {
    let _g = test_guard();
    run_shape(BLOCK_ENTRIES + 3, BLOCK_ENTRIES + 5, 7, 3, Some(2));
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

/// A record registered while a trace holds its batch stands behind the
/// batch, and the close keeps it: the retirement that follows reads both.
#[test]
fn a_registration_during_a_trace_stands_behind_its_batch() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());
    let mut arena = Arena::new();
    let class = candidate_class("CloseRetirement");
    let first = append_real(&mut arena, class, false);
    let mut window = crate::cycle::deferred_slot_reuse::ActiveTrace::open().unwrap();
    window.read_candidates();
    let second = append_real(&mut arena, class, false);
    for entity in [first, second] {
        unsafe { dismantle_candidate(entity) };
    }
    drop(window);
    let mut registrations = Vec::new();
    collect_lane_tokens(&mut registrations);
    assert_eq!(registrations, vec![first, second]);
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
/// behind it, are reached by nothing else
/// (`PLAN.md` S37.6, and the Critic round of 2026-09-10 that named the hole).
#[test]
fn a_deferring_pass_survives_an_unwind_at_each_of_its_boundaries() {
    for fault in 0..=compaction::LAST_CHECKPOINT {
        reset();
        let mut arena = Arena::new();
        let class = candidate_class("DeferringUnwind");
        let mut entities = Vec::new();
        for _ in 0..6 {
            entities.push(append_real(&mut arena, class, false));
        }

        // Every third record dies before the pass, and every other record is
        // marked, so one record is both — the ranking the Sage's ruling put on
        // the two answers. A fault before anything moves retires nothing, so
        // the dead ones stand too.
        let mut expected = Vec::new();
        for (index, &entity) in entities.iter().enumerate() {
            if index % 3 == 0 && fault != 0 {
                unsafe { dismantle_candidate(entity) };
            } else {
                expected.push(entity);
            }
        }
        expected.sort_unstable();

        // The lane's block comes out of a spare cell, and the pass may need one
        // whatever the fault does.
        assert!(refill_spares());
        let mut batch = read_batch();
        let mut seen = 0;
        batch.mark_for_deferral(|_| {
            seen += 1;
            seen % 2 == 0
        });

        let _injection = compaction::inject(fault);
        let raised = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            dispose_candidates(batch, 11);
        }));
        // One boundary this shape does not reach: 5 is the overflow buffer's,
        // and six records in the ring leave it nothing to keep. The state
        // below is asserted whether the pass unwound or ran to its end.
        assert!(
            raised.is_err() || fault == 5,
            "the boundary was reached: {fault}"
        );
        // A retirement finishes what the unwind left: the dead records the
        // pass had not reached go, and a mark it left standing is stripped.
        unsafe { retire_candidates() };

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
            "and the two counts add up to them, past a fault at {fault}"
        );
        // The first record is dead and unmarked, so a fault up to its free
        // leaves the lane empty; every later one has passed a marked record.
        assert!(
            deferred_count() > 0 || fault <= 2,
            "the pass reached its deferred arm, past a fault at {fault}"
        );

        for entity in expected {
            unsafe { dismantle_candidate(entity) };
        }
    }

    reset();
}
