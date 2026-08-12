//! Every allocation lives inside a block-aligned block, which is what
//! makes a size-less free possible; the region registry is what makes
//! a pooled block enumerable.

use super::*;

#[test]
fn blocks_are_aligned_and_mask_finds_header() {
    let _g = test_guard();
    let pool = BlockPool::global();
    let block = pool.get();

    assert_eq!(
        block as usize & BLOCK_MASK,
        0,
        "block must be BLOCK_SIZE-aligned"
    );

    // Any interior pointer maps back to the header with one mask.
    let payload = BlockHeader::payload_start(block);
    let interior = unsafe { payload.add(12345) };
    assert_eq!(BlockHeader::of_ptr(interior), block);

    assert_eq!(payload as usize - block as usize, LINE_SIZE);
    assert_eq!(
        BlockHeader::end(block) as usize - block as usize,
        BLOCK_SIZE
    );

    pool.put(block);
}

#[test]
fn region_registry_covers_every_pooled_block() {
    let _g = test_guard();
    let pool = BlockPool::global();
    let block = pool.get();

    let regions = pool.regions();
    assert!(!regions.is_empty(), "carving must register the region");
    let covered = regions.iter().any(|&r| {
        let base = r as usize;
        (block as usize) >= base && (block as usize) < base + REGION_SIZE
    });

    assert!(
        covered,
        "a pooled block must fall inside a registered region"
    );

    pool.put(block);
}
