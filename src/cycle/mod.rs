//! `rc-cycle`: cycle collection from a mutator-fed candidate set.
//!
//! The design is `rfc/model/gc/rc-cycle.md` and is normative; this module
//! is its implementation as `PLAN.md`'s S34 through S40 build it. Nothing
//! here collects yet: the crate retains a garbage ring and reclaims
//! acyclic garbage by counting, and `gc::ll_gc_collect_cycles` reports
//! zero until S36.7 wires a collection in. **Candidates are gathered
//! all the same** — [`queue`] takes one from every non-final decrement,
//! so that when the trace arrives it has a root set rather than a heap.
//!
//! # What lives here, and what does not
//!
//! The collector's own state: shadow rows, the arena they come from, the
//! trace's mark and scan, the exact test that validates a component and the
//! guards and weak-cell nulling its answer stands for. Two
//! things it deliberately does not hold. The enumeration of an entity's
//! counted children is `cells`, which knows entity kinds and no blocks;
//! the enumeration of a block's slots is `memory::heap`, which knows
//! blocks and no kinds. This module is the one place that knows both, and
//! it knows them only through their two interfaces.
//!
//! The candidate queue is the exception that proves the split: it knows
//! neither, holding entity pointers it never dereferences and pool
//! blocks it never carves.
//!
//! # What each module owns, and for how long
//!
//! Two lifetimes only. [`queue`] holds per-thread state for one thread's whole
//! life, given back at `ll_thread_exit` — the base block, and the collection
//! workspace an [`arena`] borrows. Everything else is per collection:
//! [`arena`] holds the blocks a single trace bumps into past that workspace
//! and the worklist [`stack`] draws its segments from that bump,
//! [`deferred_slot_reuse`] opens its control line over the workspace's fixed
//! region and holds every withheld return in the dying entity itself, and the rows [`shadow`], [`mark`] and
//! [`scan`] read die with it. [`row`], [`shadow`] and the test-only `testing`
//! own no memory at all — they are arithmetic over memory somebody else holds,
//! and [`validation`] reads the heap rather than a row. [`finalization`] holds
//! less than either: one counter on the frame that drives the teardown, the
//! writes it makes standing in the members' own headers until the counted
//! release of `PLAN.md` S36.5.
//!
//! **A collection's memory is refusable, and the refusal ends the collection
//! rather than the process.** A refused block leaves the heap byte-identical,
//! because the trace writes into rows and never into an entity. The one
//! refusal a collection can meet before it starts is the workspace, drawn at a
//! thread's first collection and held until that thread exits: a thread that
//! has collected once opens every later window without asking the memory
//! manager, its rows, worklist and withheld returns all standing in memory it
//! already holds. What stands outside the claim is below, with the per-thread
//! aborts, because it has the same shape as they do.
//!
//! The per-thread half is where a refusal can end something. The queue's base
//! block is drawn twice: at thread init, where a refusal is a thread that
//! never starts (`rfc/dev/DECISIONS.md`, "the baseline overflow segment is
//! allocator-issued", which is that block), and at the first registration of a
//! thread the runtime never registered, where the same refusal aborts because
//! there is no caller left to report it to. Past both stands the overflow
//! buffer's own bound, which aborts when it fills. The window over withheld
//! returns draws nothing at all: a death it withholds is held in the dying
//! entity's own memory, and one no row of the collection names is returned at
//! once ([`deferred_slot_reuse`], `classify`). Thread
//! exit inside an open window aborts, `ll_thread_exit` being
//! `extern "C"` and having no caller to refuse to.
//!
//! The ordering the whole module rests on is one sentence: **the rows die at
//! the trace token's release, and everything that reads a row happens before
//! it** (`rfc/model/gc/rc-cycle.md`, "Concurrency"). The scan's sweep is
//! therefore the last row read of a collection, and validation, teardown and
//! the slot returns all run after it, untokened — which is why
//! [`validation`] re-reads the heap instead.

// `ActiveTrace` owns the `TraceScratchArena`, fixing sweep-before-return even
// before the production collection opens one in S36.7.
pub(crate) mod arena;
// The validation over a component the scan proposed, reached from
// [`finalization`], which is where its answer is acted on and which no
// production path runs until S36.7.
pub(crate) mod validation;
// The guard references and the weak-reference invalidation a confirmed
// component takes before the first destructor.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the driver that opens a finalization is `PLAN.md` S36.7's"
    )
)]
pub(crate) mod finalization;
// The first phase of a trace, reached from [`trace`] rather than from a
// collection: the collection that drives one is S36.7's.
pub(crate) mod mark;
// The list a pressure collection takes out of its rows before the blocks go
// back, and the region of the workspace it stands in.
pub(crate) mod members;
// Physical slot return waits while a trace can still address the slot's shadow
// row. The production trace that opens the window arrives in S36.7; S36.2
// builds the window and the return-path half first.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the production trace that opens this window is `PLAN.md` S36.7's"
    )
)]
pub(crate) mod deferred_slot_reuse;
// The traced-slot and traced-group reading a measurement takes off the
// touched list after a trace. Test builds only.
#[cfg(test)]
pub(crate) mod density;
pub(crate) mod queue;
// The record chain the trace's worklist is built on.
pub(crate) mod records;
pub(crate) mod row;
// The second phase, and the proposal a collection reads: reached from
// [`trace`], which S36.7 drives.
pub(crate) mod scan;
pub(crate) mod shadow;
// The worklist both phases of a trace share, held by the arena whose memory
// it stands on.
pub(crate) mod stack;
// The two phases of one trace, in the order the rows require. Dead until a
// collection drives them, which is S36.7.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the collection that drives both phases is S36.7")
)]
pub(crate) mod trace;
// The row readers the mark's tests and the scan's tests share. Test
// builds only.
#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
