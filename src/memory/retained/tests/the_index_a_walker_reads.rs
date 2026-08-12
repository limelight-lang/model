//! A bump-filled former-arena block has no stride to divide by, so
//! this inventory is the only way its occupants can be enumerated. It
//! is sorted whatever order it arrives in, and safe to read while the
//! enumerator holds it.

use super::*;

/// Registration sorts, because the census binary-searches the index
/// and the reset discovers survivors in trace order.
#[test]
fn an_index_is_stored_sorted_whatever_order_it_arrives_in() {
    let _g = crate::memory::block_pool::test_guard();
    let (block, cells, _live) = walkable_index(3);
    let _empty = unsafe { register(block, vec![cells[2], cells[0], cells[1]]) };
    let found = snapshot()
        .into_iter()
        .find(|&(b, _)| b == block)
        .expect("registered block is in the snapshot");
    let mut ascending = cells.clone();
    ascending.sort_unstable();
    assert_eq!(&*found.1, &ascending[..]);
    drop_index(block);
}

/// The synchronous enumerator walks a registered index without
/// checking that the block exists, so a registered address is
/// dereferenced by whichever thread walks next. A zeroed cell reads
/// refcount 0 and is skipped, which is the contract; a fabricated
/// address is a wild read, which is what this pins against.
#[test]
fn a_registered_index_is_safe_for_the_enumerator_to_read() {
    let _g = crate::memory::block_pool::test_guard();
    let (block, cells, _live) = walkable_index(4);
    let _empty = unsafe { register(block, cells.clone()) };
    let mut seen = 0usize;
    unsafe {
        crate::memory::heap::for_each_entity_slot(|slot| {
            if cells.contains(&(slot as usize)) {
                seen += 1;
            }
        })
    };

    drop_index(block);
    assert_eq!(seen, 0, "zeroed cells read refcount 0 and are skipped");
}
