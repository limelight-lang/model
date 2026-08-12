//! A pooled block and an OS-direct run share a shape and nothing
//! else: exhaustion of one leaves the other alone, and only the run
//! half is found through the registry.

use super::*;

/// Exhaustion is reported, not absorbed — and the two halves draw on
/// different allocators, which is what makes the refusal legible:
/// with the pool refusing, the pooled half is null while the run half
/// is served, so a null here can only have come from the pool.
#[test]
fn the_pooled_half_reports_pool_exhaustion_and_the_run_half_is_unaffected() {
    let _g = test_guard();
    crate::memory::block_pool::FORCE_OOM.store(true, std::sync::atomic::Ordering::Relaxed);

    let pooled = alloc(BLOCK_PAYLOAD);
    let run = alloc(BLOCK_PAYLOAD + 1);

    crate::memory::block_pool::FORCE_OOM.store(false, std::sync::atomic::Ordering::Relaxed);
    assert!(pooled.is_null(), "the pool refused and the refusal carried");
    assert!(
        !run.is_null(),
        "the run comes from the system allocator, which was not refusing"
    );

    let block = (run as usize & !BLOCK_MASK) as *mut u8;
    unsafe { free(block, BLOCK_KIND_ENTITY_LARGE_RUN) };
}

/// One byte more and the allocation is a run: outside every region,
/// so the registry is what makes it findable, and the entry goes
/// when the memory does.
#[test]
fn a_run_is_registered_while_it_lives_and_gone_after_it_is_freed() {
    let _g = test_guard();
    let size = BLOCK_PAYLOAD + 1;

    let entity = alloc(size);
    assert!(!entity.is_null());
    let block = (entity as usize & !BLOCK_MASK) as *mut u8;
    assert_eq!(
        unsafe {
            (*(block as *const LargeEntityHeader))
                .kind
                .load(Ordering::Relaxed)
        },
        BLOCK_KIND_ENTITY_LARGE_RUN
    );
    assert_eq!(
        entity as usize - block as usize,
        LINE_SIZE,
        "the entity starts after the header line here too — the block
         round-up leaves slack at the tail, so no write in this test
         would fault on a lost line"
    );
    assert!(
        snapshot().contains(&(block as usize)),
        "a run is enumerated from the registry or not at all"
    );
    assert_eq!(unsafe { occupant(block) }.1, size);

    // The last byte the entity claims is inside the allocation: a
    // round-up that lost the header line would fault here.
    unsafe { entity.add(size - 1).write(0xab) };

    unsafe { free(block, BLOCK_KIND_ENTITY_LARGE_RUN) };
    assert!(
        !snapshot().contains(&(block as usize)),
        "and the entry goes before the memory"
    );
}
