# The mutator-cost case

The case for one claim: **on the mutator's hot paths this runtime
carries no known avoidable work, within the stated resolution of the
instruments that measured them.** The evidence is three-legged — fresh
same-HEAD measurements with per-instrument resolution lines, naive
C++ canaries in the same binaries as the shipped operations, and
instruction-level decompositions tying every hot-path instruction to a
contract sentence. Every figure in this file is a citation into
`dev/BENCHMARKS.md` by entry title and date; **that file stays
normative — where the two disagree, this file is stale and the journal
is right.** The claims this case does not make come first, because
they are the half a reader will ask about.

> **The two build configurations these figures name were deleted on
> 2026-08-26.** `rc-walk` and `rc-trace` are gone from the crate, and the
> code that produced every measurement below is on the branch
> `archive/pre-rc-cycle`. The figures stand as the baseline `rc-cycle` is
> to be measured against, not as a description of what the tree builds
> today (`dev/DECISIONS.md`, "where the two deleted collectors live:
> `archive/pre-rc-cycle`").


## What this case does not claim

- **No end-to-end speed claim.** No PHP executes on this runtime yet;
  Phase D (the vertical slice) is unbuilt. Every figure here is a
  micro-path under a probe, not a workload.
- **No counted-operation density.** How often real PHP retains,
  releases and publishes per unit of work is unknown until real PHP
  runs. The per-operation figures bound the tax rate, not the tax.
- **No Zend comparison.** The one comparison that would answer "faster
  than PHP?" needs Phase D. The canaries are the external comparand
  until then, and they compare operations, not languages.
- **No cross-thread claim.** The counter is non-atomic by the
  single-mutator design; every figure here is single-threaded, and the
  11.6 ns atomic row below is what the design refuses, not what it
  pays.

## Method: how a figure earns a row

Three rules, each bought by a review finding this stage recorded.

1. **One code state, own resolution.** Every quoted figure was
   re-taken in per-bracket sessions across one day's commits, from
   `b18f6d2` on — a span that touched no non-test library source: the
   changes were documents, `bench-external/`, comments in one test
   module and a bench arm — with each instrument's repetition spread
   stated beside its figure
   ("fresh brackets on one HEAD, and the 2.78 contradiction dies of
   staleness", 2026-08-16 — the entry in which July's 2.78 ns pair
   figure died of staleness rather than being explained away).
2. **The comparand is a canary, not a self-authored floor.** A naive,
   clean C++ loop does the same job in the same binary, past
   disassembly acceptance re-run on every rebuild
   (`dev/DECISIONS.md`, "the performance case's external comparand is
   a canary, not a self-authored floor"). A bare canary prices its
   loop, never "naive RC": it carries no flags test, no immortality
   gate, no null path.
3. **Differences below the instrument's measured zero print
   "unresolved".** The canary binary's zero is a whole-pass mode:
   ll-shaped arms sit in states ≈ 0.21 ns apart, so a difference under
   ≈ 0.4 ns between them is unresolved at this instrument's zero ("the
   pair against its canaries", 2026-08-16). The in-lib store probe's
   zero is its null sweep, within ±0.04 ns per record at the wide hot
   point ("the null sweep bounds the instrument", 2026-08-16).

## The pair: retain and release

| shape | figure | source entry, 2026-08-16 |
|---|---|---|
| in-crate, rc-walk | 1.84–1.87 ns | "fresh brackets on one HEAD" (A/A2 spread 1.6 %) |
| in-crate, rc-trace | 2.25 ns | same |
| through a real ABI call | 1.77–2.20 ns, mode-stepped | "the pair against its canaries" |
| bare non-atomic inc/dec + branch | 0.55 ns | same |
| `std::shared_ptr` scope pattern, second thread alive | 11.57 ns | same |

The counted pair under rc-walk measures 18 % below its rc-trace
sibling and ≈ 3.6x a bare inc/dec no shipping runtime is — through a
real call, while production inlines, so the ratio is an upper bound.
The atomic scope pattern a multi-threaded ARC pays costs 5.8x our
pair, with the caveat inline: that row carries a null-check destructor
branch and dispatch beside its two locked operations, so it prices the
pattern, not atomics alone. The ABI call's own price is **unresolved at
the canary instrument's zero**: in the canary's most common mode the
pair sits 0.11–0.14 ns above the in-crate figure ("fresh brackets on
one HEAD"), the declared bias direction, but the arm's modes span
0.21 ns steps and its low mode inverts the sign ("the pair against its
canaries"), so the entries state the mode-conditional fact and this
file does not strengthen it into a price.

## The counted publish

| shape | figure | source entry, 2026-08-16 |
|---|---|---|
| counted publish, heap into heap, hot | 2.74–2.82 ns | "the null sweep bounds the instrument" (session floor 0.5–3.3 %) |
| plain 8-byte pointer store, hot | 0.33 ns | "store and lifecycle canaries" |
| the release-at-reset record | ≈ 0.45 ns hot, ≈ 0.5–0.6 cold | "the null sweep bounds the instrument" |

The ≈ 2.4 ns between a counted publish and a plain store is the
semantics, priced: the retain, the category test and the COW door —
cross-instrument, which both measured zeros permit at this effect
size. The record's figure stands net of a null sweep whose own slope
is zero by construction. One caveat the journal carries: the barrier
cannot be reached from C at all today — no ABI door constructs a
context — so the publish comparand is the in-lib probe by necessity,
not by preference ("store and lifecycle canaries", 2026-08-16).

## The lifecycle

| shape | figure | source entry, 2026-08-16 |
|---|---|---|
| `ll_reference_new` + release-to-death (24 B, full teardown, hot) | 3.4–4.0 ns steady | "store and lifecycle canaries" |
| `malloc` + three-word init + `free` (24 B, glibc, hot) | 6.4–6.9 ns | same |
| classed object create+die, in-crate | 16.45–16.93 ns rc-walk (A/A2), 14.27 rc-trace | "fresh brackets on one HEAD" |

In its steady mode the full entity lifecycle — factory contract,
kind-dispatched teardown, slot recycling — runs about half glibc's
malloc/free on the same size. Two honest asterisks travel with the
row and are quoted from its entry: a 14–15 ns first-pass mode per
process, unresolved, sitting near the classed-object figure, so the
two entity kinds must not be conflated; and 1–2 rounds in 15 spike to
44–52 ns. The rc-walk tax on a classed create+die holds at +15–18 %
against rc-trace — the checkpoint test, decomposed in
`docs/performance-case-decompositions.md`, and the parking branch,
which sits inside the measured figure and inside no listing: the
decompositions' own scope line says so.

## The decompositions

`docs/performance-case-decompositions.md` ties every instruction of
the pair, the counted publish and the death branch to a named contract
sentence or a residue row with its removal lead. The residue is short:
the out-of-line ABI doors' call frames (their removal lead is the
merged-bitcode inlining, observed once for `ll_retain` on an older
toolchain and carried as design for the store), and two instructions
of the release's counter ride, priced to the relaxed-atomic annotation
the concurrent collector demands. Everything else on those paths is
contract. This is the sense in which "no known avoidable work" is
meant: not that the paths are cheap, but that each instruction has a
sentence it answers to.

The claim has one piece of removal evidence behind it, and the case
cites it rather than leaving it in the journal: the last avoidable
work found on these paths was priced and deleted. Wide header reads
over fresh narrow stores cost ≈ 3.0 ns per store — measured directly
by a scratch-branch revert, a per-occurrence penalty of which only
≈ 0.6 ns hides under independent work ("the failed store-forward is
the stall itself, not the log serialized behind it", 2026-08-16) —
and the narrowing that removed them took `heap → arena` from
4.82 to 1.53 ns (`dev/DECISIONS.md`, 2026-08-15, "a header is read as
narrowly as it is written, and through the helpers only"). The width
rule the decompositions cite on their contract rows is what that
finding left behind.

## The collector's costs

The mutator pays the collector nothing per operation, and that zero
is bought on the collector's side; the case states both prices from
its entries of 2026-08-16:

- **Time, off the mutator's thread**: a stepped epoch costs 32–41 ns
  per entity on singleton heaps and 72–108 ns with an edge per entity,
  resolution the range across three runs ("fresh brackets on one
  HEAD"). The design's trade — the collector may be slow — spends
  this freely.
- **Memory, while an epoch is in flight**: one parked record per death,
  one for one, birth time irrelevant — the churn-times-duration bound
  read directly by a count instrument whose figures repeat exactly
  ("what an epoch parks, in counts that repeat exactly"). The
  wall-time reading is the reader's arithmetic: an epoch's duration at
  an assumed churn rate times one record per death. The unbuilt
  young-free exemption would remove all of that table, both of its arms
  being young by the exemption's predicate. What it removes on a running
  heap is the records of entities dying before the second walk that meets
  them: nothing from a population that outlives two epochs, three quarters
  at an epoch as long as the mean lifetime ("the young-free exemption").
- **Portability of the zero-cost claim**: on AArch64 both header paths
  compile to plain `ldr`/`str` — no exclusive pair, no LSE atomic, no
  fence ("AArch64 reads the header with plain loads and stores"). The
  cost half stays open for want of hardware, and instruction identity
  is deliberately not offered as a cost claim.

## Unresolved questions

- The ll-arm whole-pass modes (≈ 0.21 ns steps) — the canary
  instrument's zero, cause unplaced between code and data placement.
- The rc-trace sweep anomaly: a slope of 1.12–1.32 on log code that is
  byte-identical to rc-walk's, with its null pair 0.25–0.43 below
  equality ("the null sweep bounds the instrument", 2026-08-16).
- The lifecycle's first-pass mode, above.
- Every claim in "What this case does not claim", until Phase D.
