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
//! trace's mark and scan, and the exact test that judges a component. Two
//! things it deliberately does not hold. The enumeration of an entity's
//! counted children is `cells`, which knows entity kinds and no blocks;
//! the enumeration of a block's slots is `memory::heap`, which knows
//! blocks and no kinds. This module is the one place that knows both, and
//! it knows them only through their two interfaces.
//!
//! The enrolment queue is the exception that proves the split: it knows
//! neither, holding entity pointers it never dereferences and pool
//! blocks it never carves.

// `ActiveTrace` owns the `TraceScratchArena`, fixing reset-before-replay even
// before the production collection opens one in S36.7.
pub(crate) mod arena;
// The judgement over a component the scan condemned, and dead until the
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
pub(crate) mod row;
// The verdict over the rows the mark counted, and dead until a
// collection runs one.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the collection that scans is S36.7")
)]
pub(crate) mod scan;
pub(crate) mod shadow;
// The worklist both phases of a trace share, and dead for the same
// reason they are.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the collection that traces is S36.7")
)]
pub(crate) mod stack;
// The row readers the mark's tests and the scan's tests share. Test
// builds only.
#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
