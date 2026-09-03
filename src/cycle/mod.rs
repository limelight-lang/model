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
//! trace's mark and scan, and the exact test that validates a component. Two
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
//! and the worklist [`stack`] lays over a fixed region of it,
//! [`deferred_slot_reuse`] holds the blocks its
//! withheld returns are recorded in, and the rows [`shadow`], [`mark`] and
//! [`scan`] read die with it. [`row`], [`shadow`] and the test-only `testing`
//! own no memory at all — they are arithmetic over memory somebody else holds,
//! and [`validation`] reads the heap rather than a row.
//!
//! **A collection's memory is refusable, and the refusal ends the collection
//! rather than the process.** A refused block leaves the heap byte-identical,
//! because the trace writes into rows and never into an entity. The
//! withheld-return chain is inside that claim by where it draws: its first
//! block comes at the window's open, before a slot is in hand, so a refusal
//! there is a collection that does not start. What stands outside the claim is
//! below, with the per-thread aborts, because it has the same shape as they
//! do.
//!
//! The per-thread half is where a refusal can end something. The queue's base
//! block is drawn twice: at thread init, where a refusal is a thread that
//! never starts (`rfc/dev/DECISIONS.md`, "the baseline overflow segment is
//! allocator-issued", which is that block), and at the first registration of a
//! thread the runtime never registered, where the same refusal aborts because
//! there is no caller left to report it to. Past both stands the overflow
//! buffer's own bound, which aborts when it fills. Past the trace's own first
//! chain block stands [`deferred_slot_reuse`]'s growth, which aborts for the
//! reason the overflow buffer does: it is holding a slot it may neither return
//! nor drop, and `ll_free` has no frame to report a refusal through. Thread
//! exit inside an open window aborts there too, `ll_thread_exit` being
//! `extern "C"` and having no caller to refuse to.
//!
//! The ordering the whole module rests on is one sentence: **the rows die at
//! the trace token's release, and everything that reads a row happens before
//! it** (`rfc/model/gc/rc-cycle.md`, "Concurrency"). The scan's sweep is
//! therefore the last row read of a collection, and validation, teardown and
//! the slot returns all run after it, untokened — which is why
//! [`validation`] re-reads the heap instead.

// `ActiveTrace` owns the `TraceScratchArena`, fixing reset-before-replay even
// before the production collection opens one in S36.7.
pub(crate) mod arena;
// The validation over a component the scan proposed, and dead until the
// teardown that opens with it.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the teardown that opens with the validation test is `PLAN.md` S36.3's"
    )
)]
pub(crate) mod validation;
// Nothing marks in the production build either: the collection that
// runs a trace is S36.7's, and this module is what it will run.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the collection that marks is S36.7")
)]
pub(crate) mod mark;
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
pub(crate) mod queue;
// The record chain the collection's lists are built on.
pub(crate) mod records;
pub(crate) mod row;
// The verdict over the rows the mark counted, and dead until a
// collection runs one.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the collection that scans is S36.7")
)]
pub(crate) mod scan;
pub(crate) mod shadow;
// The worklist both phases of a trace share, held by the arena whose memory
// it stands on.
pub(crate) mod stack;
// The row readers the mark's tests and the scan's tests share. Test
// builds only.
#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
