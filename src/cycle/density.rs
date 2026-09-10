//! What share of a touched block's slots one trace met, read after the
//! trace and before the commit (`PLAN.md` S40.1).
//!
//! **Before the commit and not merely before the arena's reset.** The commit
//! runs the maturation descent, which takes the working count of every row the
//! scan left live for its own component index
//! (`crate::cycle::maturation`). Past that call a live row reads as a count
//! this module would report as internal edges, and the first component closed
//! reads as saturated; both figures would be the descent's arithmetic rather
//! than the trace's. A row the scan left potentially unreachable is untouched
//! by it and carries what the trace left.
//!
//! # Why the reading is taken from the rows rather than from the path
//!
//! A row's final state already carries the answer. Colour zero is
//! reserved for a slot the trace never reached
//! (`crate::cycle::shadow`), so a walk over the touched list separates
//! met slots from unmet ones without any counter on the path that meets
//! them. That keeps the mark's and the scan's operation counts what they
//! were, which is what `crate::cycle::shadow::written_bytes` and
//! `crate::cycle::row::take_edge_dispatches` are asserted against.
//!
//! # Two denominators, and they are not interchangeable
//!
//! The index space is what the row array reserves rows for, and it is
//! the denominator the row form is decided on: the flat array costs
//! `4 × index space` whatever is occupied. Occupancy is what the heap
//! holds, and it is the portable figure — the one a corpus arm could be
//! compared against once one exists. Both are reported for every block.
//!
//! **Neither is averaged across populations.** A retained block's index
//! space is its survivor list, which has no free positions at all, and a
//! large entity's is one row by construction; folding those into an
//! entity block's density would move the figure without measuring
//! anything. [`TraceDensity`] therefore keeps the three apart by shape
//! rather than by a caller's discipline.
//!
//! # Groups, beside rows
//!
//! The flat array initialises a group of eight rows at a time and the
//! chunked alternative carries one directory entry per group of eight,
//! so the bytes turn on groups where the design's crossing is stated in
//! slots. The bitmap already holds the group reading and a popcount is
//! what it costs.
//!
//! # The one header half this module reads that the trace does not
//!
//! Occupancy comes from `BlockPrivate::used`, which is the owner's half
//! of the block header — the half every allocation borrows as `&mut`,
//! and the half the design keeps the collector out of by splitting
//! `kind`, `size_class` and `owner` out of it. Reading it here is sound
//! on the terms the in-line owner trace already runs on: the owning
//! thread, with no mutator beside it, which is the same condition
//! `heap::for_each_entity_slot` reads `private.bump` under. It is a
//! `cfg(test)` reading and no production path takes it. **Whether a
//! collector worker may take it is not settled**: the block-disjointness
//! proof is incomplete for adoption, moved objects, actor sharing and
//! FFI entry (`rfc/model/gc/rc-cycle.md`), so an off-thread reader of
//! this figure needs an answer this module does not have.
//!
//! # What this module owns
//!
//! Nothing. It reads rows the caller's arena holds, allocates nothing
//! and writes nothing — including no row: it goes through
//! [`shadow::row`] and never through `ensure_row`, whose meeting would
//! initialise the very rows the reading is about. Test builds only.

use crate::cycle::arena::TraceScratchArena;
use crate::cycle::row::{Population, entity_at};
use crate::cycle::shadow::{self, Color, RowArray};
use crate::refcount::header_refcount;

/// What one trace met in one touched block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct BlockDensity {
    /// Which population the block belongs to, and therefore which
    /// arithmetic produced the two denominators below.
    pub(crate) population: Population,
    /// Rows the block's index space holds: slots for an entity block,
    /// survivor-list positions for a retained one, one for a large
    /// entity.
    pub(crate) index_space: u32,
    /// Occupied positions of that index space, read from the heap rather
    /// than from the rows.
    pub(crate) occupied: u32,
    /// Rows the trace met: colour above [`Color::Untouched`], in a group
    /// the trace zeroed.
    pub(crate) rows_met: u32,
    /// Met rows whose working count is a lower bound rather than a
    /// total. They are conservatively live whatever the trace subtracts,
    /// so an in-edge count taken as `refcount - count` is wrong for them
    /// and they are reported apart rather than folded in at zero.
    pub(crate) rows_saturated: u32,
    /// Groups the index space holds, including the group the array's
    /// rounding adds.
    pub(crate) groups: u32,
    /// Groups the trace zeroed, which is what the flat form actually
    /// wrote and what a chunked form would reserve a directory entry
    /// for.
    pub(crate) groups_met: u32,
}

/// The sum over one population's touched blocks.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct PopulationDensity {
    pub(crate) blocks: u32,
    pub(crate) index_space: u64,
    pub(crate) occupied: u64,
    pub(crate) rows_met: u64,
    pub(crate) rows_saturated: u64,
    pub(crate) groups: u64,
    pub(crate) groups_met: u64,
}

impl PopulationDensity {
    /// Add one block's reading.
    fn add(&mut self, block: &BlockDensity) {
        self.blocks += 1;
        self.index_space += u64::from(block.index_space);
        self.occupied += u64::from(block.occupied);
        self.rows_met += u64::from(block.rows_met);
        self.rows_saturated += u64::from(block.rows_saturated);
        self.groups += u64::from(block.groups);
        self.groups_met += u64::from(block.groups_met);
    }
}

/// One trace's reading, kept per population.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct TraceDensity {
    /// Ordinary entity blocks, and the only population the row form is
    /// decided on.
    pub(crate) slotted: PopulationDensity,
    /// Retained former-arena blocks. The index space is the survivor
    /// list the reset wrote once; the occupancy is the count word, which
    /// every occupant's death lowers, so the two are equal only until
    /// the first death inside the block
    /// (`crate::memory::retained::held_occupant_count`). The share a
    /// trace meets is not fixed either — a block holding one traced
    /// survivor among fourteen untraced ones reads what an entity block
    /// of fifteen slots would.
    pub(crate) retained: PopulationDensity,
    /// Large entities, one row each. Arithmetic rather than a
    /// measurement: the share is one of one whatever the trace does.
    pub(crate) single_entity: PopulationDensity,
}

/// Internal in-edges recoverable from the rows one completed mark left.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct InternalEdgeCensus {
    /// `refcount - shadow count`, summed only where the shadow count is exact.
    pub(crate) recoverable_internal_edges: u64,
    /// Met rows whose shadow count is a lower bound. They contribute to
    /// neither an edge numerator nor denominator and are reported apart.
    pub(crate) saturated_rows: u64,
}

/// Read every touched block of `arena`, newest first, and hand each
/// reading to `visit`.
///
/// # Safety
/// `arena` has not been reset, so every array is still stamped on its
/// block, and **every block the arena touched is still mapped**. In a
/// collection the open trace window is what guarantees the second
/// condition, a slot's physical return being withheld for as long as a
/// trace can address its row (`rfc/model/gc/rc-cycle.md`, "Zero-count
/// entities pending slot reuse"); a caller that closed the window first
/// reads rows the pool has lent out again. A caller that met rows
/// without a window — a test driving `ensure_row` — owes the same
/// condition by holding the entities alive itself. An entity block's
/// occupancy is read out of the owner's half of its header, so this runs
/// on the thread that owns it.
unsafe fn for_each_touched_block(arena: &TraceScratchArena, mut visit: impl FnMut(BlockDensity)) {
    let mut array = arena.touched_head();
    while !array.is_null() {
        visit(unsafe { read_block(array) });
        array = unsafe { (*array).next };
    }
}

/// The sum of [`for_each_touched_block`]'s readings, per population.
///
/// # Safety
/// As [`for_each_touched_block`].
pub(crate) unsafe fn totals(arena: &TraceScratchArena) -> TraceDensity {
    let mut density = TraceDensity::default();
    unsafe {
        for_each_touched_block(arena, |block| {
            match block.population {
                Population::Slotted => density.slotted.add(&block),
                Population::Retained => density.retained.add(&block),
                Population::SingleEntity => density.single_entity.add(&block),
            };
        });
    }

    density
}

/// Count the internal in-edges a completed mark found.
///
/// For a non-saturated row, mark copied the entity's refcount and subtracted
/// one for every internal in-edge it encountered. The difference between the
/// current header refcount and the remaining shadow count is therefore the
/// number of internal in-edges targeting this entity. A later scan changes
/// only colour, so the same reading is valid after a completed trace and
/// invalid after the commit, whose descent takes the count of every live row
/// (module doc).
///
/// This is deliberately **not** a maturation simulator. It carries no age or
/// identity across collections, reads no stamp or epoch, groups no component,
/// and applies no threshold. A caller measuring pruning must combine this
/// denominator with the stamps S37.0's producer writes and the descent S37.1
/// builds; this function cannot manufacture that policy by construction.
///
/// # Safety
/// As [`totals`]. Every met row was produced by a completed mark, no commit has
/// run over these rows, and no entity
/// refcount or edge changed before this read; a test may instead construct a
/// row directly when its block-to-entity mapping is valid and its supplied
/// shadow count is the exact post-mark count the case claims (or saturation,
/// which is excluded before subtraction).
pub(crate) unsafe fn internal_edges(arena: &TraceScratchArena) -> InternalEdgeCensus {
    let mut census = InternalEdgeCensus::default();
    census.saturated_rows = unsafe {
        for_each_recoverable_internal_edge_target(arena, |_entity, edges| {
            census.recoverable_internal_edges = census
                .recoverable_internal_edges
                .checked_add(edges)
                .expect("the internal-edge census cannot overflow");
        })
    };
    census
}

/// Hand every non-saturated target and its recoverable internal in-edge count
/// to `visit`, and answer how many saturated rows were excluded.
///
/// This is a test-only extraction point, not a production hook: `density` is
/// compiled only under `cfg(test)` and this function is private to it. S37.1
/// owns its traversal in `mark` and cannot call this function. After that
/// production path exists, a measurement in this module may reuse this visitor
/// to inspect the stamps it wrote without adding age or threshold policy here.
///
/// # Safety
/// As [`internal_edges`], and the call stands before the commit that would
/// take the live rows' counts for the maturation descent (module doc).
unsafe fn for_each_recoverable_internal_edge_target(
    arena: &TraceScratchArena,
    mut visit: impl FnMut(*mut crate::refcount::RcHeader, u64),
) -> u64 {
    let mut saturated_rows = 0;
    let mut array = arena.touched_head();
    while !array.is_null() {
        let block = unsafe { (*array).block };
        let population = unsafe { (*array).population };
        let row_count = if population == Population::SingleEntity {
            1
        } else {
            unsafe { (*array).row_count }
        };

        for index in 0..row_count {
            let word = if population == Population::SingleEntity {
                unsafe { *crate::memory::large_entity::shadow_row(block) }
            } else {
                if !unsafe { shadow::group_is_initialized(array, index) } {
                    continue;
                }
                unsafe { *shadow::row(array, index) }
            };
            if shadow::color(word) == Color::Untouched {
                continue;
            }
            if shadow::is_saturated(word) {
                saturated_rows += 1;
                continue;
            }

            let entity = unsafe { entity_at(block, population, index) }
                .expect("a touched row names a live entity");
            let references = u64::from(unsafe { header_refcount(entity) });
            let shadow_count = u64::from(shadow::count(word));
            let edges = references
                .checked_sub(shadow_count)
                .expect("the owner mark left no shadow count above the refcount");
            visit(entity, edges);
        }

        array = unsafe { (*array).next };
    }

    saturated_rows
}

/// One array's reading.
///
/// # Safety
/// As [`for_each_touched_block`], for the block `array` describes.
unsafe fn read_block(array: *mut RowArray) -> BlockDensity {
    let block = unsafe { (*array).block };
    let population = unsafe { (*array).population };

    if population == Population::SingleEntity {
        // The one population with no array behind its header: the row is
        // a word of the block's own header and the index space is one by
        // construction, so there is nothing here to walk.
        let word = unsafe { *crate::memory::large_entity::shadow_row(block) };
        let met = u32::from(shadow::color(word) != Color::Untouched);
        return BlockDensity {
            population,
            index_space: 1,
            occupied: 1,
            rows_met: met,
            rows_saturated: met * u32::from(shadow::is_saturated(word)),
            groups: 1,
            groups_met: met,
        };
    }

    // The index space and never the array's own row count: the array
    // reserves up to a whole group past the index space
    // (`shadow::bytes_for`), and at the 256-byte class that is eight
    // rows that are not slots. Counting them can only inflate the share.
    let index_space = unsafe { (*array).row_count };
    let mut rows_met = 0;
    let mut rows_saturated = 0;
    for index in 0..index_space {
        // The group bit first. An untouched group's rows are whatever
        // the block that held this arena memory before left in them, so
        // reading one as a colour reports the previous tenant.
        if !unsafe { shadow::group_is_initialized(array, index) } {
            continue;
        }

        let word = unsafe { *shadow::row(array, index) };
        if shadow::color(word) == Color::Untouched {
            continue;
        }

        rows_met += 1;
        if shadow::is_saturated(word) {
            rows_saturated += 1;
        }
    }

    let occupied = match population {
        Population::Slotted => unsafe { crate::memory::heap::block_occupancy(block) },
        Population::Retained => unsafe {
            crate::memory::retained::held_occupant_count(block as usize)
        },
        Population::SingleEntity => unreachable!("answered above"),
    };

    BlockDensity {
        population,
        index_space,
        occupied,
        rows_met,
        rows_saturated,
        groups: shadow::group_count(index_space),
        groups_met: unsafe { shadow::groups_met(array) },
    }
}

#[cfg(test)]
mod tests;
