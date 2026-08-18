# Proof Horizon: borrow protection paid where proof ends

**Status:** design sketch, not implemented. Revision 4: Critic round 3
(2026-08-18, same three lenses) attacked round 2's fixes and found
the drop-point policy killing anchors under live borrows, the
uniqueness demotion without a sound local lowering, the shadow
counters wired backwards, and the scan's kill gate unable to fire;
this revision closes each. All three rounds' records are at the end.
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

**The sound configuration's free region, named honestly.** Before
any analysis lands, the conservative defaults compose to: a borrow
survives only a lifetime containing no object store, no release of a
non-pure-closure class, no owned death and no unsummarized call —
read-only lifetimes over destructor-free data, roughly one statement
in idiomatic untyped code. Every widening is bought by a named
instrument: summaries widen calls, the may-alias lifter below widens
stores, purity classification widens releases, and the "free region
grows call-deep" sentence in the literature section holds only
through callees that are transitively store-free with pure-closure
internal releases. The corpus scan measures the bought region, not
the dream.

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
their count and destructor-bearing targets stay owned: death,
`__destruct` timing and COW read the same numbers as today, and
every borrow is covered by a counted root.

### What round 2 changed

The horizon list bound destructor severing to checkpoints, but eager
death runs `__destruct` at every release that reaches zero — the
release horizon closed it. The placement rule was restated over the
live range; parameters, `$this` and call results got lattice states;
the checkpoint condition's root set became the condemned set's
downward closure; the chain invariant was re-declared an extension
with its own soundness sentence; the promotion retain was banned
against a unique-ownership sentinel; the scan became two-sided.

### What round 3 changed

Round 2's fixes were attacked in turn. The drop-point policy would
release an anchor at its last syntactic use while a live borrow
still leaned on it — the borrow-is-use rule below closes it. A
borrow of a destructor-bearing target moved `__destruct` earlier
than the policy's scope-end pin — such targets are now owned from
birth. Uniqueness demotion had no lowering local to the borrower's
unit — it is now a whole-program fixpoint with the upstream blast
radius priced. The shadow build's counters were wired so the debug
runtime broke its own walk and COW — they swap. The release
horizon's predicate and the scan's bounds are restated over what
purity actually computes, and the scan's deliverable becomes the
doubt map, its kill read from a graded bound that keeps provable
horizons.

## The ownership lattice

Every IR local is in one of two states, assigned by the compiler
over SSA-form borrows — the phi is the disagreement detector the
failure default reads.

**Owned** — a counted reference, today's code exactly: acquisition
retains (or absorbs the creation reference of `new`), release per
the drop-point policy (`rfc/model/memory/static-lifetimes.md`,
"Drop Point Policy"), eager death at zero, `__destruct` at the
release that reaches it. Owned by construction:

- the result of `new`;
- **every call result** — the callee retains the returned reference
  before its epilogue, and that retain precedes the batched
  scope-exit release run and the epilogue checkpoint, so the value
  cannot die under it. A borrowed return would surface behind the
  epilogue checkpoint, outside any caller-side promotion, so
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
- **every borrow whose target's class is not transitively
  destructor-free**: eliding such a borrow's count lets a severing
  store between the borrow's last use and the scope's end reach
  zero early, moving `__destruct` off the drop-point policy's
  scope-end pin — a Zend-observable timing change. Owned from birth
  keeps the pin; the corpus scan prices the exclusion by its own
  channel;
- **every borrow whose path crosses a unique-ownership entity**: the
  chain invariant's premise is that every path edge is a counted
  heap edge, and a unique entity's owning slot pays no count — the
  composition happens to stay sound (the entity is never condemned
  and its overwrite is a may-alias severing store), but the
  invariant as stated fails, so the case compiles owned;
- every local the analysis fails on, and every borrow whose birth
  does not dominate every horizon and every exit of its live range —
  the direct, checkable form of the failure default; a borrow born
  inside a loop with a horizon reachable over the back-edge fails it
  and is owned. Analysis failure selects owned, never guesses
  anchored — restated here normatively; first recorded in the
  superseded document's embedded review, axis 1.

**Anchored** — an uncounted borrow, `$b = $a->property` as a plain
load. The chain invariant: the anchor is a counted root — an owned
local, and equally any root category rc-walk names: an arena slot, a
static, an immortal, an FFI handle — or a borrow whose own chain ends
in one. **Every point of a live borrow is a use of its transitive
anchor for the drop-point policy**: the anchor's release site is
computed over the borrow's live range, not the anchor's own last
syntactic use, otherwise the policy frees the anchor under the
borrow it covers.

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
the safe direction (`src/walk.rs`, the version-bracket skip). On
acceptance the extension and this argument are owed to the two RFC
sections above and to the DC5 scenario notes of
`rfc/model/gc/rc-walk-proof.md` — the debt is recorded here per the
repo's design-stage practice.

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
The must-not-alias instrument that lifts it is named, because
without one the rule makes most object stores horizons: closed-class
typed properties give type-incompatibility disjointness — the same
closed types the hybrid already targets — and nothing else is
assumed. The corpus scan carries a severing-store channel so the
free fraction is measured under the sound rule. COW values are the
self-repairing case — a foreign alias copies before writing — so
typed-array paths are the cheap population.

## The horizon list

A horizon is any of:

- a call without a trusted summary;
- dynamic dispatch the class set cannot close;
- reflection;
- a by-reference escape;
- **a release of a class whose transitive-purity closure is not
  pure** — any store displacement, `unset`, `null` assignment or
  scope exit, with NR counting as impure, because NR admits external
  writes that sever live paths. Eager death runs `__destruct` at the
  release site, no drain involved, so the destructor hazard is a
  property of releases. The predicate is deliberately the one purity
  computes — one boolean per class over the field-type closure
  (`dev/design/pure-destructors.md`, "Purity is transitive") — and
  deliberately over-approximate; a finer store-effect analysis of
  destructor bodies is a separately owed instrument if the coarse
  rule proves too expensive. No finality conjunct: "may reach zero"
  is never dischargeable without count-value analysis nobody plans,
  so every such release is a horizon. The lemma that keeps the rule
  from swallowing pure cascades: an object that reaches zero is off
  every live anchor chain (severing a chain edge is itself an
  own-code horizon before the cascade begins), so the own-slot
  stores of a dying pure cascade never sever a live path;
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
runs can store into the anchor path. The condition binds **any
checkpoint that can drain a verdict** — under the hand-off design
that is two arms: the prologue visit, which runs P2 calls, and the
unchanged whole drain that an NR-or-impure component takes at
whatever death or poll picks the verdict up; if pure-destructors'
open questions move user-code duties into the sliced tail, every
checkpoint carrying a slice inherits the condition. The discharge is
reverse reachability whose root set is the **downward closure of
the condemned set** — the sever releases external children
"destructors and all", so the cascade's classes are in scope, and
that closure is exactly what transitive purity computes: a condemned
set whose closure is pure certifies the checkpoint. Until the
analysis exists, a checkpoint not proven safe is a horizon.

## At the horizon: promotion

The payment is promotion: one ordinary retain, after which the
borrow is an owned local, released per the same drop-point policy as
any other — so promotion changes no lifetime against today's owned
lowering of the same borrow. Placement rules:

- The promotion point is the **closest point dominated by the
  borrow's birth that dominates every horizon and every exit of the
  borrow's live range**; a borrow with no such point is owned from
  birth by the lattice's failure default, so the rule is total.
- Promotion cannot precede the birth: the retain's operand exists
  only after the load. This is also the static argument that death
  order is preserved — a promoted borrow holds its count over a
  subrange of exactly the lifetime today's owned borrow holds it.
- A loop containing a horizon promotes before the loop when the
  borrow is born before it; born inside, the back-edge fails the
  dominance test and the borrow is owned.
- On unwind, a landing pad releases the owned set live at its call
  site; the promotion point dominating the sites after it makes that
  set static per site.
- **A borrow of a unique-ownership entity cannot be promoted**: the
  count word holds the occupancy sentinel and a retain written into
  it protects nothing. A horizon-reaching borrow demotes the
  uniqueness proof — and demotion is a **whole-program fixpoint**,
  not a local lowering: the owner's unit compiled the plain-store
  overwrite and the sentinel factory, so a later-compiled borrower
  forces the owner's recompile, an upstream blast radius the
  economics prices. The conservative default until the fixpoint
  exists: uniqueness is lawful only for entities whose every access
  site compiles in the same session, and a foreign-unit borrow of a
  unique entity is forbidden by summary. Recorded also in
  `dev/design/owned-slots-and-the-walk.md`, with the corollary that
  demotion revives the COW check for the entity's writers.

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

Granularity — **ruled by Edmond, 2026-08-18** (`dev/DECISIONS.md`,
"proof-horizon granularity"). The class bit stays the emitter's
default; on top of it, a closed set of **always-provable elision
rules** applies at any site in either regime, the way Swift ARC's
guaranteed optimizations do — rules whose soundness follows from the
language semantics alone, with no summary, no heuristic and no
cross-unit assumption, a redundant pair inside a proven horizon-free
region being the model case. A counted class's local may lose its
pair under such a rule. Summary-driven or heuristic per-site
deviation stays barred until two instruments exist together, neither
sufficient alone: a **per-site certificate** — anchor chain, summary
IDs, horizon set per entry — whose independent checker soundly
warrants the checkable surface (chain well-formedness,
syntactic-horizon coverage, summary-version freshness) and cannot
warrant may-alias completeness, which any checker would inherit from
the shared oracle; and the shadow-count lowering, whose dynamic
cross-check is the only detector for what the certificate cannot
see. The ruling's standing constraint: no rule of either kind
introduces a write barrier or any other mutator work beyond the
program's own code.

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
  it: owned locals keep the count that drives eager death, and
  destructor-bearing targets never lose a holder to elision.

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
region grows call-deep, through callees that are transitively
store-free with pure-closure internal releases — the condition the
sound-configuration paragraph states (`dev/RESEARCH.md`, 2026-08-18,
the static family).

## Economics

```
saved = (borrow acquisitions whose lifetime reaches no horizon) × pair cost
```

with the pair at 1.84–1.87 ns (`docs/performance-case.md`, "The
pair: retain and release") as a **unit** cost; the in-situ marginal
cost of a pair disperses by context (the crate has recorded 10.2 ns
under a wide-load pairing and partial masking behind independent
work), so the product carries the pair-cost dispersion as its error
bar — a factor-band, not "exact". The population excludes the owned
base cases — `new` results, call results, parameters, COW-eligible
values, destructor-bearing targets, unique-crossing paths — and the
horizon list prices releases and may-alias stores; every exclusion
has a scan channel.

The counting instrument is an **elision-site counter in the release
lowering behind a build flag**: the compiler statically knows the
elided sites, counting their executions perturbs less than the debug
journal, and the count is taken from the build that ships. The
shadow build's count is a verification by-product, not the
economics' number — its lowering can flip lattice outcomes the
release build would not.

Three costs sit outside the formula and are named rather than
implied away. Compile time and code size: the borrow analysis,
summary computation and per-site landing-pad sets are paid per
function at every build. The recompilation blast radius, in **both
directions**: downstream, a stdlib update that adds a destructor or
a severing store invalidates every caller compiled against the old
summary; upstream, a uniqueness demotion forces the owner-unit
recompile. The scan's blast-radius channel sizes the downstream
half. And the ack budget: an elided borrow's release was non-final
by the borrow's own obligations, so the death-branch ack rate is
unchanged; what thins is the batched scope-exit ack pair for scopes
whose whole release run is elided, and pickup sites that move when a
destructor-free death nests into a cascade — a different site and
magnitude than the fast class's death-branch thinning in
`dev/design/owned-slots-and-the-walk.md`, but the same
epoch-progress budget, and the compensating-poll rule (that
document's open question 3) is the shared dependency.

The baseline is marginal, not gross, and the rule is asymmetric: **a
gross number may only kill, only the marginal number may open.**
Unique ownership's borrow clause and the birth count elide
overlapping slices of the same traffic, so the census carries
**per-acquisition coverage flags** from each family analysis —
which concedes that the classifiers of unique ownership and the
birth count are census instrumentation, built before any verdict.
The full channel list is owed to `dev/DECISIONS.md` **before the
Phase D census is specified**, not on acceptance — a census built
without the flags can price this design only grossly, and a gross
number opens nothing.

Confirmation is by count, not by clock: confirmed saving is the
release-build counter's elided pairs × the unit cost, within the
dispersion band. A wall-clock A/B is a cross-check only, on a
workload whose density clears the floor's upper edge: at 1.85 ns
per pair, 3 % of a second is about 16 M pairs, and no existing
bench has that shape — the Phase D bench plan owes one.

Measurement order, and what each can decide:

1. **Corpus scan, compiler-free, graded.** Per *lifetime*: a
   lifetime is horizon-free only when every operation it spans is
   proven, so the scan walks lifetimes, not call sites. Every site
   is classified three ways — provably-horizon, provably-free,
   unresolved — and **provable horizons stay horizons in both
   bounds**: a visible severing store, a release of a provably
   impure closure. The deliverable is the doubt map — where the
   unresolved mass concentrates — through these channels: the
   free-fraction bracket over the graded classification; the
   unresolved-receiver share; the severing-store share; the
   per-release purity tier (P0-syntactic / closure-unresolved /
   provably-impure, computed under both readings of the pending
   child-release-order ruling); the destructor-bearing-target share;
   the referent's static class where known; and the
   **summary-dependency channel** — per stdlib or vendor class, the
   transitive share of corpus functions whose summaries reach it,
   which sizes the downstream blast radius from data the lifetime
   walk already holds. The kill rule reads the graded optimistic
   bound — unresolved sites resolved favourably, provable horizons
   kept — and a corpus where one stdlib class change invalidates
   most compiled functions is kill evidence of its own. The corpus
   is deployed PHP applications with their vendor trees; the
   concrete names are owed by Edmond (open question 3) and recorded
   before the scan runs.
2. **The Phase D publish census** — with the channels this design
   needs: borrow-acquisition density per class, horizon crossings
   per borrow lifetime, live borrows per horizon, and the family
   coverage flags above. The census as recorded (`dev/DECISIONS.md`,
   2026-08-17 and 2026-08-18) counts publishes, which prices birth
   count and unique ownership but none of these.

The operational status, stated without decoration: **closed, and no
pre-D step can change that status** — the scan's verdict cannot
open (kill-only), the census is undated and gated outside this
crate, and every verification artifact needs the compiler. Pre-D
work is instrument preparation: the graded scan, the channel-list
recording, and the summary-language question, whose rulings inside
(who writes stdlib summaries, the versioning rule) are Edmond's.
`PLAN.md` carries the line.

## Verification artifacts, a precondition of implementation

Form A's virtue — the collector never learns the feature exists —
removes every runtime detection point: a misplaced horizon and a
correct elision are the same instruction stream, so a compiler bug
surfaces as corruption far from its cause. Three instruments are
owed before any lowering ships; none is buildable before Phase D
supplies the compiler, which is part of why the design is closed
until then.

- **Shadow-count lowering.** The **classic pairs drive the real
  header count** — so the walk's occupancy test, COW's uniqueness
  read, the release asserts and death itself behave exactly as the
  classic build — and the elided stream feeds a shadow word. The
  divergence signal is the shadow reaching zero while the real count
  is nonzero, logged with the per-object journal of
  elided-acquisition site IDs that names the sites whose retains
  are missing. (The reverse wiring, elided-authoritative, breaks
  the debug runtime it instruments: a real count of zero on a live
  object reads as a free slot to the walker and as "unique" to
  COW.)
- **Differential lowering.** The same program built with horizons
  off and on. The oracle is **the destructor sequence and the death
  set per checkpoint batch** — not "timing": an elided borrow of a
  destructor-free target legitimately moves the *free* from its own
  release into the parent's cascade, same teardown, different
  nesting; destructor-bearing targets are owned from birth, so
  their timing is pinned and any destructor-sequence diff is a real
  defect.
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
  for does not exist yet and is open question 5. The sentinel
  constraint and the demotion fixpoint above are the second
  composition point, recorded in both documents.
- **Birth count:** adjacent populations, marginal accounting per the
  economics above.
- **Pure destructors:** transitive purity is the instrument for both
  destructor horizons — the release horizon's closure predicate and
  the checkpoint condition's condemned-set closure; the P0 fast
  paths and death itself are untouched, because owned locals keep
  the count. The checkpoint condition binds every verdict-draining
  checkpoint, so pure-destructors' open hand-off questions are a
  named dependency: if user-code duties move to the sliced tail,
  the condition moves with them.
- **rc-walk and S28:** the collector code is untouched; the protocol
  dependency is the ack-budget paragraph in the economics. On
  acceptance the chain extension is owed to "Uncounted borrows",
  "What may own a borrow" and the DC5 notes, per the debt line in
  the lattice section.

## Open questions

1. The summary language: what a summary states — severable paths,
   destructor reachability of internal releases, callee-side
   promotion for borrowed returns, the uniqueness-demotion
   constraint — who writes stdlib summaries, the conservative
   default at every unknown (a horizon, always), and the versioning
   rule from the verification section. The rulings inside are
   Edmond's.
2. Borrow scopes across suspensions: a yield is a horizon unless the
   summary system learns resumption points, and a fiber suspended
   across an arena reset carries frame borrows the reset cannot see
   — one question, and it shapes the IR early.
3. The corpus names for the scan, owed by Edmond; the criterion —
   deployed applications with their vendor trees — is recorded.
4. ~~The hybrid's granularity~~ — ruled by Edmond, 2026-08-18: the
   class bit is the default, always-provable Swift-style elision is
   lawful per site in both regimes, fallible per-site deviation
   stays behind the certificate-plus-shadow-lowering gate, and no
   rule introduces a write barrier (`dev/DECISIONS.md`,
   "proof-horizon granularity"; the hybrid section carries it).
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
reclamation claim survived attack. **Composition:** the horizon
list's openness over eager death, independently; the chain
invariant claimed to restate the rule it extends; the ack-thinning
caveat named the wrong site; the differential oracle's "timing
identical" flags correct compiles; "analysis failure selects owned"
was cited to a do-not-rely file; the checkpoint condition depended
on the hand-off's drain shape without naming it; `dev/INDEX.md`
still described revision 1; the family-wide ruling was dropped, not
answered. **Verification:** the placement rule was unsatisfiable
for branch-born borrows; the scan's kill authority failed on
unresolved receivers; no planning artifact recorded the closed
status; the marginal baseline was not computable from the named
channels; the density gate demanded the clock where the crate's
method counts; the shadow count detects only if death defers to it;
the decision log graded itself. Accepted in full; revision 3 was
the fix.

Critic round 3, 2026-08-18, on revision 3's fixes. **Soundness:**
the drop-point policy released an anchor at its last syntactic use
under a live borrow, and the return-site retain's order against the
epilogue was unstated (critical); uniqueness demotion had no sound
lowering local to the borrower's unit (critical); the shadow build's
elided-authoritative death broke the walk's occupancy test, COW and
the release asserts; "can reach a severing store" was not the
predicate purity computes, in both directions; the finality conjunct
was never dischargeable; the placement rule needed SSA and the
back-edge case; the sound configuration's free region is read-only
lifetimes, unstated. Survived a second consecutive round: the chain
invariant's reclamation discharge (re-verified against the
implemented exact test), the release-horizon relocation, the owned
base cases, promotion as a plain retain. **Composition:** eliding a
borrow of a destructor-bearing target moved `__destruct` off the
scope-end pin the design itself cites, and the document both denied
and licensed the move; the owned-slots cross-note dropped "against a
counted entity" and admitted a sentinel-retain reading; the release
horizon's predicate was not purity's; the checkpoint condition's
event binding named the prologue while the dangerous drains run at
arbitrary pickups; the chain extension owed forward notes to the
RFC; the unique-entity path broke the invariant's premise silently.
INDEX.md and PLAN.md matched. **Verification:** the two trivial
bounds made the kill gate unable to fire — the optimistic bound
discarded the very horizons revision 3 added; the purity closure is
the same unavailable inference as receivers, with no channel; the
shadow count was the wrong economics instrument and "exact" hid the
pair-cost dispersion; the channel list was owed too late ("if
accepted"); the certificate overclaimed alias completeness; the
blast radius had no instrument. Accepted in full; this revision is
the fix, with the corpus names still open with Edmond.
