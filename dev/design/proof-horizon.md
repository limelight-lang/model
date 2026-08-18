# Proof Horizon: borrow protection paid where proof ends

**Status:** design sketch, not implemented. Revision 3: Critic round 2
(2026-08-18, same three lenses) attacked round 1's fixes and found the
horizon list open over eager death, the promotion placement rule
unsatisfiable for branch-born borrows, and the corpus scan's kill
authority unearned; this revision closes each. Both rounds' records
are at the end. Further rounds pending.
**Author of the algorithm:** Edmond, 2026-08-18. Successor to the
stack-exit epoch model (`docs/history/stack-exit-epoch-gc-2026-08-18.md`,
superseded the day it was recorded); shaped by that model's five-axis
review (`dev/STACK_EXIT_EPOCH_GC_REVIEW.md`) and by the two standing
refusals in `dev/DECISIONS.md`, 2026-08-17 and 2026-08-18.

## The algorithm in two sentences

A local is either **owned** — a counted holder paying today's
retain/release pair — or an **anchored borrow**, paying nothing while
the compiler's proofs hold: `live(anchor) ∧ stable_path`, with every
anchor chain ending in a counted root. A borrow pays only at a
**proof horizon** — a point its proofs stop covering — and the
payment is promotion to owned by an ordinary retain, emitted once per
lifetime at a point that dominates every horizon in its live range.

The cost model is the idea: protection is priced per point of doubt,
not per read, per copy or per store. Lean-style call summaries push
the horizon outward; a borrow no summary covers is promoted at its
first horizon, once, and a borrow whose lifetime reaches no horizon
pays nothing at all.

### What round 1 changed

Revision 1 elided the count on every local. Three consequences killed
it, one per lens. An object held only by locals never died: the
factory's creation reference was never released, so every acyclic
local-only object leaked, and `__destruct` timing broke in both
directions. An uncounted anchor chain could sit inside a component
the exact test condemns, because that test balances counted
references only (`rfc/model/gc/rc-walk.md`, "Uncounted borrows"), so
the drain freed under a live borrow. And COW's `refcount == 1`
uniqueness test read a count missing its local holders, so shared
arrays mutated in place. All three vanish when owning locals keep
their count: death, `__destruct` timing and COW read the same numbers
as today, and every borrow is covered by a counted root.

### What round 2 changed

Round 1's fixes were attacked in turn. The horizon list bound
destructor severing to checkpoints, but eager death runs `__destruct`
at every release that reaches zero, so an ordinary `$tmp = null`
could sever a borrowed path through a destructor at a site the list
did not name — the release horizon below closes it. The placement
rule demanded a point dominating both the horizons and the frame
exit, which for a branch-born borrow lies before the borrow's operand
exists — the rule is restated over the live range. Parameters,
`$this` and call results had no lattice state — the conventions below
assign them. The checkpoint condition's root set omitted the sever
cascade, the chain invariant misdescribed itself as a restatement of
the rule it extends, the promotion retain was undefined against a
unique-ownership sentinel, and the scan could kill a good design on
unresolved receivers — each corrected below, with the round record
carrying the rest.

## The ownership lattice

Every IR local is in one of two states, assigned by the compiler.

**Owned** — a counted reference, today's code exactly: acquisition
retains (or absorbs the creation reference of `new`), release per the
drop-point policy (`rfc/model/memory/static-lifetimes.md`, "Drop
Point Policy"), eager death at zero, `__destruct` at the release that
reaches it. Owned by construction:

- the result of `new`;
- **every call result** — the callee retains the returned reference
  before its epilogue, so a return is an ownership transfer in every
  case. A borrowed return would surface after the callee's epilogue
  checkpoint, a window no caller-side promotion can dominate, so
  borrowed returns do not exist until the summary language learns
  callee-side promotion (open question 1);
- **the receiver and every by-value parameter** — the callee frame
  holds a counted reference for each, today's calling convention,
  because an anchored parameter's chain would end in the caller's
  frame, and per-function horizon detection cannot see a re-entrant
  store that kills the caller's slot mid-call. Cheapening this via
  caller-guarantee summaries is open question 6;
- every reference to a COW-eligible value — array, string, reference
  box — because their uniqueness test reads the count and an
  uncounted holder falsifies it;
- every local the analysis fails on, and every borrow whose live
  range's paths disagree on definition or on horizon structure.
  Analysis failure selects owned, never guesses anchored — restated
  here normatively; first recorded in the superseded document's
  embedded review, axis 1.

**Anchored** — an uncounted borrow, `$b = $a->property` as a plain
load. The chain invariant: the anchor is a counted root — an owned
local, and equally any root category rc-walk names: an arena slot, a
static, an immortal, an FFI handle — or a borrow whose own chain ends
in one.

The invariant **extends** rc-walk's legality rule for uncounted
borrows rather than restating it. The rule ("Uncounted borrows";
`rfc/model/memory/static-lifetimes.md`, "What may own a borrow")
requires the covering counted reference to be a root and says a heap
field never qualifies; a chained borrow's immediate cover *is* a heap
field. The extension's own soundness argument: every edge of the
anchor path is a counted heap edge, so at any drain a condemned
component intersecting the path has an external counted in-edge
traceable to the root, the exact test acquits it whole, and the walk
reaches the target through the live chain — an incoherent-array skip
on the path only inflates `RC − IN` toward roothood, conservative in
the safe direction (`src/walk.rs`, the version-bracket skip). What
the extension does not weaken: the chain must still end in a genuine
root, so path stability, not reclamation, is the obligation the rest
of this document spends on.

Anchor identity survives representation changes: the anchor is the
owned local itself, not the entity it referenced at borrow time, and
`stable_path` means counted reachability from the anchor's current
referent — a COW separation re-seating the anchor's array, or a
`sort()` that keeps every element counted, does not invalidate the
borrow, while any mutation through the anchor local is a horizon.

## Inside the horizon: what the borrow must prove

The borrow's three obligations are the anchor outliving it, the
anchor-to-target path staying intact, and every operation able to
invalidate either being visible in the IR. The same IR analysis is
meant to serve unique ownership's borrow clause
(`rfc/model/gc/rc-walk.md`, "Unique ownership") with a different
invalidation discipline — the five-axis review's "one analysis with
two invalidation disciplines", whose family-wide ruling is still
owed (open question 5).

Path visibility is bounded by aliasing, and the rule is conservative:
**a store through any may-alias of a path base is a severing store.**
A must-not-alias proof lifts it; without one, `$c->child = null`
severs `$a->child` whenever `$c` and `$a` may name one object. In
untyped code this makes many object stores horizons, which is a
priced cost, not a footnote: the corpus scan carries a severing-store
channel so the free fraction is measured under the sound rule. COW
values are the self-repairing case — a foreign alias copies before
writing — so typed-array paths are the cheap population.

## The horizon list

A horizon is any of:

- a call without a trusted summary;
- dynamic dispatch the class set cannot close;
- reflection;
- a by-reference escape;
- **a possibly-final release of a non-pure class** — any store
  displacement, `unset`, `null` assignment or scope exit whose
  target's count may reach zero and whose class's transitive-purity
  closure (`dev/design/pure-destructors.md`, "Purity is transitive")
  contains a destructor that can reach a severing store. Eager death
  runs `__destruct` at the release site, no drain involved, so the
  destructor hazard is a property of releases, not of checkpoints;
  summaries must carry the same closure for callee-internal releases;
- a checkpoint that fails the condition below;
- an own-code store that severs a borrowed path, under the may-alias
  rule above.

A checkpoint threatens a borrow in two distinct ways, and only the
second survives as a condition.

**Reclamation.** The drain severs and frees condemned components
whether or not destructors exist — the P0 raw-sever arm of
`dev/design/pure-destructors.md` runs no user code. The chain
invariant answers this unconditionally: the exact test balances
counted references and the chain ends in a counted root, so no
component on the anchor path is condemnable. Discharged by
construction, at every checkpoint, including the hand-off drain's
collector-side sever between checkpoints.

**Path severing by drain destructors.** A `__destruct` the drain
runs can store into the anchor path. The condition binds the event
that runs those destructors — under the hand-off drain, the prologue
checkpoint visit; if pure-destructors' open questions move any
user-code duty into the sliced tail, every checkpoint carrying a
slice inherits the condition, and this document must move with that
ruling. The discharge is reverse reachability whose root set is the
**downward closure of the condemned set** — the sever releases
external children "destructors and all", so the cascade's classes
are in scope, and that closure is exactly what transitive purity
computes: a condemned set whose closure is destructor-free certifies
the checkpoint, and purity is thereby the root-set instrument, not
merely an input. Until the analysis exists, a checkpoint not proven
safe is a horizon.

## At the horizon: promotion

The payment is promotion: one ordinary retain, after which the
borrow is an owned local, released per the same drop-point policy as
any other — last use for destructor-free classes — so promotion
changes no lifetime against today's owned lowering. Placement rules:

- The promotion point is the **closest point dominated by the
  borrow's birth that dominates every horizon and every exit of the
  borrow's live range**. The live range is the definition-to-last-use
  region, not PHP's frame scope; a borrow whose paths disagree on
  definition or horizon structure is owned from birth (the lattice's
  failure default), so a satisfying point always exists and no
  path-dependent release bookkeeping arises.
- Promotion cannot precede the birth: the retain's operand exists
  only after the load. This is also the static argument that death
  order is preserved — a promoted borrow holds its count over a
  subrange of exactly the lifetime today's owned borrow holds it.
- A loop containing a horizon promotes before the loop; the
  dominator rule implies it and keeps the payment "once".
- On unwind, a landing pad releases the owned set live at its call
  site; the promotion point dominating the sites after it makes that
  set static per site.
- **A borrow of a unique-ownership entity cannot be promoted**: the
  count word holds the occupancy sentinel and a retain written into
  it protects nothing. A horizon-reaching borrow of a unique entity
  demotes the uniqueness proof — the entity becomes counted — or the
  borrow is owned from birth against a counted entity; the summary
  language carries the cross-compilation constraint, recorded also
  in `dev/design/owned-slots-and-the-walk.md`.

The rule bounds the scheme's cost: a promoted borrow pays one pair
over a subrange of the lifetime today's code pays it over, so per
borrow the scheme never costs more than the current code, and the
whole difference is the borrows that are never promoted. Overpayment
— a promotion point on a path that reaches no horizon — loses
savings, never adds cost.

The collector is untouched: promotion is a compiler-emitted retain,
with no protection set, no candidate-test arm and no death-branch
test.

## The hybrid: counted class, horizon class

Whether locals referencing a class's instances enter the lattice at
all is a class property in policy and a per-site decision in
mechanism. In form A the two regimes differ only in where the
compiler emits pairs, so the class bit is the default the emitter
follows, and every anchored site still owes its site-local proof.

- **A counted class**: locals are owned, today's code, no proofs and
  no horizon bookkeeping.
- **A horizon class**: locals enter the lattice — anchored where the
  proofs hold, promoted at horizons.

A slot whose static class the compiler cannot narrow (`mixed`, an
open hierarchy) is counted, and analysis failure selects counted:
both defaults land on today's behaviour. A subclass may differ in
regime from its parent; a parent-typed site follows the parent's
bit over any instance, which in form A is a cost decision only,
since instances of both regimes are runtime-identical. The selection
heuristic is economic: horizon classes are the closed,
summary-friendly types in provable scopes; counted classes are the
ones crossing reflection, callbacks and suspensions, where analysis
costs more than the pairs it removes.

Granularity — open question 4, narrowed by rounds 1 and 2. Per-site
deviation from the class default is lawful in mechanism but
unauditable until the compiler emits a **per-site certificate**, not
merely a log: each entry carries the anchor chain, the summary IDs
relied on and the horizon set, and an independent checker validates
the three obligations against them — a log replayed against the
compiler's own analysis would grade itself. The recommendation on
record: class-only until that certificate exists. The ruling is
Edmond's.

## The two forms

**Form A — over maintained heap RC. This is the design.** Heap
counts, owned-local counts, eager death, `__destruct` timing, COW,
the arena's counted promotion and rc-walk's protocol are today's.
The elision applies to anchored borrows only.

**Form B — without heap RC** is the superseded model's road, not
pursued: the architecture inventory dies with the count and
deterministic destruction needs the count back (the five-axis
review), and the walk's write barrier *is* the count (the 2026-08-18
stack-bit refusal, `dev/DECISIONS.md`).

## What the superseded model's problems become

The history file's supersession banner records three: a critical
untouch/retirement race, a load path that dominates the RC pair it
replaces, and the loss of deterministic destruction.

- The untouch race is gone: promotion is a plain retain that
  precedes the horizon, so nothing is retracted and nothing races
  the collector.
- The load path is gone: an anchored borrow costs zero instructions
  between horizons, with no per-load guard.
- Deterministic destruction is preserved by the lattice, and only by
  it: owned locals keep the count that drives eager death.

## Named against the literature

Deutsch–Bobrow defer stack counts and keep a zero-count table whose
reconciliation scan is the *freeing* mechanism for stack-only
objects. This design keeps freeing on the owned count instead, so
nothing replaces the table: owning locals never leave the count, and
proofs replace reconciliation for borrows only. In Perceus terms the
dup/drop pairs stay at ownership transfers and the borrow inference
is pushed across summarized calls. The delta from plain borrow
inference is the summary system: without summaries every call is a
horizon and the scheme reduces to the five-axis review's extraction
— a covering-borrow elision over maintained RC; with them the free
region grows call-deep (`dev/RESEARCH.md`, 2026-08-18, the static
family).

## Economics

```
saved = (borrow acquisitions whose lifetime reaches no horizon) × pair cost
```

with the pair at 1.84–1.87 ns (`docs/performance-case.md`, "The
pair: retain and release"). The population excludes the owned base
cases — `new` results, call results, parameters, COW-eligible values
— and the horizon list now includes possibly-final releases and
may-alias severing stores, both of which shrink the free fraction
and are measured by their own scan channels. Promotion is
cost-neutral against today (the bound above), so the formula has no
negative runtime term.

Three costs sit outside the formula and are named rather than
implied away. Compile time and code size: the borrow analysis,
summary computation and per-site landing-pad sets are paid per
function at every build. The recompilation blast radius: a summary
is a soundness assumption, so a stdlib point release that adds a
destructor or a severing store invalidates every caller compiled
against the old summary, and that rebuild bill recurs for the
scheme's whole life — the census verdict weighs it. And the ack
budget: an elided borrow's release was non-final by the borrow's own
obligations, so the death-branch ack rate is unchanged; what thins
is the batched scope-exit ack pair for scopes whose whole release
run is elided, and pickup sites that move when a death nests into a
cascade. That is a different site and magnitude than the fast
class's death-branch thinning in
`dev/design/owned-slots-and-the-walk.md`, but it drains the same
epoch-progress budget, and the compensating-poll rule (that
document's open question 3) is the shared dependency.

The baseline is marginal, not gross: unique ownership's borrow
clause and the birth count elide overlapping slices of the same
traffic. Attributing an acquisition to one design requires the other
designs' classifiers to run over the same corpus, so the census must
carry **per-acquisition coverage flags** from each family analysis —
which concedes that pricing this design's margin needs at least the
classifiers of unique ownership and the birth count implemented
first. Until they are, `saved` is a gross upper bound and is labeled
so.

Confirmation is by count, not by clock. The crate's established
instrument for effects under the noise floor is an exact counter
(`dev/BENCHMARKS.md`: the parked-memory table, the payload-move
count), and the shadow-count lowering below produces the elided-pair
count as a by-product, so the confirmed saving is
`elided pairs × measured pair cost` — exact at any density. A
wall-clock A/B is a cross-check only, on a workload whose density
clears the floor's upper edge: at 1.85 ns per pair, 3 % of a second
is about 16 M pairs, and no existing bench has that shape — the
Phase D bench plan owes one.

Measurement order, and what each can decide:

1. **Corpus scan, compiler-free, kill-only, two-sided.** Per
   *lifetime*: a lifetime is horizon-free only when every operation
   it spans is proven, so the scan walks lifetimes, not call sites.
   Its static proxies err in both directions — a `final` callee may
   still sever paths (optimistic), and an unresolved receiver in
   untyped code is not a proven horizon (pessimistic) — so the scan
   reports a **bracket**: the pessimistic bound counts every
   unresolved receiver, severing-store candidate and possibly-final
   release as a horizon; the optimistic bound counts none of them.
   The kill rule reads the optimistic bound: a design that cannot
   pay even if every unresolved site resolves favourably is closed.
   A wide bracket decides nothing and says the scan needs receiver
   resolution, priced as its own step. Channels: the free-fraction
   bracket, the unresolved-receiver share, the severing-store share,
   the possibly-final-release share, and the referent's static class
   where known, so the hybrid prices regimes per class. The corpus
   is deployed PHP applications with their vendor trees; the
   concrete names are owed by Edmond (open question 3) and recorded
   before the scan runs.
2. **The Phase D publish census** — with four channels this design
   needs, named now so the instrument is built carrying them:
   borrow-acquisition density per class, horizon crossings per
   borrow lifetime, live borrows per horizon, and the family
   coverage flags above. The census as recorded (`dev/DECISIONS.md`,
   2026-08-17 and 2026-08-18) counts publishes, which prices birth
   count and unique ownership but none of these. The channel list is
   owed to `dev/DECISIONS.md` if the design is accepted.

The default at ambiguity: the design stays closed until the census
decides; the corpus scan can only close it earlier, never open it.
Phase D is undated and gated outside this crate (`PLAN.md`), so the
operational status is: closed indefinitely, pre-D work limited to
the corpus scan and the summary-language question — `PLAN.md`
carries the line.

## Verification artifacts, a precondition of implementation

Form A's virtue — the collector never learns the feature exists —
removes every runtime detection point: a misplaced horizon and a
correct elision are the same instruction stream, so a compiler bug
surfaces as corruption far from its cause. Three instruments are
owed before any lowering ships; none is buildable before Phase D
supplies the compiler, which is part of why the design is closed
until then.

- **Shadow-count lowering.** A debug build emits the classic pairs
  into a shadow counter beside the elided real one, and **death
  defers to the shadow count**: a real-count zero with a nonzero
  shadow is logged and the object lives on, so the run keeps the
  classic build's behaviour and the divergence is a detection, not a
  post-mortem. Naming the guilty site needs more than two integers:
  the shadow build journals elided-acquisition site IDs per object,
  and the log at divergence lists the sites whose retains are
  missing. The elided-pair count falls out as the economics'
  confirmation instrument.
- **Differential lowering.** The same program built with horizons
  off and on. The oracle is **the destructor sequence and the death
  set per checkpoint batch** — not "timing": an elided borrow
  legitimately moves a child's death from its own release into the
  parent's cascade, same destructors, different nesting, one fewer
  pickup site, and an oracle that diffs nesting flags correct
  compiles.
- **Summary versioning.** A summary is a soundness assumption about
  a callee, so a stdlib update that adds a destructor or a severing
  store invalidates every caller compiled against the old summary.
  Open question 1 carries the versioning rule; without one, every
  stdlib update is a silent soundness event.

## Composition with the designed family

- **Unique ownership:** one borrow analysis, two invalidation
  disciplines — the ownership clause bans checkpoint crossing
  outright, this design substitutes the chain invariant plus the
  path-severing condition. The family-wide ruling the review asked
  for ("the family needs it ruled once") does not exist yet and is
  open question 5. The sentinel constraint above is the second
  composition point: a horizon-reaching borrow demotes uniqueness.
- **Birth count:** adjacent populations, marginal accounting per the
  economics above.
- **Pure destructors:** transitive purity is the root-set instrument
  for both destructor horizons — the release horizon's closure and
  the checkpoint condition's condemned-set closure; the P0 fast
  paths and death itself are untouched, because owned locals keep
  the count. The path-severing condition binds the drain event that
  runs destructors, so pure-destructors' open hand-off questions are
  a named dependency: if user-code duties move to the sliced tail,
  the condition moves with them.
- **rc-walk and S28:** the collector code is untouched; the protocol
  dependency is the ack-budget paragraph in the economics.

## Open questions

1. The summary language: what a summary states — severable paths,
   destructor reachability of internal releases, callee-side
   promotion for borrowed returns — who writes stdlib summaries, the
   conservative default at every unknown (a horizon, always), and
   the versioning rule from the verification section.
2. Borrow scopes across suspensions: a yield is a horizon unless the
   summary system learns resumption points, and a fiber suspended
   across an arena reset carries frame borrows the reset cannot see
   — one question, and it shapes the IR early.
3. Whether the corpus scan's bracket justifies summaries at all —
   the kill-only gate above — and the corpus names, owed by Edmond.
4. The hybrid's granularity: class-only, or per-site deviation
   behind the per-site certificate. The recommendation on record is
   class-only until the certificate exists; the ruling is Edmond's.
5. The family-wide borrow-analysis ruling: one IR-level borrow
   analysis parameterized by the invalidation set, serving unique
   ownership and this design — asked by the five-axis review, not
   yet ruled; the ruling is Edmond's.
6. Anchored parameters: whether caller-guarantee summaries can lift
   the receiver and by-value parameters out of the owned default,
   and what the re-entrancy obligation costs there.

## The record

The name is `proof-horizon`. The superseded model and its review
stay as the map of the space already searched: the refusals of
2026-08-17 and 2026-08-18 closed the no-heap-RC roads, and this
design keeps every count they defended — including the owned
locals' — and removes only the pairs the proofs make redundant.

Critic round 1, 2026-08-18, three lenses. **Soundness:** uncounted
owning locals leak every acyclic local-only object and move
`__destruct` timing in both directions; the checkpoint hazard is the
drain's own sever-and-free, so the destructor-freedom lift was
inverted; COW's uniqueness test read a falsified count; the
protection-set paragraph contradicted "no walk-side changes"; loop
and conditional horizons made "pay once" unsound without a placement
rule. **Composition:** the elision re-entered rc-walk's
uncounted-borrows prohibition and dropped Deutsch–Bobrow's freeing
half while citing the deferral; "the same three obligations"
misquoted unique ownership's borrow clause; the superseded model's
third recorded problem is deterministic destruction, which revision
1 did not dissolve; the pair-cost citation named the wrong section;
the ack-thinning dependency went unnamed. **Verification:** the
per-call-site scan bounds no lifetime fraction; the census lacks
every channel this design needs; the publication rule's per-crossing
reading admits unbounded negative savings; family savings
double-counted at the shared gate; no falsification artifact
existed. Accepted in full; revision 2 was the fix.

Critic round 2, 2026-08-18, on revision 2's fixes. **Soundness:**
eager death runs severing destructors at ordinary releases the
horizon list did not name (critical); the checkpoint condition's
root set omitted the sever cascade; parameters and `$this` had no
lattice state and re-entrancy killed the caller-frame chain; a
borrowed return surfaces after the callee's epilogue checkpoint,
outside any caller-side promotion; severing stores were undecidable
without a may-alias rule; a promotion retain against a
unique-ownership sentinel protects nothing; COW-container borrows
needed the anchor-identity definition; promotion pinned to scope
exit regressed the drop-point policy. The chain invariant's
reclamation claim survived attack, verified against the exact test
and the incoherent-array skip. **Composition:** the horizon list's
openness over eager death, independently; the chain invariant
claimed to restate the rule it extends, while dropping four root
categories and widening over heap-field chains; the ack-thinning
caveat named the wrong site — borrow releases are never final, so
the death-branch rate is unchanged; the differential oracle's
"timing identical" flags correct compiles; "analysis failure selects
owned" was cited to a do-not-rely file; the checkpoint condition
depended on the hand-off keeping destructors in the prologue without
naming it; `dev/INDEX.md` still described revision 1; the
family-wide ruling was dropped, not answered. **Verification:** the
placement rule was unsatisfiable for branch-born borrows; the scan's
kill authority failed on unresolved receivers, and the corpus stayed
unnamed; no planning artifact recorded the design's closed status;
the marginal baseline was not computable from the named channels;
the density gate demanded the clock where the crate's method counts;
the shadow count detects only if death defers to it, and two
integers name no site; the decision log graded itself. Accepted in
full; this revision is the fix, with the corpus names left open with
Edmond.
