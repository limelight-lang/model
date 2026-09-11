//! The mark: trial deletion from one candidate root, over the shadow
//! rows and never over the heap.
//!
//! Per edge the trace follows, it subtracts one from the child's working
//! count; per entity it meets a row once, and the count that row starts from
//! is the entity's own refcount. A row that still reads above zero when the
//! scan arrives is therefore held from outside the traced component
//! (`rfc/model/gc/rc-cycle.md`, "Candidate registration and trial
//! deletion"). Two edges are not followed and take no subtraction: one out
//! of the GC heap, and one into the mature live core (below).
//!
//! **No entity is written.** Mark and scan touch shadow rows, the met
//! bitmap and this module's worklist, all three of them in the
//! collection's arena, so a collection that gives up halfway leaves the
//! heap byte-identical and owes nothing but `TraceScratchArena::reset`
//! (`crate::cycle::arena`).
//!
//! # The descent turns on the meeting
//!
//! An edge into an entity this collection has already expanded takes the
//! decrement and stops there. That and the prune below are what terminate
//! the trace, and a ring re-entered at every in-edge would not terminate at
//! all. The bit saying which reach this was is `RowLookup::first_visit`,
//! carried out of the meeting because the meeting is what destroys it
//! (`crate::cycle::arena::TraceScratchArena::ensure_row`).
//!
//! The descent carries an explicit worklist rather than the machine
//! stack, and why is `crate::cycle::stack`.
//!
//! # The mature live core is not descended into
//!
//! An edge target carrying this collection's epoch at an age that has reached
//! [`TRAVERSAL_AGE_THRESHOLD`] is read as an opaque live external: the edge is
//! neither subtracted nor expanded, exactly as an edge out of the GC heap is.
//! That is the one mechanism in this design that bounds the closure — the
//! subgraph a median candidate root reaches was the whole object population on
//! the corpus of 2026-08-25 (`rfc/model/gc/cycle/questions.md`, Y9) — and what
//! it costs is recall: a component that lost its last external reference while
//! it was mature reads live at the trace that meets it, its root is deferred on
//! that reading (`crate::cycle::deferred_slot_reuse`), and the turnover is what
//! offers it again (`crate::cycle::queue::reoffer_deferred_if_epoch_moved`).
//!
//! **A target a queue entry names is never pruned, whatever its stamp**
//! (`rfc/model/gc/rc-cycle.md`, "Candidate registration and trial
//! deletion"). What the exemption saves is a ring whose members are all
//! registered: its garbage is found by the trace that meets it, every edge
//! between members being an edge into a candidate. A ring one of whose mature
//! members never observed a non-final decrement is not saved by it — the edge
//! into that member is pruned, the ring reads live and its root waits for the
//! turnover. That shape is ordinary rather than rare, since a live component
//! is stamped whole and a member that was only ever retained is a normal
//! population. The rule is about the target of an edge and not about the
//! entity the trace started from, which is why it lives in [`visit_child`] and
//! not in `resolve_edge_target`, whose second caller is the root's own
//! meeting.
//!
//! **The prune cuts the subgraph the commit ages as well.** A pruned target
//! has no row, so `crate::cycle::maturation`'s descent closes its neighbours'
//! components without it and the target keeps its own stamp until the
//! turnover; and a row that reads live only because an in-edge from a mature
//! target was never subtracted ages on that reading and can reach the
//! threshold itself. Both are recall paid inside one epoch and neither is a
//! free (`rfc/model/gc/rc-cycle.md`, "What a commit stamps").
//!
//! **The epoch is one reading per call.** It is a division over a
//! process-global counter (`crate::cycle::epoch::current`), so a reading per
//! edge would put that on every edge of the trace; two roots of one collection
//! that read the counter across a turnover prune less than one reading would,
//! never more.
//!
//! **The trace writes no stamp.** A stamp of another epoch is retired by being
//! read against the epoch beside it, never by being cleared in place, so
//! everything this module does to a mature entity is one byte-wide load
//! (`crate::refcount::read_maturation_stamp`).
//!
//! # What it owns, and what a refusal costs
//!
//! Nothing outlives the call: the rows, the bitmap and the worklist are the
//! caller's arena, and the mark holds a `&mut` to it for one trace. Two
//! things it can ask that arena for — a row array, through
//! [`TraceScratchArena::ensure_row`](crate::cycle::arena::TraceScratchArena::ensure_row),
//! and a worklist segment, through
//! [`TraceScratchArena::push_work`](crate::cycle::arena::TraceScratchArena::push_work).
//! Either refused
//! answers [`MarkResult::AllocationFailed`], which abandons the trace where it
//! stands. Abandoning is free precisely because no entity was written.
//!
//! One ordering matters and it is this module's: the row is ensured before
//! the count is subtracted, so an entity reached for the first time starts
//! from its refcount rather than from a subtraction against a row that does
//! not exist yet.
//!
//! # A root at count zero is expanded by nothing
//!
//! The candidate queue holds entries for entities that have since died: the
//! entry keeps the slot out of the allocator's hands, and nothing retires it
//! at the death. What the entry does not keep is the entity's contents —
//! teardown released every counted child and left the cells naming them
//! (`crate::object::ll_default_dispose`, phase 2) — so those cells are
//! addresses of slots the allocator may have handed to somebody else.
//!
//! So the count is read before the cells, and a root at zero contributes
//! nothing. The read is meaningful because the mutator does not free an
//! entity the root queue names (`crate::memory::stdapi::ll_free`, the
//! candidate arm): a count above zero cannot fall to a torn-down entity under
//! the call that read it. The rule lives here rather than in the caller that
//! drains the queue, so that a second caller of [`mark`] inherits it.

use crate::cells::{self, PlainCells};
use crate::cycle::arena::{RowLookup, TraceScratchArena};
use crate::cycle::epoch;
use crate::cycle::row::{EdgeTarget, resolve_edge_target};
use crate::cycle::shadow;
use crate::cycle::stack::WorklistEntry;
use crate::refcount::{
    MATURATION_AGE_MAX, RcHeader, header_refcount, is_registered_candidate, mutator_flags,
    read_maturation_stamp,
};

/// The age at which an edge target stops being descended into: `k`, the
/// traversal age threshold of `rfc/model/gc/rc-cycle.md`, "Candidate
/// registration and trial deletion".
///
/// 3, provisional after the only published value of the design this rule comes
/// from (`rfc/model/gc/cycle/questions.md`, Y9: promote age 3). What a real
/// workload wants is the pruned-edge share at 1, 2 and 3, which `PLAN.md`
/// S40.1 measures and this constant then takes or keeps.
///
/// A threshold above [`MATURATION_AGE_MAX`] would prune nothing, the age
/// saturating there, so the assertion below is the whole of what the two
/// numbers owe each other.
pub(crate) const TRAVERSAL_AGE_THRESHOLD: u32 = 3;

const _: () = assert!(
    TRAVERSAL_AGE_THRESHOLD <= MATURATION_AGE_MAX,
    "a threshold above the age field's bound prunes no edge at all"
);

/// What a mark from one root answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MarkResult {
    /// The closure is exhausted: every entity the root reaches through the
    /// GC heap short of a mature target has been met, and every edge the trace
    /// followed between two met entities has been subtracted from the row it
    /// points at.
    Complete,
    /// Both allocation paths refused, so the collection aborts. The heap is
    /// byte-identical and the arena's reset is the whole of the debt.
    AllocationFailed,
}

/// Trial-delete the component reachable from `root`, leaving the verdict
/// to the scan.
///
/// Every entity the descent reaches is met once, its row initialised from
/// its refcount; every edge the trace follows between two met entities is
/// subtracted from the target's row. **Two populations are reached and not
/// met**, and an edge into either is counted as an external reference and
/// followed no further: what stands outside the GC heap, which keeps a ring
/// through the arena — broken by the arena's own reset — out of the
/// collector's reach (`crate::cycle::row`); and a mature edge target no queue
/// entry names, for the epoch it matured in (module doc). Both raise the rows
/// this trace leaves rather than lowering them, so what either costs is
/// recall.
///
/// `arena` belongs to the collection rather than to the root, and carries the
/// worklist with it: a second root inside the first one's closure meets rows
/// that already say met, expands nothing twice, and reuses the segments the
/// first root's depth drew.
///
/// **Nothing is written into any entity**, so [`MarkResult::AllocationFailed`]
/// leaves the heap byte-identical and the caller's whole duty is
/// `TraceScratchArena::reset`.
///
/// **A root at count zero was torn down, and is expanded by nothing** (module
/// doc), which is not a refusal: the answer is [`MarkResult::Complete`] and
/// the collection carries on with its other roots.
///
/// # Safety
/// `root` is an entity header of this thread's heap whose slot is still its
/// own — a candidate the queue names, live or dead — and the trace runs where
/// `cells::trace_cells` may read an entity's cells plainly: on the owning
/// thread, with no mutator running beside it.
pub(crate) unsafe fn mark(arena: &mut TraceScratchArena, root: *mut RcHeader) -> MarkResult {
    // Read here rather than taken from the caller: the prune is this module's
    // rule, so a second caller of `mark` inherits it with no argument to
    // forget, and the cost of the reading is one per root instead of one per
    // edge (module doc).
    let epoch = epoch::current();
    if !unsafe { schedule_root_if_unvisited(arena, root) } {
        return MarkResult::AllocationFailed;
    }

    while let Some(entry) = arena.pop_work() {
        // The row the entry carries is the scan's to read; the mark's
        // expansion needs the entity alone, and the two phases keep one
        // entry shape (`crate::cycle::stack::WorklistEntry`).
        let entity = entry.entity;
        // The kind is loaded here and passed down rather than read
        // inside the tracer, which is the contract `trace_cells` states:
        // a collector holds the kind from its own reading of the header
        // and does not go back to a word the mutator may be writing.
        let kind = unsafe { cells::entity_kind(entity) };
        let mut refused = false;
        unsafe {
            cells::trace_cells::<PlainCells>(entity, kind, |cell| {
                // The refusal cannot break out of the tracer, so the
                // remaining cells of this entity are read and dropped.
                // They cost a load each and nothing else: the collection
                // is over, and every row it wrote dies with the arena.
                if refused {
                    return;
                }

                refused = !visit_child(arena, cell.child, epoch);
            })
        };

        if refused {
            return MarkResult::AllocationFailed;
        }
    }

    MarkResult::Complete
}

/// Meet the root's own row and queue it for expansion. False when both
/// allocation paths refused.
///
/// **The root takes no subtraction.** The row starts at the entity's
/// refcount and the trace subtracts the edges it finds; the queue entry
/// that named this root is not one of them, and subtracting for it would
/// read a component held by a single external reference as unreachable.
///
/// # Safety
/// As [`mark`].
unsafe fn schedule_root_if_unvisited(arena: &mut TraceScratchArena, root: *mut RcHeader) -> bool {
    let refcount = unsafe { header_refcount(root) };
    if refcount == 0 {
        // An entity the queue is still holding after its teardown. The entry
        // keeps the slot out of the allocator's hands, so the address is this
        // entity's; the teardown released every counted child and left the
        // cells naming them (`crate::object::ll_default_dispose`, phase 2), so
        // expanding it would subtract edges the heap no longer holds — from
        // the rows of whatever occupies those slots now.
        //
        // The count may be read here because the mutator does not free an
        // entity the root queue names (`crate::memory::stdapi::ll_free`, the
        // candidate arm), so a count above zero cannot fall to a torn-down
        // entity under this call.
        return true;
    }

    let EdgeTarget::Tracked(row) = (unsafe { resolve_edge_target(root) }) else {
        // The candidate gate admits none: an entity outside the GC heap
        // never reaches the queue (`rfc/model/gc/rc-cycle.md`,
        // "Zero-count entities pending slot reuse"). Answered rather than asserted, because the
        // collection's cost of being wrong here is one root that traces
        // nothing.
        return true;
    };

    match unsafe { arena.ensure_row(row, refcount) } {
        RowLookup::AllocationFailed => false,
        RowLookup::Untracked => true,
        RowLookup::Ready { row, first_visit } => {
            if first_visit {
                arena.push_work(WorklistEntry { entity: root, row })
            } else {
                true
            }
        }
    }
}

/// Take one out-edge of an entity being expanded: subtract it from the
/// child's working count, and queue the child when this collection has
/// not seen it before. False when both allocation paths refused.
///
/// An edge the row dispatch cannot place — a child outside the GC heap, or a
/// retained block whose survivor list does not name it — is counted as an
/// external live reference and followed no further, which keeps the referent
/// alive rather than reading it as unreachable on a row the trace guessed.
///
/// **A mature target takes the same answer**, and the test for it stands above
/// the block dispatch: `epoch` is the collection's reading of the epoch, and a
/// child that reads mature against it is left to the entity's own count
/// without a dispatch of any kind (module doc, "The mature live core is not
/// descended into").
///
/// # Safety
/// As [`mark`], and `child` is a counted child `cells::trace_cells`
/// yielded, hence a live entity header.
unsafe fn visit_child(arena: &mut TraceScratchArena, child: *mut RcHeader, epoch: u32) -> bool {
    if unsafe { stands_as_an_opaque_live_external(child, epoch) } {
        note_edge_pruned();
        return true;
    }

    let EdgeTarget::Tracked(row) = (unsafe { resolve_edge_target(child) }) else {
        return true;
    };

    // A counted edge is a reference, so the entity it names holds at least
    // that one. A zero here is an expansion of a torn-down entity's residual
    // cells, which [`schedule_root_if_unvisited`] is what keeps out of the
    // descent.
    debug_assert_ne!(
        unsafe { header_refcount(child) },
        0,
        "a counted child at count zero: the trace expanded a corpse"
    );

    match unsafe { arena.ensure_row(row, header_refcount(child)) } {
        RowLookup::AllocationFailed => false,
        RowLookup::Untracked => true,
        RowLookup::Ready { row, first_visit } => {
            unsafe { shadow::subtract(row, 1) };
            if first_visit {
                arena.push_work(WorklistEntry { entity: child, row })
            } else {
                true
            }
        }
    }
}

/// Whether the collections of `epoch` have read this edge target's component
/// as held from outside often enough for the descent to stop at it.
///
/// Two fields of one byte decide the first half — an age that has reached
/// [`TRAVERSAL_AGE_THRESHOLD`] under this collection's own epoch, a stamp of
/// any other epoch reading as no age at all — and the mutator's flags decide
/// the second: a target a queue entry names, in whichever lane that entry
/// stands, is never pruned (module doc). The flags are read only where the stamp already
/// says mature, which is why the two loads are in this order and not the
/// reverse.
///
/// # Safety
/// As [`visit_child`]: `child` is a live published entity header. The byte is
/// the owning thread's to write and this is that thread, so the stamp read
/// here is whole (`crate::refcount::read_maturation_stamp`).
#[inline]
unsafe fn stands_as_an_opaque_live_external(child: *const RcHeader, epoch: u32) -> bool {
    let stamp = unsafe { read_maturation_stamp(child) };
    stamp.age >= TRAVERSAL_AGE_THRESHOLD
        && stamp.epoch == epoch
        && !is_registered_candidate(unsafe { mutator_flags(child) })
}

/// Add one to [`EDGES_PRUNED`], and nothing at all without `cfg(test)`.
///
/// The counter is the trace's own and not the density instrument's: what it
/// reports is an event no final row state records, a target the mark did not
/// meet being indistinguishable from one no edge named
/// (`PLAN.md` S40.1, whose pruned-edge share this is the built form of).
#[inline]
fn note_edge_pruned() {
    #[cfg(test)]
    EDGES_PRUNED.with(|count| count.set(count.get() + 1));
}

// Edges the marks of this thread have pruned (tests only). Per thread,
// because a collection is.
#[cfg(test)]
thread_local! {
    static EDGES_PRUNED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Edges the marks of this thread pruned since this last answered, which it
/// leaves at zero.
///
/// Reading and clearing together, for the reason
/// [`crate::cycle::row::take_edge_dispatches`] does it: every caller prices
/// the collections it drove, and what stands before them is another case's.
#[cfg(test)]
pub(crate) fn take_edges_pruned() -> usize {
    EDGES_PRUNED.with(|count| count.replace(0))
}

#[cfg(test)]
mod tests;
