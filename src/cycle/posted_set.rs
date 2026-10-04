//! The set a collector's batch proved unreachable, posted beside its verdicts
//! for the mutator's collection over P to validate and free as it stands
//! (`dev/DECISIONS.md`, "the collector posts the set it proved unreachable, and
//! the owner validates that set alone").
//!
//! # Why the set and not its roots
//!
//! A batch proposes roots, and a proposed root is garbage only together with
//! the component it stands in. Given the roots alone, the owner has to find
//! that component again, and the only trace that finds it whatever its shape
//! expands rows above zero as well — the held stack's final drain — which on a
//! garbage head with an edge into the long-lived state walks the state
//! (`crate::cycle::mark`, "The held stack"). Given the set, the owner reads
//! nothing outside it: it meets every listed member, follows only the edges
//! between members, scans, and frees what the scan leaves unreachable
//! (`crate::cycle::collect`). Its work is the set and one layer of edges out
//! of it.
//!
//! **Any set of live entities is a sound input.** Trial deletion restricted to
//! a set subtracts only the edges inside it, so an edge it does not follow
//! leaves its target's row higher, and the rows the owner's scan leaves
//! potentially unreachable are closed under referrers: every reference to one
//! is counted from inside the set, and a referrer read live would have
//! coloured it live. A listed slot whose occupant died since the post, and was
//! handed to another entity, costs that member a refusal and nothing else; a
//! listed slot that is free reads count zero and is not met.
//!
//! # What a listed address may become
//!
//! Under `POSTED` the mutator runs free and returns memory at once, so a block
//! holding a listed address can go back to the pool, whose thread cache hands
//! it to the next draw of any class or kind, and a run can be unmapped. The
//! owner must never read a listed address in such memory. A block a listed
//! member stands in is empty only if that member died — a member proposed
//! wrongly; one the mutator tore down during the grant is not listed — so the
//! set records the blocks its members stand in, listing a member's block
//! before the member, and a return of one of them under `POSTED` drops the
//! set whole
//! ([`drop_before_a_return`]); every other return passes it by. The hooks
//! stand at the pool's `put` and at the unmapping of a run.
//!
//! # The chain
//!
//! Two chains of GC blocks per grant, drawn on the collector's thread and
//! handed to the mutator's with the release (`gc_metadata::hand_over`): the
//! members' addresses, and the addresses of
//! the blocks they stand in, sorted in each block of the second chain at the
//! publication so that a return reads a binary search per chain block. The
//! first block of the members' chain names the first of the blocks' chain.
//! No bound but the pool's: a set cut short proves only what lies inside it,
//! and a garbage component that never fits would never be freed
//! (`dev/DECISIONS.md`, "the collector's trace has no rows ceiling"). A block
//! the pool refuses closes the set with what it holds, which the owner's
//! reading of a subset makes safe.

use std::ops::ControlFlow;

use crate::cycle::arena::TraceScratchArena;
use crate::cycle::mutator_record::MutatorRecord;
use crate::cycle::row;
use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockHeader, LINE_SIZE};
use crate::memory::gc_metadata;
use crate::refcount::RcHeader;

/// Addresses one block's payload holds.
const ENTRIES_PER_BLOCK: usize = BLOCK_PAYLOAD / size_of::<usize>();

/// A block of either chain, as its header line lays it out: the pool's
/// header, whose `next` links the chain, the addresses the payload holds, and
/// in the first block of the members' chain the first block of the blocks'
/// chain.
#[repr(C)]
struct SetBlock {
    header: BlockHeader,
    entries: usize,
    blocks: *mut SetBlock,
    /// In the first block of the members' chain: whether the collector proved
    /// the set garbage by its tags (`crate::cycle::delta_test`), and the edges
    /// its mark recorded between two members, which the proof makes the sum
    /// of the members' counts.
    #[cfg(feature = "recycler-over-counts")]
    proved_by_its_tags: bool,
    #[cfg(feature = "recycler-over-counts")]
    internal_edges: usize,
}

const _: () = assert!(size_of::<SetBlock>() <= LINE_SIZE);

/// The addresses `block` holds.
///
/// # Safety
/// `block` is a block of a chain nobody writes.
unsafe fn entries_of<'a>(block: *mut SetBlock) -> &'a mut [usize] {
    unsafe {
        std::slice::from_raw_parts_mut(
            BlockHeader::payload_start(block.cast()).cast(),
            (*block).entries,
        )
    }
}

/// The blocks of a chain, from `head`.
fn blocks_from(head: *mut SetBlock) -> impl Iterator<Item = *mut SetBlock> {
    std::iter::successors((!head.is_null()).then_some(head), |&block| {
        let next = unsafe { (*block).header.next }.cast::<SetBlock>();
        (!next.is_null()).then_some(next)
    })
}

/// Give a chain's blocks back to the pool, on the thread whose figures hold
/// them.
///
/// # Safety
/// `head` is the first block of a chain nobody else reads, or null.
unsafe fn release_chain(head: *mut SetBlock) {
    let mut block = head;
    while !block.is_null() {
        let next = unsafe { (*block).header.next }.cast::<SetBlock>();
        gc_metadata::discharge(BLOCK_PAYLOAD);
        gc_metadata::release(block.cast());
        block = next;
    }
}

/// One chain as a writer grows it.
struct Chain {
    head: *mut SetBlock,
    tail: *mut SetBlock,
    blocks: usize,
}

impl Chain {
    const fn empty() -> Self {
        Self {
            head: std::ptr::null_mut(),
            tail: std::ptr::null_mut(),
            blocks: 0,
        }
    }

    /// Write `address` at the chain's end, growing it by a block where the
    /// last is full; false where the pool refused the block.
    fn push(&mut self, address: usize) -> bool {
        if (self.tail.is_null() || unsafe { (*self.tail).entries } == ENTRIES_PER_BLOCK)
            && !self.grow()
        {
            return false;
        }

        unsafe {
            let entries = (*self.tail).entries;
            BlockHeader::payload_start(self.tail.cast())
                .cast::<usize>()
                .add(entries)
                .write(address);
            (*self.tail).entries = entries + 1;
        }
        true
    }

    fn grow(&mut self) -> bool {
        let block = gc_metadata::acquire().cast::<SetBlock>();
        if block.is_null() {
            return false;
        }

        gc_metadata::charge(BLOCK_PAYLOAD);
        unsafe {
            (&raw mut (*block).header.next).write(std::ptr::null_mut());
            (&raw mut (*block).entries).write(0);
            (&raw mut (*block).blocks).write(std::ptr::null_mut());
            #[cfg(feature = "recycler-over-counts")]
            (&raw mut (*block).proved_by_its_tags).write(false);
            #[cfg(feature = "recycler-over-counts")]
            (&raw mut (*block).internal_edges).write(0);
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

/// The collector's side: the set one grant's batch writes, from the rows its
/// trace left standing.
///
/// Dropped unpublished — a batch that unwound, one that proposed nothing, or
/// one released to `FREE` — it gives its blocks back on the collector's
/// thread.
pub(crate) struct Writer {
    members: Chain,
    blocks: Chain,
    /// Whether the set takes no more: the pool refused a block.
    closed: bool,
    /// Whether the walk left a potentially unreachable row out: an address
    /// it could not recover, or a member torn down during the grant.
    #[cfg(feature = "recycler-over-counts")]
    left_one_out: bool,
    /// Whether the collector proved the set garbage by its tags
    /// (`crate::cycle::delta_test`), and its internal edges.
    #[cfg(feature = "recycler-over-counts")]
    proved_by_its_tags: bool,
    #[cfg(feature = "recycler-over-counts")]
    internal_edges: usize,
    /// What the collector freed itself under this grant, published beside the
    /// set (`crate::cycle::collector_frees`).
    #[cfg(feature = "recycler-over-counts")]
    frees: Option<crate::cycle::collector_frees::Frees>,
}

impl Writer {
    /// An empty set. Draws nothing until the first append.
    pub(crate) const fn new() -> Self {
        Self {
            members: Chain::empty(),
            blocks: Chain::empty(),
            closed: false,
            #[cfg(feature = "recycler-over-counts")]
            left_one_out: false,
            #[cfg(feature = "recycler-over-counts")]
            proved_by_its_tags: false,
            #[cfg(feature = "recycler-over-counts")]
            internal_edges: 0,
            #[cfg(feature = "recycler-over-counts")]
            frees: None,
        }
    }

    /// Append every entity whose met row stands potentially unreachable
    /// (`crate::cycle::shadow::is_proposable`): after a completed scan the
    /// rows it proved so, after a trace stopped short the zero closure of the
    /// proposed roots (`crate::cycle::scan::colour_the_zero_closure`). The
    /// walk reads the met groups of every touched array, as the arena's reset
    /// does, and no recall: a stopped trace's mutator waits for that reset
    /// anyway.
    ///
    /// # Safety
    /// The trace ran on this thread and its rows still stand: before the
    /// arena's reset.
    pub(crate) unsafe fn append(&mut self, arena: &TraceScratchArena) {
        let mut array = arena.touched_head();
        while !array.is_null() && !self.closed {
            let (block, population) = unsafe { ((*array).block, (*array).population) };
            let mut listed_here = false;
            let _ = unsafe {
                row::for_each_proposable_met(array, block, population, |index| {
                    // A row whose address cannot be recovered is left out, as
                    // the harvest's walk leaves it.
                    let Some(entity) = row::entity_at(block, population, index) else {
                        #[cfg(feature = "recycler-over-counts")]
                        {
                            self.left_one_out = true;
                        }
                        return ControlFlow::Continue(());
                    };

                    // A member the mutator tore down during the grant is no
                    // garbage the owner needs, and its withheld return could
                    // empty the block after the release and drop the set; the
                    // slot is withheld, so its state reads the death.
                    if crate::refcount::slot_state(entity) != crate::refcount::SlotState::Live {
                        #[cfg(feature = "recycler-over-counts")]
                        {
                            self.left_one_out = true;
                        }
                        return ControlFlow::Continue(());
                    }

                    if !self.list(entity, block, !listed_here) {
                        self.closed = true;
                        return ControlFlow::Break(());
                    }

                    listed_here = true;
                    ControlFlow::Continue(())
                })
            };
            array = unsafe { (*array).next };
        }
    }

    /// Mark the set proved garbage by the collector's Δ-test
    /// (`crate::cycle::delta_test`), where it lists every member the test
    /// read: the mark travels with it to the owner. A set the pool closed
    /// early, or whose walk left a row out, is left unmarked — a part of a
    /// garbage set is garbage, but not one that may be freed alone, the rest
    /// still naming it (the Critic of S68.5, finding 1).
    #[cfg(feature = "recycler-over-counts")]
    pub(crate) fn mark_proved_by_its_tags(&mut self, internal_edges: usize) {
        self.proved_by_its_tags = !self.closed && !self.left_one_out;
        self.internal_edges = internal_edges;
    }

    /// Carry what the collector freed itself to the release, which publishes
    /// it beside the set.
    #[cfg(feature = "recycler-over-counts")]
    pub(crate) fn carry_the_frees(&mut self, frees: crate::cycle::collector_frees::Frees) {
        debug_assert!(self.frees.is_none(), "one free a grant");
        self.frees = Some(frees);
    }

    /// List `entity`, and first `block` where `first_in_block` says the block
    /// is not listed yet; false where the pool refused a block. The block goes
    /// first: a member listed without its block would let the block's return
    /// pass the hook, and the owner read the member's address in whatever the
    /// pool handed the block to.
    fn list(&mut self, entity: *mut RcHeader, block: *mut u8, first_in_block: bool) -> bool {
        if first_in_block {
            #[cfg(test)]
            if testing::refuses_the_next_block() {
                return false;
            }

            if !self.blocks.push(block as usize) {
                return false;
            }
        }

        self.members.push(entity as usize)
    }

    /// Leave the set on `mutator`'s record for its collection over P, and
    /// with it this thread's hold on its blocks. Under the grant, before its
    /// release; a set with no member leaves nothing.
    #[cfg_attr(not(feature = "recycler-over-counts"), allow(unused_mut))]
    pub(crate) fn publish(mut self, mutator: &MutatorRecord) {
        #[cfg(feature = "recycler-over-counts")]
        if let Some(frees) = self.frees.take() {
            crate::cycle::collector_frees::publish(frees, mutator);
        }
        let this = std::mem::ManuallyDrop::new(self);
        if this.members.head.is_null() {
            unsafe { release_chain(this.blocks.head) };
            return;
        }

        for block in blocks_from(this.blocks.head) {
            unsafe { entries_of(block) }.sort_unstable();
        }
        let head = this.members.head;
        unsafe { (*head).blocks = this.blocks.head };
        #[cfg(feature = "recycler-over-counts")]
        unsafe {
            (*head).proved_by_its_tags = this.proved_by_its_tags;
            (*head).internal_edges = this.internal_edges;
        };
        #[cfg(test)]
        testing::note_members_posted(
            blocks_from(head)
                .map(|block| unsafe { (*block).entries })
                .sum(),
        );
        let blocks = this.members.blocks + this.blocks.blocks;
        gc_metadata::hand_over(blocks, blocks * BLOCK_PAYLOAD);
        mutator.publish_posted_set(head.cast());
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        // A free the collector made is in the heap already: its record may
        // only leave by publication. A batch that freed always posts.
        #[cfg(feature = "recycler-over-counts")]
        debug_assert!(
            self.frees.is_none() || std::thread::panicking(),
            "the collector's frees were dropped unpublished"
        );
        unsafe {
            release_chain(self.members.head);
            release_chain(self.blocks.head);
        }
    }
}

/// The mutator's side: a set taken off its record, which its collection over
/// P reads and whose blocks go back when it drops.
pub(crate) struct PostedSet {
    head: *mut SetBlock,
}

impl PostedSet {
    /// Whether the collector proved the set garbage by its tags
    /// (`crate::cycle::delta_test`).
    #[cfg(feature = "recycler-over-counts")]
    pub(crate) fn proved_by_its_tags(&self) -> bool {
        unsafe { (*self.head).proved_by_its_tags }
    }

    /// The edges the collector's mark recorded between two members: the sum
    /// of the members' counts, where the set is proved by its tags.
    #[cfg(feature = "recycler-over-counts")]
    pub(crate) fn internal_edges(&self) -> usize {
        unsafe { (*self.head).internal_edges }
    }

    /// Every member, in the order the collector listed them.
    pub(crate) fn members(&self) -> impl Iterator<Item = *mut RcHeader> + '_ {
        blocks_from(self.head).flat_map(|block| {
            unsafe { entries_of(block) }
                .iter()
                .map(|&address| address as *mut RcHeader)
        })
    }
}

impl Drop for PostedSet {
    fn drop(&mut self) {
        unsafe {
            release_chain((*self.head).blocks);
            release_chain(self.head);
        }
    }
}

/// Take the set the last grant left on this thread's record, for the
/// collection over P that holds the token: `None` where none stands.
pub(crate) fn take_this_threads() -> Option<PostedSet> {
    let record = crate::cycle::mutator_record::this_thread_record();
    if record.is_null() {
        return None;
    }

    unsafe { take_from(&*record) }
}

/// Give back the set standing on this thread's record unread: a collection
/// over R whole, under pressure, at the exit, or the disposition of P with no
/// trace window, none of which validates it.
pub(crate) fn drop_this_threads() {
    let _ = take_this_threads();
}

/// Take the set off `record`, taking over its blocks' figures on this thread.
///
/// # Safety
/// `record` is this thread's, and this thread read `POSTED` on it or holds
/// its token.
unsafe fn take_from(record: &MutatorRecord) -> Option<PostedSet> {
    let head = record.take_posted_set().cast::<SetBlock>();
    if head.is_null() {
        return None;
    }

    let blocks = blocks_from(head).count() + blocks_from(unsafe { (*head).blocks }).count();
    gc_metadata::take_over(blocks, blocks * BLOCK_PAYLOAD);
    Some(PostedSet { head })
}

/// Drop the set standing on this thread's record before `block` goes back,
/// where one of its members stands in that block: the pool's thread cache
/// would hand the block to the next draw of any class, and the owner would
/// read a listed address in somebody else's memory
/// (`memory::block_pool::BlockPool::put`, the run arm of
/// `memory::large_entity::free`). One thread-local load and one load of the
/// record's word where no set stands.
#[inline]
pub(crate) fn drop_before_a_return(block: *mut u8) {
    let record = crate::cycle::mutator_record::this_thread_record();
    if record.is_null() || unsafe { (*record).posted_set() }.is_null() {
        return;
    }

    drop_at_a_return(unsafe { &*record }, block);
}

/// [`drop_before_a_return`] past its filter. The word is non-null here under
/// `POSTED`, or under this thread's own claim between a take from `POSTED` and
/// the collection over P that takes the set; a return under `COLLECTOR` is
/// withheld ahead of the hook — the pool's `put` withholds a block under a
/// foreign trace before it, and a run is freed only by a death, which the free
/// entry withholds whole under one (`crate::cycle::deferred_slot_reuse`, "A
/// foreign holder of the token") — so the collector writing the word is never
/// read here.
#[cold]
#[inline(never)]
fn drop_at_a_return(record: &MutatorRecord, block: *mut u8) {
    let head = record.posted_set().cast::<SetBlock>();
    let listed = blocks_from(unsafe { (*head).blocks }).any(|chain_block| {
        unsafe { entries_of(chain_block) }
            .binary_search(&(block as usize))
            .is_ok()
    });
    if listed {
        #[cfg(test)]
        testing::note_a_set_dropped_at_a_return();
        // The word goes null before the chain's blocks go back, which are
        // returns that read it.
        drop(unsafe { take_from(record) });
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::Writer;
    use crate::memory::block_pool::BlockHeader;
    use crate::refcount::RcHeader;

    /// Leave `members` on this thread's record as the set a batch posted, for
    /// a case standing in for the collector whose stand-in posted no set.
    /// After the stand-in's release, so that the record reads `POSTED`.
    pub(crate) fn post_for_test(members: &[*mut RcHeader]) {
        let record = crate::cycle::mutator_record::this_thread_record();
        assert!(!record.is_null(), "this thread has a record");
        let mut set = Writer::new();
        let mut blocks: Vec<usize> = members
            .iter()
            .map(|&member| BlockHeader::of_ptr(member.cast()) as usize)
            .collect();
        blocks.sort_unstable();
        blocks.dedup();
        for &member in members {
            assert!(set.members.push(member as usize), "the pool served the set");
        }
        for block in blocks {
            assert!(set.blocks.push(block), "the pool served the set");
        }
        set.publish(unsafe { &*record });
    }

    static DROPPED_AT_A_RETURN: AtomicUsize = AtomicUsize::new(0);
    /// The block listing a set refuses next, counted from one; zero for
    /// none.
    static REFUSE_AT_THE_BLOCK: AtomicUsize = AtomicUsize::new(0);
    static MEMBERS_POSTED: AtomicUsize = AtomicUsize::new(usize::MAX);

    /// Refuse the next block a set lists, as the pool would.
    pub(crate) fn refuse_the_next_block() {
        REFUSE_AT_THE_BLOCK.store(1, Ordering::Relaxed);
    }

    /// Refuse the second block a set lists from here: the set keeps the
    /// members of its first block and closes short of the rest.
    #[cfg(feature = "recycler-over-counts")]
    pub(crate) fn refuse_the_second_block() {
        REFUSE_AT_THE_BLOCK.store(2, Ordering::Relaxed);
    }

    pub(super) fn refuses_the_next_block() -> bool {
        let mut refused = false;
        let _ = REFUSE_AT_THE_BLOCK.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |at| {
            refused = at == 1;
            at.checked_sub(1)
        });
        refused
    }

    pub(super) fn note_members_posted(members: usize) {
        MEMBERS_POSTED.store(members, Ordering::Relaxed);
        SETS_POSTED.fetch_add(1, Ordering::Relaxed);
        MEMBERS_IN_ALL.fetch_add(members, Ordering::Relaxed);
        MEMBERS_MOST.fetch_max(members, Ordering::Relaxed);
    }

    /// The sets published, their members in all and at the most, since a
    /// call to [`take_sets_posted`]; process-wide, for the rig.
    static SETS_POSTED: AtomicUsize = AtomicUsize::new(0);
    static MEMBERS_IN_ALL: AtomicUsize = AtomicUsize::new(0);
    static MEMBERS_MOST: AtomicUsize = AtomicUsize::new(0);

    /// The sets published, their members in all and at the most, since the
    /// last call, which leaves zero.
    pub(crate) fn take_sets_posted() -> (usize, usize, usize) {
        (
            SETS_POSTED.swap(0, Ordering::Relaxed),
            MEMBERS_IN_ALL.swap(0, Ordering::Relaxed),
            MEMBERS_MOST.swap(0, Ordering::Relaxed),
        )
    }

    /// The members the last published set held, or `None` since the last
    /// call.
    pub(crate) fn take_members_posted() -> Option<usize> {
        match MEMBERS_POSTED.swap(usize::MAX, Ordering::Relaxed) {
            usize::MAX => None,
            members => Some(members),
        }
    }

    /// List `entity` in `set` as `Writer::append` does, its block first where
    /// `first_in_block` says so: the case of a refused block.
    pub(crate) fn list(set: &mut Writer, entity: *mut RcHeader, first_in_block: bool) -> bool {
        set.list(
            entity,
            BlockHeader::of_ptr(entity.cast()).cast(),
            first_in_block,
        )
    }

    /// Whether `set` lists any member, and how many blocks.
    pub(crate) fn listed(set: &Writer) -> (bool, usize) {
        (!set.members.head.is_null(), set.blocks.blocks)
    }

    pub(super) fn note_a_set_dropped_at_a_return() {
        DROPPED_AT_A_RETURN.fetch_add(1, Ordering::Relaxed);
    }

    /// Sets a return dropped since the last call.
    pub(crate) fn take_sets_dropped_at_a_return() -> usize {
        DROPPED_AT_A_RETURN.swap(0, Ordering::Relaxed)
    }
}
