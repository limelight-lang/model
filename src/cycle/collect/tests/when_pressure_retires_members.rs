//! The successful pressure teardown's split between member retirement and
//! external-child drops, including S39.4's reproducible final-only comparison.

use super::*;
use crate::cycle::testing::dismantle_ring;
use crate::memory::block_pool::{BLOCK_PAYLOAD, BLOCK_SIZE, BlockHeader, budget_blocks};
use crate::memory::heap::{SIZE_CLASSES, entity_alloc, size_class_index};
use crate::memory::stdapi::free_unpublished;
use crate::object::ll_object_die;
use crate::refcount::SlotState;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static REQUESTED_SIZE: AtomicUsize = AtomicUsize::new(0);
static ALLOCATION: AtomicUsize = AtomicUsize::new(0);
static ALLOCATION_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
static BLOCKS_AT_DESTRUCTOR: AtomicUsize = AtomicUsize::new(0);
static RESET_ARENA: AtomicUsize = AtomicUsize::new(0);
static CANDIDATE_CLASS: AtomicUsize = AtomicUsize::new(0);
static DESTRUCTOR_MODE: AtomicUsize = AtomicUsize::new(0);
static RESURRECTED_MEMBER: AtomicUsize = AtomicUsize::new(0);
static EXTERNAL_CHILD_ROOT: AtomicUsize = AtomicUsize::new(0);

const RESET: usize = 1;
const CREATE_COMPLETED_CANDIDATE: usize = 2;

unsafe extern "C" fn allocating_child_destructor(_object: *mut Object) {
    BLOCKS_AT_DESTRUCTOR.store(
        crate::memory::block_pool::BlockPool::global().blocks_out(),
        Ordering::Relaxed,
    );
    ALLOCATION_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
    let allocation = unsafe { entity_alloc(REQUESTED_SIZE.load(Ordering::Relaxed)) };
    ALLOCATION.store(allocation as usize, Ordering::Relaxed);

    let mode = DESTRUCTOR_MODE.load(Ordering::Relaxed);
    if mode & CREATE_COMPLETED_CANDIDATE != 0 {
        let arena = RESET_ARENA.load(Ordering::Relaxed) as *mut Arena;
        let class = CANDIDATE_CLASS.load(Ordering::Relaxed) as *const Class;
        let mut context = LLContext { arena };
        let candidate = unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) };
        unsafe { ll_retain(candidate as *mut RcHeader) };
        assert!(!unsafe { ll_release(candidate as *mut RcHeader) });
        assert!(unsafe { ll_release(candidate as *mut RcHeader) });
        unsafe { ll_object_die(candidate) };
    }
    if mode & RESET != 0 {
        let arena = RESET_ARENA.load(Ordering::Relaxed) as *mut Arena;
        unsafe { crate::promote::arena_reset_full(arena) };
    }
}

unsafe extern "C" fn allocation_free_child_destructor(_object: *mut Object) {
    BLOCKS_AT_DESTRUCTOR.store(
        crate::memory::block_pool::BlockPool::global().blocks_out(),
        Ordering::Relaxed,
    );
}

unsafe extern "C" fn resurrecting_member_destructor(object: *mut Object) {
    unsafe { ll_retain(object as *mut RcHeader) };
    RESURRECTED_MEMBER.store(object as usize, Ordering::Relaxed);
}

unsafe extern "C" fn releasing_child_root(_object: *mut Object) {
    let child = EXTERNAL_CHILD_ROOT.swap(0, Ordering::Relaxed) as *mut RcHeader;
    assert!(
        !child.is_null(),
        "one member owns the fixture's external root"
    );
    assert!(
        !unsafe { ll_release(child) },
        "the member's edge still holds the child until the sever"
    );
}

fn exact_class(name: &str, properties: usize, destructor: *const ()) -> *const Class {
    let names: Vec<String> = (0..properties).map(|index| format!("p{index}")).collect();
    let mut builder = ClassBuilder::new(name);
    for property in &names {
        builder = builder.prop(property, true);
    }
    if !destructor.is_null() {
        builder = builder.destructor(destructor);
    }

    let class = builder.build();
    let size = unsafe { (*class).object_size } as usize;
    let class_index = size_class_index(size).expect("a heap size class serves the fixture");
    assert_eq!(SIZE_CLASSES[class_index], size);
    class
}

unsafe fn stage_empty_block(size: usize) -> Vec<*mut u8> {
    let capacity = BLOCK_PAYLOAD / size;
    let mut held = Vec::with_capacity(capacity + 1);
    let first = unsafe { entity_alloc(size) };
    assert!(!first.is_null());
    let first_block = BlockHeader::of_ptr(first);
    held.push(first);
    loop {
        let slot = unsafe { entity_alloc(size) };
        assert!(!slot.is_null(), "the unrestricted pool stages the fixture");
        if BlockHeader::of_ptr(slot) != first_block {
            unsafe { free_unpublished(slot) };
            return held;
        }
        held.push(slot);
        assert!(held.len() <= capacity);
    }
}

#[derive(Clone, Copy, Debug)]
struct Reading {
    returned_slots: usize,
    returned_bytes: usize,
    reused_slots: usize,
    allocations_served: usize,
    allocations_refused: usize,
    queue: crate::cycle::queue::QueueWork,
    peak_bytes: usize,
    elapsed: Duration,
}

unsafe fn run(
    final_only: bool,
    child: Option<*const Class>,
    requested_size: usize,
    mode: usize,
) -> Reading {
    crate::cycle::queue::release_queue_segments();
    let member_class = exact_class(
        "PressureRetirementMember",
        63,
        releasing_child_root as *const (),
    );
    let silent_member_class = exact_class("PressureRetirementSilentMember", 63, std::ptr::null());
    let base_blocks = crate::memory::block_pool::BlockPool::global().blocks_out();
    let member_size = unsafe { (*member_class).object_size } as usize;
    let capacity = BLOCK_PAYLOAD / member_size;
    let mut held = unsafe { stage_empty_block(member_size) };
    if requested_size != member_size {
        held.extend(unsafe { stage_empty_block(requested_size) });
        let requested_capacity = BLOCK_PAYLOAD / requested_size;
        for _ in 0..requested_capacity {
            let slot = unsafe { entity_alloc(requested_size) };
            assert!(!slot.is_null());
            held.push(slot);
        }
    }

    let mut arena = Arena::new();
    let classes = if child.is_some() {
        [member_class, silent_member_class]
    } else {
        [silent_member_class, silent_member_class]
    };
    let [first, second] = unsafe { ring(&mut arena, classes) };
    assert_eq!(
        BlockHeader::of_ptr(first.cast()),
        BlockHeader::of_ptr(second.cast())
    );

    let mut fillers = Vec::with_capacity(capacity - 2);
    let mut context = LLContext { arena: &mut arena };
    for _ in 0..capacity - 2 {
        fillers.push(unsafe {
            new_constructed(&mut context, silent_member_class, MemoryCategory::GcHeap)
        });
    }

    if let Some(child_class) = child {
        let external =
            unsafe { new_constructed(&mut context, child_class, MemoryCategory::GcHeap) };
        unsafe { store_prop(&mut arena, first, prop_offset(1), external) };
        EXTERNAL_CHILD_ROOT.store(external as usize, Ordering::Relaxed);
    }

    REQUESTED_SIZE.store(requested_size, Ordering::Relaxed);
    ALLOCATION.store(0, Ordering::Relaxed);
    ALLOCATION_ATTEMPTS.store(0, Ordering::Relaxed);
    BLOCKS_AT_DESTRUCTOR.store(0, Ordering::Relaxed);
    RESET_ARENA.store((&raw mut arena) as usize, Ordering::Relaxed);
    CANDIDATE_CLASS.store(silent_member_class as usize, Ordering::Relaxed);
    DESTRUCTOR_MODE.store(mode, Ordering::Relaxed);

    let _final_only = final_only.then(FinalOnlyRetirement::take);
    let _budget = budget_blocks(0);
    let _ = crate::cycle::queue::take_queue_work();
    let _ = take_early_returned_slots();
    let blocks_before = crate::memory::block_pool::BlockPool::global().blocks_out();
    let start = Instant::now();
    let freed = unsafe { collect_under_pressure() };
    let elapsed = start.elapsed();
    let queue = crate::cycle::queue::take_queue_work();
    let returned_slots = take_early_returned_slots();
    let blocks_at_destructor = BLOCKS_AT_DESTRUCTOR.load(Ordering::Relaxed);
    let blocks_after = crate::memory::block_pool::BlockPool::global().blocks_out();
    drop(_budget);
    drop(_final_only);
    assert_eq!(freed, 2);

    let allocation = ALLOCATION.swap(0, Ordering::Relaxed) as *mut u8;
    let allocation_attempts = ALLOCATION_ATTEMPTS.swap(0, Ordering::Relaxed);
    let members = [first as *mut u8, second as *mut u8];
    let reused_slots = usize::from(members.contains(&allocation));
    if !allocation.is_null() {
        unsafe { free_unpublished(allocation) };
    }
    for filler in fillers {
        assert!(unsafe { ll_release(filler as *mut RcHeader) });
        unsafe { ll_object_die(filler) };
    }
    for slot in held {
        unsafe { free_unpublished(slot) };
    }
    assert_eq!(EXTERNAL_CHILD_ROOT.load(Ordering::Relaxed), 0);

    Reading {
        returned_slots,
        returned_bytes: returned_slots * member_size,
        reused_slots,
        allocations_served: usize::from(!allocation.is_null()),
        allocations_refused: allocation_attempts - usize::from(!allocation.is_null()),
        queue,
        peak_bytes: blocks_before
            .max(blocks_at_destructor)
            .max(blocks_after)
            .saturating_sub(base_blocks)
            * BLOCK_SIZE,
        elapsed,
    }
}

#[test]
fn an_external_childs_destructor_can_allocate_from_an_early_member_slot() {
    let _guard = test_guard();
    let child = exact_class(
        "PressureRetirementAllocatingChild",
        0,
        allocating_child_destructor as *const (),
    );
    let member_size =
        unsafe { (*exact_class("PressureRetirementSize", 63, std::ptr::null())).object_size }
            as usize;

    let baseline = unsafe { run(true, Some(child), member_size, 0) };
    let early = unsafe { run(false, Some(child), member_size, 0) };
    assert_eq!(
        (
            baseline.allocations_served,
            baseline.allocations_refused,
            baseline.returned_slots,
            baseline.returned_bytes,
            baseline.reused_slots,
        ),
        (0, 1, 0, 0, 0)
    );
    assert_eq!(
        (
            early.allocations_served,
            early.allocations_refused,
            early.returned_slots,
            early.returned_bytes,
            early.reused_slots,
        ),
        (1, 0, 2, 2 * member_size, 1)
    );
    assert!(baseline.peak_bytes > 0 && early.peak_bytes > 0);
    assert_eq!(baseline.queue.record_passes, 2);
    assert_eq!(early.queue.record_passes, 3);
}

#[test]
fn a_reset_and_a_new_completed_death_are_safe_across_the_external_drain() {
    let _guard = test_guard();
    let child = exact_class(
        "PressureRetirementResettingChild",
        0,
        allocating_child_destructor as *const (),
    );
    let member_size =
        unsafe { (*exact_class("PressureRetirementResetSize", 63, std::ptr::null())).object_size }
            as usize;
    let reading = unsafe {
        run(
            false,
            Some(child),
            member_size,
            RESET | CREATE_COMPLETED_CANDIDATE,
        )
    };
    assert_eq!(reading.allocations_served, 1);
    assert_eq!(crate::cycle::queue::candidate_count(), 0);
}

#[test]
fn a_resurrection_takes_only_the_final_retirement() {
    let _guard = test_guard();
    let resurrecting = node_class(
        "PressureRetirementResurrectingMember",
        resurrecting_member_destructor as *const (),
    );
    let silent = node_class(
        "PressureRetirementSilentMember",
        counting_destructor as *const (),
    );
    let mut arena = Arena::new();
    let ring = unsafe { ring(&mut arena, [resurrecting, silent]) };
    RESURRECTED_MEMBER.store(0, Ordering::Relaxed);
    let _ = crate::cycle::queue::take_queue_work();
    assert_eq!(unsafe { collect_under_pressure() }, 0);
    assert_eq!(crate::cycle::queue::take_queue_work().record_passes, 2);

    let kept = RESURRECTED_MEMBER.swap(0, Ordering::Relaxed) as *mut RcHeader;
    assert!(!kept.is_null());
    unsafe {
        assert!(!ll_release(kept));
        dismantle_ring(&mut arena, ring);
    }
}

#[test]
fn a_refused_drop_reservation_takes_no_early_retirement() {
    let _guard = test_guard();
    let releasing = ClassBuilder::new("PressureRetirementRefusedMember")
        .prop("next", true)
        .prop("child", true)
        .destructor(releasing_child_root as *const ())
        .build();
    let silent = ClassBuilder::new("PressureRetirementRefusedPeer")
        .prop("next", true)
        .prop("child", true)
        .destructor(counting_destructor as *const ())
        .build();
    let child_class = ClassBuilder::new("PressureRetirementRefusedChild").build();
    let mut arena = Arena::new();
    let ring = unsafe { ring(&mut arena, [releasing, silent]) };
    let mut context = LLContext { arena: &mut arena };
    let child = unsafe { new_constructed(&mut context, child_class, MemoryCategory::GcHeap) };
    unsafe { store_prop(&mut arena, ring[0], prop_offset(1), child) };
    EXTERNAL_CHILD_ROOT.store(child as usize, Ordering::Relaxed);

    let _refusal = crate::cycle::arena::refuse_drop_reservation();
    let _ = crate::cycle::queue::take_queue_work();
    assert_eq!(unsafe { collect_under_pressure() }, 0);
    assert_eq!(crate::cycle::queue::take_queue_work().record_passes, 2);
    assert_eq!(EXTERNAL_CHILD_ROOT.load(Ordering::Relaxed), 0);
    for member in ring {
        assert_eq!(
            unsafe { crate::refcount::slot_state(member as *mut RcHeader) },
            SlotState::Live
        );
    }

    unsafe { dismantle_ring(&mut arena, ring) };
    unsafe { crate::cycle::queue::retire_candidates() };
}

#[test]
#[ignore = "S39.4 measurement; recorded in dev/BENCHMARKS.md"]
fn measure_early_pressure_retirement() {
    let _guard = test_guard();
    let allocating = exact_class(
        "PressureRetirementMeasuredChild",
        0,
        allocating_child_destructor as *const (),
    );
    let quiet = exact_class(
        "PressureRetirementQuietChild",
        0,
        allocation_free_child_destructor as *const (),
    );
    let member_size = unsafe {
        (*exact_class("PressureRetirementMeasuredSize", 63, std::ptr::null())).object_size
    } as usize;
    let nonmatching_size = SIZE_CLASSES[size_class_index(member_size).unwrap() - 1];

    for (name, child, requested) in [
        ("no-child", None, member_size),
        ("quiet-child", Some(quiet), member_size),
        ("matching", Some(allocating), member_size),
        ("nonmatching", Some(allocating), nonmatching_size),
    ] {
        let mut final_only = Vec::new();
        let mut early = Vec::new();
        for round in 0..31 {
            let (first_final, second_final) = if round % 2 == 0 {
                (true, false)
            } else {
                (false, true)
            };
            let first = unsafe { run(first_final, child, requested, 0) };
            let second = unsafe { run(second_final, child, requested, 0) };
            if first_final {
                final_only.push(first);
                early.push(second);
            } else {
                early.push(first);
                final_only.push(second);
            }
        }

        final_only.sort_by_key(|reading| reading.elapsed);
        early.sort_by_key(|reading| reading.elapsed);
        let baseline = final_only[final_only.len() / 2];
        let early = early[early.len() / 2];
        eprintln!(
            "{name}: final-only returned-slots={} returned-bytes={} reused-slots={} served={} refused={} peak-bytes={} passes={} read={} moved={} time-ns={}; early returned-slots={} returned-bytes={} reused-slots={} served={} refused={} peak-bytes={} passes={} read={} moved={} time-ns={}",
            baseline.returned_slots,
            baseline.returned_bytes,
            baseline.reused_slots,
            baseline.allocations_served,
            baseline.allocations_refused,
            baseline.peak_bytes,
            baseline.queue.record_passes,
            baseline.queue.records_read,
            baseline.queue.records_moved,
            baseline.elapsed.as_nanos(),
            early.returned_slots,
            early.returned_bytes,
            early.reused_slots,
            early.allocations_served,
            early.allocations_refused,
            early.peak_bytes,
            early.queue.record_passes,
            early.queue.records_read,
            early.queue.records_moved,
            early.elapsed.as_nanos(),
        );
    }
}
