//! The live core a collector's batch read, handed to the mutator as a list of
//! entity addresses and stamped by the mutator without a descent: at its take
//! of the token from `POSTED`, or earlier, at the first return of a block or a
//! run of its own under `POSTED`; and dropped unstamped under pressure and at
//! the exit (`rfc/model/gc/rc-cycle.md`, "The live list of a batch";
//! `rfc/dev/design/trace-token-handshake.md`, E13).
//!
//! # Why a list, and why flat
//!
//! A part of a collector's batch that reads its root live has read a live
//! core, and a maturation stamp on each member is what the next trace's prune
//! stops at (`crate::cycle::mark`). Byte 6 has one writer, the owner
//! (`rfc/model/classes.md`, "Flags layout"), so the collector lists the
//! members and the owner writes the stamps. The stamp is `{e, 1}` on every
//! listed member, where `e` is the epoch the batch's arena read: at the
//! traversal threshold of one it prunes what a stamp per strongly connected
//! component would (`crate::cycle::maturation`), so no component is computed
//! and nothing is descended.
//!
//! # The chain
//!
//! One chain per grant of at most [`MAX_BLOCKS`] GC blocks threaded through
//! `BlockHeader::next`, each holding [`ENTRIES_PER_BLOCK`] entity pointers in
//! its payload and its count and the epoch cell's reading in its header line
//! ([`ListBlock`]). The collector appends after each part whose root it read
//! live, walking that part's live rows before the arena's reset
//! ([`Writer::append_the_part`]), and publishes the head on the record's hold
//! line before the release that stores `POSTED`. A block the pool refuses and
//! a chain at its bound keep what is written and take no more: any subset of
//! the live core is safe to stamp. The walk reads the recall every
//! `crate::cycle::arena::RECALL_STRIDE` rows and stops at a recall, keeping
//! what it wrote, so the list adds no walk the recall cannot stop before the
//! release (`dev/S65-PLAN-CRITIC.md`, F1).
//!
//! A list the owner has not taken by the epoch's next advance is given back
//! by the collector the record is named to, at its round's visit
//! ([`give_back_a_stale_list`]): an owner asleep under `POSTED` would
//! otherwise hold the chain's blocks for its whole sleep, and after the
//! advance its own take would give the list back unread. The owner and the
//! collector each take the list by one swap of the word, and the side that
//! reads it non-null owns it.
//!
//! The blocks are drawn on the collector's thread and given back on the
//! mutator's, the one GC memory that crosses threads: the test build's
//! per-thread figures move with them ([`gc_metadata::hand_over`]).
//!
//! # What a stamp may land on
//!
//! Between `POSTED` and the take the mutator runs free, returning memory as
//! under `FREE`: a listed member can die and its slot be handed out again. A
//! stamp on a slot that is free is overwritten by the next publication, which
//! stores the whole header word, and a stamp on the slot's next occupant
//! prunes at it until the turnover — recall, and never a wrong free, the
//! exact validation reading no stamp. What a stamp must never land on is
//! memory the thread gave back, so a block or a run leaving the thread under
//! `POSTED` with a list standing is stamped from first
//! ([`stamp_before_a_return`]), and after that the list is gone.

use std::ops::ControlFlow;

use crate::cycle::arena::TraceScratchArena;
use crate::cycle::mutator_record::MutatorRecord;
use crate::cycle::row;
use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockHeader, LINE_SIZE};
use crate::memory::gc_metadata;
use crate::refcount::RcHeader;

/// L: the most blocks one grant's list takes, 130,560 entries — the live core
/// of the package's largest example, 130,000 members, in one list
/// (`dev/CYCLE-SPLIT-PACKAGE-3.md`, section 5). A borrowed number: the rig of
/// S65.17 reads it (`PLAN.md`).
pub(crate) const MAX_BLOCKS: usize = 16;

/// Entity pointers one block's payload holds.
pub(crate) const ENTRIES_PER_BLOCK: usize = BLOCK_PAYLOAD / size_of::<*mut RcHeader>();

/// A block of the chain, as its header line lays it out: the pool's header,
/// whose `next` links the chain, then the entries the payload holds and the
/// epoch cell as the batch's arena read it.
#[repr(C)]
struct ListBlock {
    header: BlockHeader,
    entries: usize,
    turnovers: u64,
}

const _: () = assert!(size_of::<ListBlock>() <= LINE_SIZE);

/// The first of `block`'s entries.
///
/// # Safety
/// `block` is a block of a chain.
unsafe fn entries_of(block: *mut ListBlock) -> *mut *mut RcHeader {
    BlockHeader::payload_start(block.cast()).cast()
}

/// Give a chain's blocks back to the pool, on the thread whose figures
/// hold them.
///
/// # Safety
/// `head` is the first block of a chain nobody else reads, or null.
unsafe fn release_chain(mut block: *mut ListBlock) {
    while !block.is_null() {
        let next = unsafe { (*block).header.next }.cast::<ListBlock>();
        gc_metadata::discharge(BLOCK_PAYLOAD);
        gc_metadata::release(block.cast());
        block = next;
    }
}

/// The collector's side: the chain one grant's batch writes.
///
/// Dropped unpublished — a batch that unwound, or one whose list no part
/// wrote into — it gives its blocks back on the collector's thread.
pub(crate) struct Writer {
    head: *mut ListBlock,
    tail: *mut ListBlock,
    blocks: usize,
    /// The epoch cell as the batch's arena read it, which the mutator
    /// compares with its own before it stamps.
    turnovers: u64,
    /// Whether the chain takes no more: it holds [`MAX_BLOCKS`], or the pool
    /// refused a block.
    closed: bool,
}

impl Writer {
    /// An empty chain for a batch whose arena read the epoch cell at
    /// `turnovers`. Draws nothing until the first append.
    pub(crate) fn new(turnovers: u64) -> Self {
        Self {
            head: std::ptr::null_mut(),
            tail: std::ptr::null_mut(),
            blocks: 0,
            turnovers,
            closed: false,
        }
    }

    /// Append the address of every entity a completed part's rows left live,
    /// reading the recall every stride of rows: `Break` where it stood, with
    /// the entries written before the reading kept. They are live rows of a
    /// part that completed, as safe to stamp as a chain closed at its bound,
    /// and keeping them puts nothing between the reading and the release.
    ///
    /// # Safety
    /// The part completed on this thread and its rows still stand: after the
    /// scan and before the arena's reset to the watermark.
    pub(crate) unsafe fn append_the_part(
        &mut self,
        arena: &mut TraceScratchArena,
    ) -> ControlFlow<()> {
        let mut recalled = false;
        #[cfg(test)]
        let mut raised_at = None;
        let mut array = arena.touched_head();
        while !array.is_null() && !self.closed && !recalled {
            let (block, population) = unsafe { ((*array).block, (*array).population) };
            let _ = unsafe {
                row::for_each_live_met(array, block, population, |index| {
                    if arena.inspect_position().is_break() {
                        recalled = true;
                        return ControlFlow::Break(());
                    }

                    // A row whose address cannot be recovered is left out, as
                    // the stamp's own walk leaves it (`crate::cycle::maturation`).
                    let Some(entity) = row::entity_at(block, population, index) else {
                        return ControlFlow::Continue(());
                    };
                    if !self.push(entity) {
                        return ControlFlow::Break(());
                    }

                    #[cfg(test)]
                    if testing::after_a_listed_row() {
                        raised_at = Some(arena.positions_inspected());
                    }
                    ControlFlow::Continue(())
                })
            };
            array = unsafe { (*array).next };
        }

        #[cfg(test)]
        if let Some(from) = raised_at {
            crate::cycle::worker::testing::note_positions_after_the_hook(
                arena.positions_inspected() - from,
            );
        }
        if recalled {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }

    /// Leave the chain on `mutator`'s hold line for the mutator's take, and
    /// with it this thread's hold on its blocks, `published_at` being the
    /// serve clock's reading ([`give_back_a_stale_list`]). Under the grant,
    /// before its release.
    pub(crate) fn publish(self, mutator: &MutatorRecord, published_at: u64) {
        let this = std::mem::ManuallyDrop::new(self);
        if this.head.is_null() {
            return;
        }

        gc_metadata::hand_over(this.blocks, this.blocks * BLOCK_PAYLOAD);
        mutator.publish_live_list(this.head.cast(), published_at);
    }

    /// Write `entity` at the chain's end, growing it by a block where the
    /// last is full; false once the chain takes no more.
    fn push(&mut self, entity: *mut RcHeader) -> bool {
        if (self.tail.is_null() || unsafe { (*self.tail).entries } == ENTRIES_PER_BLOCK)
            && !self.grow()
        {
            return false;
        }

        unsafe {
            let entries = (*self.tail).entries;
            entries_of(self.tail).add(entries).write(entity);
            (*self.tail).entries = entries + 1;
        }
        true
    }

    /// Link one more block at the chain's end, or close the chain at its
    /// bound or at the pool's refusal.
    fn grow(&mut self) -> bool {
        if self.blocks == max_blocks() {
            self.closed = true;
            return false;
        }

        let block = gc_metadata::acquire().cast::<ListBlock>();
        if block.is_null() {
            self.closed = true;
            return false;
        }

        gc_metadata::charge(BLOCK_PAYLOAD);
        unsafe {
            (&raw mut (*block).header.next).write(std::ptr::null_mut());
            (&raw mut (*block).entries).write(0);
            (&raw mut (*block).turnovers).write(self.turnovers);
        }
        if self.tail.is_null() {
            self.head = block;
        } else {
            unsafe { (*self.tail).header.next = block.cast() };
        }
        self.tail = block;
        self.blocks += 1;
        true
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        unsafe { release_chain(self.head) };
    }
}

/// [`MAX_BLOCKS`], or the bound a case set (`testing::bound_the_chain`).
fn max_blocks() -> usize {
    #[cfg(test)]
    if let Some(bound) = testing::chain_bound() {
        return bound;
    }

    MAX_BLOCKS
}

/// Stamp from the list the last grant left on this thread's record, and give
/// its blocks back: the take of the token from `POSTED` off the poll and by
/// the explicit call. Nothing where no list stands.
///
/// The stamps are written in the epoch the batch's arena read, and only while
/// the cell still reads it: after an advance a stamp of the old epoch reads as
/// no stamp at all, and the list is given back unread.
///
/// # Safety
/// `record` is this thread's, and this thread has just read `POSTED` on it
/// — by the take that consumed it, or under a hold that leaves it.
pub(crate) unsafe fn stamp_from(record: &MutatorRecord) {
    let _ = unsafe { consume(record, true) };
}

/// Give back the list the last grant left on this thread's record without
/// stamping from it: under pressure, whose collection is after memory rather
/// than after the next trace's saving, and at the exit, whose heap no later
/// trace reads.
///
/// # Safety
/// As [`stamp_from`].
pub(crate) unsafe fn drop_from(record: &MutatorRecord) {
    let _ = unsafe { consume(record, false) };
}

/// Give back the list standing on this thread's record where the byte reads
/// `POSTED`: the teardown's refusal under pressure, whose retirement pass
/// holds the byte at `POSTED` rather than taking it
/// (`crate::cycle::collect`).
pub(crate) fn drop_this_threads() {
    let record = crate::cycle::mutator_record::this_thread_record();
    if record.is_null() {
        return;
    }

    let record = unsafe { &*record };
    if !record.live_list().is_null() && reads_posted(record) {
        unsafe { drop_from(record) };
    }
}

/// Stamp from the list standing on this thread's record before a block or
/// a run of this thread's goes back, where the byte reads `POSTED`: the
/// list may hold addresses into that memory, and a stamp written after the
/// pool or the system handed it to somebody else would write into whatever
/// occupies it (`memory::block_pool::BlockPool::put`, the run arm of
/// `memory::large_entity::free`). One thread-local load and one relaxed load
/// where no list stands, which is every return but the first after a grant
/// that listed something.
#[inline]
pub(crate) fn stamp_before_a_return() {
    let record = crate::cycle::mutator_record::this_thread_record();
    if record.is_null() || unsafe { (*record).live_list() }.is_null() {
        return;
    }

    stamp_at_a_return(unsafe { &*record });
}

/// [`stamp_before_a_return`] past its filter.
///
/// The byte reads `POSTED` whenever the word is non-null here, and the
/// relaxed filter relies on it: under `COLLECTOR` the word may be written
/// before the release, but no return reaches either caller then. The pool's
/// `put` withholds a block under a foreign trace ahead of this call, and a
/// run is freed only by a death, which the free entry withholds whole under
/// one (`crate::cycle::deferred_slot_reuse`, "A foreign holder of the
/// token"); both readings are acquire loads of the byte on this thread, which
/// order the word after the collector's release when it reads `POSTED`.
#[cold]
#[inline(never)]
fn stamp_at_a_return(record: &MutatorRecord) {
    if !reads_posted(record) {
        debug_assert!(
            false,
            "a return reached the live list outside `POSTED`, past the foreign trace's gates"
        );
        return;
    }

    #[cfg(test)]
    testing::note_a_stamp_at_a_return();
    unsafe { stamp_from(record) };
}

/// Give back, on the thread of the collector `record` is named to, the list
/// the last grant left on it where the epoch has advanced since its
/// publication: the owner has not taken its token since, and its take would
/// give the list back unread, a stamp of the old epoch reading as no stamp. So
/// the give-back costs no stamp, and an owner asleep under `POSTED` — which is
/// served no batch, so its epoch advances at X — holds the list's blocks for X
/// and one wait of the round at most. Called at the round's visit, after the
/// advance. A list the owner takes by its swap first is the owner's.
pub(crate) fn give_back_a_stale_list(record: &MutatorRecord) {
    if record.live_list().is_null() || record.advanced_at() <= record.live_list_published_at() {
        return;
    }

    if unsafe { consume(record, false) } {
        #[cfg(test)]
        testing::note_a_stale_list_given_back();
    }
}

/// Whether `record`'s byte reads `POSTED`: the acquire reading behind which
/// the list word the collector wrote before its release is the mutator's.
fn reads_posted(record: &MutatorRecord) -> bool {
    crate::cycle::token::state(record.token.read()) == crate::cycle::token::POSTED
}

/// Take the list off `record`, stamp from it where `stamp` asks and the epoch
/// still reads the batch's, and give its blocks back on this thread; false
/// where no list stood.
///
/// # Safety
/// As [`stamp_from`] where `stamp` is set; with it clear, any thread may call
/// it, the swap deciding who gives the blocks back.
unsafe fn consume(record: &MutatorRecord, stamp: bool) -> bool {
    // Null first: the release of the chain below returns blocks, and a
    // return reads the word ([`stamp_before_a_return`]).
    let head = record.take_live_list().cast::<ListBlock>();
    if head.is_null() {
        return false;
    }

    #[cfg(test)]
    let blocks_before = gc_metadata::thread_stats().current_blocks();

    let mut blocks = 0;
    let mut block = head;
    while !block.is_null() {
        blocks += 1;
        block = unsafe { (*block).header.next }.cast();
    }
    gc_metadata::take_over(blocks, blocks * BLOCK_PAYLOAD);

    let turnovers = unsafe { (*head).turnovers };
    if stamp && turnovers == record.turnovers() {
        let epoch = crate::cycle::epoch::epoch_of(turnovers);
        let mut block = head;
        while !block.is_null() {
            let entries =
                unsafe { std::slice::from_raw_parts(entries_of(block), (*block).entries) };
            for &entity in entries {
                unsafe { crate::refcount::stamp_as_read_live(entity, epoch) };
            }

            #[cfg(test)]
            testing::note_stamps(entries.len());
            block = unsafe { (*block).header.next }.cast();
        }
    }

    unsafe { release_chain(head) };
    #[cfg(test)]
    testing::note_blocks_across_a_list(blocks_before, gc_metadata::thread_stats().current_blocks());
    true
}

#[cfg(test)]
pub(crate) mod testing;
