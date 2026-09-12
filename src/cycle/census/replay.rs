//! The arena's grants replayed over one collection's census, for the flat
//! form the collector has and for the chunked form
//! `dev/SHADOW-ROW-REPRESENTATION-ANALYSIS.md`, "The specified chunked form"
//! specifies (`PLAN.md` S40.5). Test builds only.
//!
//! # What a replay is
//!
//! Arithmetic over a [`CollectionReport`], and never a collection: the bump
//! the arena runs — the workspace's [`WORKSPACE_BUMP_BYTES`], then one
//! [`BLOCK_PAYLOAD`] block at a time, every request rounded to eight, a block
//! left behind when a request does not fit in it — is a few lines, and the
//! requests a form makes follow from the blocks a trace touched, the groups it
//! met in each and the segments its chains drew, all of which the census reads.
//! The flat form's requests are the arrays the arena reserved, so its replay
//! is checked against the census's own counters and holds to the byte; the
//! chunked form's requests are the specification's, over the same `G` and `T`,
//! so its replay is what that form would have asked the same arena for.
//!
//! # The order of requests, and what the census does not record
//!
//! Where an arena block ends decides the tails, and that depends on the order
//! the requests arrive in. The replay makes them in the order a ring's
//! collection does: the first root's row array, then the worklist's first
//! segment at the first push, then every other block's array at its first
//! touch, then the segments the descent and the teardown draw
//! (`crate::cycle::mark`, `crate::cycle::maturation`). That is the order of
//! every load S40.3 records, because a ring keeps the mark's worklist at
//! depth one; a mark that queues more entities at once than a segment holds
//! ([`SEGMENT_ENTRIES`](crate::cycle::stack::SEGMENT_ENTRIES)) draws a
//! segment between two arrays, and the census, which reads each chain's
//! deepest standing at the close and not the mark's depth, cannot say where.
//! So the flat replay's agreement with the census is asserted on
//! every load rather than assumed, and a load with such a fan-out would fail
//! that assertion on its tails. The chunked form's chunks arrive at group
//! first touches, whose order across blocks the census does not record
//! either. [`chunked`] takes the order the loads have by construction, each
//! block's groups consecutively, and [`bound`] brackets the draw count for
//! every order: at least what the bytes alone need, at most what the bytes
//! need with every growth charged the largest tail a request can leave and
//! every directory the continuations it can take.

use crate::cycle::arena::WORKSPACE_BUMP_BYTES;
use crate::cycle::census::CollectionReport;
use crate::cycle::drops::SEGMENT_BYTES as DROP_SEGMENT_BYTES;
use crate::cycle::row::Population;
use crate::cycle::shadow;
use crate::cycle::stack::SEGMENT_BYTES;
use crate::memory::block_pool::BLOCK_PAYLOAD;

/// Bytes of one chunk: eight rows of four.
pub(crate) const CHUNK_BYTES: usize = shadow::GROUP as usize * size_of::<u32>();

/// Bytes of the directory's header: the flat form's 24-byte prologue and the
/// continuation word.
const DIRECTORY_HEADER_BYTES: usize = size_of::<shadow::RowArray>() + size_of::<usize>();

/// Bytes a chunked directory takes for `groups`: the header and one `u16`
/// entry per group, rounded to the arena's eight.
pub(crate) const fn directory_bytes(groups: u32) -> usize {
    directory_write_bytes(groups).next_multiple_of(8)
}

/// Bytes a chunked directory writes when it is placed: the header and every
/// entry, cleared whole, and none of the rounding.
const fn directory_write_bytes(groups: u32) -> usize {
    DIRECTORY_HEADER_BYTES + groups as usize * size_of::<u16>()
}

/// One touched block as the replay needs it: its population, its index space
/// and the groups it holds and the trace met.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct BlockShape {
    pub(crate) population: Population,
    pub(crate) index_space: u32,
    pub(crate) groups: u32,
    pub(crate) groups_met: u32,
}

impl BlockShape {
    /// Bytes the flat form requests for this block: its whole row array, or
    /// the prologue alone for a large entity.
    fn flat_request(&self) -> usize {
        match self.population {
            Population::SingleEntity => shadow::bytes_for(0),
            Population::Slotted | Population::Retained => shadow::bytes_for(self.index_space),
        }
    }

    /// Bytes the chunked form's directory takes for this block, with the
    /// first chunk that rides in the same request; a large entity's directory
    /// is the prologue alone, its row being in its own block header.
    fn directory_request(&self) -> usize {
        match self.population {
            Population::SingleEntity => directory_bytes(0),
            Population::Slotted | Population::Retained => {
                directory_bytes(self.groups) + CHUNK_BYTES
            }
        }
    }

    /// Chunks placed after the first, each a request of its own.
    fn later_chunks(&self) -> usize {
        match self.population {
            Population::SingleEntity => 0,
            Population::Slotted | Population::Retained => {
                (self.groups_met as usize).saturating_sub(1)
            }
        }
    }

    /// Bytes the flat form writes at this block's first touch and at each
    /// group's: the prologue and the bitmap, then a group of eight rows per
    /// group met (`shadow::init`, `shadow::ensure_group_initialized`).
    fn flat_first_touch_writes(&self) -> usize {
        let groups_met = match self.population {
            Population::SingleEntity => 0,
            Population::Slotted | Population::Retained => self.groups_met as usize,
        };
        let request = self.flat_request();
        let rows = shadow::group_count(self.index_space) as usize * CHUNK_BYTES;
        request - rows + groups_met * CHUNK_BYTES
    }

    /// Bytes the chunked form writes at this block's first touch and at each
    /// group's: the directory cleared whole, then a chunk per group met.
    fn chunked_first_touch_writes(&self) -> usize {
        match self.population {
            Population::SingleEntity => directory_write_bytes(0),
            Population::Slotted | Population::Retained => {
                directory_write_bytes(self.groups) + self.groups_met as usize * CHUNK_BYTES
            }
        }
    }
}

/// What one collection asked the arena for, as the census read it: the
/// blocks in the order the trace touched them, and the segments each chain
/// held at the close.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct CollectionShape {
    /// Oldest first, which is the order the arrays were reserved in; the
    /// census lists the touched list newest first.
    pub(crate) blocks: Vec<BlockShape>,
    pub(crate) worklist_segments: usize,
    pub(crate) component_segments: usize,
    pub(crate) drop_segments: usize,
}

impl CollectionShape {
    /// The shape of the collection `report` describes.
    ///
    /// # Panics
    /// When the report lacks either reading: a collection that never reached
    /// its scan's end or its close has no shape to replay.
    pub(crate) fn of(report: &CollectionReport) -> Self {
        let scan = report.scan.as_ref().expect("the scan ended");
        let close = report.close.expect("the close was reached");
        Self {
            blocks: scan
                .blocks
                .iter()
                .rev()
                .map(|block| BlockShape {
                    population: block.population,
                    index_space: block.index_space,
                    groups: block.groups,
                    groups_met: block.groups_met,
                })
                .collect(),
            worklist_segments: close.worklist_segments,
            component_segments: close.component_segments,
            drop_segments: close.drop_segments,
        }
    }

    /// The segments' requests, in the order the collection makes them past
    /// the worklist's first: the descent's and the teardown's.
    fn later_segments(&self) -> impl Iterator<Item = usize> {
        let stack = self.worklist_segments.saturating_sub(1) + self.component_segments;
        std::iter::repeat_n(SEGMENT_BYTES, stack)
            .chain(std::iter::repeat_n(DROP_SEGMENT_BYTES, self.drop_segments))
    }

    /// Bytes the segments take, all chains together.
    fn segment_bytes(&self) -> usize {
        (self.worklist_segments + self.component_segments) * SEGMENT_BYTES
            + self.drop_segments * DROP_SEGMENT_BYTES
    }
}

/// What one form's replay cost the arena.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Outcome {
    /// Requests on the rows' account: arrays for the flat form; directories,
    /// chunks and continuations for the chunked one.
    pub(crate) row_requests: usize,
    /// Bytes those requests asked for, and what the bump granted them after
    /// its rounding to eight.
    pub(crate) row_bytes_requested: usize,
    pub(crate) row_bytes_granted: usize,
    /// Continuation directories the chunked form placed; zero for the flat
    /// form.
    pub(crate) continuations: usize,
    /// Bytes the form writes at the blocks' and the groups' first touches:
    /// prologue and bitmap or directory, and one group of rows per group met.
    pub(crate) first_touch_writes: usize,
    /// Bytes granted to every consumer, rows and segments.
    pub(crate) granted: usize,
    /// Blocks the bump grew into past the workspace.
    pub(crate) draws: usize,
    /// Tails the bump left behind in the blocks it grew past, and their
    /// bytes.
    pub(crate) tails: usize,
    pub(crate) tail_bytes: usize,
    /// Bytes the bump could still grant at the close.
    pub(crate) remainder: usize,
}

/// The draw count of a form over every order its requests can arrive in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct DrawBound {
    /// Draws the bytes alone need, with no tail and no continuation.
    pub(crate) least: usize,
    /// Draws that suffice whatever the order: every growth charged the
    /// largest tail one request can leave, every directory the continuations
    /// it can take under that many growths.
    pub(crate) most: usize,
    /// The continuations charged at `most`.
    pub(crate) continuations_at_most: usize,
}

/// The arena's bump, as arithmetic: what it grants, what it abandons, what
/// it draws.
struct Bump {
    /// Bytes left in the block under the cursor.
    left: usize,
    /// Which block the cursor is in: zero for the workspace, then one per
    /// draw.
    block: usize,
    draws: usize,
    tails: usize,
    tail_bytes: usize,
    granted: usize,
}

impl Bump {
    fn over_the_workspace() -> Self {
        Self {
            left: WORKSPACE_BUMP_BYTES,
            block: 0,
            draws: 0,
            tails: 0,
            tail_bytes: 0,
            granted: 0,
        }
    }

    /// Grant `bytes`, rounded to eight, growing into a fresh block when they
    /// do not fit; the block the grant landed in.
    fn grant(&mut self, bytes: usize) -> usize {
        let rounded = bytes.next_multiple_of(8);
        assert!(
            rounded <= BLOCK_PAYLOAD,
            "a request the arena refuses outright"
        );
        if rounded > self.left {
            self.tails += 1;
            self.tail_bytes += self.left;
            self.draws += 1;
            self.block += 1;
            self.left = BLOCK_PAYLOAD;
        }

        self.left -= rounded;
        self.granted += rounded;
        self.block
    }
}

/// The rows' account of an outcome, kept while the requests are replayed.
#[derive(Default)]
struct RowAccount {
    requests: usize,
    requested: usize,
    granted: usize,
    continuations: usize,
    first_touch_writes: usize,
}

impl RowAccount {
    fn request(&mut self, bump: &mut Bump, bytes: usize) -> usize {
        self.requests += 1;
        self.requested += bytes;
        self.granted += bytes.next_multiple_of(8);
        bump.grant(bytes)
    }
}

/// The replay of `shape` under the flat form: one array per touched block,
/// in the collector's order of requests.
pub(crate) fn flat(shape: &CollectionShape) -> Outcome {
    let mut bump = Bump::over_the_workspace();
    let mut rows = RowAccount::default();
    for (touched, block) in shape.blocks.iter().enumerate() {
        rows.request(&mut bump, block.flat_request());
        rows.first_touch_writes += block.flat_first_touch_writes();
        if touched == 0 && shape.worklist_segments > 0 {
            bump.grant(SEGMENT_BYTES);
        }
    }

    for segment in shape.later_segments() {
        bump.grant(segment);
    }

    outcome(bump, rows)
}

/// The replay of `shape` under the specified chunked form, in the order the
/// loads have by construction: each block's directory with its first chunk,
/// then its other met groups' chunks one by one, a continuation wherever the
/// bump has left the block the chain's last directory stands in.
pub(crate) fn chunked(shape: &CollectionShape) -> Outcome {
    let mut bump = Bump::over_the_workspace();
    let mut rows = RowAccount::default();
    for (touched, block) in shape.blocks.iter().enumerate() {
        let mut chain_tail = rows.request(&mut bump, block.directory_request());
        rows.first_touch_writes += block.chunked_first_touch_writes();
        if touched == 0 && shape.worklist_segments > 0 {
            bump.grant(SEGMENT_BYTES);
        }

        for _ in 0..block.later_chunks() {
            if bump.block == chain_tail && bump.left >= CHUNK_BYTES {
                rows.request(&mut bump, CHUNK_BYTES);
            } else {
                chain_tail = rows.request(&mut bump, block.directory_request());
                rows.continuations += 1;
                rows.first_touch_writes += directory_write_bytes(block.groups);
            }
        }
    }

    for segment in shape.later_segments() {
        bump.grant(segment);
    }

    outcome(bump, rows)
}

/// The draw count of the chunked form over `shape` for every order its
/// requests can arrive in.
pub(crate) fn chunked_bound(shape: &CollectionShape) -> DrawBound {
    let bytes = shape
        .blocks
        .iter()
        .map(|block| block.directory_request() + block.later_chunks() * CHUNK_BYTES)
        .sum::<usize>()
        + shape.segment_bytes();
    let largest_request = shape
        .blocks
        .iter()
        .map(BlockShape::directory_request)
        .chain(shape.later_segments())
        .chain(std::iter::repeat_n(
            SEGMENT_BYTES,
            shape.worklist_segments.min(1),
        ))
        .max()
        .unwrap_or(0);
    let continuations_under = |draws: usize| {
        shape
            .blocks
            .iter()
            .map(|block| block.later_chunks().min(draws) * directory_bytes(block.groups))
            .sum::<usize>()
    };
    bound(bytes, largest_request, continuations_under, |draws| {
        shape
            .blocks
            .iter()
            .map(|block| block.later_chunks().min(draws))
            .sum()
    })
}

/// The draw count of the flat form over `shape` for every order its
/// requests can arrive in, which brackets the observed count: the flat replay
/// is exact, and this is the same bracket [`chunked_bound`] gives the other
/// form.
pub(crate) fn flat_bound(shape: &CollectionShape) -> DrawBound {
    let bytes = shape
        .blocks
        .iter()
        .map(|block| block.flat_request().next_multiple_of(8))
        .sum::<usize>()
        + shape.segment_bytes();
    let largest_request = shape
        .blocks
        .iter()
        .map(|block| block.flat_request().next_multiple_of(8))
        .chain(shape.later_segments())
        .chain(std::iter::repeat_n(
            SEGMENT_BYTES,
            shape.worklist_segments.min(1),
        ))
        .max()
        .unwrap_or(0);
    bound(bytes, largest_request, |_| 0, |_| 0)
}

/// The bracket both forms' bounds are computed by.
///
/// `bytes` is what the requests take with no tail and no continuation, so no
/// order fits in fewer draws than `least`. For `most`, a count of draws
/// suffices when the capacity it opens holds `bytes`, the continuations that
/// many growths can force and one tail of `largest_request − 8` per growth:
/// a tail is what a request that did not fit left behind, so it is smaller
/// than that request, and both are multiples of eight.
fn bound(
    bytes: usize,
    largest_request: usize,
    continuation_bytes_under: impl Fn(usize) -> usize,
    continuations_under: impl Fn(usize) -> usize,
) -> DrawBound {
    let capacity = |draws: usize| WORKSPACE_BUMP_BYTES + draws * BLOCK_PAYLOAD;
    let least = bytes
        .saturating_sub(WORKSPACE_BUMP_BYTES)
        .div_ceil(BLOCK_PAYLOAD);
    let largest_tail = largest_request.saturating_sub(8);
    let mut most = least;
    while capacity(most) < bytes + continuation_bytes_under(most) + most * largest_tail {
        most += 1;
    }

    DrawBound {
        least,
        most,
        continuations_at_most: continuations_under(most),
    }
}

fn outcome(bump: Bump, rows: RowAccount) -> Outcome {
    Outcome {
        row_requests: rows.requests,
        row_bytes_requested: rows.requested,
        row_bytes_granted: rows.granted,
        continuations: rows.continuations,
        first_touch_writes: rows.first_touch_writes,
        granted: bump.granted,
        draws: bump.draws,
        tails: bump.tails,
        tail_bytes: bump.tail_bytes,
        remainder: bump.left,
    }
}

#[cfg(test)]
mod tests;
