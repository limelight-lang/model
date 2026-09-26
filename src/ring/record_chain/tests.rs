//! A record's chain: entries come out of the front in the order they went in,
//! across blocks, a commit gives back every block it consumed whole, the
//! death check's tombstones are skipped by every reading and its cursor
//! resumes where a stop left it or on the entry it stopped at, the expiry
//! detaches whole blocks by their stamps up to a stop, and the pack for a
//! splice leaves ring blocks R's reader reads.

use super::*;
use crate::memory::block_pool::test_guard;
use crate::memory::gc_metadata;

fn fresh() -> *mut BlockHeader {
    gc_metadata::acquire()
}

/// Entries 8, 16, … — eight-aligned, never zero.
fn entry(index: usize) -> usize {
    (index + 1) * 8
}

fn filled(chain: &RecordChain, count: usize, stamp: u64) {
    for index in 0..count {
        unsafe { chain.push(entry(index), stamp, fresh) }.expect("the pool served");
    }
}

fn contents(chain: &RecordChain) -> Vec<usize> {
    let mut seen = Vec::new();
    unsafe { chain.walk(|entry| seen.push(entry)) };
    seen
}

fn dismantle(chain: &RecordChain) {
    if let Some((first, _)) = unsafe { chain.take_compacted(gc_metadata::release) } {
        let mut block = first;
        while !block.is_null() {
            let next = unsafe { (*ring(block)).link.next.load(Ordering::Relaxed) };
            gc_metadata::release(block);
            block = next;
        }
    }
}

#[test]
fn a_peek_and_its_commit_take_the_front_across_blocks_and_give_back_what_they_consumed() {
    let _g = test_guard();
    let chain = RecordChain::empty();
    let count = BLOCK_ENTRIES + 10;
    filled(&chain, count, 1);
    assert_eq!(unsafe { chain.block_count() }, 2);
    assert_eq!(chain.len(), count);

    let mut out = vec![0; BLOCK_ENTRIES + 3];
    let peek = unsafe { chain.peek(&mut out) };
    assert_eq!(peek.copied, BLOCK_ENTRIES + 3);
    assert!(
        out.iter()
            .enumerate()
            .all(|(index, &seen)| seen == entry(index))
    );
    assert_eq!(chain.len(), count, "a peek takes nothing");

    let mut given_back = 0;
    unsafe {
        chain.commit(peek, |block| {
            given_back += 1;
            gc_metadata::release(block);
        })
    };
    assert_eq!(given_back, 1, "the first block, consumed whole");
    assert_eq!(chain.len(), 7);
    assert_eq!(
        contents(&chain),
        (BLOCK_ENTRIES + 3..count).map(entry).collect::<Vec<_>>()
    );
    dismantle(&chain);
}

#[test]
fn a_commit_that_consumes_the_last_block_empties_the_chain() {
    let _g = test_guard();
    let chain = RecordChain::empty();
    filled(&chain, 5, 3);
    let mut out = [0; 8];
    let peek = unsafe { chain.peek(&mut out) };
    unsafe { chain.commit(peek, gc_metadata::release) };
    assert_eq!(
        (
            chain.len(),
            unsafe { chain.has_a_block() },
            chain.oldest_stamp()
        ),
        (0, false, u64::MAX)
    );
    // And the next push starts a block of its own.
    filled(&chain, 1, 4);
    assert_eq!(chain.oldest_stamp(), 4);
    dismantle(&chain);
}

#[test]
fn a_death_check_tombstones_what_it_takes_and_every_reading_skips_it() {
    let _g = test_guard();
    let chain = RecordChain::empty();
    filled(&chain, 10, 1);
    let read = unsafe {
        chain.check(
            usize::MAX,
            || false,
            |seen| {
                if seen % 16 == 0 {
                    Checked::Take
                } else {
                    Checked::Keep
                }
            },
        )
    };
    assert_eq!(read, 10);
    assert_eq!(chain.len(), 5);
    let kept: Vec<usize> = (0..10).map(entry).filter(|seen| seen % 16 != 0).collect();
    assert_eq!(contents(&chain), kept);

    let mut out = [0; 10];
    let peek = unsafe { chain.peek(&mut out) };
    assert_eq!(
        &out[..peek.copied],
        &kept[..],
        "a peek skips the tombstones"
    );
    dismantle(&chain);
}

#[test]
fn a_stopped_check_resumes_at_its_cursor_and_a_finished_lap_starts_again() {
    let _g = test_guard();
    let chain = RecordChain::empty();
    filled(&chain, BLOCK_ENTRIES + 20, 1);
    let mut seen = Vec::new();
    let mut stops = 0;
    // A check that spends a budget of 30, then one stopped after five.
    let read = unsafe {
        chain.check(
            30,
            || false,
            |entry| {
                seen.push(entry);
                Checked::Keep
            },
        )
    };
    assert_eq!(read, 30);
    let read = unsafe {
        chain.check(
            usize::MAX,
            || {
                stops += 1;
                stops > 5
            },
            |entry| {
                seen.push(entry);
                Checked::Keep
            },
        )
    };
    assert_eq!(read, 5);
    assert_eq!(
        seen,
        (0..35).map(entry).collect::<Vec<_>>(),
        "no entry read twice or skipped"
    );

    // The rest of the lap, then a check that finds nothing unread starts
    // the next lap from the front.
    let rest = unsafe { chain.check(usize::MAX, || false, |_| Checked::Keep) };
    assert_eq!(rest, BLOCK_ENTRIES + 20 - 35);
    let mut again = Vec::new();
    let read = unsafe {
        chain.check(
            3,
            || false,
            |entry| {
                again.push(entry);
                Checked::Keep
            },
        )
    };
    assert_eq!((read, again), (3, (0..3).map(entry).collect::<Vec<_>>()));
    dismantle(&chain);
}

#[test]
fn the_expiry_detaches_whole_blocks_by_their_stamps_in_order() {
    let _g = test_guard();
    let waiting = RecordChain::empty();
    let ready = RecordChain::empty();
    filled(&waiting, BLOCK_ENTRIES, 1);
    filled(&waiting, BLOCK_ENTRIES, 2);
    filled(&waiting, 4, 3);
    assert_eq!(waiting.oldest_stamp(), 1);

    let detached =
        unsafe { waiting.detach_while(|stamp| stamp < 3, || false) }.expect("two blocks due");
    unsafe { ready.append(detached) };
    assert_eq!((ready.len(), waiting.len()), (2 * BLOCK_ENTRIES, 4));
    assert_eq!((ready.oldest_stamp(), waiting.oldest_stamp()), (1, 3));
    assert!(unsafe { waiting.detach_while(|stamp| stamp < 3, || false) }.is_none());
    dismantle(&waiting);
    dismantle(&ready);
}

#[test]
fn a_check_stopped_on_an_entry_reads_that_entry_first_the_next_time() {
    let _g = test_guard();
    let chain = RecordChain::empty();
    filled(&chain, 10, 1);
    let read = unsafe {
        chain.check(
            usize::MAX,
            || false,
            |seen| {
                if seen == entry(3) {
                    Checked::KeepAndStop
                } else {
                    Checked::Keep
                }
            },
        )
    };
    assert_eq!(read, 4, "the entry it stopped on was read");
    assert_eq!(chain.len(), 10, "and kept");

    let mut next = Vec::new();
    let _ = unsafe {
        chain.check(
            2,
            || false,
            |seen| {
                next.push(seen);
                Checked::Keep
            },
        )
    };
    assert_eq!(next, vec![entry(3), entry(4)]);
    dismantle(&chain);
}

#[test]
fn a_stopped_expiry_detaches_the_blocks_it_read_before_the_stop() {
    let _g = test_guard();
    let waiting = RecordChain::empty();
    filled(&waiting, BLOCK_ENTRIES, 1);
    filled(&waiting, BLOCK_ENTRIES, 1);
    let mut asked = 0;
    let detached = unsafe {
        waiting.detach_while(
            |_| true,
            || {
                asked += 1;
                asked > 1
            },
        )
    }
    .expect("one block before the stop");
    assert_eq!(
        (detached.entries, waiting.len()),
        (BLOCK_ENTRIES, BLOCK_ENTRIES)
    );
    assert!(
        unsafe { waiting.detach_while(|_| true, || true) }.is_none(),
        "a stop before the first block detaches nothing"
    );

    // Two blocks of tombstones alone: a stop after the first gives back one.
    let ready = RecordChain::empty();
    unsafe { ready.append(detached) };
    filled(&ready, BLOCK_ENTRIES, 2);
    let _ = unsafe { ready.check(usize::MAX, || false, |_| Checked::Take) };
    assert_eq!(unsafe { ready.block_count() }, 2);
    let mut asked = 0;
    unsafe {
        ready.give_back_leading_empty_blocks(gc_metadata::release, || {
            asked += 1;
            asked > 1
        })
    };
    assert_eq!(unsafe { ready.block_count() }, 1);
    dismantle(&waiting);
    dismantle(&ready);
}

#[test]
fn the_pack_for_a_splice_drops_tombstones_and_consumed_positions() {
    let _g = test_guard();
    let chain = RecordChain::empty();
    filled(&chain, 12, 1);
    let mut out = [0; 2];
    let peek = unsafe { chain.peek(&mut out) };
    unsafe { chain.commit(peek, gc_metadata::release) };
    let _ = unsafe {
        chain.check(
            usize::MAX,
            || false,
            |seen| {
                if seen == entry(5) {
                    Checked::Take
                } else {
                    Checked::Keep
                }
            },
        )
    };

    let (first, last) =
        unsafe { chain.take_compacted(gc_metadata::release) }.expect("entries left");
    assert_eq!(first, last);
    let b = ring(first);
    let (front, tail) = unsafe {
        (
            (*b).reader.front.load(Ordering::Relaxed),
            (*b).writer.tail.load(Ordering::Relaxed),
        )
    };
    let packed: Vec<usize> = (front..tail)
        .map(|index| unsafe { *(*b).slots[index].get() })
        .collect();
    let expected: Vec<usize> = (2..12).filter(|&index| index != 5).map(entry).collect();
    assert_eq!((front, packed), (0, expected));
    assert_eq!((chain.len(), unsafe { chain.has_a_block() }), (0, false));
    gc_metadata::release(first);
}
