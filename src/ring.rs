//! A single-producer single-consumer ring of pool blocks: one thread writes
//! entries at the tail, another reads them behind it at the front, and
//! neither touches the other's index. The form is moodycamel's
//! `ReaderWriterQueue` (`rfc/dev/SPSC-QUEUE-SURVEY.md`; `rfc/dev/DECISIONS.md`,
//! "the candidate queue is read behind its writer, and the collector's
//! verdicts come back by a second ring"), over 64 KiB pool blocks instead
//! of `malloc`ed ones and with release/acquire on the index words instead of
//! fences.
//!
//! # The shape
//!
//! Blocks are linked in a circle through a `next` word the writer owns.
//! Each block holds `front` and `tail`, indices into its slots that wrap at
//! [`CAPACITY`] with one slot always left empty, so that `front == tail` is
//! the empty block and never the full one. `front` is the reader's word,
//! `tail` the writer's, and each stands on its own line beside a local copy
//! of the other's: the writer reads `front` only when its local copy says
//! the block is full, the reader reads `tail` only when its local copy says
//! the block is empty, so the common case of both touches no line the other
//! thread writes.
//!
//! ```text
//!         writer ──▶ tail block ──next──▶ consumed ──next──▶ consumed ─┐
//!                        ▲                                             │
//!                        └── … ◀──next── front block ◀── reader ◀──────┘
//! ```
//!
//! The writer fills the tail block and moves to `next` when that block is
//! not the reader's front block — every block between the tail and the
//! front around the circle has been read — and otherwise takes a fresh block
//! from the caller and links it in after the tail. The reader empties the
//! front block and moves to its `next` when the front block is not the tail
//! block. Neither ever passes the other.
//!
//! # Where the two block pointers live
//!
//! Not here. The ring is two words, the front block and the tail block, and
//! the caller keeps them where its own lines put them ([`Slots`]): the
//! reader's word beside what the reader owns, the writer's beside what the
//! writer owns. A ring with both words null holds no block and is empty; the
//! writer's first push takes its first block and publishes both words.
//!
//! # Who may do what
//!
//! [`Writer`] is the one producer's handle and [`Reader`] the one consumer's;
//! the two run on different threads at once. The writer also splices a chain
//! of blocks in after its tail block and unlinks a consumed block for
//! return, both with the reader running: the blocks past the tail block are
//! the writer's by the protocol. [`Quiescent`] is the owner's handle while no
//! reader is active: it walks the entries in place and packs the ring after
//! some are dropped. That nobody reads while a `Quiescent` acts is the
//! caller's exclusion to keep, not this module's.
//!
//! # Memory
//!
//! Nothing here allocates. A fresh block comes from the closure the writer
//! is handed, and a block leaves the ring only through the writer's unlink
//! and the owner's dismantle, which hand it back to the caller.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockHeader};

/// Bytes of one cache line, which each control word of a block is given.
const LINE: usize = 64;

/// Slots one block holds. One stays empty by construction, so a block holds
/// at most `CAPACITY - 1` entries.
pub(crate) const CAPACITY: usize = (BLOCK_PAYLOAD - 3 * LINE) / size_of::<usize>();

/// Entries one block holds when it is full.
pub(crate) const BLOCK_ENTRIES: usize = CAPACITY - 1;

/// The reader's line: the index it reads at, and its copy of the writer's.
#[repr(C, align(64))]
struct ReaderLine {
    /// Elements are read from here. Written by the reader with release,
    /// loaded by the writer with acquire when its local copy says full.
    front: AtomicUsize,
    /// The reader's copy of [`WriterLine::tail`], refreshed when it says
    /// the block is empty. The reader's alone.
    local_tail: UnsafeCell<usize>,
}

/// The writer's line: the index it writes at, and its copy of the reader's.
#[repr(C, align(64))]
struct WriterLine {
    /// Elements are written here. Written by the writer with release after
    /// the slot's store, loaded by the reader with acquire when its local
    /// copy says empty.
    tail: AtomicUsize,
    /// The writer's copy of [`ReaderLine::front`], refreshed when it says
    /// the block is full. The writer's alone.
    local_front: UnsafeCell<usize>,
}

/// The link's line: on its own so that the tail's stores never share a line
/// with a word the reader loads at a block change.
#[repr(C, align(64))]
struct LinkLine {
    /// The next block around the circle. The writer's word, stored with
    /// release when a block is linked in; the reader loads it with acquire
    /// at its block change, and only for a block that is not the tail block.
    next: AtomicPtr<BlockHeader>,
}

/// A block's payload in ring form.
#[repr(C)]
struct RingBlock {
    reader: ReaderLine,
    writer: WriterLine,
    link: LinkLine,
    /// The entries. A slot is written by the writer before the tail store
    /// that publishes it and read by the reader after the tail load that
    /// saw it, which is the ordering that makes the plain accesses sound;
    /// a slot the reader has passed is the writer's again after its acquire
    /// load of `front`.
    slots: [UnsafeCell<usize>; CAPACITY],
}

const _: () = assert!(size_of::<RingBlock>() == BLOCK_PAYLOAD);
const _: () = assert!(std::mem::offset_of!(RingBlock, writer) == LINE);
const _: () = assert!(std::mem::offset_of!(RingBlock, link) == 2 * LINE);

/// The index after `index`, wrapping at [`CAPACITY`].
#[inline]
fn step(index: usize) -> usize {
    if index + 1 == CAPACITY { 0 } else { index + 1 }
}

/// Entries between `front` and `tail` in one block.
#[inline]
fn span(front: usize, tail: usize) -> usize {
    if tail >= front {
        tail - front
    } else {
        tail + CAPACITY - front
    }
}

#[inline]
fn ring(block: *mut BlockHeader) -> *mut RingBlock {
    BlockHeader::payload_start(block) as *mut RingBlock
}

/// Put `block` in ring form: empty, unlinked.
///
/// # Safety
/// `block` is a pool block nobody else uses.
unsafe fn init_block(block: *mut BlockHeader) {
    let b = ring(block);
    unsafe {
        (*b).reader.front.store(0, Ordering::Relaxed);
        *(*b).reader.local_tail.get() = 0;
        (*b).writer.tail.store(0, Ordering::Relaxed);
        *(*b).writer.local_front.get() = 0;
        (*b).link
            .next
            .store(std::ptr::null_mut(), Ordering::Relaxed);
    }
}

/// The ring's two words, wherever the caller keeps them: the front block is
/// the reader's, the tail block the writer's. Both null is a ring with no
/// block.
#[derive(Clone, Copy)]
pub(crate) struct Slots<'a> {
    pub(crate) front_block: &'a AtomicPtr<BlockHeader>,
    pub(crate) tail_block: &'a AtomicPtr<BlockHeader>,
}

impl Slots<'_> {
    /// Make `block` the ring's one block, empty, linked to itself: the form
    /// a ring that never grows is given at its birth, so that its writer
    /// needs no fresh block and its reader finds a block to read.
    ///
    /// # Safety
    /// The ring holds no block, no `Writer`, `Reader` or `Quiescent` over
    /// these slots is in use, and `block` is a pool block nobody else uses.
    pub(crate) unsafe fn install_single_block(&self, block: *mut BlockHeader) {
        debug_assert!(
            self.front_block.load(Ordering::Relaxed).is_null(),
            "the ring holds no block"
        );
        unsafe {
            init_block(block);
            (*ring(block)).link.next.store(block, Ordering::Relaxed);
        }
        self.tail_block.store(block, Ordering::Release);
        self.front_block.store(block, Ordering::Release);
    }
}

/// What a push answers when the tail block is full and the caller's closure
/// gave no block: the entry was not written.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct NoBlock;

/// The one producer's handle.
pub(crate) struct Writer<'a>(Slots<'a>);

impl<'a> Writer<'a> {
    /// The writer over `slots`.
    ///
    /// # Safety
    /// The calling thread is the ring's one producer, and no other `Writer`
    /// over these slots is in use.
    pub(crate) unsafe fn new(slots: Slots<'a>) -> Self {
        Self(slots)
    }

    /// Append `entry`. The tail block takes it while it has room; a full
    /// tail block moves the writer to the next block of the circle when the
    /// reader is not in it, and otherwise to the block `fresh` answers, which
    /// is linked in after the tail. `fresh` answering null is [`NoBlock`],
    /// and the entry is not written.
    pub(crate) fn push(
        &self,
        entry: usize,
        fresh: impl FnOnce() -> *mut BlockHeader,
    ) -> Result<(), NoBlock> {
        let tail_block = self.0.tail_block.load(Ordering::Relaxed);
        if tail_block.is_null() {
            return self.push_into_fresh(entry, fresh, std::ptr::null_mut());
        }

        let b = ring(tail_block);
        let tail = unsafe { (*b).writer.tail.load(Ordering::Relaxed) };
        let next_tail = step(tail);
        let mut front = unsafe { *(*b).writer.local_front.get() };
        if next_tail == front {
            front = unsafe { (*b).reader.front.load(Ordering::Acquire) };
            unsafe { *(*b).writer.local_front.get() = front };
        }

        if next_tail != front {
            unsafe {
                *(*b).slots[tail].get() = entry;
                (*b).writer.tail.store(next_tail, Ordering::Release);
            }
            return Ok(());
        }

        // The tail block is full. The next block of the circle is free when
        // the reader is not in it: every block between the tail and the
        // front has been read to its end.
        let next = unsafe { (*b).link.next.load(Ordering::Relaxed) };
        if next != self.0.front_block.load(Ordering::Acquire) {
            let n = ring(next);
            let front = unsafe { (*n).reader.front.load(Ordering::Acquire) };
            let tail = unsafe { (*n).writer.tail.load(Ordering::Relaxed) };
            debug_assert_eq!(front, tail, "a block behind the front block is empty");
            unsafe {
                *(*n).writer.local_front.get() = front;
                *(*n).slots[tail].get() = entry;
                (*n).writer.tail.store(step(tail), Ordering::Release);
            }
            self.0.tail_block.store(next, Ordering::Release);
            return Ok(());
        }

        self.push_into_fresh(entry, fresh, tail_block)
    }

    /// Whether the next push writes into the tail block as it stands: the
    /// block has a free slot, read the way a push reads it — the local copy
    /// of `front` first, the reader's word only when the copy says full.
    /// False for a ring with no block and for a full tail block, whose
    /// writer moves on — into the next block of the circle or into a fresh
    /// one — before it writes.
    pub(crate) fn tail_block_has_room(&self) -> bool {
        let tail_block = self.0.tail_block.load(Ordering::Relaxed);
        if tail_block.is_null() {
            return false;
        }

        let b = ring(tail_block);
        let next_tail = step(unsafe { (*b).writer.tail.load(Ordering::Relaxed) });
        let mut front = unsafe { *(*b).writer.local_front.get() };
        if next_tail == front {
            front = unsafe { (*b).reader.front.load(Ordering::Acquire) };
            unsafe { *(*b).writer.local_front.get() = front };
        }

        next_tail != front
    }

    /// Entries the tail block takes before it is full, read the way a push
    /// reads its room: the local copy of `front` first, the reader's word
    /// only when the copy says the block is full. Zero for a ring with no
    /// block. A writer that may not take a fresh block — P's, which never
    /// grows — bounds what it writes by this before it writes.
    pub(crate) fn room_in_tail_block(&self) -> usize {
        let tail_block = self.0.tail_block.load(Ordering::Relaxed);
        if tail_block.is_null() {
            return 0;
        }

        let b = ring(tail_block);
        let tail = unsafe { (*b).writer.tail.load(Ordering::Relaxed) };
        let mut front = unsafe { *(*b).writer.local_front.get() };
        let mut room = BLOCK_ENTRIES - span(front, tail);
        if room == 0 {
            front = unsafe { (*b).reader.front.load(Ordering::Acquire) };
            unsafe { *(*b).writer.local_front.get() = front };
            room = BLOCK_ENTRIES - span(front, tail);
        }

        room
    }

    /// Take a block from `fresh`, write `entry` as its first slot, and link
    /// it in after `after` — or, with `after` null, as the ring's first
    /// block, publishing both words.
    fn push_into_fresh(
        &self,
        entry: usize,
        fresh: impl FnOnce() -> *mut BlockHeader,
        after: *mut BlockHeader,
    ) -> Result<(), NoBlock> {
        let block = fresh();
        if block.is_null() {
            return Err(NoBlock);
        }

        unsafe { init_block(block) };
        let n = ring(block);
        unsafe {
            *(*n).slots[0].get() = entry;
            (*n).writer.tail.store(1, Ordering::Relaxed);
        }

        if after.is_null() {
            // A circle of one. The reader's word is published by the writer
            // this once; from here the reader alone stores it. The tail
            // block goes first: the reader keys off the front block, and its
            // acquire of that word carries the tail block's store with it,
            // so a non-null front block is never read beside a null tail.
            unsafe { (*n).link.next.store(block, Ordering::Relaxed) };
            self.0.tail_block.store(block, Ordering::Release);
            self.0.front_block.store(block, Ordering::Release);
            return Ok(());
        }

        let a = ring(after);
        let after_next = unsafe { (*a).link.next.load(Ordering::Relaxed) };
        unsafe {
            (*n).link.next.store(after_next, Ordering::Relaxed);
            // The reader may see the new `next` before the new tail block;
            // it cannot advance into it before the tail block moves, since
            // it reads `next` only off a block that is not the tail block.
            (*a).link.next.store(block, Ordering::Release);
        }
        self.0.tail_block.store(block, Ordering::Release);
        Ok(())
    }

    /// Link the chain `first..=last` in after the tail block and make `last`
    /// the tail block, while the reader may be running: the chain's blocks
    /// lie inside the reader's region from the store of the tail block on,
    /// and the reader enters the first of them only through the old tail
    /// block's `next`, which is stored with release after the chain is
    /// complete. Every block of the chain holds at least one entry, so the
    /// reader's rule that the block ahead of an emptied front block holds an
    /// entry is kept; `last.next` is rewritten here.
    ///
    /// # Safety
    /// The chain's blocks are the caller's, in ring form as [`Chain`] leaves
    /// them, and none of them is in a ring.
    pub(crate) unsafe fn splice_after_tail(&self, first: *mut BlockHeader, last: *mut BlockHeader) {
        let tail = self.0.tail_block.load(Ordering::Relaxed);
        if tail.is_null() {
            unsafe { (*ring(last)).link.next.store(first, Ordering::Relaxed) };
            self.0.tail_block.store(last, Ordering::Release);
            self.0.front_block.store(first, Ordering::Release);
            return;
        }

        let t = ring(tail);
        let after = unsafe { (*t).link.next.load(Ordering::Relaxed) };
        unsafe {
            (*ring(last)).link.next.store(after, Ordering::Relaxed);
            (*t).link.next.store(first, Ordering::Release);
        }
        self.0.tail_block.store(last, Ordering::Release);
    }

    /// Unlink and answer the block after the tail block when it is empty and
    /// not the front block, or null: the one block the circle can spare
    /// while the reader runs, since the reader never walks past the tail
    /// block. Its emptiness is read with acquire, after the reader's release
    /// of its last read there.
    // The poll's shrink is its caller (`PLAN.md` S49.6); the tests drive it
    // until then.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn unlink_after_tail(&self) -> *mut BlockHeader {
        let tail = self.0.tail_block.load(Ordering::Relaxed);
        if tail.is_null() {
            return std::ptr::null_mut();
        }

        let t = ring(tail);
        let spare = unsafe { (*t).link.next.load(Ordering::Relaxed) };
        if spare == tail || spare == self.0.front_block.load(Ordering::Acquire) {
            return std::ptr::null_mut();
        }

        let s = ring(spare);
        let empty = unsafe {
            (*s).reader.front.load(Ordering::Acquire) == (*s).writer.tail.load(Ordering::Relaxed)
        };
        if !empty {
            return std::ptr::null_mut();
        }

        let after = unsafe { (*s).link.next.load(Ordering::Relaxed) };
        unsafe {
            (*t).link.next.store(after, Ordering::Release);
            (*s).link
                .next
                .store(std::ptr::null_mut(), Ordering::Relaxed);
        }
        spare
    }
}

/// Entries a [`Reader::peek`] read and has not yet consumed: where they
/// stand, so that [`Reader::commit`] can advance past exactly them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Peeked {
    /// The block the entries begin in, and how many of them it holds.
    first: *mut BlockHeader,
    in_first: usize,
    /// The block the entries continue in, read off `first`'s link before
    /// anything moved, and how many of them it holds; null for none.
    second: *mut BlockHeader,
    in_second: usize,
}

impl Peeked {
    /// Entries peeked.
    pub(crate) fn len(&self) -> usize {
        self.in_first + self.in_second
    }
}

/// The one consumer's handle: the owner's over P, and the collector's over
/// R.
pub(crate) struct Reader<'a>(Slots<'a>);

impl<'a> Reader<'a> {
    /// The reader over `slots`.
    ///
    /// # Safety
    /// The calling thread is the ring's one consumer, and no other `Reader`
    /// or [`Quiescent`] over these slots is in use.
    pub(crate) unsafe fn new(slots: Slots<'a>) -> Self {
        Self(slots)
    }

    /// Take up to `out.len()` entries from the front, oldest first, and
    /// answer how many were taken. Zero is the ring read empty.
    // The collector's batch reads through the peek/commit pair, and the
    // tests are the consuming read's only driver.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn take(&self, out: &mut [usize]) -> usize {
        let mut front_block = self.0.front_block.load(Ordering::Acquire);
        let mut taken = 0;
        while taken < out.len() && !front_block.is_null() {
            let b = ring(front_block);
            let front = unsafe { (*b).reader.front.load(Ordering::Relaxed) };
            let mut tail = unsafe { *(*b).reader.local_tail.get() };
            if front == tail {
                tail = unsafe { (*b).writer.tail.load(Ordering::Acquire) };
                unsafe { *(*b).reader.local_tail.get() = tail };
            }

            if front != tail {
                out[taken] = unsafe { *(*b).slots[front].get() };
                unsafe { (*b).reader.front.store(step(front), Ordering::Release) };
                taken += 1;
                continue;
            }

            // The front block read empty. Whether another block is ahead is
            // read off the tail block *after* that reading: the writer can
            // fill this block and move on between the two, and a reader
            // that read the tail block first would skip a filled block.
            #[cfg(test)]
            testing::between_the_reads();
            if front_block == self.0.tail_block.load(Ordering::Acquire) {
                break;
            }

            let tail = unsafe { (*b).writer.tail.load(Ordering::Acquire) };
            unsafe { *(*b).reader.local_tail.get() = tail };
            if front != tail {
                // Filled between the two reads: taken from here next round.
                continue;
            }

            let next = unsafe { (*b).link.next.load(Ordering::Acquire) };
            // The tail block moves only after a write into it, so the block
            // ahead holds an entry.
            self.0.front_block.store(next, Ordering::Release);
            front_block = next;
        }

        taken
    }

    /// Read up to `out.len()` entries from the front, oldest first, over at
    /// most two blocks, without consuming them: `front` and the front block
    /// stay where they are until [`Reader::commit`] moves them past exactly
    /// these entries. A consumer that must not lose entries it has not yet
    /// acted on reads this way and commits when it is done; `take` is the
    /// same read with the commit built in.
    pub(crate) fn peek(&self, out: &mut [usize]) -> Peeked {
        let mut peeked = Peeked {
            first: std::ptr::null_mut(),
            in_first: 0,
            second: std::ptr::null_mut(),
            in_second: 0,
        };
        let front_block = self.0.front_block.load(Ordering::Acquire);
        if front_block.is_null() || out.is_empty() {
            return peeked;
        }

        peeked.first = front_block;
        let (read, tail) = self.read_block(front_block, out);
        peeked.in_first = read;
        if read == out.len() {
            return peeked;
        }

        // The front block is read to its tail. Whether a block is ahead is
        // read off the tail block after that reading, then the front block's
        // tail again ([`Reader::take`] says why); a block ahead holds an
        // entry, and its link is read before anything moves.
        #[cfg(test)]
        testing::between_the_reads();
        if front_block == self.0.tail_block.load(Ordering::Acquire) {
            return peeked;
        }

        let b = ring(front_block);
        let tail_again = unsafe { (*b).writer.tail.load(Ordering::Acquire) };
        if tail_again != tail {
            unsafe { *(*b).reader.local_tail.get() = tail_again };
            let mut index = unsafe { (*b).reader.front.load(Ordering::Relaxed) };
            for _ in 0..read {
                index = step(index);
            }
            while index != tail_again && peeked.in_first < out.len() {
                out[peeked.in_first] = unsafe { *(*b).slots[index].get() };
                peeked.in_first += 1;
                index = step(index);
            }
            return peeked;
        }

        let next = unsafe { (*b).link.next.load(Ordering::Acquire) };
        peeked.second = next;
        let (read, _) = self.read_block(next, &mut out[peeked.in_first..]);
        peeked.in_second = read;
        peeked
    }

    /// Read `block`'s entries from its front into `out`, as many as fit, and
    /// answer how many and the tail they were read against.
    fn read_block(&self, block: *mut BlockHeader, out: &mut [usize]) -> (usize, usize) {
        let b = ring(block);
        let mut index = unsafe { (*b).reader.front.load(Ordering::Relaxed) };
        let mut tail = unsafe { *(*b).reader.local_tail.get() };
        if index == tail {
            tail = unsafe { (*b).writer.tail.load(Ordering::Acquire) };
            unsafe { *(*b).reader.local_tail.get() = tail };
        }

        let mut read = 0;
        while index != tail && read < out.len() {
            out[read] = unsafe { *(*b).slots[index].get() };
            read += 1;
            index = step(index);
        }
        (read, tail)
    }

    /// Consume what `peeked` read: advance the first block's front past its
    /// entries, move the front block to the second where the read crossed,
    /// and advance that block's front past the rest. Three stores at most,
    /// each a release, and the caller's guard is what makes them one act
    /// against an unwind between them.
    pub(crate) fn commit(&self, peeked: Peeked) {
        if peeked.first.is_null() {
            return;
        }

        let f = ring(peeked.first);
        let mut front = unsafe { (*f).reader.front.load(Ordering::Relaxed) };
        for _ in 0..peeked.in_first {
            front = step(front);
        }
        unsafe { (*f).reader.front.store(front, Ordering::Release) };
        if peeked.second.is_null() {
            return;
        }

        self.0.front_block.store(peeked.second, Ordering::Release);
        let s = ring(peeked.second);
        let mut front = unsafe { (*s).reader.front.load(Ordering::Relaxed) };
        for _ in 0..peeked.in_second {
            front = step(front);
        }
        unsafe { (*s).reader.front.store(front, Ordering::Release) };
    }

    /// Consume the first `count` entries without reading them: the front
    /// moves past them block by block as [`Reader::take`] would move it,
    /// and the block ahead of a drained one is entered on the same double
    /// read. A caller that read the entries in place and answered for every
    /// one of them advances this way.
    ///
    /// # Panics
    /// In a debug build, when fewer than `count` entries stand.
    pub(crate) fn advance(&self, mut count: usize) {
        let mut front_block = self.0.front_block.load(Ordering::Acquire);
        while count != 0 {
            debug_assert!(!front_block.is_null(), "the entries stood");
            let b = ring(front_block);
            let front = unsafe { (*b).reader.front.load(Ordering::Relaxed) };
            let mut tail = unsafe { *(*b).reader.local_tail.get() };
            if front == tail {
                tail = unsafe { (*b).writer.tail.load(Ordering::Acquire) };
                unsafe { *(*b).reader.local_tail.get() = tail };
            }

            let here = span(front, tail).min(count);
            if here != 0 {
                let mut index = front;
                for _ in 0..here {
                    index = step(index);
                }
                unsafe { (*b).reader.front.store(index, Ordering::Release) };
                count -= here;
                continue;
            }

            debug_assert!(
                front_block != self.0.tail_block.load(Ordering::Acquire),
                "the entries stood"
            );
            let next = unsafe { (*b).link.next.load(Ordering::Acquire) };
            self.0.front_block.store(next, Ordering::Release);
            front_block = next;
        }
    }

    /// Entries not yet taken, as of the tail the reader sees now: the front
    /// block's span and every block's between it and the tail block.
    // The collector's round reads it (`PLAN.md` S49.7); the tests drive it
    // until then.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn unread(&self) -> usize {
        let front_block = self.0.front_block.load(Ordering::Acquire);
        if front_block.is_null() {
            return 0;
        }

        let tail_block = self.0.tail_block.load(Ordering::Acquire);
        let mut count = 0;
        let mut block = front_block;
        loop {
            let b = ring(block);
            let front = unsafe { (*b).reader.front.load(Ordering::Relaxed) };
            let tail = unsafe { (*b).writer.tail.load(Ordering::Acquire) };
            count += span(front, tail);
            if block == tail_block {
                return count;
            }

            block = unsafe { (*b).link.next.load(Ordering::Acquire) };
        }
    }
}

/// The owner's handle while no reader is active: every word of the ring is
/// its own to read and write plainly.
pub(crate) struct Quiescent<'a>(Slots<'a>);

impl<'a> Quiescent<'a> {
    /// The owner's handle over `slots`.
    ///
    /// # Safety
    /// No [`Reader`] over these slots runs while this handle is in use, and
    /// the calling thread is the ring's one producer.
    pub(crate) unsafe fn new(slots: Slots<'a>) -> Self {
        Self(slots)
    }

    fn front_block(&self) -> *mut BlockHeader {
        self.0.front_block.load(Ordering::Relaxed)
    }

    fn tail_block(&self) -> *mut BlockHeader {
        self.0.tail_block.load(Ordering::Relaxed)
    }

    /// Whether the ring holds a block at all.
    pub(crate) fn has_blocks(&self) -> bool {
        !self.front_block().is_null()
    }

    /// Entries between the front and the tail, with no entry read.
    pub(crate) fn count(&self) -> usize {
        let mut count = 0;
        self.for_each_block_in_order(|b| {
            count += unsafe {
                span(
                    (*b).reader.front.load(Ordering::Relaxed),
                    (*b).writer.tail.load(Ordering::Relaxed),
                )
            };
        });
        count
    }

    /// Call `visit` on every entry from the front to the tail, oldest first,
    /// stopping at the first answer of false. **False** when it stopped.
    pub(crate) fn walk(&self, mut visit: impl FnMut(usize) -> bool) -> bool {
        let mut ok = true;
        self.for_each_block_in_order(|b| {
            if !ok {
                return;
            }
            let mut index = unsafe { (*b).reader.front.load(Ordering::Relaxed) };
            let tail = unsafe { (*b).writer.tail.load(Ordering::Relaxed) };
            while index != tail {
                if !visit(unsafe { *(*b).slots[index].get() }) {
                    ok = false;
                    return;
                }
                index = step(index);
            }
        });
        ok
    }

    /// Rewrite every entry from the front to the tail in place: `keep`
    /// answers the word to write back, or `None` to drop the entry. The kept
    /// words are packed from the front block's front in their order, the tail
    /// block becomes the last block with a kept entry, every block whose tail
    /// moved has the reader's local copy rewritten to match, and the blocks
    /// past the new tail stay in the circle, empty, for the writer's next
    /// round.
    ///
    /// **An unwind out of `keep` leaves the ring whole**, on the terms of
    /// [`Packing`]: the entry in `keep`'s hands is kept as it stood.
    #[cfg(test)]
    pub(crate) fn rewrite(&self, mut keep: impl FnMut(usize) -> Option<usize>) {
        let mut pass = self.packing();
        while let Some(entry) = pass.read() {
            match keep(entry) {
                Some(kept) => pass.write(kept),
                None => pass.discard(),
            }
        }
    }

    /// Open a packing pass over the ring, for a caller that decides each
    /// entry's fate in its own frame ([`Packing`]).
    pub(crate) fn packing(&self) -> Packing<'_> {
        Packing::open(self)
    }

    /// Hand `rewrite` each of the first `count` entries from the front in
    /// place, moving nothing: the marks a reading writes over the entries it
    /// read, and the null an owner writes over one it has answered for. The
    /// slot is the closure's for the call, so what it writes stands before
    /// anything the closure does after the write.
    pub(crate) fn map_prefix_in_place(&self, count: usize, mut rewrite: impl FnMut(&mut usize)) {
        let mut left = count;
        self.for_each_block_in_order(|b| {
            let mut index = unsafe { (*b).reader.front.load(Ordering::Relaxed) };
            let tail = unsafe { (*b).writer.tail.load(Ordering::Relaxed) };
            while left != 0 && index != tail {
                rewrite(unsafe { &mut *(*b).slots[index].get() });
                index = step(index);
                left -= 1;
            }
        });
        debug_assert_eq!(left, 0, "the prefix is within the entries");
    }

    /// Fill the tail block to capacity with `entry`, for a case that reaches
    /// the writer's block change without the registrations that would fill
    /// it. A ring with no block is left as it is.
    #[cfg(test)]
    pub(crate) fn fill_tail_block(&self, entry: usize) {
        let tail_block = self.tail_block();
        if tail_block.is_null() {
            return;
        }

        let b = ring(tail_block);
        let front = unsafe { (*b).reader.front.load(Ordering::Relaxed) };
        let mut tail = unsafe { (*b).writer.tail.load(Ordering::Relaxed) };
        while step(tail) != front {
            unsafe { *(*b).slots[tail].get() = entry };
            tail = step(tail);
        }
        unsafe { (*b).writer.tail.store(tail, Ordering::Relaxed) };
    }
}

/// One packing pass over a ring: a read cursor over the entries from the
/// front, and a write cursor that runs through the same blocks behind it,
/// never passing it since it writes at most what was read. [`Packing::read`]
/// takes the next entry in hand; the caller then answers for it once, with
/// [`Packing::write`] of the word to keep, [`Packing::keep`] of the entry as
/// it stood, or [`Packing::discard`]. The drop closes the pass, so an unwind
/// leaves the ring whole: the entry in hand, if the caller had not yet
/// answered for it, is kept as it stood, and every entry not yet read is
/// packed behind the ones written.
pub(crate) struct Packing<'a> {
    ring: &'a Quiescent<'a>,
    last: *mut BlockHeader,
    read_block: *mut BlockHeader,
    read: usize,
    read_tail: usize,
    write_block: *mut BlockHeader,
    write: usize,
    /// The entry in hand, and whether the caller has still to answer for it.
    in_hand: usize,
    holding: bool,
}

impl<'a> Packing<'a> {
    fn open(quiet: &'a Quiescent<'a>) -> Self {
        let first = quiet.front_block();
        let (read, read_tail) = if first.is_null() {
            (0, 0)
        } else {
            unsafe {
                (
                    (*ring(first)).reader.front.load(Ordering::Relaxed),
                    (*ring(first)).writer.tail.load(Ordering::Relaxed),
                )
            }
        };
        Self {
            ring: quiet,
            last: quiet.tail_block(),
            read_block: first,
            read,
            read_tail,
            write_block: first,
            write: read,
            in_hand: 0,
            holding: false,
        }
    }

    /// Take the next entry from the front in hand, or `None` past the tail.
    /// An entry still in hand from the last read is kept as it stood.
    pub(crate) fn read(&mut self) -> Option<usize> {
        if self.holding {
            self.keep();
        }

        let entry = self.next_entry();
        if let Some(entry) = entry {
            self.in_hand = entry;
            self.holding = true;
        }
        entry
    }

    /// Keep the entry in hand as it stood.
    pub(crate) fn keep(&mut self) {
        debug_assert!(self.holding, "an entry is in hand");
        let entry = self.in_hand;
        self.write(entry);
    }

    /// Drop the entry in hand: it is the caller's from here.
    pub(crate) fn discard(&mut self) {
        debug_assert!(self.holding, "an entry is in hand");
        self.holding = false;
    }

    /// The next entry from the front, or `None` past the tail.
    fn next_entry(&mut self) -> Option<usize> {
        loop {
            if self.read_block.is_null() {
                return None;
            }

            if self.read != self.read_tail {
                let entry = unsafe { *(*ring(self.read_block)).slots[self.read].get() };
                self.read = step(self.read);
                return Some(entry);
            }

            if self.read_block == self.last {
                self.read_block = std::ptr::null_mut();
                return None;
            }

            self.read_block = unsafe { (*ring(self.read_block)).link.next.load(Ordering::Relaxed) };
            let rb = ring(self.read_block);
            self.read = unsafe { (*rb).reader.front.load(Ordering::Relaxed) };
            self.read_tail = unsafe { (*rb).writer.tail.load(Ordering::Relaxed) };
        }
    }

    /// Write `kept` at the write cursor, in place of the entry in hand.
    pub(crate) fn write(&mut self, kept: usize) {
        self.holding = false;
        let wb = ring(self.write_block);
        if step(self.write) == unsafe { (*wb).reader.front.load(Ordering::Relaxed) } {
            // The write block is full: close it and move on. The next block
            // is the one the read cursor is in or one it has passed, so its
            // front is where its entries begin.
            unsafe { self.ring.set_tail(self.write_block, self.write) };
            self.write_block = unsafe { (*wb).link.next.load(Ordering::Relaxed) };
            self.write = unsafe {
                (*ring(self.write_block))
                    .reader
                    .front
                    .load(Ordering::Relaxed)
            };
        }
        unsafe { *(*ring(self.write_block)).slots[self.write].get() = kept };
        self.write = step(self.write);
    }
}

impl Drop for Packing<'_> {
    fn drop(&mut self) {
        if self.last.is_null() {
            return;
        }

        // The unwind's case, and a no-op on the return: an entry in hand
        // the caller never answered for is kept, and so is everything not
        // yet read.
        if self.holding {
            self.keep();
        }
        while let Some(entry) = self.next_entry() {
            self.write(entry);
        }

        // Close the write block and empty every block from it to the old
        // tail block.
        unsafe { self.ring.set_tail(self.write_block, self.write) };
        let mut block = self.write_block;
        while block != self.last {
            block = unsafe { (*ring(block)).link.next.load(Ordering::Relaxed) };
            let b = ring(block);
            let front = unsafe { (*b).reader.front.load(Ordering::Relaxed) };
            unsafe { self.ring.set_tail(block, front) };
        }
        self.ring
            .0
            .tail_block
            .store(self.write_block, Ordering::Relaxed);
    }
}

impl<'a> Quiescent<'a> {
    /// Set `block`'s tail, the reader's local copy of it, and the writer's
    /// local copy of `front`. The last is what makes the writer's full test
    /// sound again: the pass writes from the block's current `front`, which
    /// a reader may have advanced past the copy the writer refreshed last,
    /// and a copy left behind lets the next pushes run through `front`.
    unsafe fn set_tail(&self, block: *mut BlockHeader, tail: usize) {
        let b = ring(block);
        unsafe {
            (*b).writer.tail.store(tail, Ordering::Relaxed);
            *(*b).reader.local_tail.get() = tail;
            *(*b).writer.local_front.get() = (*b).reader.front.load(Ordering::Relaxed);
        }
    }

    /// Take every block out of the ring, handing each to `give_back` in
    /// circle order from the front block, and leave the ring with no block.
    /// The entries go with the blocks.
    pub(crate) fn dismantle(&self, mut give_back: impl FnMut(*mut BlockHeader)) {
        let first = self.front_block();
        if first.is_null() {
            return;
        }

        self.0
            .front_block
            .store(std::ptr::null_mut(), Ordering::Relaxed);
        self.0
            .tail_block
            .store(std::ptr::null_mut(), Ordering::Relaxed);
        let mut block = first;
        loop {
            let next = unsafe { (*ring(block)).link.next.load(Ordering::Relaxed) };
            unsafe {
                (*ring(block))
                    .link
                    .next
                    .store(std::ptr::null_mut(), Ordering::Relaxed)
            };
            give_back(block);
            if next == first {
                return;
            }
            block = next;
        }
    }

    /// Blocks in the circle.
    #[cfg(test)]
    pub(crate) fn block_count(&self) -> usize {
        let first = self.front_block();
        if first.is_null() {
            return 0;
        }

        let mut count = 1;
        let mut block = unsafe { (*ring(first)).link.next.load(Ordering::Relaxed) };
        while block != first {
            count += 1;
            block = unsafe { (*ring(block)).link.next.load(Ordering::Relaxed) };
        }
        count
    }

    /// `visit` over the blocks from the front block to the tail block, in
    /// read order.
    fn for_each_block_in_order(&self, mut visit: impl FnMut(*mut RingBlock)) {
        let mut block = self.front_block();
        if block.is_null() {
            return;
        }
        let last = self.tail_block();
        loop {
            visit(ring(block));
            if block == last {
                return;
            }
            block = unsafe { (*ring(block)).link.next.load(Ordering::Relaxed) };
        }
    }
}

/// A linear chain of blocks in ring form, filled by its owner alone and
/// spliced into a ring whole ([`Writer::splice_after_tail`]). Every block
/// but the last is full, and the last holds at least one entry: an empty
/// block would break the reader's rule that the block ahead of an emptied
/// front block holds an entry.
pub(crate) struct Chain {
    first: *mut BlockHeader,
    last: *mut BlockHeader,
    entries: usize,
}

impl Chain {
    pub(crate) const fn empty() -> Self {
        Self {
            first: std::ptr::null_mut(),
            last: std::ptr::null_mut(),
            entries: 0,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.first.is_null()
    }

    /// Entries the chain holds.
    pub(crate) fn len(&self) -> usize {
        self.entries
    }

    /// Append `entry`, taking a block from `fresh` when the last one is full
    /// or there is none; null from `fresh` is [`NoBlock`].
    pub(crate) fn push(
        &mut self,
        entry: usize,
        fresh: impl FnOnce() -> *mut BlockHeader,
    ) -> Result<(), NoBlock> {
        if !self.last.is_null() {
            let l = ring(self.last);
            let tail = unsafe { (*l).writer.tail.load(Ordering::Relaxed) };
            if tail < BLOCK_ENTRIES {
                unsafe {
                    *(*l).slots[tail].get() = entry;
                    (*l).writer.tail.store(tail + 1, Ordering::Relaxed);
                    *(*l).reader.local_tail.get() = tail + 1;
                }
                self.entries += 1;
                return Ok(());
            }
        }

        let block = fresh();
        if block.is_null() {
            return Err(NoBlock);
        }

        unsafe { init_block(block) };
        let b = ring(block);
        unsafe {
            *(*b).slots[0].get() = entry;
            (*b).writer.tail.store(1, Ordering::Relaxed);
            *(*b).reader.local_tail.get() = 1;
        }
        if self.last.is_null() {
            self.first = block;
        } else {
            unsafe { (*ring(self.last)).link.next.store(block, Ordering::Relaxed) };
        }
        self.last = block;
        self.entries += 1;
        Ok(())
    }

    /// Keep the entries `keep` answers true for, packed in their order, and
    /// drop the rest; a block left with no entry leaves the chain through
    /// `give_back`, so that the chain keeps its rule of no empty block.
    ///
    /// **An unwind out of `keep` leaves the chain whole**, on the terms of
    /// [`Quiescent::rewrite`]: the entry in `keep`'s hands is dropped and
    /// every entry behind it is kept.
    pub(crate) fn retain(
        &mut self,
        mut keep: impl FnMut(usize) -> bool,
        give_back: impl FnMut(*mut BlockHeader),
    ) {
        let mut pass = Retaining::open(self, give_back);
        while let Some(entry) = pass.read() {
            if keep(entry) {
                pass.write(entry);
            }
        }
    }

    /// Call `visit` on every entry, oldest first.
    #[cfg(test)]
    pub(crate) fn walk(&self, mut visit: impl FnMut(usize)) {
        let mut block = self.first;
        while !block.is_null() {
            let b = ring(block);
            let tail = unsafe { (*b).writer.tail.load(Ordering::Relaxed) };
            for index in 0..tail {
                visit(unsafe { *(*b).slots[index].get() });
            }
            block = unsafe { (*b).link.next.load(Ordering::Relaxed) };
        }
    }

    /// The chain's blocks, first and last, leaving this empty: what a splice
    /// takes.
    pub(crate) fn take(&mut self) -> Option<(*mut BlockHeader, *mut BlockHeader)> {
        if self.first.is_null() {
            return None;
        }

        let taken = (self.first, self.last);
        *self = Self::empty();
        Some(taken)
    }

    /// Hand every block to `give_back`, entries and all, leaving this empty.
    pub(crate) fn dismantle(&mut self, mut give_back: impl FnMut(*mut BlockHeader)) {
        let mut block = self.first;
        while !block.is_null() {
            let next = unsafe { (*ring(block)).link.next.load(Ordering::Relaxed) };
            give_back(block);
            block = next;
        }
        *self = Self::empty();
    }

    /// Blocks in the chain.
    #[cfg(test)]
    pub(crate) fn block_count(&self) -> usize {
        let mut count = 0;
        let mut block = self.first;
        while !block.is_null() {
            count += 1;
            block = unsafe { (*ring(block)).link.next.load(Ordering::Relaxed) };
        }
        count
    }
}

/// One pass of [`Chain::retain`]: a read and a write cursor inside the block
/// in hand, the block before it for the unlink, and a drop that finishes the
/// pass on an unwind by keeping every entry not yet read.
struct Retaining<'a, G: FnMut(*mut BlockHeader)> {
    chain: &'a mut Chain,
    give_back: G,
    block: *mut BlockHeader,
    read: usize,
    read_tail: usize,
    write: usize,
    previous: *mut BlockHeader,
}

impl<'a, G: FnMut(*mut BlockHeader)> Retaining<'a, G> {
    fn open(chain: &'a mut Chain, give_back: G) -> Self {
        let first = chain.first;
        chain.entries = 0;
        let mut pass = Self {
            chain,
            give_back,
            block: first,
            read: 0,
            read_tail: 0,
            write: 0,
            previous: std::ptr::null_mut(),
        };
        pass.enter_block();
        pass
    }

    /// Start the cursors on the block in hand.
    fn enter_block(&mut self) {
        self.read = 0;
        self.write = 0;
        self.read_tail = if self.block.is_null() {
            0
        } else {
            unsafe { (*ring(self.block)).writer.tail.load(Ordering::Relaxed) }
        };
    }

    /// The next entry, or `None` past the last block. Leaving a block
    /// closes it ([`Retaining::close_block`]).
    fn read(&mut self) -> Option<usize> {
        loop {
            if self.block.is_null() {
                return None;
            }

            if self.read != self.read_tail {
                let entry = unsafe { *(*ring(self.block)).slots[self.read].get() };
                self.read += 1;
                return Some(entry);
            }

            let next = unsafe { (*ring(self.block)).link.next.load(Ordering::Relaxed) };
            self.close_block();
            self.block = next;
            self.enter_block();
        }
    }

    fn write(&mut self, kept: usize) {
        unsafe { *(*ring(self.block)).slots[self.write].get() = kept };
        self.chain.entries += 1;
        self.write += 1;
    }

    /// Close the block in hand at the write cursor: its tail is what was
    /// written, and a block with nothing written is unlinked and given back.
    fn close_block(&mut self) {
        let b = ring(self.block);
        let next = unsafe { (*b).link.next.load(Ordering::Relaxed) };
        if self.write != 0 {
            unsafe {
                (*b).writer.tail.store(self.write, Ordering::Relaxed);
                *(*b).reader.local_tail.get() = self.write;
            }
            self.previous = self.block;
            if next.is_null() {
                self.chain.last = self.block;
            }
            return;
        }

        if self.previous.is_null() {
            self.chain.first = next;
        } else {
            unsafe {
                (*ring(self.previous))
                    .link
                    .next
                    .store(next, Ordering::Relaxed)
            };
        }
        if next.is_null() {
            self.chain.last = self.previous;
        }
        unsafe {
            (*b).link
                .next
                .store(std::ptr::null_mut(), Ordering::Relaxed)
        };
        (self.give_back)(self.block);
    }
}

impl<G: FnMut(*mut BlockHeader)> Drop for Retaining<'_, G> {
    fn drop(&mut self) {
        // The unwind's case, and a no-op on the return: every entry not yet
        // read is kept, in the block in hand and in the blocks behind it.
        while let Some(entry) = self.read() {
            self.write(entry);
        }
    }
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests;
