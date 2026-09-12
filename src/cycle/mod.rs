//! `rc-cycle`: cycle collection from a mutator-fed candidate set.
//!
//! The design is `rfc/model/gc/rc-cycle.md` and is normative; this module
//! is its implementation as `PLAN.md`'s S36 through S40 build it. Nothing
//! [`collect`] is the order the rest of them run in, and the two ABI entries
//! of `crate::gc` are its callers. **Candidates are gathered by the mutator**
//! — [`queue`] takes one from every non-final decrement — so that when a trace
//! arrives it has a root set rather than a heap.
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
//! neither layout, holding entity pointers and pool blocks it never carves.
//! Owner retirement reads the entity's count/free mark through `refcount`
//! and returns its slot through `memory::stdapi::ll_free`.
//!
//! # What each module owns, and for how long
//!
//! Three lifetimes. [`epoch`] holds one word for the process, a count of
//! closed commits that owns no memory and is never given back. [`queue`] holds
//! per-thread state for one thread's whole
//! life, given back at `ll_thread_exit` — the base block, and the collection
//! workspace an [`arena`] borrows. Everything else is per collection:
//! [`arena`] holds the blocks a single trace bumps into past that workspace
//! and the worklist [`stack`] draws its segments from that bump,
//! [`deferred_slot_reuse`] opens its control line over the workspace's fixed
//! region and holds every withheld return in the dying entity itself, and the rows [`shadow`], [`mark`] and
//! [`scan`] read die with it. [`row`], [`shadow`] and the test-only `testing`
//! own no memory at all — they are arithmetic over memory somebody else holds,
//! and [`validation`] reads the heap rather than a row. [`finalization`] holds
//! less than either: four counters, three on the frame that drives the
//! teardown and one on the answer a component's reading gives it, the writes
//! they make standing in the members' own headers until a counted release
//! takes them off — its own, over a component it reads as externally
//! referenced, or the teardown's ([`reclamation`], which holds the queue of
//! children its sever displaced out of a component — segments of the arena's
//! bump, emptied at the end of every component).
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
//! once ([`deferred_slot_reuse`], `classify`). Thread exit waits for a
//! foreign holder of the token and then collects what the thread left
//! registered ([`collect::collect_before_exit`]); an exit a destructor asks
//! for during a collection, a teardown or a reset waits for the thread's top
//! (`memory::heap::thread_exit_pending`). An exit with a window open on the
//! calling frame's own stack aborts, `ll_thread_exit` being `extern "C"` and
//! having no caller to refuse to, and no path from user code reaches that.
//!
//! The ordering the whole module rests on is one sentence: **the right to
//! trace ends at the token's release, and the rows die at the window's
//! close** (`rfc/model/gc/rc-cycle.md`, "Concurrency"). The scan's end off
//! the poll and the harvest sweep under pressure are the last things the token
//! covers; validation, teardown and the slot returns run after its release,
//! untokened, and [`validation`] re-reads the heap rather than a row. A collection off the poll keeps its rows open through the teardown
//! ([`membership`]), a collection under pressure gives them back before it
//! ([`members`]) — and on neither path does a row outlive its window.

// `ActiveTrace` owns the `TraceScratchArena`, which the collection opens.
pub(crate) mod arena;
// The order the pieces below run in, and the two paths a collection takes
// through them. The ABI entries of `crate::gc` are its callers.
pub(crate) mod collect;
// The validation over a component the scan proposed, reached from
// [`finalization`], which is where its answer is acted on.
pub(crate) mod validation;
// The guard references and the weak-reference invalidation a confirmed
// component takes, the destructors that run behind both, and the second
// reading of each component the guard is subtracted in.
pub(crate) mod finalization;
// The count of closed commits and the epoch a maturation stamp carries. Read
// by the commit that writes a stamp and by the descent that reads one.
pub(crate) mod epoch;
// The first phase of a trace, reached from [`trace`] rather than from a
// collection.
pub(crate) mod mark;
// The stamp a commit writes into the live components its trace read, which is
// what a later trace's descent stops at.
pub(crate) mod maturation;
// The list a pressure collection takes out of its rows before the blocks go
// back, and the region of the workspace it stands in.
pub(crate) mod members;
// The two forms a commit's membership takes — the harvested list, and the rows
// a collection off the poll keeps — behind the three questions every reader of
// one asks.
pub(crate) mod membership;
// Physical slot return waits while a trace can still address the slot's shadow
// row. The window is what [`collect`] opens a collection inside.
pub(crate) mod deferred_slot_reuse;
// The traced-slot and traced-group reading a measurement takes off the
// touched list after a trace. Test builds only.
#[cfg(test)]
pub(crate) mod density;
// What one collection cost, read at the scan's end and at the close and
// counted across it. Test builds only.
#[cfg(test)]
pub(crate) mod census;
// The loads S40.3 reads, built once for the census and for the driver in
// `benches/` that links the ordinary library under `bench-loads`.
#[cfg(any(test, feature = "bench-loads"))]
pub mod loads;
pub(crate) mod queue;
// The queue a teardown holds the children its sever displaced in, held by the
// arena whose bump its segments come from.
pub(crate) mod drops;
// The teardown of a confirmed component: the sever, the frees, and the drops
// of the children the sever displaced. `reclaim` is reached from [`collect`]
// and from nothing else; the queue type beside it is the arena's.
pub(crate) mod reclamation;
// The record chain the trace's worklist and the teardown's deferred drops are
// built on.
pub(crate) mod records;
pub(crate) mod row;
// The second phase, and the proposal a collection reads: reached from
// [`trace`], which [`collect`] drives.
pub(crate) mod scan;
pub(crate) mod shadow;
// The worklist both phases of a trace share, held by the arena whose memory
// it stands on.
pub(crate) mod stack;
// The per-thread trace token: taken by [`collect`] around its trace,
// waited for by an owner whose graph a collector is tracing.
pub(crate) mod token;
// The two phases of one trace, in the order the rows require.
pub(crate) mod trace;
// The row readers and the ring fixtures the collector's tests share. Test
// builds only.
#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
