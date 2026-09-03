//! Which shadow row a traced edge lands on, and which edges have none.
//!
//! The trace holds a child's address and needs the row that carries the
//! collector's working count for it. No single formula answers that: the
//! GC heap holds three populations, and one of them — the retained
//! former-arena block — was filled by a bump allocator at mixed sizes and
//! has no stride to divide by. So the answer is a dispatch on the
//! block's kind, whose input is already in hand: the block header holds
//! the kind at offset zero, and the trace has to touch that line before
//! it can reach any row at all (`rfc/model/gc/rc-cycle.md`, "Where the
//! shadow count lives").
//!
//! The dispatch sits here rather than in `cells`, one level above the
//! child enumerator, so that the enumerator keeps knowing entity kinds
//! and out-edges and nothing about rows, blocks or the collector.
//!
//! **This module owns nothing and allocates nothing.** It reads a block
//! header and answers a locator; the rows it names belong to the collection's
//! arena, and a locator outlives them, so a `RowKey` is only meaningful while
//! the trace that made it holds its token. The one ordering it depends on is
//! the block header's: the kind is read before anything derived from it, which
//! is the same read the caller has already made.

use crate::memory::block_pool::{
    BLOCK_KIND_ENTITY, BLOCK_KIND_ENTITY_LARGE, BLOCK_KIND_ENTITY_LARGE_RUN, BLOCK_KIND_RETAINED,
    BlockHeader, collector_load_block_kind,
};
use crate::refcount::{MemoryCategory, RcHeader, mutator_flags};

/// The row index of a large entity, which is the sole occupant of its
/// own block or run and gets one row in that block's header rather than
/// an array (`rfc/model/gc/rc-cycle.md`, "Where the shadow count
/// lives").
pub(crate) const SINGLE_ENTITY_INDEX: u32 = 0;

/// Which of the three GC-heap populations a block belongs to, and with
/// it where the block's rows are and how many of them there are.
///
/// The trace reads the block's kind before it can reach any row, so the
/// answer is carried out of that read rather than taken again: the two
/// readings could differ only if the block changed hands mid-trace,
/// which the trace token forbids, and one dispatch is what the row
/// lookup was measured at (`rfc/model/gc/rc-cycle.md`, "Where the
/// shadow count lives").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub(crate) enum Population {
    /// An ordinary entity block: one row per slot, indexed by the
    /// reciprocal multiply, and the array is reached through the
    /// block's collector triple.
    Slotted,
    /// A retained former-arena block: one row per **occupant** of its
    /// survivor list, indexed by position in it, and the array is reached
    /// through the same collector line, which also names the list and
    /// its length, the index space (`crate::memory::retained`).
    Retained,
    /// A large entity, the sole occupant of its block: one row, held in
    /// the block's own header rather than in an array
    /// (`crate::memory::large_entity`).
    SingleEntity,
}

/// The shadow row of one entity: the block whose rows carry it, the
/// entity's index among them, and which population the block belongs to.
///
/// It is an identity and not an address. Where the rows themselves are
/// reserved is `crate::cycle::arena`; two entities of one block
/// resolving to one `RowKey` is a defect however the rows are laid out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct RowKey {
    /// The block header's address, 64 KiB-aligned. For a large entity
    /// held in an OS-direct run, the run's first block, which is the one
    /// carrying the header.
    pub block: usize,
    /// The entity's index into that block's rows: its slot index in an
    /// entity block, its position in the survivor list of a retained
    /// block, and [`SINGLE_ENTITY_INDEX`] for a large entity.
    pub index: u32,
    /// Where those rows are, which the block's kind has already said.
    pub population: Population,
}

/// What the trace does with one edge it has just read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EdgeTarget {
    /// The child is a GC-heap entity with this row: the descent
    /// continues through it and the row takes the decrement.
    Tracked(RowKey),
    /// The child is outside the GC heap, so the descent stops and the
    /// edge counts as an external live reference. An arena entity is the
    /// case that matters — a ring through the arena is broken by the
    /// arena's own reset — and an immortal entity answers the same way.
    Untracked,
}

/// Resolve one traced edge: the row of `child`, or the refusal to
/// descend through it.
///
/// The block's kind is read first and decides everything after it, so a
/// child whose block was never commissioned as an entity block is
/// answered without any other header word being touched. That ordering
/// is a safety requirement rather than a shape: only `kind` and the pool
/// link are initialised in an uncommissioned block, and reading a size
/// class or a cursor there reads uninitialised memory.
///
/// An address the retained population cannot place answers
/// [`EdgeTarget::Untracked`] as well, which keeps its referent alive rather
/// than reading it as unreachable on a row the trace guessed
/// (`memory::retained::occupant_index`). That arm reads the block's own
/// header line and nothing process-wide: the survivor list's address and
/// length stand beside the shadow pointer, and the search is over them.
///
/// **The kind alone does not settle the large-entity arm**, and that arm
/// reads the child's memory category as well. The other two arms need no
/// such test: an entity block and a retained block hold entities of the
/// collected heap only, and the `LongLived` category, which shares the
/// entity blocks, is out of use.
///
/// # Safety
/// `child` must be a live entity header. Its block must be mapped and
/// commissioned, and its own flags word readable, which the large arm
/// depends on.
pub(crate) unsafe fn resolve_edge_target(child: *mut RcHeader) -> EdgeTarget {
    note_dispatch();

    // Through `BlockHeader::of_ptr` rather than a mask of its own: the
    // address-to-block step is an integer-to-pointer cast, which puts
    // Miri into permissive provenance wherever it appears, and the crate
    // keeps it in the one place that owns block addresses.
    let header = BlockHeader::of_ptr(child as *const u8);
    let block = header as usize;
    let kind = unsafe { collector_load_block_kind(&raw const (*header).kind) };
    match kind {
        BLOCK_KIND_ENTITY => EdgeTarget::Tracked(RowKey {
            block,
            index: unsafe { crate::memory::heap::entity_slot_index(child as *mut u8) },
            population: Population::Slotted,
        }),
        BLOCK_KIND_RETAINED => {
            match unsafe { crate::memory::retained::occupant_index(block, child as usize) } {
                Some(position) => EdgeTarget::Tracked(RowKey {
                    block,
                    index: position as u32,
                    population: Population::Retained,
                }),
                None => {
                    // Two states answer alike here and only one of them
                    // is expected: a block stamped retained with no
                    // survivor list — held for a payload alone, not yet
                    // published by its reset, or published without a
                    // list because none could be placed. A listed block
                    // that does not name a live occupant is a
                    // disagreement between this lookup and the
                    // classification `promote` performs in one place,
                    // and this is the only site in the process that can
                    // see it — `Untracked` would hide it for the rest of
                    // the run.
                    debug_assert!(
                        !unsafe { crate::memory::retained::has_survivor_list(block) }
                            || unsafe { crate::refcount::header_refcount(child) } == 0,
                        "a listed retained block does not name a live occupant"
                    );
                    EdgeTarget::Untracked
                }
            }
        }
        BLOCK_KIND_ENTITY_LARGE | BLOCK_KIND_ENTITY_LARGE_RUN => {
            // The one population whose kind does not say which heap it
            // belongs to: `arena::alloc_entity` hands an entity past one
            // block payload to the very allocator the GC heap uses, so a
            // `RequestArena` entity carries these kinds too. Descending
            // into one would trial-delete an entity the arena's reset is
            // about to free, and an unreachable component would free it a
            // second time. Promotion rewrites the category in place and
            // deliberately leaves the kind alone, so the category is the
            // word that is right on both sides of a reset
            // (`rfc/model/gc/rc-cycle.md`, "A large entity's block kind
            // does not say which heap it belongs to"; `promote.rs`, the
            // surviving-run arm).
            if unsafe { MemoryCategory::from_flags(mutator_flags(child)) } == MemoryCategory::GcHeap
            {
                EdgeTarget::Tracked(RowKey {
                    block,
                    index: SINGLE_ENTITY_INDEX,
                    population: Population::SingleEntity,
                })
            } else {
                EdgeTarget::Untracked
            }
        }
        _ => EdgeTarget::Untracked,
    }
}

// Dispatches this thread has made through `resolve_edge_target` (tests only).
//
// The dispatch is the trace's per-edge cost — a block-kind load, and for a
// retained block a search of the survivor list — and the worklist entry's
// shape decides how many of them a scan makes per entity. Nothing collects
// yet, so the figure that decides the shape is this count and not a duration
// (`dev/CYCLE-COLLECTOR-REVIEW.md`, finding 2).
//
// Per thread, because the tests that trace run beside each other.
#[cfg(test)]
thread_local! {
    static EDGE_DISPATCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Add one to `EDGE_DISPATCHES`, and nothing at all without `cfg(test)`.
#[inline]
fn note_dispatch() {
    #[cfg(test)]
    EDGE_DISPATCHES.with(|count| count.set(count.get() + 1));
}

/// Dispatches this thread has made since this last answered, which it
/// leaves at zero. Reading and clearing together, because every caller
/// prices one traversal and the one before it is another test's.
#[cfg(test)]
pub(crate) fn take_edge_dispatches() -> usize {
    EDGE_DISPATCHES.with(|count| count.replace(0))
}

#[cfg(test)]
mod tests;
