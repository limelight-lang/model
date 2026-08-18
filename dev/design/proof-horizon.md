# Proof Horizon: borrow protection paid where proof ends

**Status:** design sketch, not implemented. Revision 2: Critic round 1
(2026-08-18, three lenses — soundness, composition, verification)
found revision 1's core rule, "local references are never counted",
unsound on three independent axes; this revision replaces it with the
owned/anchored lattice below. The round's record is at the end.
Further rounds pending.
**Author of the algorithm:** Edmond, 2026-08-18. Successor to the
stack-exit epoch model (`docs/history/stack-exit-epoch-gc-2026-08-18.md`,
superseded the day it was recorded); shaped by that model's five-axis
review (`dev/STACK_EXIT_EPOCH_GC_REVIEW.md`) and by the two standing
refusals in `dev/DECISIONS.md`, 2026-08-17 and 2026-08-18.

## The algorithm in two sentences

A local is either **owned** — a counted holder paying today's
retain/release pair — or an **anchored borrow**, paying nothing while
the compiler's proofs hold: `live(anchor) ∧ stable_path`, with every
anchor chain ending in an owned local. A borrow pays only at a
**proof horizon** — a point its proofs stop covering — and the
payment is promotion to owned by an ordinary retain, emitted once per
lifetime at a point that dominates every horizon in its scope.

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
the drain freed under a live borrow — at exactly the checkpoints the
purity clause declared safe. And COW's `refcount == 1` uniqueness
test read a count missing its local holders, so shared arrays mutated
in place. All three vanish when owning locals keep their count: death,
`__destruct` timing and COW read the same numbers as today, and every
borrow is covered by a counted root.

## The ownership lattice

Every IR local is in one of two states, assigned by the compiler.

**Owned** — a counted reference, today's code exactly: acquisition
retains (or absorbs the creation reference of `new`), scope exit
releases, eager death at zero, `__destruct` at the release that
reaches it. Owned by construction: the result of `new`; a call
result that transfers ownership; every reference to a COW-eligible
value — array, string, reference box — because their uniqueness test
reads the count and an uncounted holder falsifies it; and every
local the analysis fails on. Analysis failure selects owned, never
guesses anchored — the rule the five-axis review recorded and this
design keeps.

**Anchored** — an uncounted borrow, `$b = $a->property` as a plain
load. The chain invariant: the anchor is an owned local, or a borrow
whose own chain ends in one. This is rc-walk's legality rule for
uncounted borrows stated as an IR invariant — a borrow is covered
only by a counted reference the collector treats as a root, and "a
field of some heap object" does not qualify, because that object may
itself be garbage. The invariant makes the covering root a frame
slot in every case, so the exact test can never condemn a component
a live borrow points into.

## Inside the horizon: what the borrow must prove

The borrow's three obligations are the anchor outliving it, the
anchor-to-target path staying intact, and every operation able to
invalidate either being visible in the IR. The same IR analysis
serves unique ownership's borrow clause (`rfc/model/gc/rc-walk.md`,
"Unique ownership"), but the invalidation disciplines differ, as the
five-axis review already noted: the ownership clause forbids a borrow
to survive a checkpoint outright, while here a borrow crosses a
checkpoint under the two conditions below.

A checkpoint threatens a borrow in two distinct ways.

**Reclamation.** The drain severs and frees condemned components
whether or not destructors exist — the P0 raw-sever arm of
`dev/design/pure-destructors.md` runs no user code at all. The chain
invariant answers this unconditionally: the exact test balances
counted references, the chain ends in a counted frame root, so no
component on the anchor path is condemnable. Discharged by
construction, at every checkpoint.

**Path severing.** A `__destruct` body run by the drain can store
into the anchor path. The proof this needs is reverse reachability:
from every destructor the drain might run to the classes on the
anchor path. Transitive purity (`dev/design/pure-destructors.md`,
"Purity is transitive") walks the other direction — from a dying
class down through its counted fields — and its root set is one
class, while this proof's root set is whatever the epoch condemned.
So purity is input to this analysis, not a substitute for it, and
revision 1's claim that a destructor-freedom summary "lifts the
restriction" was inverted: purity is precisely what lets the drain
free fastest. Until the reverse analysis exists, a checkpoint not
proven free of destructor-bearing deaths is a horizon.

## At the horizon: promotion

A horizon is any of: a call without a trusted summary, dynamic
dispatch the class set cannot close, reflection, a by-reference
escape, a checkpoint that fails the path-severing condition, an
own-code store that severs a borrowed path.

The payment is promotion: one ordinary retain, after which the
borrow is an owned local — released at scope exit like any other.
Placement rules:

- The promotion point **dominates every horizon in the borrow's
  scope and the scope's exit**, so the exit release is unconditional
  and no path-dependent bookkeeping exists. The closest such point
  minimizes overpayment; the farthest is the borrow's birth, which
  is owned-from-birth and costs exactly today's pair.
- A loop containing a horizon promotes before the loop, not per
  iteration — the dominator rule implies it, and it is what keeps
  the horizon payment "once".
- On unwind, a landing pad releases the owned set live at its call
  site; because the promotion point dominates the sites after it,
  that set is static per site, with no merge ambiguity.

The rule bounds the scheme's cost: a promoted borrow pays one pair,
which is what the same borrow pays today, so per borrow the scheme
never costs more than the current code, and the whole difference is
the borrows that are never promoted. Overpayment — a promotion point
sitting on a path that reaches no horizon — loses savings, never adds
cost.

The collector is untouched, and now the sentence is true: promotion
is a compiler-emitted retain, there is no protection set, no
candidate-test arm and no death-branch test. Revision 1 carried those
three from an earlier draft while also claiming "no walk-side
changes"; the contradiction is resolved by striking the mechanism,
and the acquittal it would have provided is unnecessary under the
chain invariant.

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
both defaults land on today's behaviour. The selection heuristic is
economic: horizon classes are the closed, summary-friendly types in
provable scopes; counted classes are the ones crossing reflection,
callbacks and suspensions, where analysis costs more than the pairs
it removes.

Granularity — open question 4, narrowed by round 1. Per-site
deviation from the class default is lawful in mechanism but
unauditable: no runtime observation distinguishes a deviating site
from a miscompiled one, so deviation stays barred until the compiler
emits a per-site decision log an auditor can replay. Revision 1
argued for class granularity on the ground that the bit "hardens
into a runtime property if a form B is ever pursued"; that argument
is deleted — under form B a `mixed` slot holding both regimes needs
a per-object discriminant and a dispatching barrier, which is a
rewrite, not a hardening. The recommendation on record: class-only
now, revisit when the decision log exists. The ruling is Edmond's.

## The two forms

**Form A — over maintained heap RC. This is the design.** Heap
counts, owned-local counts, eager death, `__destruct` timing, COW,
the arena's counted promotion and rc-walk's protocol are today's.
The elision applies to anchored borrows only.

**Form B — without heap RC** is the superseded model's road, priced
by the five-axis review and not pursued: the architecture inventory
dies with the count, deterministic destruction needs the count back,
and the walk's write barrier is the count.

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
  it: owned locals keep the count that drives eager death. Revision
  1, which uncounted every local, lost this — the third recorded
  problem was the one it silently failed to dissolve.

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
pair: retain and release" — revision 1 cited "The counted publish",
whose figure is a different number). The population excludes owned
base cases: `new` results, ownership-transferring returns and every
COW-eligible value are outside the elidable set, which revision 1's
"local acquisitions" wholesale count overstated. Promotion is
cost-neutral against today (the bound above), so the formula has no
negative term; what varies is only how much of the borrow population
the proofs keep free.

The baseline is marginal, not gross: unique ownership's borrow
clause and the birth count elide overlapping slices of the same
traffic and are priced by the same census, so `saved` is defined
against today *plus the already-designed pair*, and the census must
report marginal shares per design — summed gross claims would exceed
the actual pair bill.

Resolution on this box: `dev/BENCHMARKS.md` puts the noise floor at
1.5–3 %. At 1.85 ns per pair, 1.5 % of a second of runtime is about
8 M pairs, so a workload eliding fewer than roughly 8–16 M borrow
pairs per second cannot confirm the effect here; the Phase D bench
plan needs at least one shape above that density.

Measurement order, and what each can decide:

1. **Corpus scan, compiler-free, kill-only.** Per *lifetime*, not
   per call site: a lifetime counts as horizon-free only when every
   call it spans passes the summary proxy (final/private/builtin
   callee, no reflection, no by-ref) and none of the other horizon
   kinds intervenes. A per-call-site fraction bounds nothing — a
   lifetime spanning zero calls is free at any site fraction, and
   one unsummarizable call in every lifetime zeroes the true
   fraction at any site fraction. The corpus is deployed PHP
   applications with their vendor trees, named before the scan runs;
   php-src is C source and measures nothing about PHP-code
   lifetimes. The scan records the referent's static class where
   one is known, so the hybrid can price regimes per class. It is
   kill-only: a low fraction closes the summary investment; a high
   fraction funds nothing by itself, because the proxy
   over-approximates summarizability — a `final` method that stores
   `null` into a field severs paths and still passes it.
2. **The Phase D publish census** — with three channels this design
   needs, named now so the instrument is built carrying them:
   borrow-acquisition density per class, horizon crossings per
   borrow lifetime, and live borrows per horizon. The census as
   recorded (`dev/DECISIONS.md`, 2026-08-17 and 2026-08-18) counts
   publishes, which prices birth count and unique ownership but
   none of the three quantities above. The channel list is owed to
   `dev/DECISIONS.md` if the design is accepted.

The default at ambiguity: the design stays closed until the census
decides. The corpus scan can only close it earlier, never open it —
which resolves the gate direction revision 1 left pointing both
ways.

## Verification artifacts, a precondition of implementation

Form A's virtue — the collector never learns the feature exists —
removes every runtime detection point: a misplaced horizon and a
correct elision are the same instruction stream, so a compiler bug
surfaces as corruption far from its cause. Three instruments are
owed before any lowering ships:

- **Shadow-count lowering.** A debug build emits the classic pairs
  into a shadow counter beside the elided real one; scope exit and
  death cross-check the two, and a divergence names the site. This
  is the seen-red instrument for the borrow analysis itself.
- **Differential lowering.** The same program built with horizons
  off and on, diffing death order and `__destruct` timing — the
  lattice claims both are identical, and this is the test of that
  claim.
- **Summary versioning.** A summary is a soundness assumption about
  a callee, so a stdlib update that adds a destructor or a severing
  store invalidates every caller compiled against the old summary.
  Open question 1 carries the versioning rule; without one, every
  stdlib update is a silent soundness event.

## Composition with the designed family

- **Unique ownership:** one borrow analysis, two invalidation
  disciplines — the ownership clause bans checkpoint crossing
  outright, this design substitutes the chain invariant plus the
  path-severing proof. Revision 1's "the same three obligations"
  overstated the match.
- **Birth count:** adjacent populations, marginal accounting per the
  economics above.
- **Pure destructors:** purity feeds the path-severing proof and
  does not discharge it (the transpose obligation above); the P0
  fast paths and death itself are untouched, because owned locals
  keep the count.
- **rc-walk and S28:** the collector code is untouched, but the
  protocol has a dependency to name: elided borrow releases thin the
  checkpoint-ack sites riding `ll_release`'s death branch — the same
  thinning `dev/design/owned-slots-and-the-walk.md` prices for the
  fast class, answered by the compensating-poll rule that is that
  document's open question. Horizon-class-heavy code inherits that
  dependency; "untouched" without this caveat was revision 1's
  overclaim.

## Open questions

1. The summary language: what a summary states — severable paths,
   destructor reachability — who writes stdlib summaries, the
   conservative default at every unknown (a horizon, always), and
   the versioning rule from the verification section.
2. Borrow scopes across suspensions: a yield is a horizon unless the
   summary system learns resumption points, and a fiber suspended
   across an arena reset carries frame borrows the reset cannot see
   — one question, and it shapes the IR early.
3. Whether the corpus scan's fraction justifies summaries at all —
   the kill-only gate above.
4. The hybrid's granularity, narrowed by round 1: class-only, or
   per-site deviation behind a compiler-emitted decision log. The
   recommendation on record is class-only until the log exists; the
   ruling is Edmond's.

## The record

The name is `proof-horizon`. The superseded model and its review
stay as the map of the space already searched: the refusals of
2026-08-17 and 2026-08-18 closed the no-heap-RC roads, and this
design keeps every count they defended — including, since revision
2, the owned locals' — and removes only the pairs the proofs make
redundant.

Critic round 1, 2026-08-18, three lenses. **Soundness:** uncounted
owning locals leak every acyclic local-only object and move
`__destruct` timing in both directions; the checkpoint hazard is the
drain's own sever-and-free, so the destructor-freedom lift was
inverted; COW's uniqueness test read a falsified count; the
protection-set paragraph contradicted "no walk-side changes"; loop
and conditional horizons made "pay once" unsound without a placement
rule. **Composition:** the elision re-entered rc-walk's
uncounted-borrows prohibition (chains not ending in a counted root)
and dropped Deutsch–Bobrow's freeing half while citing the
deferral; "the same three obligations" misquoted unique ownership's
borrow clause; the superseded model's third recorded problem is
deterministic destruction, which revision 1 did not dissolve; the
pair-cost citation named the wrong section; the ack-thinning
dependency went unnamed. **Verification:** the per-call-site scan
bounds no lifetime fraction and could not fund; the census lacks
every channel this design needs; the publication rule's per-crossing
reading admits unbounded negative savings; family savings
double-counted at the shared gate; no falsification artifact
existed. Accepted in full — this revision is the fix: the lattice,
the two-condition checkpoint rule, dominator promotion with the
cost bound, COW exclusion, the kill-only per-lifetime scan, the
named census channels, the marginal baseline, and the verification
artifacts.
