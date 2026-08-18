# Stack-exit epoch GC: five-axis review

**Reviewed:** 2026-08-18, against `dev/STACK_EXIT_EPOCH_GC.md` as pulled
at `995dcee`, by five parallel review passes (protocol races, mutator
cost, architecture cost, family compatibility, economics and gates).
Disposition is Edmond's; this file records the findings so they survive
the session.

## The verdict in three sentences

The document contains one genuinely new and valuable idea — the
anchored borrow, which protects a local by a conjunction of locally
discharged proofs (`live(anchor) ∧ stable_path`) with zero per-holder
state, escapes the 2026-08-18 refusal's disjunction argument entirely,
and composes with today's rc-walk as a retain-elision pass without any
new collector. The collector replacement around it is unsound as
written (one critical race), economically negative on the load path by
an operation-dominance argument, and architecturally a rewrite that
deletes deterministic destruction for the whole language — a semantics
ruling, not an optimization. It also re-enters space closed by two
recorded rulings (`dev/DECISIONS.md`, 2026-08-17 and 2026-08-18)
without citing either.

## Axis 1 — protocol races

- **Critical.** `untouch` is a plain store with no arbitration against
  the retirement CAS, which runs *before* the end handshake: the stamp
  can clobber `RETIRING`, the post-handshake re-check reads hazards
  only, and a full use-after-free interleaving exists from the
  document's literal text. Fix: `untouch` becomes a CAS on the packed
  word; on `RETIRING` it keeps its hazard and waits for cancellation;
  `DEAD` is itself a CAS from `RETIRING`.
- **High.** "Changes the packed observed state from `LIVE(old_epoch)`"
  admits a CAS-retry-loop reading that re-reads the current word and
  thereby deletes the stamp protection. The CAS must be single-shot
  with the candidacy-time expected value, and the post-handshake
  re-check must re-run the full candidate test, not hazards alone.
- **High.** Both handshake guarantees are definitional: they hold only
  if acknowledgements occur at safepoints and never between the epoch
  read and the stamp, or inside the acquisition loop — a rule the
  document does not state. "End handshake **or grace period**" licenses
  the weaker mechanism where the stronger one is load-bearing, and the
  acquisition loop's missing store-load fence is sound only through the
  handshake — a dependency to record before any fast path skips it.
- **Medium.** Two protection encodings (packed word, hazard table) are
  never reconciled per operation; same-thread multiple holders
  (coroutines, nested scopes) have no chosen mechanism even in the
  single-mutator model, and suspended frames' locals are nowhere
  required to be published roots.
- The central invariant itself is a real safety argument, and the
  slot-ABA, RETIRING-source-read and two-walk-retention scenarios hold.

## Axis 2 — the mutator's bill

The unanchored acquisition/exit cycle strictly dominates the
retain/release pair it replaces in every cost class: 3 stores against
2, 3 branches against 1, equal loads but acquire-ordered, plus either
a store-load fence or a dead validating re-load — against a measured
pair of 1.84–1.87 ns with no ordering at all. Anchored loads cancel
out of the comparison, because the same static family elides the
retain under maintained RC too. The whole win therefore lives on
unprovable publications (2.4 ns each), giving the flip condition
`L_u·Δ < P_u·(2.4 − k)` — fewer than ~2.4 unanchored loads per
unprovable publication fence-free, ~0.4 fenced. The identified winning
residue is one shape: fan-out republication of an already-held target,
unmeasured. The ownership barrier concentrates RMWs at exception
unwinds — the least predictable site — inverting the stated
philosophy, and multiple holders reintroduce a count through the back
door while paying a holder census RC's decrement answered for free.

## Axis 3 — what it costs the architecture

- Deterministic destruction: detecting "the last reference died" *is*
  reference counting; the design's one-line exclusion disposes of the
  contract the runtime exists to honour. The only exits are the
  refused hybrid (count destructor-bearing classes) or a language
  ruling accepting trace-time finalization.
- COW and reference collapse read `refcount == 1`
  (`refcount.rs`, `array/entity.rs`); without a count the collapse
  site gives wrong answers, not slow ones — `$b = $a` starts observing
  `$a`'s writes.
- Inventory: of the built substrate, the array version bracket
  transfers cleanly and the deferred-free identity queue transfers in
  shape; the central identity, Phases 3–4, eager death and the corpse
  rule, the checkpoint fabric (acks ride `ll_release`'s death branch —
  deleting it silently reverses the 2026-07-27 decision against
  compiler polls), weak tables, retained blocks and the acyclic flag
  die or need replacement.
- Memory categories do not occur in the document; the arena reset is a
  mutator-side wholesale free the design's "a collector is the only
  physical freer" forbids, and promotion reads hold-counts kept in the
  count field.
- The start handshake gates all reclamation on the slowest thread — a
  30-second blocking call stops every walk process-wide, a regression
  against rc-walk's accepted F2 limit.

## Axis 4 — the family and the rulings

- The stamp-on-**last**-departure reconstructs the refused
  disjunction; the document's own production footnote dissolves it —
  per-thread tables plus stamping the monotone maximum of the acked
  epoch on *every* departure, idempotent, nothing to cancel. That form
  should be the primary model.
- The honest form of the whole design is the refused hybrid with
  hazard tables in place of the bit — or a language-wide semantics
  change; the document must choose and currently does neither.
- Birth count dies vacuously (no count to seed); unique ownership
  mutates from optimization into the sole source of eager death — a
  status change neither document records. The anchored borrow and
  uniqueness's borrow clause are one analysis with two invalidation
  disciplines; the family needs it ruled once.
- After the mandatory machinery, the novelty residue is a lazily-timed
  C4 loaded-value barrier — and the laziness (stamp at departure
  rather than repair at load) is what creates the only hard problem.
  C4's repair-on-load is idempotent and monotone; "last departure" is
  not.
- `dev/DECISIONS.md` needs a successor entry naming this document
  against the 2026-08-18 refusal and its census precondition.

## Axis 5 — economics and gates

- Acceptance-gate condition 5 is undecidable as written: one comparand
  (`dev/NO_RC_PUBLISHED_EPOCH_GC.md`) was refused and deleted
  2026-08-17, the other needs Phase D workloads, the box's noise floor
  is 1.5–3 %, and no threshold is stated. The gate discards the
  document's own countable-metrics list in favour of a clock race.
- The gate's ordering is inverted: the exhaustive model, lowering
  proofs and Loom work precede every cheap kill. The kill list, in
  order: an afternoon canary of the safe-acquisition sequence in
  `bench-external/canary/` against the shipped 1.84 ns pair; a
  compiler-free corpus scan bounding anchored coverage from above; the
  Phase D publish census that three other customers already fund.
- Retained garbage is a categorical worsening presented as a line
  item: every death waits walk period + handshake + grace, against
  today's deaths-in-flight parking, and the grace-period quarantine
  attacks the measured slot-reuse advantage.
- The census baseline (leaks every cycle) should be demoted explicitly
  to a model-checking stepping stone and struck from the gate.

## What to extract regardless of disposition

The anchored borrow, as a retain-elision pass over the existing
rc-walk: unify it with unique ownership's borrow clause into one
IR-level borrow analysis parameterized by the runtime's invalidation
set, and price it with the same Phase D census. It needs none of the
collector to pay off.

**Authorship note, after the review (Edmond, 2026-08-18):** the
anchored borrow *is* the author's idea — "`$b = $a->property` pays no
barrier when the compiler proved `$a` alive" — and the surrounding
no-RC collector, the hazards and the epochs are elaboration the
document grew around it. That places the one surviving element and the
original intent in the same spot: a covering-borrow elision over
maintained RC, sound today because the tracer reaches the target
through the live anchor, with the same three obligations unique
ownership's borrow clause already carries — anchor live for the
borrow, path unsevered, borrow not crossing a checkpoint (a drain-run
destructor is an invalidation point). Where the borrow must outlive
the proof, one ordinary retain at that point restores the full
contract.
