//! The zero pass is what makes a commissioned block safe, not the
//! publication order: the entity's first bytes read dead until a
//! factory publishes, so a walker may meet the block first.

use super::*;

/// Up to one block payload the allocation is a pooled block: its
/// kind says so, the entity sits at `+LINE_SIZE`, its header word
/// reads zero, and no registry entry is made — the region scan
/// already reaches it.
#[test]
fn a_pooled_large_entity_is_block_aligned_and_reads_dead_until_published() {
    let _g = test_guard();
    let before = snapshot().len();

    let entity = alloc(BLOCK_PAYLOAD);
    assert!(!entity.is_null());
    let block = (entity as usize & !BLOCK_MASK) as *mut u8;
    assert_eq!(
        entity as usize - block as usize,
        LINE_SIZE,
        "the entity starts after the header line"
    );
    assert_eq!(
        unsafe {
            (*(block as *const LargeEntityHeader))
                .kind
                .load(Ordering::Relaxed)
        },
        BLOCK_KIND_ENTITY_LARGE
    );
    assert_eq!(
        unsafe { *(entity as *const u64) },
        0,
        "the occupancy word is zeroed before the kind is published"
    );
    assert_eq!(unsafe { occupant(block) }, (entity, BLOCK_PAYLOAD));
    assert_eq!(snapshot().len(), before, "a pooled block needs no registry");

    unsafe { free(block, BLOCK_KIND_ENTITY_LARGE) };
}
