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

// Nothing constructs a `ShadowArena` in the production build: the mark
// takes one and has no production caller either, the collection that
// runs it being S36.7's. `cfg(not(test))` because the tests inside it do
// construct one, which would leave an unconditional expectation
// unfulfilled.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the collection that takes an arena is S36.7")
)]
pub(crate) mod arena;
// Nothing marks in the production build either: the collection that
// runs a trace is S36.7's, and this module is what it will run.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the collection that marks is S36.7")
)]
pub(crate) mod mark;
pub(crate) mod queue;
pub(crate) mod row;
// Nothing scans in the production build for the same reason nothing
// marks: the collection that runs a trace is S36.7's.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the collection that scans is S36.7")
)]
pub(crate) mod scan;
pub(crate) mod shadow;
// The row readers the mark's tests and the scan's tests share. Test
// builds only.
#[cfg(test)]
pub(crate) mod testing;
