//! What it costs to be in the registry. A run's mapping comes from the
//! operating system and its entry must come from the same place: a
//! collection frees a run inside its own close, where an allocation is
//! refused outright, and the free path is where a table of its own would
//! give memory back.

use super::*;
use crate::test_support::allocation_probe;

/// Twelve runs rather than one, because a table with room for the entry
/// grows on some later insert and gives its nodes back on some later
/// remove: a fixture of one run reports zero over a structure that
/// allocates, which is the answer it was written to disprove.
const RUN_COUNT: usize = 12;

/// Registering twelve runs and freeing them again reaches neither the
/// global allocator nor the pool, in either direction.
///
/// The free half is the half with no other instrument: a path that only
/// gives memory back allocates nothing while it does it, so every
/// allocation-counting test in the crate passes over it.
#[test]
#[cfg_attr(
    miri,
    ignore = "under Miri `os::map_aligned` remembers each whole mapping in a \
              Vec, so mapping the run allocates on the probe's own counter"
)]
fn twelve_runs_come_and_go_without_the_registry_taking_memory() {
    let _g = test_guard();
    let size = BLOCK_PAYLOAD + 1;
    // At its final capacity before the bracket opens: the vector holding
    // the test's own addresses is not the subject of the count.
    let mut blocks: Vec<*mut u8> = Vec::with_capacity(RUN_COUNT);

    let _ = allocation_probe::take_allocations();
    let _ = allocation_probe::take_heap_deallocations();

    for _ in 0..RUN_COUNT {
        let entity = alloc(size);
        assert!(!entity.is_null(), "the system served the mapping");
        blocks.push((entity as usize & !BLOCK_MASK) as *mut u8);
    }
    let (allocated_registering, pool_registering) = allocation_probe::take_allocations();
    let freed_registering = allocation_probe::take_heap_deallocations();

    for &block in &blocks {
        unsafe { free(block, BLOCK_KIND_ENTITY_LARGE_RUN) };
    }
    let (allocated_freeing, pool_freeing) = allocation_probe::take_allocations();
    let freed_freeing = allocation_probe::take_heap_deallocations();

    assert_eq!(
        (allocated_registering, freed_registering, pool_registering),
        (0, 0, 0),
        "registration writes into the run's own header"
    );
    assert_eq!(
        (allocated_freeing, freed_freeing, pool_freeing),
        (0, 0, 0),
        "and the entry dies with the mapping that carries it"
    );
}
