//! The collector frees a set it proved by its tags itself, off its owner's
//! thread, and leaves the owner what only the owner may do
//! (`dev/design/recycler-over-counts.md`, §5a, S68.6b).
//!
//! **What the collector frees.** The first form takes W — every row the scan
//! over the record left potentially unreachable, proved garbage by the Δ-test
//! (`crate::cycle::delta_test`) — whole or not at all, and only where every
//! member is one it can free off the owner's thread: an object of a class with
//! the default dispose, no destructor and no outside cells; a reference; a
//! string with its bytes inline; an array whose storage, if any, is a body in
//! a block of kind `BLOCK_KIND_BUFFER`; with no weak references, in a slot of
//! an entity-heap block, every counted child outside W a plain one (no
//! ownership mark). Anything else sends W the owner's way (S68.6a).
//!
//! **Two phases, so that no part of W is freed alone.** A part freed and the
//! rest left would leak the rest, which still holds counts from the freed
//! part, or, given those as drops, release cells naming slots already handed
//! out again (the Critic of the plan, finding 1).
//! - [`prepare`] reads and writes nothing of W: it checks every member, builds
//!   every drop — one record a counted cell naming an entity outside W, in the
//!   order the member's dispose would release it — and one chain record a
//!   block, and draws every metadata block these need. A recall at the stop
//!   level, a pool refusal, an ineligible member or a W past [`MEMBER_CAP`]
//!   gives W back to the owner's way with nothing written.
//! - [`commit`] is one act, under the grant: every member's count to zero and
//!   its slot taken (`DEAD_IN_PLACE`); a registered member stays so for the
//!   owner's retirement pass; every other slot linked onto its block's chain
//!   through the free-list word; an array's body posted to its block's remote
//!   stack.
//!
//! **What the owner applies** ([`apply_this_threads`]), from a word of its
//! record beside the posted set: each chain spliced into its block, the
//! registered members counted as candidate deaths, then each drop through
//! `drop_ref(GcHeap, child)` — the dead holder's category, the child's read by
//! `drop_ref` at application. Every path that gives the posted set back
//! unread applies these first; dropped, they would leave each child a count
//! no one holds and each block a `used` counting dead slots.

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::cells::{AtomicCells, Cell, CellVisitor};
use crate::cycle::arena::TraceScratchArena;
use crate::cycle::mutator_record::MutatorRecord;
use crate::cycle::row::{self, EdgeTarget, Population};
use crate::cycle::shadow::{self, Color};
use crate::memory::block_pool::{BLOCK_MASK, BLOCK_PAYLOAD, BlockHeader, LINE_SIZE};
use crate::memory::gc_metadata;
use crate::refcount::{EntityKind, MemoryCategory, RcHeader};

/// The largest W the collector frees itself, the act the recall waits for
/// being bounded by it: a W past it goes the owner's way. A first setting,
/// which S68.8's runs read.
pub(crate) const MEMBER_CAP: usize = 65_536;

/// The cap in force: [`MEMBER_CAP`], or what a measurement set.
static CAP: AtomicUsize = AtomicUsize::new(MEMBER_CAP);

/// Set the cap a measurement reads the act under, and answer the one it
/// replaces.
#[cfg(test)]
pub(crate) fn set_member_cap_for_test(cap: usize) -> usize {
    CAP.swap(cap, Ordering::Relaxed)
}

/// Addresses one metadata block holds.
const ENTRIES_PER_BLOCK: usize = BLOCK_PAYLOAD / size_of::<usize>();

/// A metadata block of a chain: the pool's header, whose `next` links the
/// chain, and the count of addresses in its payload; in the first block of
/// the drops' chain, the chains' chain and the registered members' count.
#[repr(C)]
struct FreesBlock {
    header: BlockHeader,
    entries: usize,
    chains: *mut FreesBlock,
    registered: usize,
    members: usize,
}

const _: () = assert!(size_of::<FreesBlock>() <= LINE_SIZE);
// A chain record is four words, and none may straddle two blocks.
const _: () = assert!(ENTRIES_PER_BLOCK % 4 == 0);

/// One chain of metadata blocks as the collector grows it.
struct Chain {
    head: *mut FreesBlock,
    tail: *mut FreesBlock,
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

    /// Append `address`; false where the pool refused a block.
    fn push(&mut self, address: usize) -> bool {
        if (self.tail.is_null() || unsafe { (*self.tail).entries } == ENTRIES_PER_BLOCK)
            && !self.grow()
        {
            return false;
        }

        unsafe {
            let entries = (*self.tail).entries;
            entries_of(self.tail)
                .as_mut_ptr()
                .add(entries)
                .write(address);
            (*self.tail).entries = entries + 1;
        }
        true
    }

    fn grow(&mut self) -> bool {
        let block = gc_metadata::acquire().cast::<FreesBlock>();
        if block.is_null() {
            return false;
        }

        gc_metadata::charge(BLOCK_PAYLOAD);
        unsafe {
            (&raw mut (*block).header.next).write(std::ptr::null_mut());
            (&raw mut (*block).entries).write(0);
            (&raw mut (*block).chains).write(std::ptr::null_mut());
            (&raw mut (*block).registered).write(0);
            (&raw mut (*block).members).write(0);
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

/// The payload of `block`, as far as it is written.
///
/// # Safety
/// `block` is a block of a chain only the caller reads or writes.
unsafe fn entries_of<'a>(block: *mut FreesBlock) -> &'a mut [usize] {
    unsafe {
        std::slice::from_raw_parts_mut(
            BlockHeader::payload_start(block.cast()).cast(),
            (*block).entries,
        )
    }
}

/// The blocks of a chain, from `head`.
fn blocks_from(head: *mut FreesBlock) -> impl Iterator<Item = *mut FreesBlock> {
    std::iter::successors((!head.is_null()).then_some(head), |&block| {
        let next = unsafe { (*block).header.next }.cast::<FreesBlock>();
        (!next.is_null()).then_some(next)
    })
}

/// Give a chain's blocks back, on the thread whose figures hold them.
///
/// # Safety
/// `head` is the first block of a chain nobody else reads, or null.
unsafe fn release_chain(head: *mut FreesBlock) {
    let mut block = head;
    while !block.is_null() {
        let next = unsafe { (*block).header.next }.cast::<FreesBlock>();
        gc_metadata::discharge(BLOCK_PAYLOAD);
        gc_metadata::release(block.cast());
        block = next;
    }
}

/// What [`prepare`] built for a W the collector can free: the drops, and four
/// words a block of W — the block, its unregistered members, and the chain's
/// head and tail [`commit`] fills in.
pub(crate) struct Frees {
    drops: Chain,
    chains: Chain,
    registered: usize,
    members: usize,
}

impl Drop for Frees {
    fn drop(&mut self) {
        unsafe {
            release_chain(self.drops.head);
            release_chain(self.chains.head);
        }
    }
}

/// Why a W went the owner's way. Counted for the runs (`frees_counts`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NotFreed {
    /// A member the first form does not free off the owner's thread.
    Ineligible,
    /// W past [`MEMBER_CAP`].
    PastTheCap,
    /// The mutator's recall at the stop level.
    Recalled,
    /// The pool refused a metadata block.
    AllocationFailed,
    /// The owner has not applied the last grant's frees yet.
    FreesStand,
}

static SETS_FREED: AtomicUsize = AtomicUsize::new(0);
static MEMBERS_FREED: AtomicUsize = AtomicUsize::new(0);
static DROPS_POSTED: AtomicUsize = AtomicUsize::new(0);
static NOT_FREED: [AtomicUsize; 5] = [const { AtomicUsize::new(0) }; 5];

/// The collector's frees since the process started: sets, members, drops,
/// and the sets that went the owner's way by reason, in [`NotFreed`]'s order.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct FreesCounts {
    pub(crate) sets: usize,
    pub(crate) members: usize,
    pub(crate) drops: usize,
    pub(crate) not_freed: [usize; 5],
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn frees_counts() -> FreesCounts {
    FreesCounts {
        sets: SETS_FREED.load(Ordering::Relaxed),
        members: MEMBERS_FREED.load(Ordering::Relaxed),
        drops: DROPS_POSTED.load(Ordering::Relaxed),
        not_freed: std::array::from_fn(|reason| NOT_FREED[reason].load(Ordering::Relaxed)),
    }
}

fn note_not_freed(reason: NotFreed) -> NotFreed {
    NOT_FREED[reason as usize].fetch_add(1, Ordering::Relaxed);
    reason
}

/// Whether `entity` is a member of W: its row met and potentially
/// unreachable.
///
/// # Safety
/// `entity` is a counted child a member's cells name, under the grant.
unsafe fn is_a_member(entity: *mut RcHeader) -> bool {
    let EdgeTarget::Tracked(key) = (unsafe { row::resolve_edge_target(entity) }) else {
        return false;
    };
    unsafe { crate::cycle::arena::find_initialized_row(key) }
        .is_some_and(|row| shadow::color(unsafe { *row }) == Color::PotentiallyUnreachable)
}

/// Whether the first form frees `member` off the owner's thread.
///
/// # Safety
/// `member` is a member of W, its slot withheld under the grant.
unsafe fn eligible(member: *mut RcHeader, kind: u32, flags: u32) -> bool {
    if flags & crate::refcount::HAS_WEAK_REFERENCES != 0
        || crate::refcount::MemoryCategory::from_flags(flags) != MemoryCategory::GcHeap
    {
        return false;
    }

    match kind {
        k if k == EntityKind::Object as u32 => {
            let class = unsafe { (*(member as *mut crate::object::Object)).class };
            let class_ref = unsafe { &*class };
            !class_ref.has_destructor()
                && class_ref.dispose == crate::object::ll_default_dispose as *const ()
                && unsafe { crate::class::Class::outside_cells(class) }.is_none()
        }
        k if k == EntityKind::Reference as u32 || k == EntityKind::String as u32 => true,
        k if k == EntityKind::Array as u32 => {
            match unsafe {
                crate::array::entity::body_of(member as *mut crate::array::entity::LLArray)
            } {
                None => true,
                Some((body, _)) => {
                    let kind = unsafe {
                        crate::memory::block_pool::load_block_kind(
                            ((body as usize) & !BLOCK_MASK) as *const std::sync::atomic::AtomicU32,
                        )
                    };
                    kind == crate::memory::block_pool::BLOCK_KIND_BUFFER
                }
            }
        }
        _ => false,
    }
}

/// The visitor [`prepare`] reads a member's counted cells with: a child
/// outside W becomes a drop, one with an ownership mark makes the member
/// ineligible.
struct Drops<'a> {
    drops: &'a mut Chain,
    count: &'a mut usize,
    failed: &'a mut Option<NotFreed>,
}

impl CellVisitor for Drops<'_> {
    fn cell(&mut self, cell: Cell) -> ControlFlow<()> {
        let child = cell.child;
        if unsafe { is_a_member(child) } {
            return ControlFlow::Continue(());
        }

        if unsafe { crate::refcount::mutator_flags(child) } & crate::refcount::OWNERSHIP_MARK != 0 {
            *self.failed = Some(NotFreed::Ineligible);
            return ControlFlow::Break(());
        }

        if !self.drops.push(child as usize) {
            *self.failed = Some(NotFreed::AllocationFailed);
            return ControlFlow::Break(());
        }

        *self.count += 1;
        ControlFlow::Continue(())
    }
}

/// One step of the walk over W the two phases share.
enum Step {
    /// The first member of this block, of this population, is next.
    BlockStart(*mut u8, Population),
    /// A member.
    Member(*mut RcHeader),
    /// The block's last member was the last step.
    BlockEnd(*mut u8),
}

/// Walk every member of W, block by block in the touched list's order: the
/// order [`prepare`] and [`commit`] share. `step` answers `Break` to stop; a
/// row whose address cannot be recovered stops the walk too.
///
/// # Safety
/// The rows of a completed record scan stand, under the grant.
unsafe fn for_each_member(
    arena: &TraceScratchArena,
    mut step: impl FnMut(Step) -> ControlFlow<()>,
) -> ControlFlow<()> {
    let mut array = arena.touched_head();
    while !array.is_null() {
        let (block, population) = unsafe { ((*array).block, (*array).population) };
        let mut any = false;
        let walked = unsafe {
            row::for_each_proposable_met(array, block, population, |index| {
                if !any {
                    any = true;
                    step(Step::BlockStart(block, population))?;
                }
                match row::entity_at(block, population, index) {
                    Some(member) => step(Step::Member(member)),
                    None => ControlFlow::Break(()),
                }
            })
        };
        walked?;
        if any {
            step(Step::BlockEnd(block))?;
        }
        array = unsafe { (*array).next };
    }
    ControlFlow::Continue(())
}

/// The read-only phase: check every member of W and build what freeing it
/// needs, or answer why W goes the owner's way.
///
/// # Safety
/// The calling thread holds `mutator`'s grant; `arena`'s rows are those of a
/// completed record scan whose set the Δ-test proved garbage at the
/// checkpoint, still standing.
pub(crate) unsafe fn prepare(
    mutator: &MutatorRecord,
    arena: &mut TraceScratchArena,
) -> Result<Frees, NotFreed> {
    if mutator.collectors_frees_stand() {
        return Err(note_not_freed(NotFreed::FreesStand));
    }

    let mut frees = Frees {
        drops: Chain::empty(),
        chains: Chain::empty(),
        registered: 0,
        members: 0,
    };
    let mut failed = None;
    let mut unregistered_here = 0usize;
    let mut drops = 0usize;
    let arena_ptr: *mut TraceScratchArena = arena;
    let walked = unsafe {
        for_each_member(&*arena_ptr, |step| match step {
            Step::BlockStart(block, population) => {
                unregistered_here = 0;
                // An entity block of the granting mutator's own heap: a
                // registered member stands in its ring, and its chain is
                // spliced into its heap.
                if population == Population::Slotted
                    && crate::memory::heap::Heap::owner_of_the_block(block) == mutator.owner_heap()
                    && !mutator.owner_heap().is_null()
                {
                    ControlFlow::Continue(())
                } else {
                    failed = Some(NotFreed::Ineligible);
                    ControlFlow::Break(())
                }
            }
            Step::Member(member) => {
                if (*arena_ptr).inspect_position().is_break() {
                    failed = Some(NotFreed::Recalled);
                    return ControlFlow::Break(());
                }
                frees.members += 1;
                if frees.members > CAP.load(Ordering::Relaxed) {
                    failed = Some(NotFreed::PastTheCap);
                    return ControlFlow::Break(());
                }
                let flags = crate::refcount::mutator_flags(member);
                let kind = crate::cells::entity_kind(member);
                if !eligible(member, kind, flags) {
                    failed = Some(NotFreed::Ineligible);
                    return ControlFlow::Break(());
                }
                let mut count = 0;
                let mut refused = None;
                let _ = crate::cells::trace_cells_until::<AtomicCells>(
                    member,
                    kind,
                    Drops {
                        drops: &mut frees.drops,
                        count: &mut count,
                        failed: &mut refused,
                    },
                );
                if let Some(reason) = refused {
                    failed = Some(reason);
                    return ControlFlow::Break(());
                }
                drops += count;
                if flags & crate::refcount::CANDIDATE_BIT != 0 {
                    frees.registered += 1;
                } else {
                    unregistered_here += 1;
                }
                ControlFlow::Continue(())
            }
            Step::BlockEnd(block) => {
                if unregistered_here == 0 {
                    return ControlFlow::Continue(());
                }
                // The block, the members linked on its chain, then the head
                // and tail the commit writes.
                for word in [block as usize, unregistered_here, 0, 0] {
                    if !frees.chains.push(word) {
                        failed = Some(NotFreed::AllocationFailed);
                        return ControlFlow::Break(());
                    }
                }
                ControlFlow::Continue(())
            }
        })
    };
    if walked.is_break() {
        return Err(note_not_freed(failed.unwrap_or(NotFreed::Ineligible)));
    }

    // The drops' chain carries the rest to the owner, so it holds a block
    // even where W drops nothing, drawn here where a refusal is still free.
    if frees.drops.head.is_null() && !frees.drops.grow() {
        return Err(note_not_freed(NotFreed::AllocationFailed));
    }

    let _ = drops;
    Ok(frees)
}

/// The exact check a debug build runs beside every verdict the Δ-test proves,
/// read-only on the collector's thread before anything is written: every
/// member's count is the number of counted cells of members naming it, so
/// nothing outside W refers into it. A primitive that changed a count or a
/// slot and left no tag fails here, on the first set it touches.
///
/// # Safety
/// As [`prepare`].
#[cfg(debug_assertions)]
pub(crate) unsafe fn check_every_count_is_internal(arena: &TraceScratchArena) {
    let mut internal = std::collections::HashMap::<usize, u32>::new();
    let _ = unsafe {
        for_each_member(arena, |step| {
            if let Step::Member(member) = step {
                internal.entry(member as usize).or_insert(0);
                let kind = crate::cells::entity_kind(member);
                let _ =
                    crate::cells::trace_cells_until::<AtomicCells>(member, kind, |cell: Cell| {
                        if is_a_member(cell.child) {
                            *internal.entry(cell.child as usize).or_insert(0) += 1;
                        }
                    });
            }
            ControlFlow::Continue(())
        })
    };
    for (&member, &references) in &internal {
        assert_eq!(
            unsafe { crate::refcount::header_refcount(member as *const RcHeader) },
            references,
            "a member of a set the collector proved garbage is referred to from outside it: {member:#x}"
        );
    }
}

/// The act: free every member of W as [`prepare`] found it, writing the
/// chains' heads and tails into the records it drew. No stop falls inside it.
///
/// # Safety
/// As [`prepare`], straight after it answered `frees` for the same rows.
pub(crate) unsafe fn commit(arena: &TraceScratchArena, frees: &mut Frees) {
    let mut chain_blocks = blocks_from(frees.chains.head);
    let mut chain_block = chain_blocks.next();
    let mut at = 0usize;
    let mut head: *mut u8 = std::ptr::null_mut();
    let mut tail: *mut u8 = std::ptr::null_mut();
    let mut linked = 0usize;
    let _ = unsafe {
        for_each_member(arena, |step| {
            match step {
                Step::BlockStart(..) => {
                    head = std::ptr::null_mut();
                    tail = std::ptr::null_mut();
                    linked = 0;
                }
                Step::Member(member) => {
                    let kind = crate::cells::entity_kind(member);
                    if kind == EntityKind::Array as u32
                        && let Some((body, capacity)) = crate::array::entity::body_of(
                            member as *mut crate::array::entity::LLArray,
                        )
                    {
                        crate::memory::buffer_arena::post_a_body_remote(body, capacity);
                    }
                    crate::refcount::set_header_refcount(member, 0);
                    let flags = crate::refcount::take_slot_for_free(member)
                        .expect("a member of W is freed once, by this act");
                    if flags & crate::refcount::CANDIDATE_BIT == 0 {
                        let slot = member as *mut u8;
                        (slot.add(crate::memory::heap::FREE_LIST_LINK_OFFSET) as *mut *mut u8)
                            .write(head);
                        if tail.is_null() {
                            tail = slot;
                        }
                        head = slot;
                        linked += 1;
                    }
                }
                Step::BlockEnd(block) => {
                    if linked == 0 {
                        return ControlFlow::Continue(());
                    }
                    // The records of this block, four words, in the order the
                    // preparation wrote them.
                    while at
                        >= (*chain_block.expect("the preparation drew a record a block")).entries
                    {
                        chain_block = chain_blocks.next();
                        at = 0;
                    }
                    let entries =
                        entries_of(chain_block.expect("the preparation drew a record a block"));
                    debug_assert_eq!(entries[at], block as usize);
                    debug_assert_eq!(entries[at + 1], linked);
                    // The count the splice lowers `used` by is the act's own.
                    entries[at + 1] = linked;
                    // The chain runs head to tail, the head the last slot linked.
                    entries[at + 2] = head as usize;
                    entries[at + 3] = tail as usize;
                    at += 4;
                }
            }
            ControlFlow::Continue(())
        })
    };
    SETS_FREED.fetch_add(1, Ordering::Relaxed);
    MEMBERS_FREED.fetch_add(frees.members, Ordering::Relaxed);
    DROPS_POSTED.fetch_add(
        blocks_from(frees.drops.head)
            .map(|block| unsafe { (*block).entries })
            .sum(),
        Ordering::Relaxed,
    );
}

/// Leave what the collector freed on `mutator`'s record for the owner, with
/// this thread's hold on its blocks. Under the grant, before its release.
pub(crate) fn publish(frees: Frees, mutator: &MutatorRecord) {
    let this = std::mem::ManuallyDrop::new(frees);
    let head = this.drops.head;
    debug_assert!(
        !head.is_null(),
        "the preparation drew the drops' first block"
    );
    unsafe {
        (*head).chains = this.chains.head;
        (*head).registered = this.registered;
        (*head).members = this.members;
    }
    let blocks = this.drops.blocks + this.chains.blocks;
    gc_metadata::hand_over(blocks, blocks * BLOCK_PAYLOAD);
    mutator.publish_collectors_frees(head.cast());
}

/// Apply what a collector freed on this thread's behalf, if anything stands:
/// splice each chain, count the registered members as candidate deaths, then
/// drop each child outside the freed set through `drop_ref` — where the
/// destructors those deaths reach run. Answers the members the collector
/// freed, registered ones among them, which this application makes the
/// thread's: the poll counts them as freed, as an owner's collection counts
/// the members it frees and leaves dead in place for the retirement pass.
///
/// # Safety
/// On a thread at a point where user destructors may run: the poll under an
/// open gate, a collection under pressure, the exit under its final claim.
pub(crate) unsafe fn apply_this_threads() -> usize {
    let Some(head) = (unsafe { take_this_threads() }) else {
        return 0;
    };
    let members = unsafe { (*head).members };
    unsafe { splice(head) };
    for block in blocks_from(head) {
        for &child in unsafe { entries_of(block) }.iter() {
            unsafe {
                crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, child as *mut RcHeader)
            };
        }
    }
    unsafe { release_chain(head) };
    members
}

/// The part of [`apply_this_threads`] that runs no user code — the chains
/// spliced, the registered members counted — for a thread under pressure
/// inside a teardown, which wants its slots and may not run a destructor; the
/// drops stay on the record for the next open poll.
///
/// # Safety
/// On the owning thread.
pub(crate) unsafe fn splice_this_threads() {
    let Some(head) = (unsafe { take_this_threads() }) else {
        return;
    };
    unsafe { splice(head) };
    let record = crate::cycle::mutator_record::this_thread_record();
    unsafe { (*record).put_back_collectors_frees(head.cast()) };
    // The figures move back with the word: the next application takes them
    // over again.
    let blocks = blocks_from(head).count();
    gc_metadata::hand_over(blocks, blocks * BLOCK_PAYLOAD);
}

/// Take what stands on this thread's record, with the figures of its blocks.
///
/// # Safety
/// On the owning thread.
unsafe fn take_this_threads() -> Option<*mut FreesBlock> {
    let record = crate::cycle::mutator_record::this_thread_record();
    if record.is_null() {
        return None;
    }

    let head = unsafe { (*record).take_collectors_frees() }.cast::<FreesBlock>();
    if head.is_null() {
        return None;
    }

    let blocks = blocks_from(head).count() + blocks_from(unsafe { (*head).chains }).count();
    gc_metadata::take_over(blocks, blocks * BLOCK_PAYLOAD);
    Some(head)
}

/// Splice the chains `head` carries and count its registered members, once:
/// the chains go back to the pool and the head keeps the drops alone.
///
/// # Safety
/// `head` was taken off this thread's record.
unsafe fn splice(head: *mut FreesBlock) {
    let chains = unsafe { (*head).chains };
    let heap = crate::memory::heap::thread_entity_heap();
    for block in blocks_from(chains) {
        for record in unsafe { entries_of(block) }.chunks_exact(4) {
            let (slots, n, first, last) = (record[0], record[1], record[2], record[3]);
            unsafe {
                (*heap).splice_a_collectors_chain(
                    slots as *mut u8,
                    first as *mut u8,
                    last as *mut u8,
                    n as u32,
                )
            };
        }
    }
    for _ in 0..unsafe { (*head).registered } {
        crate::cycle::queue::note_a_candidate_death();
    }
    unsafe {
        release_chain(chains);
        (*head).chains = std::ptr::null_mut();
        (*head).registered = 0;
    }
}

#[cfg(test)]
mod tests;
