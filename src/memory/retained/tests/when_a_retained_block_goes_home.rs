//! The live count is what returns the block: the last occupant's
//! death hands the block to the pool, its list going with it, and an
//! occupant already dead at registration holds nothing.

use super::*;

/// The last live occupant's death empties the block, and the block
/// goes home with the list still in its own tail — the list dies with
/// the block it describes, never before it.
#[test]
fn the_last_live_occupant_empties_the_block() {
    let _g = crate::memory::block_pool::test_guard();
    let (block, cells, live) = walkable_index(2);
    unsafe {
        live[0].write(1);
        live[1].write(1);
    }

    let _empty = unsafe { register(block, &cells, list_room(block, 2)) };
    assert!(unsafe { has_survivor_list(block) });
    assert!(
        !unsafe { occupant_freed(block) },
        "one of two occupants emptied it"
    );
    assert!(unsafe { has_survivor_list(block) });
    assert!(
        unsafe { occupant_freed(block) },
        "the second death left it occupied"
    );
    unsafe {
        live[0].write(0);
        live[1].write(0);
    }

    give_back(block);
    assert_eq!(
        kind_of(block),
        BLOCK_KIND_FREE,
        "the block outlived its last occupant"
    );
}

/// An occupant already dead when the list is published is not counted,
/// or the block would wait forever for a death that has happened.
#[test]
fn an_occupant_dead_at_registration_holds_nothing() {
    let _g = crate::memory::block_pool::test_guard();
    let (block, cells, live) = walkable_index(2);
    let _empty = unsafe {
        live[0].write(1);
        register(block, &cells, list_room(block, 2))
    };

    assert!(
        unsafe { occupant_freed(block) },
        "the dead occupant was counted live"
    );
    unsafe { live[0].write(0) };
    give_back(block);
    assert_eq!(kind_of(block), BLOCK_KIND_FREE);
}

/// The last death can arrive on a thread other than the one that
/// published the list, and it finds the list: the count word is
/// published last, by an increment whose release half covers the list's
/// store, so the decrement that reaches zero synchronises with it and
/// reads the address it must spend the holder's hold through
/// (`release_emptied`). Published the other way round, a last death
/// that lands between the two stores reads a null address, returns the
/// block, and leaves the holder held for a list nobody spends.
///
/// The window is two stores wide, so the pair is run many times, and
/// the other thread waits on the count word the way a freeing thread's
/// decrement does.
#[test]
fn the_last_death_on_another_thread_finds_the_list() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let _g = crate::memory::block_pool::test_guard();
    let (block, cells, live) = walkable_index(1);
    unsafe { live[0].write(1) };
    let room = list_room(block, 1) as usize;
    let rounds = if cfg!(miri) { 8 } else { 50_000 };
    let published = AtomicUsize::new(0);
    let spent = AtomicUsize::new(0);
    let missing = AtomicUsize::new(0);

    std::thread::scope(|scope| {
        scope.spawn(|| {
            for round in 1..=rounds {
                while published.load(Ordering::Acquire) != round {
                    std::hint::spin_loop();
                }

                while !unsafe { has_live_occupants(block) } {
                    std::hint::spin_loop();
                }

                assert!(
                    unsafe { occupant_freed(block) },
                    "the one occupant's death did not empty the block"
                );
                if !unsafe { has_survivor_list(block) } {
                    missing.fetch_add(1, Ordering::Relaxed);
                }

                spent.store(round, Ordering::Release);
            }
        });

        for round in 1..=rounds {
            // The line as the reset leaves it before `register`: no
            // count, no list.
            unsafe { crate::memory::heap::clear_collector_line(block as *mut u8) };
            published.store(round, Ordering::Release);
            let _empty = unsafe { register(block, &cells, room as *mut usize) };
            while spent.load(Ordering::Acquire) != round {
                std::hint::spin_loop();
            }
        }
    });

    unsafe { live[0].write(0) };
    give_back(block);
    assert_eq!(
        missing.load(Ordering::Relaxed),
        0,
        "the last death read the count without the list"
    );
}

/// A block goes home with the words of its retention still in its
/// collector line: `release_emptied` restamps the kind and nothing else,
/// and the pool trip writes the pool's own words only. The commissioning
/// that retains the block again is what clears them, or the previous
/// list's address and length would name a list in bytes the next arena
/// bumps over — and a block retained for a payload alone, which no
/// `register` follows, would carry that list for its whole second life.
#[test]
fn a_block_commissioned_again_carries_nothing_of_its_previous_retention() {
    let _g = crate::memory::block_pool::test_guard();
    let (block, cells, live) = walkable_index(2);
    unsafe {
        live[0].write(1);
        live[1].write(1);
    }

    let _empty = unsafe { register(block, &cells, list_room(block, 2)) };
    assert!(!unsafe { occupant_freed(block) });
    assert!(unsafe { occupant_freed(block) });
    unsafe {
        live[0].write(0);
        live[1].write(0);
    }

    assert!(
        unsafe { has_survivor_list(block) },
        "the list left with the occupants, so this test proves nothing"
    );

    unsafe { commission_retained_block(block) };
    assert!(
        !unsafe { has_survivor_list(block) },
        "the previous retention's list survived the commissioning"
    );
    assert_eq!(unsafe { occupant_count(block) }, None);
    assert!(unsafe { holds_nothing(block) });
    give_back(block);
    assert_eq!(kind_of(block), BLOCK_KIND_FREE);
}
