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

// Nothing constructs a `ShadowArena` in the production build: the
// collection that takes one is S35.1's mark, which is also the first
// caller of the rows it reserves. `cfg(not(test))` because the tests
// inside it do construct one, which would leave an unconditional
// expectation unfulfilled.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the collection that takes an arena is S35.1")
)]
pub(crate) mod arena;
pub(crate) mod queue;
pub(crate) mod row;
pub(crate) mod shadow;
