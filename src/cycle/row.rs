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

use crate::memory::block_pool::{
    BLOCK_KIND_ENTITY, BLOCK_KIND_ENTITY_LARGE, BLOCK_KIND_ENTITY_LARGE_RUN, BLOCK_KIND_RETAINED,
    BlockHeader, collector_load_block_kind,
};
use crate::refcount::{MemoryCategory, RcHeader, mutator_flags};

/// The row index of a large entity, which is the sole occupant of its
/// own block or run and gets one row in that block's header rather than
/// an array (`rfc/model/gc/rc-cycle.md`, "Where the shadow count
/// lives").
const SOLE_OCCUPANT: u32 = 0;

/// The shadow row of one entity: the block whose rows carry it, and the
/// entity's index among them.
///
/// It is an identity and not an address. Where the rows themselves are
/// reserved, and how the index reaches one, is S33.2 of `PLAN.md`; two
/// entities of one block resolving to one `Row` is that stage's defect
/// however the rows are laid out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Row {
    /// The block header's address, 64 KiB-aligned. For a large entity
    /// held in an OS-direct run, the run's first block, which is the one
    /// carrying the header.
    pub block: usize,
    /// The entity's index into that block's rows: its slot index in an
    /// entity block, its position in the occupant index of a retained
    /// block, and [`SOLE_OCCUPANT`] for a large entity.
    pub index: u32,
}

/// What the trace does with one edge it has just read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Edge {
    /// The child is a GC-heap entity with this row: the descent
    /// continues through it and the row takes the decrement.
    Interior(Row),
    /// The child is outside the GC heap, so the descent stops and the
    /// edge counts as an external live reference. An arena entity is the
    /// case that matters — a ring through the arena is broken by the
    /// arena's own reset — and an immortal entity answers the same way.
    External,
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
/// [`Edge::External`] as well, which keeps its referent alive rather
/// than condemning it on a row the trace guessed
/// (`memory::retained::occupant_index`).
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
// Not `#[expect]` unconditionally: the tests below call it, so under
// `cfg(test)` the lint would not fire and the expectation itself would
// warn. A release build is where the debt has to report.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the mark that traces these edges is S35.1")
)]
pub(crate) unsafe fn edge_to(child: *mut RcHeader) -> Edge {
    // Through `BlockHeader::of_ptr` rather than a mask of its own: the
    // address-to-block step is an integer-to-pointer cast, which puts
    // Miri into permissive provenance wherever it appears, and the crate
    // keeps it in the one place that owns block addresses.
    let header = BlockHeader::of_ptr(child as *const u8);
    let block = header as usize;
    let kind = unsafe { collector_load_block_kind(&raw const (*header).kind) };
    match kind {
        BLOCK_KIND_ENTITY => Edge::Interior(Row {
            block,
            index: unsafe { crate::memory::heap::entity_slot_index(child as *mut u8) },
        }),
        BLOCK_KIND_RETAINED => {
            match crate::memory::retained::occupant_index(block, child as usize) {
                Some(position) => Edge::Interior(Row {
                    block,
                    index: position as u32,
                }),
                None => {
                    // Two states answer alike here and only one of them
                    // is expected: a block stamped retained whose index
                    // the reset has not registered yet, which is the
                    // window between the stamp and `register`. An
                    // indexed block that does not name a live occupant
                    // is a disagreement between this lookup and the
                    // classification `promote` performs in one place,
                    // and this is the only site in the process that can
                    // see it — `External` would hide it for the rest of
                    // the run.
                    debug_assert!(
                        !crate::memory::retained::has_occupant_index(block)
                            || unsafe { crate::refcount::header_refcount(child) } == 0,
                        "an indexed retained block does not name a live occupant"
                    );
                    Edge::External
                }
            }
        }
        BLOCK_KIND_ENTITY_LARGE | BLOCK_KIND_ENTITY_LARGE_RUN => {
            // The one population whose kind does not say which heap it
            // belongs to: `arena::alloc_entity` hands an entity past one
            // block payload to the very allocator the GC heap uses, so a
            // `RequestArena` entity carries these kinds too. Descending
            // into one would trial-delete an entity the arena's reset is
            // about to free, and a condemned component would free it a
            // second time. Promotion rewrites the category in place and
            // deliberately leaves the kind alone, so the category is the
            // word that is right on both sides of a reset
            // (`rfc/model/gc/rc-cycle.md`, "A large entity's block kind
            // does not say which heap it belongs to"; `promote.rs`, the
            // surviving-run arm).
            if unsafe { MemoryCategory::from_flags(mutator_flags(child)) } == MemoryCategory::GcHeap
            {
                Edge::Interior(Row {
                    block,
                    index: SOLE_OCCUPANT,
                })
            } else {
                Edge::External
            }
        }
        _ => Edge::External,
    }
}

#[cfg(test)]
mod tests;
