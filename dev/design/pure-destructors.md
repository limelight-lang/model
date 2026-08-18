# Pure destructors

Proposed by Edmond, 2026-08-18: an object whose destructor is absent, or
provably affects only the object's own data, is pure — and a pure object
could be reclaimed by the collector itself, making freeing simpler and
faster. This note is the analysis of that proposal against the code at
`23c3216` and `rfc/model/gc/rc-walk.md`, run the same day through three
lenses (protocol soundness, purity semantics, payoff and composition).
Status: analysis, not design — nothing here is implemented and the RFC
has no section for it yet. The ownership work this composes with is
catalogued in `owned-slots-and-the-walk.md`.

## The design philosophy this is judged by

Stated by Edmond, 2026-08-18: the collector may do more work — that is
acceptable; the mutator strives to do nothing beyond the program's own
code. Unreachable at 100 %, so every mechanism below is weighed by the
mutator-side cost it leaves, and a design that spends collector cycles
to remove mutator cycles wins the tie.

## The verdict, first

Three parts, in decreasing certainty.

1. **The runtime side of the proposal is already built.** The runtime tracks
   destructor absence per class (`CLASS_HAS_DESTRUCTOR`) and per
   instance (`DESTRUCTOR_PENDING`), registers nothing at construction
   for a destructor-less class, frees destructor-less whites raw in the
   rc-trace collector, and skips Phase 4's re-verify whenever no
   destructor ran. Purity as a compile-time fact widens those paths; it
   does not invent them.
2. **"Reclaimed by the GC itself", read literally — the collector doing
   everything on its own evidence — is unsound.** Purity removes user
   code from the drain, but none of the five owner-bound races below
   comes from user code: the exact test, the corpse rule, the guard
   writes and the weak nulling read and write state only the owner may
   touch. That prologue is irreducibly the mutator's.
3. **Purity makes the hand-off drain sound.** The prologue
   stays where today's whole drain already runs — on the mutator, at
   its own checkpoint, so no external stop and no collision with the
   principle that forbids stopping the mutator from outside
   (`rfc/model/gc/rc-walk.md`, "When the collector runs") — and
   everything after it can move to the collector once the residual
   duties named in the open questions are resolved and the tail bound
   is chosen: after the exact
   test, the guards and the weak nulling, no mutator action can reach
   an all-pure component or mint a path to it, so the collector severs
   it and returns its memory. The mutator's Phase 4 share shrinks from
   the whole drain to the prologue — one trace plus one pass over the
   members. That shrinks the per-member cost of the pause the review
   names (`dev/RC_WALK_CRITICAL_REVIEW.md`, "Phase 4 can create an
   unbounded mutator pause"); the O(component) prologue floor remains,
   because the exact test cannot be split. Purity is load-bearing: an
   impure destructor may store a member into a live slot, after which
   a half-collected component is reachable and none of this holds.

What freeing becomes, per case:

| Death | Today | Under this design |
|---|---|---|
| ordinary zero-count death, any impure class | dispose, then free | unchanged |
| ordinary zero-count death, P0 class | guard retain and release, two further header loads and three branches in `ll_default_dispose`, then free | specialized dispose: that phase removed (buildable today, no compiler) |
| condemned component with an NR or impure member | the whole drain on the mutator | unchanged |
| condemned component, every member P0 — or P2 under the order ruling | the whole drain on the mutator | the prologue on the mutator; sever and memory return on the collector |

## The purity ladder

A dying object's own fields are dead stores: any write a destructor
makes into `$this` is discarded by the free that follows. That collapses
most candidate tiers.

| Tier | Definition | Compiler obligation | What the runtime may skip |
|---|---|---|---|
| **P0** | no `__destruct` in the hierarchy | none — the class linker computes it today | everything: no registration, no guard pair, no re-verify contribution; the cycle collectors already key on it dynamically |
| **P1** | body writes only own non-counted slots; no calls except provably effect-free ones; **no throw**; external reads allowed (their results are dead) | intraprocedural effect analysis plus a no-throw proof | erased to P0: the compiler clears `CLASS_HAS_DESTRUCTOR` and emits no registration — no new runtime state at all |
| **P2** | P1 plus nulling own counted slots (`$this->x = null`, `unset`) | P1 plus null-only counted writes, no hooked or dynamic properties | the sever nulls those slots anyway, so P2 differs from P0 only in the order of child releases inside one teardown — if that order is declared unspecified, P2 erases to P0 too |
| **NR** | impure (I/O, external writes) but no address reachable through `$this` — `$this` itself or any member it can name — escapes into anything outliving the call; no throw | escape analysis of every address reachable through `$this`, through every callee | the resurrection machinery: the guard pair on ordinary death and the Phase 4 re-verify — never timing or thread freedom |
| P3 "reads external, writes none" | — | — | collapses: pure reads are unobservable, so P3 is P1; any output channel makes it NR |

The no-throw obligation is hard for P1, P2 and NR: an exception leaving
`__destruct` carries `$this` in its backtrace — an escape channel the
resurrection audit cannot otherwise close. PHP 8 arithmetic and typed
properties can throw; hypothesis, undecided until the corpus scan
below: this obligation prunes the passing population more than any
other rule.

The resurrection argument, precisely: resurrection requires a counted
store of a member's address into a slot that outlives the destructor
call. P0 runs no code; P1 writes only own scalars; P2 writes only nulls;
NR forbids by proof the escape of any address reachable through
`$this` — a member's address included, which is why the obligation is
wider than escape analysis of the receiver alone. Weak cells are
nulled before any
destructor runs, closing the one non-store channel. A P2 destructor's
own-slot stores preserve the exact-test equality term by term:
displacing an in-component child moves `rc` and `indeg` together, and
an external store touches no member row. So for a component of ≤ P2/NR
members, `rc` cannot come to exceed `indeg + guard`, and the re-verify
is redundant by construction.

## What is already built

- Construction is free for P0: generated code emits
  `ll_object_constructed` only where the class has a destructor, and
  the call returns immediately otherwise (`object.rs`).
- rc-trace frees whites raw — no dispose, no guards — once nothing in
  the white set owes a destructor (`gc.rs`, the white-free arm).
- The drain and the synchronous walk skip the guard-discounted
  re-verify when no destructor ran (`walk.rs`, `any_destructor_ran`);
  the comment there already names destructor-less classes the common
  case.
- The arena reset logs only destructor-bearing instances, so a P0
  corpse costs the reset's destructor fixpoint nothing — a corpse that
  is a weak target still costs its cell nulling in the reset's weak
  pass; and `promote.rs` already
  reserves the compile-time hook this proposal fills — "a 'pure'
  destructor needs no re-trace — the runtime stand-in for the
  compile-time purity class".

## P0 gains available without compiler work

1. **A specialized dispose.** For a P0 class, `ll_default_dispose`'s
   phase 1 is discarded knowledge: the guard retain and release, two
   further header loads, the pending test between them and the
   `refcount > 0` branch, all answering a question the class settles at
   link time. The descriptor already reserves a per-class dispose slot;
   a generated one omits the phase whole. How hot the no-destructor
   death path is on a real heap waits on the census extension below.
2. **A raw-sever arm in the drain.** For a component whose every member
   tests `DESTRUCTOR_PENDING`-clear, port the rc-trace white-free
   precedent: sever and free without guards and without per-edge
   `ll_release`, external children still deferred through the barrier.
   This is "freeing simpler and faster" for the destructor-less case —
   the common case if the corpus scan confirms it — by a per-member
   flag test the drain already performs.

## The five owner-bound races

Why the literal reading fails: each of these runs on the owning mutator
for a reason purity does not touch.

1. **The exact test reads current counts.** Counts are plain non-RMW
   fields under a single-writer contract; only the owner reads them
   race-free and current, and the test's trace reads plainly — against
   a running mutator that is undefined behaviour, not staleness.
2. **The corpse rule needs atomicity with releases.** Off-thread, a
   member can die between the collector's read and its guard write —
   the guard is written into a corpse's header and teardown runs
   twice.
3. **Guard and unguard are whole-word header stores.** They race the
   mutator's narrow count stores as a lost update; the channel that
   keeps this reachable is `ll_weakref_get`'s retain, until the cells
   are nulled.
4. **Weak nulling is owner-TLS.** The weak table is a per-thread plain
   map no other thread may even reach; skipping the nulling instead
   leaves race 3 open permanently.
5. **Teardown has owner-bound side duties.** `ll_entity_die` reports
   into the owner's reset window (TLS — consulted on the wrong thread it
   silently no-ops against an open reset), its journal record is
   written into the calling thread's ring (misattribution, not
   corruption), and external-child releases decrement counts of live,
   reachable entities — unfixable by any component-local argument; they
   must round-trip to the owner in every design.

The heap itself is not the blocker: cross-thread free exists
(`Heap::free_remote`), and parking for the owner is the deferred-free
queue's existing shape. The hand-back channel is the missing piece:
the verdict protocol runs in one direction today, and a component the
mutator returns to the collector needs the other.

## The hand-off drain

The target mechanism under the philosophy: the mutator runs the
prologue at its own checkpoint and hands the component to the
collector; the collector does the rest. Scope: condemned components
only — an ordinary zero-count death of a pure object is unchanged.
Eligible is a component whose every member is runtime-P0 after the
compiler's erasure, or P2 under a specified-order ruling — P2
destructor calls run in the prologue, after the weak nulling. An NR
member routes its component to the unchanged whole drain: NR
destructors do I/O, and the hand-off buys nothing worth extending its
soundness argument over them. Death timing is unchanged for the
members: the weak nulling in the prologue is where their death becomes
observable, exactly as today; only the sever and the physical release
move. The external children do die later — their release batch runs at
a checkpoint after the one that ran the prologue, where today the
whole drain releases them inside one visit; that delay is a cost of
the design, named beside the epoch-duration one below.

```mermaid
flowchart TD
    subgraph M ["mutator, at one checkpoint visit"]
        A["prologue, four steps in one visit:
        corpse rule + exact test + guards + weak nulling
        (any P2 destructor calls run here, last),
        then the component is handed to the collector"]
        E["external children released at a later
        checkpoint — an ordinary release batch"]
    end
    subgraph C ["collector thread"]
        S[sever every member]
        D["write the zero and return the memory —
        free_remote, or park for the owner"]
        X["post the displaced external children
        back to the owner"]
    end
    A --> S --> D --> X --> E
```

The prologue completes within one checkpoint visit, with no return to
program code between its steps — that is a scheduling rule, not a
lock: nothing else runs on this thread. A P2 destructor call inside
the visit is not such a return — today's drain already runs
destructors at the checkpoint — and the gap between those calls and
the hand-off needs no argument beyond P2's own: its writes stay inside
the dying component. What a split loses, per gap: program code between
the exact test and the guards can move a reference and invalidate the
test's result; between the guards and the weak nulling,
`ll_weakref_get` can still mint a strong reference into the component.

Why the tail is sound off-thread after the prologue and only then: the
exact test proved no external counted reference exists, the weak cells
are nulled, and purity rules out a destructor having minted a new
channel — so the mutator can neither reach the members nor mint a path
to them, and the collector's cell stores and header writes race
nothing. The cross-thread memory return exists today
(`Heap::free_remote`, and the buffer arena's remote stack); what does
not exist yet is named in the residual-duties question below: the
owner's reset-window record and the journal attribution must either be
proven unnecessary on this raw path or routed to the owner with the
external children, and the hand-back channel itself must be built.
Those children are the irreducible round-trip: they are live,
mutator-reachable entities whose counts only the owner may touch, so
the collector posts the displaced list back and the owner releases it
as an ordinary batch.

What the mutator still pays, and why each piece stays:

| Mutator work | Why irreducible |
|---|---|
| the prologue, O(component) | counts are owner-written plain fields; weak table is owner TLS |
| the external-child release batch | live entities, single-writer counts |
| checkpoint acks | the epoch's ordering, unchanged by this feature |

A fallback exists while the residual duties are unresolved: the same
pipeline entirely on the mutator, its tail sliced across checkpoints
while the prologue stays one visit — sound by the same unobservability
argument and needing no hand-back channel, but it leaves the tail on
the mutator, a cost under the stated philosophy.

The cost has a name in both forms: the verdict stays outstanding until
the tail completes, so the component holds the epoch open longer, and
the review prices epoch duration in parked memory
(`dev/RC_WALK_CRITICAL_REVIEW.md`, "Deferred memory is unbounded in
epoch duration"). A completion bound — a deadline on the collector's
tail, a per-checkpoint slice budget in the fallback — is part of this
design, not an option on it; the number is the tail-bound question
below. And the mutator's pause does not vanish: the prologue
is one trace plus one pass over the members, because splitting the
exact test would reintroduce the mutation-between-slices problem it
exists to close.

## The trust model: a soundness-bearing bit

The acyclic class flag is recall-bearing — a wrong flag leaks and frees
nothing early. A purity flag used for the re-verify skip or the sliced
tail is soundness-bearing: a wrongly-erased effectful destructor
silently drops user effects, and a wrongly-NR class frees a resurrected
component — a use-after-free. Purity therefore sits in the trust class
of `dispose` and `traced_runs`, beside the birth count's constant and
the uniqueness proof, not beside the acyclic bit — the RFC section that
introduces the flag owes that sentence. Only NR needs a new descriptor
bit at all; P1 and P2 are expressed by clearing `CLASS_HAS_DESTRUCTOR`,
whose doc already reads "with side effects" as if anticipating exactly
this erasure. Purity is re-derived per most-derived class against its
resolved vtable — a pure parent destructor calling an overridable
method is impure in the subclass that overrides it — and anything not
closed at analysis time (late classes, dynamic properties, hooks,
invoked closures) defaults impure.

## Composition, in one paragraph each

**With unique ownership** (`owned-slots-and-the-walk.md`): a unique
entity of a P0 class dies at its owning slot's overwrite with no
teardown protocol left but its children's releases; add "no counted
fields" and the overwrite is literally a plain store plus a parked
free — the fast class is unique + P0 + leaf. The same composition thins
the checkpoint fabric: a unique death bypasses the displaced
reference's death-branch ack, and the unique + P0 + leaf overwrite has
no teardown exit either, so a workload converted wholesale to that
class passes neither ack site, its ack rate falling to what the
unconverted remainder produces (unmeasured) — and the review's "Epoch
progress has no bound" finding gets more pressing, not less. The full
accounting, including how the fast class can block its own memory
return, is the balance paragraph of `owned-slots-and-the-walk.md`.

**With the acyclic flag**: same delivery channel, different analysis,
different failure tier (above). Acyclic + pure + weak-free is the true
fast class — never walked, never drained, dies at zero through a plain
free, parked while an epoch is open — and shrinks the census and the
per-epoch metadata
(`dev/RC_WALK_CRITICAL_REVIEW.md`, "Per-epoch graph metadata is
heavy").

**With the birth count**: mechanically orthogonal — birth-side versus
death-side, no shared state; they share only the compiler-proof
delivery and the Phase D gate.

**With the arena reset**: P0 corpses already cost zero; what the
compile-time bit adds is skipping the dirty re-trace that today
over-triggers on allocation by a destructor that only touched its own
dying fields, and fixpoint convergence in fewer passes for strictly
pure destructors (unmeasured). The COW reconciliation is untouched.

## Prior art, briefly

None of the sources below were re-read at a pinned revision for this
note; verify before citing as ground truth (`dev/RESEARCH.md`).
Java deprecated finalizers for legal resurrection, unspecified
ordering and unbounded delay (JEP 421); purity removes the first two
by construction and makes the third unobservable. Boehm's ordered
finalization never finalizes cycles at all, which is what cross-object
observation costs; purity makes destructor order inside a component
unobservable. .NET's `SuppressFinalize` is the precedent for a
first-class no-finalizer path. Swift's `deinit` is the timing contract
this runtime already honours and must keep for everything impure.

## What to measure first

1. A static corpus scan outside the crate: the fraction of classes
   declaring `__destruct` in php-src's stdlib and a Packagist sample,
   and of those, the fraction with trivially pure bodies. If
   destructor-bearing classes are rare, P0's existing paths plus the
   two buildable-today gains are nearly the whole payoff. Unmeasured.
2. A census extension (`walk::Census`): per-flag counts — destructor
   presence, weak-target bit, later the acyclic and purity bits — to
   price the fast class on a real heap. Mechanism needs no compiler;
   population numbers wait on Phase D workloads.
3. `EpochStats` extension: per-component member count, drain duration
   and an "all-P0, weak-free" bit — the number that decides whether the
   hand-off tail is worth building. Blocked on the production driver,
   as is the review's own recommendation under "Phase 4 can create an
   unbounded mutator pause".

## Open questions, each with its owner

1. **Is child-release order inside one teardown specified?** (Edmond —
   language surface.) Unspecified: P2 erases to P0 and the ladder is
   two-tier. Specified: P2 keeps its call and sheds only resurrection
   machinery.
2. **The hand-off tail's residual duties.** (Design.) Three must be
   resolved before the collector may finish a component: the owner's
   reset-window record (`ll_entity_die` reports completed teardowns
   into owner TLS — prove the raw pure path owes nothing there, or
   route it back), journal attribution (records are written to the
   wrong thread's ring — acceptable or not), and the hand-back channel
   itself, a second direction the verdict protocol does not have today.
3. **What bounds an open tail?** (Design, with Edmond's pressure
   ladder.) The deadline on the collector's tail — and in the sliced
   fallback the per-checkpoint budget — against the parked-memory
   currency of `dev/RC_WALK_CRITICAL_REVIEW.md`, "Deferred memory is
   unbounded in epoch duration"; which rung forces completion.
4. **The weak-table redesign** — a per-entity weak cell reachable by
   address would make nulling one atomic store from any thread. Under
   the hand-off design the nulling stays in the mutator's prologue, so
   this is worth costing only if the prologue itself is ever to leave
   the mutator; the `get()` fast path and `rfc/model/weak-references.md`
   both move.

## Recommended order

- **A. Runtime-only P0** — the specialized dispose and the raw-sever
  drain arm, still on the mutator. No ruling needed, no compiler
  needed, measurable today; it also builds the raw path the hand-off
  tail will reuse.
- **B. The hand-off drain** — the philosophy's target: prologue on the
  mutator's checkpoint, tail on the collector. Needs the residual
  duties resolved and the tail bound chosen; the sliced mutator-side
  fallback is the intermediate step if the duties resolve slowly.
- **C. Compiler tiers** — P1 erasure first (zero new runtime state),
  P2 after the child-release-order ruling, the NR bit last and only if
  the escape analysis earns its keep. Wholesale conversion of a
  workload to the unique-pure fast class additionally waits on the
  compensating poll rule (`owned-slots-and-the-walk.md`, open
  questions).
