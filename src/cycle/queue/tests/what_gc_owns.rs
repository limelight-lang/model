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

    // The figures the commit message, `PLAN.md`, `docs/memory-manager.md`
    // and `dev/BENCHMARKS.md` all name. Written out rather than derived
    // through the expressions that define them: a test that recomputes a
    // constant agrees with whatever the constant becomes.
    assert_eq!(size_of::<OwnerCycleState>(), 64);
    assert_eq!(align_of::<OwnerCycleState>(), 64);
    assert_eq!(SEGMENT_CAPACITY, 8_160);
    assert_eq!(ESCROW_ENTRIES, 8_152);
    assert_eq!(POLL_STRIDE, 4_076);

    // The escrow ends flush with the block: one control line and the
    // entries account for the payload exactly, with no tail to absorb an
    // off-by-one and nothing of a neighbour within reach.
    assert_eq!(
        size_of::<OwnerCycleState>() + ESCROW_ENTRIES * size_of::<*mut RcHeader>(),
        BLOCK_PAYLOAD
    );

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

    // What makes the capacity exact rather than merely sufficient: the
    // entry past the last one is the first byte of the next block. A
    // capacity one too large would fill without complaint on stable and
    // would be seen only by Miri.
    let past_the_last = unsafe { escrow_entries(owner).add(ESCROW_ENTRIES) } as *mut u8;
    assert_eq!(past_the_last, BlockHeader::end(floor()));

    reset();
}

#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn one_entry_past_the_escrow_capacity_aborts() {
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
            "cycle::queue::tests::what_gc_owns::one_entry_past_the_escrow_capacity_aborts",
        ])
        .env(CHILD, "1")
        .output()
        .expect("the test binary runs as its own overflow child");
    // The signal, not merely a failure: any panic in the fixture would
    // satisfy an unsuccessful exit, and the escrow's last resort is an
    // abort with no frame to report through.
    use std::os::unix::process::ExitStatusExt;
    // `SIGABRT`, which is 6 on every unix this crate builds for. Spelled
    // out because the crate takes no `libc` dependency.
    assert_eq!(
        output.status.signal(),
        Some(6),
        "capacity plus one did not abort; status {:?}",
        output.status
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
