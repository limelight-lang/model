//! The ring read behind its writer: entries come out in the order they went
//! in, none twice and none lost, while the writer keeps writing on its own
//! thread; a full block moves the writer on, a consumed block is the
//! writer's again around the circle, and a block filled between the
//! reader's two readings of it is not skipped. The owner's quiet pass packs
//! what it keeps, splices a chain in after the tail, and unlinks one spare
//! block.

use super::*;
use crate::memory::block_pool::test_guard;
use crate::memory::gc_metadata;
use std::sync::atomic::AtomicBool;

/// The ring's two words for a case, in one struct so that a case's ring is
/// one binding.
struct Words {
    front_block: AtomicPtr<BlockHeader>,
    tail_block: AtomicPtr<BlockHeader>,
}

impl Words {
    const fn new() -> Self {
        Self {
            front_block: AtomicPtr::new(std::ptr::null_mut()),
            tail_block: AtomicPtr::new(std::ptr::null_mut()),
        }
    }

    fn slots(&self) -> Slots<'_> {
        Slots {
            front_block: &self.front_block,
            tail_block: &self.tail_block,
        }
    }

    /// Give every block back to the pool, entries and all.
    fn dismantle(&self) {
        unsafe { Quiescent::new(self.slots()) }.dismantle(gc_metadata::release);
    }
}

fn fresh() -> *mut BlockHeader {
    gc_metadata::acquire()
}

fn none() -> *mut BlockHeader {
    std::ptr::null_mut()
}

/// A case-static ring shared between the case's thread and a reader or
/// writer thread it spawns.
static SHARED: Words = Words::new();
static SHARED_IN_USE: AtomicBool = AtomicBool::new(false);

/// Hold [`SHARED`] for one case: the cases that share it run one at a time
/// under the parallel harness.
struct SharedRing;

impl SharedRing {
    fn take() -> Self {
        while SHARED_IN_USE
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::thread::yield_now();
        }
        Self
    }
}

impl Drop for SharedRing {
    fn drop(&mut self) {
        SHARED.dismantle();
        SHARED_IN_USE.store(false, Ordering::Release);
    }
}

#[test]
fn an_empty_ring_holds_no_block_and_reads_empty() {
    let words = Words::new();
    let reader = unsafe { Reader::new(words.slots()) };
    let mut out = [0; 4];
    assert_eq!(reader.take(&mut out), 0);
    assert_eq!(reader.unread(), 0);
    assert!(!unsafe { Quiescent::new(words.slots()) }.has_blocks());
}

#[test]
fn the_first_push_takes_a_block_and_a_refused_block_writes_nothing() {
    let _g = test_guard();
    let words = Words::new();
    let writer = unsafe { Writer::new(words.slots()) };
    assert_eq!(writer.push(1, none), Err(NoBlock));
    assert!(words.front_block.load(Ordering::Relaxed).is_null());

    assert!(writer.push(1, fresh).is_ok());
    let block = words.front_block.load(Ordering::Relaxed);
    assert!(!block.is_null());
    assert_eq!(
        words.tail_block.load(Ordering::Relaxed),
        block,
        "a circle of one"
    );
    assert_eq!(unsafe { Quiescent::new(words.slots()) }.block_count(), 1);
    assert_eq!(unsafe { Reader::new(words.slots()) }.unread(), 1);
    words.dismantle();
}

#[test]
fn entries_come_out_in_order_across_a_wrap_within_one_block() {
    let _g = test_guard();
    let words = Words::new();
    let writer = unsafe { Writer::new(words.slots()) };
    let reader = unsafe { Reader::new(words.slots()) };
    let mut out = [0; 8];

    // Push and take in lockstep past the block's end: the indices wrap and
    // the block never fills, so the first block serves the whole run.
    assert!(writer.push(0, fresh).is_ok());
    assert_eq!(reader.take(&mut out[..1]), 1);
    let mut next = 1;
    for _ in 0..(CAPACITY / 4 + 1) {
        for entry in next..next + 4 {
            assert!(writer.push(entry, none).is_ok(), "no growth is needed");
        }
        assert_eq!(reader.take(&mut out[..4]), 4);
        assert_eq!(&out[..4], &[next, next + 1, next + 2, next + 3]);
        next += 4;
    }
    assert_eq!(reader.take(&mut out), 0);
    assert_eq!(unsafe { Quiescent::new(words.slots()) }.block_count(), 1);
    words.dismantle();
}

#[test]
fn a_peek_reads_without_consuming_and_a_commit_consumes_exactly_it() {
    let _g = test_guard();
    let words = Words::new();
    let writer = unsafe { Writer::new(words.slots()) };
    let reader = unsafe { Reader::new(words.slots()) };
    let mut out = [0; 5];

    // Two blocks: the peek crosses the block change without moving the
    // front block, and reads the same entries twice.
    for entry in 0..BLOCK_ENTRIES + 2 {
        assert!(writer.push(entry, fresh).is_ok());
    }
    let mut all = vec![0; BLOCK_ENTRIES - 3];
    assert_eq!(reader.take(&mut all), BLOCK_ENTRIES - 3);
    let front_block = words.front_block.load(Ordering::Relaxed);

    let peeked = reader.peek(&mut out);
    assert_eq!(peeked.len(), 5);
    assert_eq!(
        out,
        [
            BLOCK_ENTRIES - 3,
            BLOCK_ENTRIES - 2,
            BLOCK_ENTRIES - 1,
            BLOCK_ENTRIES,
            BLOCK_ENTRIES + 1
        ]
    );
    assert_eq!(
        words.front_block.load(Ordering::Relaxed),
        front_block,
        "nothing moved"
    );
    assert_eq!(reader.unread(), 5);
    let again = reader.peek(&mut out);
    assert_eq!(again.len(), 5, "the same entries again");

    reader.commit(peeked);
    assert_eq!(reader.unread(), 0);
    assert_eq!(
        words.front_block.load(Ordering::Relaxed),
        words.tail_block.load(Ordering::Relaxed),
        "the commit moved the front block across"
    );
    assert_eq!(reader.take(&mut out), 0);

    // A peek of a partial block, committed, leaves the rest.
    for entry in 10..14 {
        assert!(writer.push(entry, none).is_ok());
    }
    let peeked = reader.peek(&mut out[..2]);
    assert_eq!(peeked.len(), 2);
    reader.commit(peeked);
    assert_eq!(reader.take(&mut out), 2);
    assert_eq!(&out[..2], &[12, 13]);
    words.dismantle();
}

/// A peek that consumed nothing leaves the reader's copy of the tail where
/// it read it; the writer goes on, and the next peek reads what stands now
/// rather than stopping at that copy (`PLAN.md` S65.21: the close's look at
/// R's front split every batch that followed it).
#[test]
fn a_peek_after_one_that_consumed_nothing_reads_what_the_writer_added() {
    let _g = test_guard();
    let words = Words::new();
    let writer = unsafe { Writer::new(words.slots()) };
    let reader = unsafe { Reader::new(words.slots()) };
    let mut one = [0; 1];
    let mut out = [0; 8];

    for entry in 0..2 {
        assert!(writer.push(entry, fresh).is_ok());
    }
    assert_eq!(reader.peek(&mut one).len(), 1, "a look at the front");
    for entry in 2..6 {
        assert!(writer.push(entry, fresh).is_ok());
    }

    let peeked = reader.peek(&mut out);
    assert_eq!(peeked.len(), 6, "every entry the writer published");
    assert_eq!(&out[..6], &[0, 1, 2, 3, 4, 5]);
    reader.commit(peeked);
    assert_eq!(reader.unread(), 0);
    words.dismantle();
}

#[test]
fn a_rewrite_packs_across_blocks_and_over_a_wrapped_front_block() {
    let _g = test_guard();
    let words = Words::new();
    let writer = unsafe { Writer::new(words.slots()) };
    let reader = unsafe { Reader::new(words.slots()) };
    let quiet = unsafe { Quiescent::new(words.slots()) };
    let mut out = vec![0; BLOCK_ENTRIES];

    // A front block whose entries wrap: read most of a block, then fill it
    // past its end and on into two more blocks.
    for entry in 0..BLOCK_ENTRIES - 5 {
        assert!(writer.push(entry, fresh).is_ok());
    }
    assert_eq!(
        reader.take(&mut out[..BLOCK_ENTRIES - 10]),
        BLOCK_ENTRIES - 10
    );
    let total = 3 * BLOCK_ENTRIES;
    for entry in BLOCK_ENTRIES - 5..total {
        assert!(writer.push(entry, fresh).is_ok());
    }
    assert_eq!(quiet.block_count(), 3);
    let unread: Vec<usize> = (BLOCK_ENTRIES - 10..total).collect();
    assert_eq!(quiet.count(), unread.len());

    // Keep everything: the write cursor crosses both block changes.
    quiet.rewrite(Some);
    let mut seen = Vec::new();
    quiet.walk(|entry| {
        seen.push(entry);
        true
    });
    assert_eq!(seen, unread);

    // Drop every third: the kept entries pack into fewer blocks and the
    // last block empties.
    quiet.rewrite(|entry| (entry % 3 != 0).then_some(entry));
    let kept: Vec<usize> = unread.iter().copied().filter(|e| e % 3 != 0).collect();
    assert_eq!(quiet.count(), kept.len());
    let mut all = vec![0; kept.len()];
    assert_eq!(reader.take(&mut all), kept.len());
    assert_eq!(all, kept);
    assert_eq!(
        quiet.block_count(),
        3,
        "the emptied block stays in the circle"
    );

    // The writer goes on into the emptied blocks around the circle without
    // asking for a fresh one, and the reader follows.
    for entry in 0..BLOCK_ENTRIES + 1 {
        assert!(writer.push(entry, none).is_ok());
    }
    let mut all = vec![0; BLOCK_ENTRIES + 1];
    assert_eq!(reader.take(&mut all), BLOCK_ENTRIES + 1);
    assert!(all.iter().enumerate().all(|(i, &e)| i == e));
    words.dismantle();
}

#[test]
fn a_full_block_moves_the_writer_to_a_fresh_block_and_the_reader_follows() {
    let _g = test_guard();
    let words = Words::new();
    let writer = unsafe { Writer::new(words.slots()) };
    for entry in 0..BLOCK_ENTRIES {
        assert!(writer.push(entry, fresh).is_ok());
    }
    assert_eq!(
        unsafe { Quiescent::new(words.slots()) }.block_count(),
        1,
        "a block holds one entry short of its slots"
    );
    assert_eq!(
        writer.push(BLOCK_ENTRIES, none),
        Err(NoBlock),
        "the full block asks for a fresh one, and the circle of one has no spare"
    );
    assert!(writer.push(BLOCK_ENTRIES, fresh).is_ok());
    assert_eq!(unsafe { Quiescent::new(words.slots()) }.block_count(), 2);
    assert_eq!(
        unsafe { Reader::new(words.slots()) }.unread(),
        BLOCK_ENTRIES + 1
    );

    let reader = unsafe { Reader::new(words.slots()) };
    let mut out = vec![0; BLOCK_ENTRIES + 1];
    assert_eq!(reader.take(&mut out), BLOCK_ENTRIES + 1);
    assert!(
        out.iter().enumerate().all(|(i, &e)| i == e),
        "in order across the block change"
    );
    assert_eq!(
        words.front_block.load(Ordering::Relaxed),
        words.tail_block.load(Ordering::Relaxed),
        "the reader followed into the second block"
    );
    words.dismantle();
}

#[test]
fn a_consumed_block_is_the_writers_again_around_the_circle() {
    let _g = test_guard();
    let words = Words::new();
    let writer = unsafe { Writer::new(words.slots()) };
    let reader = unsafe { Reader::new(words.slots()) };
    let mut out = vec![0; BLOCK_ENTRIES];

    // Two blocks, the first read to its end and left: the reader moves on
    // only when it finds the block empty and another ahead.
    for entry in 0..BLOCK_ENTRIES + 1 {
        assert!(writer.push(entry, fresh).is_ok());
    }
    let first = words.front_block.load(Ordering::Relaxed);
    let second = words.tail_block.load(Ordering::Relaxed);
    assert_ne!(first, second);
    assert_eq!(reader.take(&mut out), BLOCK_ENTRIES);
    assert_eq!(reader.take(&mut out[..1]), 1);
    assert_eq!(words.front_block.load(Ordering::Relaxed), second);

    // Fill the second and one more: the writer moves on into the first,
    // which the reader has left, and no fresh block is asked for.
    for entry in 0..BLOCK_ENTRIES + 1 {
        assert!(writer.push(entry, none).is_ok());
    }
    assert_eq!(words.tail_block.load(Ordering::Relaxed), first);
    assert_eq!(unsafe { Quiescent::new(words.slots()) }.block_count(), 2);
    assert_eq!(reader.unread(), BLOCK_ENTRIES + 1);
    assert_eq!(reader.take(&mut out), BLOCK_ENTRIES);
    assert_eq!(reader.take(&mut out[..1]), 1);
    assert_eq!(
        out[0], BLOCK_ENTRIES,
        "the entry written into the reused block"
    );
    assert_eq!(reader.take(&mut out[..1]), 0);
    words.dismantle();
}

/// The hook: fill the shared ring's front block and move the writer past
/// it, between the reader's two readings of that block.
fn fill_and_leave_the_front_block() {
    let writer = unsafe { Writer::new(SHARED.slots()) };
    for entry in 100..100 + BLOCK_ENTRIES + 1 {
        assert!(writer.push(entry, fresh).is_ok());
    }
}

#[test]
fn a_block_filled_between_the_readers_two_readings_is_not_skipped() {
    let _g = test_guard();
    let _shared = SharedRing::take();
    let writer = unsafe { Writer::new(SHARED.slots()) };
    let reader = unsafe { Reader::new(SHARED.slots()) };
    let mut out = vec![0; BLOCK_ENTRIES + 2];

    assert!(writer.push(1, fresh).is_ok());
    assert_eq!(reader.take(&mut out[..1]), 1);
    // The front block reads empty; between that reading and the tail block's
    // the hook fills it and moves the writer on. The reader must take the
    // filled block first rather than follow the tail block past it.
    testing::run_between_the_reads(fill_and_leave_the_front_block);
    let taken = reader.take(&mut out);
    assert_eq!(taken, BLOCK_ENTRIES + 1);
    assert!(
        out[..taken].iter().enumerate().all(|(i, &e)| e == 100 + i),
        "every entry of the filled block came out, in order, before the next block's"
    );
}

#[test]
fn a_reader_behind_a_writer_on_another_thread_takes_every_entry_once() {
    let _g = test_guard();
    let _shared = SharedRing::take();
    const ENTRIES: usize = 3 * BLOCK_ENTRIES + 17;

    let producer = std::thread::spawn(|| {
        let writer = unsafe { Writer::new(SHARED.slots()) };
        for entry in 1..=ENTRIES {
            assert!(writer.push(entry, fresh).is_ok());
            if entry % 97 == 0 {
                std::thread::yield_now();
            }
        }
    });

    let reader = unsafe { Reader::new(SHARED.slots()) };
    let mut out = [0; 61];
    let mut expected = 1;
    while expected <= ENTRIES {
        let taken = reader.take(&mut out);
        for &entry in &out[..taken] {
            assert_eq!(entry, expected, "in order, none twice, none lost");
            expected += 1;
        }
        if taken == 0 {
            std::thread::yield_now();
        }
    }
    producer.join().expect("the writer finished");
    assert_eq!(reader.take(&mut out), 0);
    assert_eq!(reader.unread(), 0);
}

#[test]
fn the_quiet_pass_walks_counts_and_packs_what_it_keeps() {
    let _g = test_guard();
    let words = Words::new();
    let writer = unsafe { Writer::new(words.slots()) };
    let reader = unsafe { Reader::new(words.slots()) };
    let mut out = [0; 3];

    // Two blocks with a read prefix, so the front is not at zero.
    for entry in 0..BLOCK_ENTRIES + 10 {
        assert!(writer.push(entry, fresh).is_ok());
    }
    assert_eq!(reader.take(&mut out), 3);

    let quiet = unsafe { Quiescent::new(words.slots()) };
    assert_eq!(quiet.count(), BLOCK_ENTRIES + 7);
    let mut seen = Vec::new();
    assert!(quiet.walk(|entry| {
        seen.push(entry);
        true
    }));
    assert_eq!(seen.len(), BLOCK_ENTRIES + 7);
    assert_eq!(seen[0], 3);
    assert!(!quiet.walk(|_| false), "a stop is reported");

    // Keep every even entry, doubled: the kept words pack from the front,
    // and the tail block is the last block with a kept one.
    quiet.rewrite(|entry| (entry % 2 == 0).then_some(entry * 2));
    let kept: Vec<usize> = (3..BLOCK_ENTRIES + 10)
        .filter(|e| e % 2 == 0)
        .map(|e| e * 2)
        .collect();
    assert_eq!(quiet.count(), kept.len());
    let mut seen = Vec::new();
    quiet.walk(|entry| {
        seen.push(entry);
        true
    });
    assert_eq!(seen, kept);
    assert_eq!(
        words.tail_block.load(Ordering::Relaxed),
        words.front_block.load(Ordering::Relaxed),
        "half the entries fit in the front block, so the tail block is the front block"
    );
    assert_eq!(
        quiet.block_count(),
        2,
        "the emptied block stays in the circle"
    );

    // The reader reads what was packed, and the writer's next push goes
    // after it.
    let mut all = vec![0; kept.len() + 1];
    assert!(writer.push(999, none).is_ok());
    assert_eq!(reader.take(&mut all), kept.len() + 1);
    assert_eq!(&all[..kept.len()], &kept[..]);
    assert_eq!(all[kept.len()], 999);

    // Dropping everything leaves an empty ring with its blocks.
    quiet.rewrite(|_| None);
    assert_eq!(quiet.count(), 0);
    assert_eq!(reader.take(&mut out), 0);
    words.dismantle();
}

#[test]
fn a_chain_spliced_after_the_tail_is_read_after_what_stood_before_it() {
    let _g = test_guard();
    let words = Words::new();
    let writer = unsafe { Writer::new(words.slots()) };
    let reader = unsafe { Reader::new(words.slots()) };

    assert!(writer.push(1, fresh).is_ok());
    assert!(writer.push(2, none).is_ok());

    // Three blocks in the chain: two full and one with a remainder.
    let mut chain = Chain::empty();
    for entry in 0..2 * BLOCK_ENTRIES + 5 {
        assert_eq!(chain.push(1000 + entry, fresh), Ok(()));
    }
    assert_eq!(chain.block_count(), 3);
    assert_eq!(chain.len(), 2 * BLOCK_ENTRIES + 5);
    let (first, last) = chain.take().expect("the chain has blocks");
    assert!(chain.is_empty());

    let quiet = unsafe { Quiescent::new(words.slots()) };
    unsafe { writer.splice_after_tail(first, last) };
    assert_eq!(quiet.block_count(), 4);
    assert_eq!(words.tail_block.load(Ordering::Relaxed), last);
    assert_eq!(quiet.count(), 2 + 2 * BLOCK_ENTRIES + 5);

    // After the splice the writer continues in the last spliced block.
    assert!(writer.push(7, none).is_ok());

    let mut out = vec![0; 2 * BLOCK_ENTRIES + 8];
    assert_eq!(reader.take(&mut out), 2 * BLOCK_ENTRIES + 8);
    assert_eq!(&out[..2], &[1, 2]);
    assert!(
        out[2..2 * BLOCK_ENTRIES + 7]
            .iter()
            .enumerate()
            .all(|(i, &e)| e == 1000 + i)
    );
    assert_eq!(out[2 * BLOCK_ENTRIES + 7], 7);
    words.dismantle();
}

#[test]
fn a_chain_spliced_into_an_empty_ring_becomes_the_ring() {
    let _g = test_guard();
    let words = Words::new();
    let mut chain = Chain::empty();
    assert_eq!(chain.push(5, fresh), Ok(()));
    assert_eq!(chain.push(6, none), Ok(()));
    let mut seen = Vec::new();
    chain.walk(|entry| seen.push(entry));
    assert_eq!(seen, [5, 6]);
    let (first, last) = chain.take().expect("one block");
    assert_eq!(first, last);

    let quiet = unsafe { Quiescent::new(words.slots()) };
    unsafe { Writer::new(words.slots()).splice_after_tail(first, last) };
    assert_eq!(quiet.block_count(), 1);
    let reader = unsafe { Reader::new(words.slots()) };
    let mut out = [0; 3];
    assert_eq!(reader.take(&mut out), 2);
    assert_eq!(&out[..2], &[5, 6]);
    words.dismantle();
}

#[test]
fn the_spare_block_after_the_tail_is_unlinked_and_never_the_front_block() {
    let _g = test_guard();
    let words = Words::new();
    let writer = unsafe { Writer::new(words.slots()) };
    let reader = unsafe { Reader::new(words.slots()) };
    let quiet = unsafe { Quiescent::new(words.slots()) };

    assert!(writer.unlink_after_tail().is_null(), "no block at all");
    for entry in 0..2 * BLOCK_ENTRIES + 1 {
        assert!(writer.push(entry, fresh).is_ok());
    }
    assert_eq!(quiet.block_count(), 3);
    assert!(
        writer.unlink_after_tail().is_null(),
        "the block after the tail is the front block, which the reader is in"
    );

    // Read the first two blocks out. The reader stands in the second,
    // emptied, until it finds another block ahead; the first is behind the
    // tail block around the circle, empty, and the one after the tail.
    let mut out = vec![0; 2 * BLOCK_ENTRIES];
    assert_eq!(reader.take(&mut out), 2 * BLOCK_ENTRIES);
    let spare = writer.unlink_after_tail();
    assert!(!spare.is_null());
    assert_eq!(quiet.block_count(), 2);
    gc_metadata::release(spare);
    assert!(
        writer.unlink_after_tail().is_null(),
        "the second block is empty but the reader has not left it"
    );

    // What was unread is still read, and the reader leaves the second
    // block for it; then that block is the spare.
    assert_eq!(reader.take(&mut out[..2]), 1);
    assert_eq!(out[0], 2 * BLOCK_ENTRIES);
    let spare = writer.unlink_after_tail();
    assert!(!spare.is_null(), "and the next empty one");
    assert_eq!(quiet.block_count(), 1);
    gc_metadata::release(spare);
    assert!(
        writer.unlink_after_tail().is_null(),
        "a circle of one has nothing to spare"
    );
    words.dismantle();
}

/// The work test a reader without the token asks follows no link: it reads
/// the front block's span and whether that block is the tail block, and
/// nothing else, so an emptied front block with a block ahead reads as work
/// while its link is nulled under it — the state a pack and an unlink of
/// the block past the tail leave for a walker holding a stale tail.
#[test]
fn the_work_test_reads_the_front_block_alone() {
    let _g = test_guard();
    let words = Words::new();
    let writer = unsafe { Writer::new(words.slots()) };
    let reader = unsafe { Reader::new(words.slots()) };
    assert!(!reader.has_at_least(1), "no block");
    assert!(
        reader.front_block_reading().is_none(),
        "a ring with no front block has no reading"
    );

    // The first block is found, not filled; the push that leaves a full
    // tail block answers so.
    for entry in 0..BLOCK_ENTRIES {
        assert_eq!(writer.push(entry, fresh), Ok(Pushed::IntoTailBlock));
    }
    assert_eq!(writer.push(BLOCK_ENTRIES, fresh), Ok(Pushed::IntoNextBlock));
    let mut out = vec![0; BLOCK_ENTRIES];
    assert_eq!(reader.take(&mut out), BLOCK_ENTRIES);
    let front = words.front_block.load(Ordering::Relaxed);
    assert_ne!(
        front,
        words.tail_block.load(Ordering::Relaxed),
        "the reader stands in the emptied first block"
    );

    // Cut the link the walk would follow; a work test that followed it
    // would read null and dereference it.
    let next = unsafe {
        (*ring(front))
            .link
            .next
            .swap(std::ptr::null_mut(), Ordering::Relaxed)
    };
    assert!(
        reader.has_at_least(1),
        "an entry stands past the front block"
    );
    assert!(
        reader.has_at_least(BLOCK_ENTRIES),
        "a front block that is not the tail block is at any threshold"
    );
    let reading = reader
        .front_block_reading()
        .expect("the ring has a front block");
    assert_eq!(reading.span, 0, "the front block is read out");
    assert!(
        !reading.is_the_tail_block,
        "and an entry stands past it, which is what the count is read from"
    );
    unsafe { (*ring(front)).link.next.store(next, Ordering::Relaxed) };

    // In the tail block the threshold is the block's span.
    assert_eq!(reader.take(&mut out[..1]), 1);
    assert!(!reader.has_at_least(1), "the ring is read out");
    assert_eq!(reader.unread(), 0);
    for entry in 0..3 {
        assert!(writer.push(entry, none).is_ok());
    }
    assert!(reader.has_at_least(3));
    assert!(!reader.has_at_least(4));
    let reading = reader
        .front_block_reading()
        .expect("the ring has a front block");
    assert_eq!(reading.span, 3);
    assert!(reading.is_the_tail_block, "one block, written into");
    words.dismantle();
}

#[test]
fn a_chains_dismantle_gives_every_block_back() {
    let _g = test_guard();
    let mut chain = Chain::empty();
    for entry in 0..BLOCK_ENTRIES + 1 {
        assert_eq!(chain.push(entry, fresh), Ok(()));
    }
    let mut given = 0;
    chain.dismantle(|block| {
        given += 1;
        gc_metadata::release(block);
    });
    assert_eq!(given, 2);
    assert!(chain.is_empty());
    assert_eq!(chain.len(), 0);
}

/// A retain whose `keep` unwinds leaves the chain whole on the pack's
/// terms: the entry in `keep`'s hands is kept as it stood, and so is every
/// entry behind it; what `keep` had dropped before stays dropped.
#[test]
fn a_retain_that_unwinds_keeps_the_entry_in_hand() {
    let _g = test_guard();
    let mut chain = Chain::empty();
    for entry in 0..10 {
        assert_eq!(chain.push(entry, fresh), Ok(()));
    }

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        chain.retain(
            |entry| match entry {
                0 | 1 => false,
                5 => panic!("keep unwinds with entry 5 in hand"),
                _ => true,
            },
            gc_metadata::release,
        );
    }));
    assert!(outcome.is_err());

    let mut kept = Vec::new();
    chain.walk(|entry| kept.push(entry));
    assert_eq!(kept, [2, 3, 4, 5, 6, 7, 8, 9]);
    assert_eq!(chain.len(), 8);
    chain.dismantle(gc_metadata::release);
}

/// The pack rewrites the writer's copy of `front`: a reader had advanced
/// `front` past the copy the writer last refreshed, the pack fills the block
/// from the new `front`, and without the rewrite the writer's full test
/// would never fire again — the next pushes would run through `front` and
/// the block would read empty with every packed entry lost.
#[test]
fn a_pack_over_a_front_the_reader_moved_keeps_the_writers_full_test_sound() {
    let _g = test_guard();
    let words = Words::new();
    let writer = unsafe { Writer::new(words.slots()) };
    let reader = unsafe { Reader::new(words.slots()) };

    // A full first block, the writer's copy of its `front` refreshed at 0
    // when the block change found it full; then five taken from it and
    // four written into the second block.
    for entry in 0..BLOCK_ENTRIES + 4 {
        assert!(writer.push(entry, fresh).is_ok());
    }
    let mut out = [0; 5];
    assert_eq!(reader.take(&mut out), 5);

    // Everything kept: the pack fills the first block from `front` at 5 up
    // to one slot short of it.
    let quiet = unsafe { Quiescent::new(words.slots()) };
    quiet.rewrite(Some);
    assert_eq!(quiet.count(), BLOCK_ENTRIES - 1);

    // Two pushes: the first takes the last free slot, the second must find
    // the block full and move into the emptied second block.
    assert!(writer.push(7_000_001, none).is_ok());
    assert!(writer.push(7_000_002, none).is_ok());
    assert_eq!(
        quiet.count(),
        BLOCK_ENTRIES + 1,
        "no packed entry was written over"
    );
    let mut all = vec![0; BLOCK_ENTRIES + 1];
    assert_eq!(reader.take(&mut all), BLOCK_ENTRIES + 1);
    assert_eq!(
        all[0], 5,
        "the packed entries come out from where the reader stood"
    );
    assert_eq!(all[BLOCK_ENTRIES - 1], 7_000_001);
    assert_eq!(all[BLOCK_ENTRIES], 7_000_002);
    words.dismantle();
}
