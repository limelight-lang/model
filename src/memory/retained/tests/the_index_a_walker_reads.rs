//! A bump-filled former-arena block has no stride to divide by, so
//! this list is the only way its occupants can be enumerated. It is
//! sorted whatever order it arrives in, and safe to read while the
//! enumerator holds it.

use super::*;

/// Registration sorts, because the trace binary-searches the list
/// and the reset discovers survivors in trace order.
#[test]
fn an_index_is_stored_sorted_whatever_order_it_arrives_in() {
    let _g = crate::memory::block_pool::test_guard();
    let (block, cells, _live) = walkable_index(3);
    let _empty = unsafe { register(block, &[cells[2], cells[0], cells[1]], list_room(block, 3)) };
    let mut ascending = cells.clone();
    ascending.sort_unstable();
    assert_eq!(unsafe { survivor_list_copy(block) }, ascending);
    give_back(block);
}

/// The synchronous enumerator walks a published list without
/// checking that the block exists, so a published address is
/// dereferenced by whichever thread walks next. A zeroed cell reads
/// refcount 0 and is skipped, which is the contract; a fabricated
/// address is a wild read, which is what this pins against.
#[test]
fn a_registered_index_is_safe_for_the_enumerator_to_read() {
    let _g = crate::memory::block_pool::test_guard();
    let (block, cells, _live) = walkable_index(4);
    let _empty = unsafe { register(block, &cells, list_room(block, 4)) };
    let mut seen = 0usize;
    unsafe {
        crate::memory::heap::for_each_entity_slot(|slot| {
            if cells.contains(&(slot as usize)) {
                seen += 1;
            }
        })
    };

    give_back(block);
    assert_eq!(seen, 0, "zeroed cells read refcount 0 and are skipped");
}

/// A live occupant of a published list is visited, and a dead one is
/// not: the block has no stride, so the list is the enumerator's only
/// road to its occupants, and a walk that skipped the block would drop
/// every promoted survivor from the census.
#[test]
fn a_live_occupant_of_a_published_list_is_visited() {
    let _g = crate::memory::block_pool::test_guard();
    let (block, cells, live) = walkable_index(3);
    unsafe {
        live[0].write(1);
        live[2].write(1);
    }

    let _empty = unsafe { register(block, &cells, list_room(block, 3)) };
    let mut seen = Vec::new();
    unsafe {
        crate::memory::heap::for_each_entity_slot(|slot| {
            if cells.contains(&(slot as usize)) {
                seen.push(slot as usize);
            }
        })
    };

    seen.sort_unstable();
    assert_eq!(
        seen,
        vec![cells[0], cells[2]],
        "the enumerator did not visit exactly the live occupants of the list"
    );

    assert!(!unsafe { occupant_freed(block) });
    assert!(unsafe { occupant_freed(block) });
    unsafe {
        live[0].write(0);
        live[2].write(0);
    }

    give_back(block);
}
