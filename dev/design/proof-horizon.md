# Proof Horizon: local protection paid where proof ends

**Status:** design sketch, not implemented; a Critic round is owed
before any step exists.
**Author of the algorithm:** Edmond, 2026-08-18. Successor to the
stack-exit epoch model (`docs/history/stack-exit-epoch-gc-2026-08-18.md`,
superseded the day it was recorded); shaped by that model's five-axis
review (`dev/STACK_EXIT_EPOCH_GC_REVIEW.md`) and by the two standing
refusals in `dev/DECISIONS.md`, 2026-08-17 and 2026-08-18.

## The algorithm in two sentences

Local references pay nothing wherever the compiler's proofs hold: a
local is an anchored borrow (`live(anchor) ∧ stable_path`), and a call
with an effect summary proving it destroys nothing on the anchor path
keeps every borrow free across it. Payment happens only at a **proof
horizon** — a point the compiler cannot prove safe — and it is
wholesale: before crossing, every live local the proofs no longer
cover is published as protected.

The cost model is the idea: protection is priced per point of doubt,
not per read, per copy or per store. Lean-style summaries push the
horizon outward; what code cannot be summarized pays for all its live
locals at its boundary, once.

## The two forms, and which one this document means

**Form A — over maintained heap RC. This is the design.** Heap counts
stay exactly as today: eager death at zero, `__destruct` timing, COW's
`refcount == 1`, the arena's counted promotion, rc-walk's whole
protocol. What changes: local (stack) references are never counted.
The retain/release pair disappears from local traffic; in its place,
at each horizon, live unproven locals enter the thread's protection
set. The collector's candidate test gains one arm — a protected entity
is acquitted — and the death branch tests protection once, on the cold
path, before teardown of a zero-count entity.

**Form B — without heap RC** is the superseded model's road. The
five-axis review priced it: the architecture inventory dies with the
count, deterministic destruction needs the count back, and the walk's
own write barrier *is* the count. Form B is not pursued; its record is
the history file.

## Inside the horizon: the anchored borrow

`$b = $a->property` is a plain load, no barrier, when the compiler
proves: the anchor `$a` outlives the borrow; the path from anchor to
target is not severed or redirected inside the borrow's scope; every
operation that could invalidate either is visible in the IR. The
borrow is sound under today's rc-walk with no new machinery: the
target's count already carries the anchor's edge, and the walk reaches
the target through the live anchor. These are the same three
obligations unique ownership's borrow clause states
(`rfc/model/gc/rc-walk.md`, "Unique ownership"), which is the design
requirement recorded there: one borrow analysis in the IR, one
invalidation vocabulary, two consumers.

A checkpoint is an invalidation point unless proven otherwise: the
drain may run another object's `__destruct`, and that code can sever
any path it can reach. A summary that proves the anchor unreachable
from any destructor-bearing class lifts the restriction; the transitive
purity ruling (`dev/design/pure-destructors.md`) supplies exactly the
class analysis that proof needs.

## At the horizon: the wholesale publication

A horizon is any of: a call without a trusted summary, dynamic
dispatch the class set cannot close, reflection, a by-reference
escape, a checkpoint not proven destructor-free, an own-code store
that severs a borrowed path. Before the horizon's first instruction,
the mutator publishes protection for each live local the proofs stop
covering.

The publication is the cheap half of the trade. In form A a protected
local needs nothing the runtime does not already have: **the ownership
barrier is an ordinary retain** — the local becomes a counted holder,
exactly as every local is today — and the matching release runs at
scope exit, exactly as today. So form A's horizon payment is the
familiar pair, and the algorithm's whole gain is the region between
horizons, where today's scheme pays the pair per local and this scheme
pays nothing.

That resolution dissolves the superseded model's three hard problems
at once: there is no hazard loop (protection precedes the horizon
instead of guarding each load), no untouch race and no last-holder
disjunction (the count aggregates holders, which is what counts are
for), and no walk-side changes at all — rc-walk does not learn this
feature exists. The compiler alone decides where the pairs go; the
runtime is unchanged.

## The hybrid: the regime is a class property

Counting is not all-or-nothing (Edmond, 2026-08-18): whether local
references to a class's instances are counted is a property of the
class, chosen by the compiler and carried where the other compiler-owed
class facts live (the acyclic and purity bits' delivery channel).

- **A counted class**: locals pay the classic pair at acquisition and
  release, exactly today's code. No proofs, no summaries, no horizon
  bookkeeping — a counted local is its own protection, so horizons do
  not publish it.
- **A horizon class**: locals are free inside the proof horizon and
  are published (retained) only at a horizon they cross, as above.

Mixing is sound by construction in form A: both regimes resolve to
counts wherever liveness is decided, differing only in *when* the
count is taken. The regime is decidable at a site exactly where the
static class is — a slot whose class the compiler cannot narrow
(`mixed`, an open hierarchy) is treated as counted, the conservative
default, and regime A is likewise the mandatory answer at every
analysis failure: the superseded model's own rule, "analysis failure
must select owned, never guess". The selection heuristic is economic:
horizon classes are the closed, summary-friendly types that live in
provable scopes (value objects, DTOs, the acyclic-and-pure
population); counted classes are the ones that habitually cross
reflection, callbacks or coroutine suspensions, where the analysis
would cost more than the pairs it removes.

The hybrid also changes what the measurement decides: the corpus scan
below no longer gates the whole design on one global coverage number —
it prices the horizon regime class by class, and the design pays for
summaries only where they buy something.

## What this is, named against the literature

Proof-driven deferred stack counting: Deutsch–Bobrow's deferral with
the reconciliation scan replaced by compiler proofs, or equally
Perceus-with-borrowing where the borrow analysis is pushed across
summarized calls and the dup/drop pairs land only at horizons
(`dev/RESEARCH.md`, 2026-08-18, the static family). The delta from
plain borrow inference is the summary system: without summaries every
call is a horizon and the scheme degenerates to today's; with them,
the free region grows call-deep.

## Economics, and the one number that decides

Between horizons both this scheme and today's differ by exactly the
elided pairs. At horizons this scheme pays the same pair today's
scheme pays at acquisition. So the whole gain is:

```
saved = (local acquisitions not reaching any horizon) × pair cost
```

with pair cost measured at 1.84–1.87 ns (`docs/performance-case.md`,
"The counted publish" neighbourhood). The decisive parameter is the
fraction of local lifetimes fully inside the horizon — in PHP, with
dynamic properties, callbacks and reflection shortening summaries,
that fraction is the open empirical question the superseded model's
own review raised.

Measurement order, cheapest first:

1. **Corpus scan, compiler-free:** over php-src and a Packagist
   sample, per function, count call sites that could carry a summary
   (final/private/builtin callees, no reflection, no by-ref) — an
   upper bound on horizon-free lifetimes. Kills or funds the summary
   investment before any compiler work.
2. **The Phase D publish census** (`dev/DECISIONS.md`, 2026-08-18, the
   reopening condition): the same instrument that prices birth count
   and unique ownership prices this — the three share one static
   family and should share one gate.

No walk change, no new runtime state and no protocol work are on the
bill in form A; the entire cost is compiler-side (borrow analysis,
summaries, invalidation bookkeeping), which is also where birth count
and unique ownership already spend.

## Composition with the designed family

- **Unique ownership:** the anchored borrow generalizes its borrow
  clause; one IR analysis serves both, ruled once.
- **Birth count:** orthogonal and compatible — it elides publication
  retains, this elides local retains; together they bound the counted
  traffic to horizon crossings and unprovable publishes.
- **Pure destructors:** transitive purity is the summary that proves a
  checkpoint destructor-free, widening horizons; and the P0 fast paths
  are untouched, since form A never changes death.
- **rc-walk and S28:** untouched. This design deliberately has no
  collector half.

## Open questions

1. The summary language: what a callee's effect summary states (which
   anchor paths it may sever, whether it may run destructors), who
   writes stdlib summaries, and the conservative default at every
   unknown (a horizon, always).
2. Borrow scopes across yields: a coroutine suspension is a horizon
   unless the summary system learns resumption points — decide early,
   the answer shapes the IR.
3. Whether the corpus scan's upper bound justifies summaries at all —
   the gate before any design deepens.

## The record

The name is `proof-horizon`. The superseded model and its review stay
as the map of the space already searched: the refusals of 2026-08-17
and 2026-08-18 closed the no-heap-RC roads, and this design does not
re-enter them — it keeps every count the refusals defended and removes
only the pairs the proofs make redundant.
