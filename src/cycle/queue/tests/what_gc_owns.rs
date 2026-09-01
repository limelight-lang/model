//! The queue's blocks are owned by the GC explicitly, not merely hidden
//! from the entity walker under the generic arena kind.

use super::*;

use crate::memory::block_pool::{
    BLOCK_KIND_GC_METADATA, BLOCK_PAYLOAD, BlockHeader, load_block_kind,
};
use crate::memory::gc_metadata::stats;

fn kind_of(block: *mut BlockHeader) -> u32 {
    unsafe { load_block_kind(&raw const (*block).kind) }
}

#[test]
fn the_floor_is_gc_memory_and_its_control_cost_is_in_the_capacity() {
    let _g = test_guard();
    reset();

    assert_eq!(size_of::<OwnerCycleState>(), 64);
    assert_eq!(align_of::<OwnerCycleState>(), 64);
    assert_eq!(
        ESCROW_ENTRIES,
        (BLOCK_PAYLOAD - 64) / size_of::<*mut RcHeader>()
    );
    assert_eq!(POLL_STRIDE, ESCROW_ENTRIES / 2);
    assert!(POLL_STRIDE * 2 <= ESCROW_ENTRIES);
    assert!(SEGMENT_CAPACITY > ESCROW_ENTRIES);

    let held = floor();
    assert!(!held.is_null());
    assert_eq!(kind_of(held), BLOCK_KIND_GC_METADATA);
    assert!(stats().current_blocks() >= 1);
}

#[test]
fn a_spare_stays_one_accounted_segment_when_it_becomes_live() {
    let _g = test_guard();
    reset();
    assert!(replenish());

    let before = stats().current_blocks();

    let mut header = candidate(2);
    assert!(unsafe { !release(&raw mut header) });

    assert_eq!(
        stats().current_blocks(),
        before,
        "spare to live is a state transition, not a second acquisition"
    );
    assert_eq!(kind_of(live_segment()), BLOCK_KIND_GC_METADATA);

    reset();
}

#[test]
fn the_floor_accepts_its_exact_rederived_escrow_capacity() {
    let _g = test_guard();
    reset();
    let owner = owner();
    let mut header = candidate(2);

    for _ in 0..ESCROW_ENTRIES {
        unsafe { escrow(owner, &raw mut header) };
    }
    assert_eq!(escrowed_count(), ESCROW_ENTRIES);

    reset();
}

#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn one_entry_past_the_escrow_capacity_aborts_before_writing() {
    const CHILD: &str = "LL_QUEUE_ESCROW_OVERFLOW_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let _g = test_guard();
        reset();
        let owner = owner();
        let mut header = candidate(2);
        for _ in 0..=ESCROW_ENTRIES {
            unsafe { escrow(owner, &raw mut header) };
        }
        return;
    }

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "cycle::queue::tests::what_gc_owns::one_entry_past_the_escrow_capacity_aborts_before_writing",
        ])
        .env(CHILD, "1")
        .output()
        .expect("the test binary runs as its own overflow child");
    assert!(
        !output.status.success(),
        "capacity plus one returned and wrote beyond the floor"
    );
}

#[test]
fn the_entity_row_dispatch_never_enters_gc_metadata() {
    let _g = test_guard();
    reset();
    let pretend_child = BlockHeader::payload_start(floor()) as *mut RcHeader;

    assert_eq!(
        unsafe { crate::cycle::row::edge_to(pretend_child) },
        crate::cycle::row::Edge::External
    );
}
