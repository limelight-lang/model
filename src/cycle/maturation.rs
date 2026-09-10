//! The maturation stamp a commit writes: which live entities take one, and
//! what the age is the minimum over.
//!
//! A collection that reads a component as held from outside proves it live for
//! this collection, and the stamp is what a later trace reads instead of
//! descending into it (`rfc/model/gc/cycle/questions.md`, Y9). Two populations
//! answer that description and only one of them had a producer. A set the scan
//! proposed as unreachable whose exact validation disagreed is stamped by
//! [`crate::cycle::finalization`]; the rows the scan coloured
//! [`Color::Live`] — the mature live core the descent is meant to stop at —
//! are this module's.
//!
//! # The unit is the strongly connected component
//!
//! The age of a component is one more than the minimum age over its members,
//! so the unit has to be a set whose membership is the same at every reading.
//! The traced closure is not one: where a service container connects
//! everything, the closure is the whole live set, and a single entity met for
//! the first time reads age zero and holds every member at age one for ever
//! (`rfc/dev/DECISIONS.md`, "the component a maturation stamp ages is the
//! strongly connected component"). The strongly connected component keeps a
//! ring's members together, which is what Y9 asks of the unit, and lets an
//! entity that points into a live core without being part of it age on its
//! own.
//!
//! # The descent, and where it keeps its state
//!
//! Pearce's single-index algorithm, run from an explicit stack: one number per
//! vertex, held in the working count of the vertex's own row. That field
//! belongs to this descent from the moment it starts — the trace is over, the
//! scan has read the count, and no production reader takes it again before the
//! arena's reset ([`shadow::write_live_index`]).
//!
//! A vertex is given the next visit index and pushed on the arena's component
//! stack. Every edge out of it becomes a frame; a child already visited lowers
//! the parent's index to the child's, and a child not yet visited is opened
//! with a frame beneath its own that lowers the parent afterwards. The vertex
//! whose index survived its whole expansion is the root of a component, and
//! the component is the run of vertices standing above it on the component
//! stack. Vertices already assigned to a closed component carry an index
//! counted down from [`COUNT_MAX`], which is above every visit index, so they
//! lower nothing and the algorithm needs no second bit to say which of the two
//! a number is.
//!
//! # What it costs, and what a refusal leaves
//!
//! Two walks over the touched list and one descent over the live subgraph: per
//! entity a row load and store, a kind load and one expansion of its cells;
//! per edge a frame, a block dispatch and a row load. The frames are the
//! trace's own worklist, empty since the scan ended and holding the segments
//! that scan drew; the component stack takes segments of the same size from
//! the same bump ([`TraceScratchArena::push_component`]).
//!
//! **The two stacks stand at different heights, and neither is the trace's.**
//! The component stack holds every visited vertex until its component's root
//! closes, so on a heap whose live core is one component it reaches the whole
//! live population — sixteen bytes a vertex. The frame stack holds the
//! out-edges of every vertex on the current path at once, because a vertex is
//! expanded in one call of `trace_cells` and its children cannot be
//! re-enumerated from a position; the mark and the scan hold one entry per
//! entity of their frontier instead.
//!
//! **What the descent takes, it takes before the teardown asks.** The
//! segments come out of the same bump the teardown later reserves its
//! displaced children from, and a descent that emptied the pool can leave
//! `reclamation`'s reservation to be refused: the component the commit
//! confirmed is then kept whole for a later collection instead of being torn
//! down. That is latency rather than a lost cycle — the registration stands
//! and the next collection reaches the same graph
//! (`rfc/model/gc/rc-cycle.md`, "Cost model").
//!
//! A refused segment ends the descent where it stands. Every component the
//! descent had already closed carries its stamp whole, an open one carries the
//! stamp it had, and the commit goes on: a stamp is a reduction of a later
//! trace's suspicion and never a fact anything else reads
//! ([`Stamped::Abandoned`]).
//!
//! # It runs on the ordinary path alone
//!
//! A collection an allocation failure started has given its blocks back before
//! the commit, so it holds a list of members and no rows to walk; drawing the
//! descent's segments there would ask for the memory that collection exists to
//! return. What such a thread loses is the maturation of its live core, and
//! its stamps are the exact validation's alone (`rfc/model/gc/rc-cycle.md`,
//! "What a commit stamps").

use std::ptr;

use crate::cells::{self, PlainCells};
use crate::cycle::arena::{TraceScratchArena, find_initialized_row};
use crate::cycle::membership::Membership;
use crate::cycle::row::{self, EdgeTarget, resolve_edge_target};
use crate::cycle::shadow::{self, COUNT_MAX, Color, RowArray};
use crate::cycle::stack::WorklistEntry;
use crate::refcount::{
    MATURATION_AGE_MAX, MaturationStamp, RcHeader, read_maturation_stamp, write_maturation_stamp,
};

/// The index of a live row the descent has not visited.
///
/// Zero is not a visit index — those count up from one — so the first walk
/// leaves the whole live population unvisited by writing it.
const UNVISITED: u32 = 0;

/// What the descent left behind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Stamped {
    /// Every live component of this commit's rows carries this commit's stamp.
    Whole,
    /// The commit holds no rows to walk, its membership being a harvested
    /// list: the pressure path, whose blocks went back before the commit.
    NoRows,
    /// A segment was refused, or a live row named no entity. The components
    /// closed before it keep their stamps and the rest keep what they carried.
    Abandoned,
}

/// Stamp every live component the commit's rows name with `epoch` and the age
/// its members earn.
///
/// `epoch` is the commit's own reading, taken once at
/// [`Finalization::begin`](crate::cycle::finalization::Finalization::begin), so
/// that two components of one collection age against one epoch.
///
/// **Called before the first guard and before any destructor.** Step 4 runs
/// user code that can free a member, and a stamp written after it would land
/// in whatever occupies the slot next (`rfc/model/gc/rc-cycle.md`, "Cycle
/// finalization and reclamation").
///
/// # Safety
/// As [`Membership::for_each`]: the arena is unswept, every block its rows
/// were written for is live, and the call runs on the owning thread with no
/// mutator beside it.
pub(crate) unsafe fn stamp_live_components(
    members: &Membership<'_>,
    arena: &mut TraceScratchArena,
    epoch: u32,
) -> Stamped {
    row::note_descent_boundary();
    let stamped = unsafe { stamp_over(members, arena, epoch) };
    row::note_descent_end();
    stamped
}

/// The descent itself, with the boundary its caller records around it.
///
/// # Safety
/// As [`stamp_live_components`].
unsafe fn stamp_over(
    members: &Membership<'_>,
    arena: &mut TraceScratchArena,
    epoch: u32,
) -> Stamped {
    let Membership::Rows { touched, .. } = *members else {
        return Stamped::NoRows;
    };

    unsafe { clear_live_indexes(touched) };

    let mut descent = Descent {
        index: FIRST_INDEX,
        component: COUNT_MAX,
        epoch,
    };
    let mut array = touched;
    while !array.is_null() {
        let block = unsafe { (*array).block };
        let population = unsafe { (*array).population };
        let mut refused = false;
        unsafe {
            row::for_each_live(array, block, population, |index| {
                let row = row::row_at(array, block, population, index);
                if shadow::count(*row) != UNVISITED {
                    return true;
                }

                // A row whose address cannot be recovered is a disagreement
                // between the array and a retained block's survivor list, and
                // the reading it belongs to is given up whole, as the
                // membership's own walk gives its up
                // (`crate::cycle::row::entity_at`).
                let Some(entity) = row::entity_at(block, population, index) else {
                    refused = true;
                    return false;
                };

                refused = !descent.run_from(arena, entity, row);
                !refused
            })
        };

        if refused {
            abandon(arena);
            note_refusal();
            return Stamped::Abandoned;
        }

        array = unsafe { (*array).next };
    }

    Stamped::Whole
}

/// The first visit index, which is one so that [`UNVISITED`] can be zero.
const FIRST_INDEX: u32 = 1;

/// Where one collection's descent stands.
struct Descent {
    /// The index the next vertex opened takes.
    index: u32,
    /// The index the next component closed takes, counted down from
    /// [`COUNT_MAX`] so that a vertex of a closed component lowers nothing.
    component: u32,
    /// The epoch every stamp of this commit carries.
    epoch: u32,
}

impl Descent {
    /// Walk the live subgraph reachable from `entity`, closing every component
    /// whose root it meets. False where a segment was refused.
    ///
    /// The worklist is empty when this returns on either path, so the next
    /// root of the walk starts on an empty stack.
    ///
    /// # Safety
    /// As [`stamp_live_components`], and `row` is `entity`'s own row, which
    /// the scan left [`Color::Live`] and this descent has not visited.
    unsafe fn run_from(
        &mut self,
        arena: &mut TraceScratchArena,
        entity: *mut RcHeader,
        row: *mut u32,
    ) -> bool {
        if !unsafe { self.open(arena, entity, row) } {
            return false;
        }

        while let Some(frame) = arena.pop_work() {
            match Frame::decode(frame) {
                Frame::OutEdge { child, from } => {
                    if !unsafe { self.take_edge(arena, child, from) } {
                        return false;
                    }
                }
                Frame::Post { child, from } => unsafe { lower(from, shadow::count(*child)) },
                Frame::Finish { row, index } => unsafe { self.close(arena, row, index) },
            }
        }

        true
    }

    /// Give `entity` the next visit index, put it on the component stack and
    /// turn each of its out-edges into a frame. False where a segment was
    /// refused.
    ///
    /// # Safety
    /// As [`Descent::run_from`].
    unsafe fn open(
        &mut self,
        arena: &mut TraceScratchArena,
        entity: *mut RcHeader,
        row: *mut u32,
    ) -> bool {
        let index = self.index;
        self.index += 1;
        debug_assert!(
            self.index < self.component,
            "the visit indexes met the component indexes: the live population \
             is half the row word's count field"
        );
        unsafe { shadow::write_live_index(row, index) };
        if refuse_this_vertex() || !arena.push_component(WorklistEntry { entity, row }) {
            return false;
        }

        note_vertex();
        if !arena.push_work(Frame::Finish { row, index }.encode()) {
            return false;
        }

        // The kind is loaded here and passed down rather than read inside the
        // tracer, which is the contract `trace_cells` states.
        let kind = unsafe { cells::entity_kind(entity) };
        let mut refused = false;
        unsafe {
            cells::trace_cells::<PlainCells>(entity, kind, |cell| {
                // The refusal cannot break out of the tracer, so the remaining
                // cells are read and dropped. They cost a load each: the
                // descent is over and every index it wrote dies with the arena.
                if refused {
                    return;
                }

                refused = !arena.push_work(
                    Frame::OutEdge {
                        child: cell.child,
                        from: row,
                    }
                    .encode(),
                );
            })
        };

        !refused
    }

    /// Take one out-edge of a vertex being expanded. False where a segment was
    /// refused.
    ///
    /// An edge whose target has no row of this collection, or a row of another
    /// colour, is followed no further: the descent is over the live subgraph
    /// alone, and a target the scan left unreachable belongs to the teardown
    /// rather than to a component that survives it.
    ///
    /// # Safety
    /// As [`Descent::run_from`], and `child` is a counted child
    /// `cells::trace_cells` yielded.
    unsafe fn take_edge(
        &mut self,
        arena: &mut TraceScratchArena,
        child: *mut RcHeader,
        from: *mut u32,
    ) -> bool {
        let EdgeTarget::Tracked(key) = (unsafe { resolve_edge_target(child) }) else {
            return true;
        };

        let Some(row) = (unsafe { find_initialized_row(key) }) else {
            return true;
        };

        if shadow::color(unsafe { *row }) != Color::Live {
            return true;
        }

        let index = shadow::count(unsafe { *row });
        if index != UNVISITED {
            unsafe { lower(from, index) };
            return true;
        }

        // Beneath the child's own frames, so that it runs when the child's
        // whole descent is over and the child's index is final.
        if !arena.push_work(Frame::Post { child: row, from }.encode()) {
            return false;
        }

        unsafe { self.open(arena, child, row) }
    }

    /// Close the vertex whose expansion is over: where its index survived, it
    /// is the root of a component, and that component is stamped and taken off
    /// the stack.
    ///
    /// A vertex whose index was lowered belongs to a component whose root is
    /// still open, and it waits on the stack for that root.
    ///
    /// # Safety
    /// As [`Descent::run_from`], and `row` is the row of a vertex this descent
    /// opened at `index`.
    unsafe fn close(&mut self, arena: &mut TraceScratchArena, row: *mut u32, index: u32) {
        if shadow::count(unsafe { *row }) != index {
            return;
        }

        // The component is the run standing above this root, every member of
        // which reaches back to it and therefore holds an index no lower than
        // its own. The run is read before it is taken, so the minimum is known
        // before the first stamp is written and no allocation stands between
        // the two.
        let epoch = self.epoch;
        let mut members = 0;
        let mut youngest: Option<u32> = None;
        arena.for_each_component_from_top(|vertex| {
            if unsafe { shadow::count(*vertex.row) } < index {
                return false;
            }

            let stamp = unsafe { read_maturation_stamp(vertex.entity) };
            // A stamp of another epoch is retired by being read against the
            // epoch beside it rather than by a pass that clears it, so its
            // bearer contributes nothing to the minimum.
            let age = if stamp.epoch == epoch { stamp.age } else { 0 };
            youngest = Some(youngest.map_or(age, |carried: u32| carried.min(age)));
            members += 1;
            true
        });

        let stamp = MaturationStamp {
            epoch,
            age: (youngest.unwrap_or(0) + 1).min(MATURATION_AGE_MAX),
        };
        for _ in 0..members {
            let vertex = arena
                .pop_component()
                .expect("the component stack holds the run the reading counted");
            unsafe { write_maturation_stamp(vertex.entity, stamp) };
            unsafe { shadow::write_live_index(vertex.row, self.component) };
        }

        self.component -= 1;
        // The indexes this component spent go back to the next vertex opened:
        // every vertex above the root is assigned now, and a component index
        // is what it carries.
        self.index = index;
        note_component_closed(members);
    }
}

/// Lower the index of the vertex `row` belongs to, which is what an edge into
/// an open vertex reports.
///
/// A vertex of a closed component carries an index counted down from
/// [`COUNT_MAX`] and lowers nothing, which is the whole of the test that keeps
/// a closed component out of an open one.
///
/// # Safety
/// As [`Descent::run_from`], and `row` is a live row of this collection.
unsafe fn lower(row: *mut u32, index: u32) {
    if index < shadow::count(unsafe { *row }) {
        unsafe { shadow::write_live_index(row, index) };
    }
}

/// Leave every live row of the touched list unvisited.
///
/// The count each row carries here is what the trace's subtraction left, and
/// the descent needs a field that says "not yet visited"; the walk that writes
/// it is separate from the walk that visits, because a descent that met an
/// unwritten row would read a residual count as a visit index.
///
/// # Safety
/// As [`stamp_live_components`].
unsafe fn clear_live_indexes(touched: *mut RowArray) {
    let mut array = touched;
    while !array.is_null() {
        let block = unsafe { (*array).block };
        let population = unsafe { (*array).population };
        unsafe {
            row::for_each_live(array, block, population, |index| {
                shadow::write_live_index(row::row_at(array, block, population, index), UNVISITED);
                true
            })
        };

        array = unsafe { (*array).next };
    }
}

/// Empty both stacks, keeping their segments, which a refused descent owes:
/// the sweep that ends the collection reads a worklist that must hold no entry
/// into the rows it is about to unstamp
/// ([`TraceScratchArena`], `sweep_rows`).
fn abandon(arena: &mut TraceScratchArena) {
    while arena.pop_work().is_some() {}
    while arena.pop_component().is_some() {}
}

/// The kind of a frame, in the low two bits of its first word.
///
/// Zero is no kind, so an entry left by anything but [`Frame::encode`] fails
/// the match rather than reading as an edge.
const OUT_EDGE: usize = 1;
const POST: usize = 2;
const FINISH: usize = 3;
const KIND: usize = 0b11;

/// One step the descent owes, in the worklist's own record: two words, the
/// kind in the low two bits of the first.
///
/// The worklist is what carries them — it is empty from the end of the scan
/// and holds the segments that scan drew — and its record is two pointers
/// wide, which is what the encoding fits into. A row and an entity are both
/// aligned above four, so the two bits are free; the visit index a finish
/// carries stands in the second word as a plain number that nothing
/// dereferences.
#[derive(Clone, Copy)]
enum Frame {
    /// One out-edge of the vertex whose row is `from`.
    OutEdge {
        child: *mut RcHeader,
        from: *mut u32,
    },
    /// A child whose descent is over: its index lowers `from`'s.
    Post { child: *mut u32, from: *mut u32 },
    /// The vertex whose expansion is over, and the index its visit gave it.
    Finish { row: *mut u32, index: u32 },
}

impl Frame {
    /// This frame as the worklist's record.
    fn encode(self) -> WorklistEntry {
        match self {
            Frame::OutEdge { child, from } => WorklistEntry {
                entity: tag(child, OUT_EDGE),
                row: from,
            },
            Frame::Post { child, from } => WorklistEntry {
                entity: tag(child, POST),
                row: from,
            },
            Frame::Finish { row, index } => WorklistEntry {
                entity: tag(row, FINISH),
                row: ptr::without_provenance_mut(index as usize),
            },
        }
    }

    /// The frame a record carries.
    fn decode(entry: WorklistEntry) -> Self {
        let pointer = entry.entity.map_addr(|address| address & !KIND);
        match entry.entity.addr() & KIND {
            OUT_EDGE => Frame::OutEdge {
                child: pointer,
                from: entry.row,
            },
            POST => Frame::Post {
                child: pointer.cast::<u32>(),
                from: entry.row,
            },
            FINISH => Frame::Finish {
                row: pointer.cast::<u32>(),
                index: entry.row.addr() as u32,
            },
            _ => unreachable!("a worklist record the descent did not write"),
        }
    }
}

/// `pointer` with `kind` in its low two bits, as the worklist's entity word.
///
/// The provenance is the pointer's own, so the masked value the decode hands
/// back addresses what it addressed.
fn tag<T>(pointer: *mut T, kind: usize) -> *mut RcHeader {
    debug_assert_eq!(
        pointer.addr() & KIND,
        0,
        "a frame's pointer has its low two bits free"
    );
    pointer
        .cast::<RcHeader>()
        .map_addr(|address| address | kind)
}

/// Whether an injected refusal stands at the vertex about to be opened.
/// Always false outside a test build.
///
/// The refusal a case needs is the segment draw inside the descent, and it
/// cannot be had from `memory::block_pool::force_oom`: the arena bumps a
/// segment out of the thread's workspace before it asks the pool for anything,
/// so a graph small enough to write down never reaches the allocator here.
fn refuse_this_vertex() -> bool {
    #[cfg(test)]
    {
        let at = REFUSE_AT_VERTEX.with(std::cell::Cell::get);
        return at == Some(COUNTS.with(std::cell::Cell::get).vertices + 1);
    }

    #[cfg(not(test))]
    false
}

/// Refuse the descent its component-stack segment when it opens its `nth`
/// vertex, counting from the next reading of the counters, until the guard is
/// dropped.
#[cfg(test)]
pub(crate) fn refuse_the_descent_at(nth: usize) -> DescentRefusal {
    REFUSE_AT_VERTEX.with(|at| at.set(Some(nth)));
    DescentRefusal
}

/// The injection [`refuse_the_descent_at`] opened, which takes it down again.
#[cfg(test)]
pub(crate) struct DescentRefusal;

#[cfg(test)]
impl Drop for DescentRefusal {
    fn drop(&mut self) {
        REFUSE_AT_VERTEX.with(|at| at.set(None));
    }
}

#[cfg(test)]
thread_local! {
    /// The vertex an injected refusal stands at, or `None` where the descent
    /// runs as it does in production.
    static REFUSE_AT_VERTEX: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Count one vertex the descent opened. Empty outside a test build.
fn note_vertex() {
    #[cfg(test)]
    COUNTS.with(|counts| {
        let mut reading = counts.get();
        reading.vertices += 1;
        reading.standing += 1;
        reading.high_water = reading.high_water.max(reading.standing);
        counts.set(reading);
    });
}

/// Count one component the descent closed, over `members` vertices. Empty
/// outside a test build.
fn note_component_closed(members: usize) {
    #[cfg(test)]
    COUNTS.with(|counts| {
        let mut reading = counts.get();
        reading.components += 1;
        reading.standing -= members;
        counts.set(reading);
    });
    let _ = members;
}

/// Count one descent a refusal ended. Empty outside a test build.
fn note_refusal() {
    #[cfg(test)]
    COUNTS.with(|counts| {
        let mut reading = counts.get();
        reading.refusals += 1;
        reading.standing = 0;
        counts.set(reading);
    });
}

/// What the descents of this thread have done since the last reading.
///
/// Per thread because a collection is, and taken rather than read so that a
/// case names the collections it drove rather than every collection the
/// harness ran before it.
#[cfg(test)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct DescentCounts {
    /// Vertices opened, which is the live population the descents walked.
    pub(crate) vertices: usize,
    /// Components closed and therefore stamped.
    pub(crate) components: usize,
    /// The deepest the component stack stood.
    pub(crate) high_water: usize,
    /// Descents a refused segment or an unplaceable row ended.
    pub(crate) refusals: usize,
    /// Vertices standing on the component stack, which is zero between
    /// collections and is not part of a reading.
    standing: usize,
}

#[cfg(test)]
thread_local! {
    static COUNTS: std::cell::Cell<DescentCounts> =
        const { std::cell::Cell::new(DescentCounts {
            vertices: 0,
            components: 0,
            high_water: 0,
            refusals: 0,
            standing: 0,
        }) };
}

/// What the descents have done since this last answered, which it leaves at
/// zero.
#[cfg(test)]
pub(crate) fn take_descent_counts() -> DescentCounts {
    COUNTS.with(|counts| counts.replace(DescentCounts::default()))
}

#[cfg(test)]
mod tests;
