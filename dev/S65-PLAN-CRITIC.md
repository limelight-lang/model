# Critic on the S65 implementation plan

Date: 2026-09-23. Reviewed `dev/S65-PLAN-FOR-REVIEW.md` (deleted 2026-09-24, kept by git) against model
`b7245ce`, the current `PLAN.md`, the decision of 2026-09-23, package v3,
the lane amendment and its Critic, relevant earlier reviews, the current
implementation, and the sibling RFC's handshake and layout contracts.

This is a review of the proposed implementation and its acceptance criteria.
No implementation was changed, no probe was added, and no tests or benchmarks
were run. Source inspection establishes the mechanisms below; it does not
establish their measured cost. The supplied snapshot is preserved.

## Verdict

Revise before executing the plan as twelve independently accepted commits.
The accepted collector/mutator split need not change. The remaining problems
are in the recall contract, work charged to the mutator, interactions between
bounded structures, and the order and completeness of the implementation gates.

The snapshot faithfully reproduces the active S65 section. Its reference to
package section 11 also imports that section's tests and measurements: their
absence from the short checklist alone is not a finding. The findings below
remain after reading those inherited requirements.

| Finding | Priority | Affected steps | Problem |
|---|---|---|---|
| F1 | P1 | S65.6, S65.8 | Edge callbacks do not bound all work before token release |
| F2 | P1 | S65.4 | A small R does not bound the retirement pass over the deferred lane |
| F3 | P1 | S65.5–S65.7 | Independently landing parts widens withholding before recall exists |
| F4 | P2 | S65.8–S65.9 | Bounded/optional stamping does not guarantee clearing a parking mark |
| F5 | P1 | S65.12 | Disabling new serves leaves outstanding requests and checkpoint grants unspecified |
| F6 | P2 | S65.12 | The required comparison needs a mode scheduled after the comparison |
| F7 | P2 | S65.11 | Normative updates are scheduled after the commits that change their contracts |

P1 means an implementation gate needs a mechanism or dependency correction;
P2 means a specified interaction or acceptance criterion needs correction.
These are not claims that the proposed code has already shipped.

## F1. The recall contract misses work that emits no edges

**Location:** snapshot lines 40–43 and 80–86; package v3 sections 5–7.
**Confidence:** high.

The visitor is called only for counted references. In
`src/cells.rs:461`, `counted_box_cell` returns `None` for a scalar. The Vector
loop at `src/cells.rs:597` and the Hash loop at `src/cells.rs:614` still inspect
every used position. The object walker similarly skips calls for null pointer
fields and scalar boxes (`src/object.rs:453`). Changing the visitor's result
to `ControlFlow` does not create a call at those positions.

**Counterexample:** a candidate array has one counted edge followed by a very
large scalar suffix. The mutator sets `waiting` just after that edge. The
collector reads the entire suffix without calling the visitor, growing its
arena, or crossing a part boundary. A hooked walker can do the same while
searching sparse storage for its next counted reference. The proposed million-
cell test catches this only if it includes empty/scalar cells, rather than a
million edges.

There is a second instance after tracing. Section 5 checks `waiting` *before*
enumerating and copying the live list. A request immediately after that check
can wait through enumeration, list allocation, and copying. The list cap bounds
output storage, but does not make that work part of the promised N edge visits.
`FinishThePosts` must also finish up to K verdicts and advance R before release.
Those obligations cannot be omitted from the return contract.

Thus “N edges plus one reset” can be true as an edge count while failing to
bound the work the mutator actually waits for. This is the same distinction
that motivated replacing the old block-budget claim.

**Required correction:** make cancellation available at bounded intervals of
storage inspection, including positions that yield no reference, in both
phases and in the concurrent outside-cell API. Add checks during list
construction. State the remaining bounded posting/advance and reset work
explicitly. This is a cooperative work bound, not a hard wall-clock deadline:
pool-lock contention and scheduling remain outside it. Preserve the plain
mutator walker as required by the decision.

**Acceptance:** deterministic recall seams in a scalar Vector, a sparse Hash,
null/scalar object fields, sparse outside storage, and list construction;
count positions/rows processed after the request, not just edges delivered.
Also recall with K unjudged roots and verify exactly-once posts and advance.
Measure the complete request-to-release interval, including cleanup.

## F2. Retirement can scan a large live deferred lane every D deaths

**Location:** snapshot lines 69–72; package v3 section 8.
**Confidence:** high.

The proposed pass calls `compaction::compact(None, true, None)` when R is
below the soft threshold. The `true` first runs `deferred().retain(...)`
over the entire deferred lane (`src/cycle/queue/compaction.rs:57–83`), then
packs R and visits the other queues. The size of R does not bound the size
of the deferred lane.

**Counterexample:** the collector has accumulated A live deferred candidates
within an epoch. R is empty or small. D candidates complete their deaths,
arming `Retire`. The next poll visits all A lane entries even if only D can
be returned. Repeat before turnover with another D deaths. The new mutator
work is proportional to A per D deaths, not to the small R or the number
of deaths. A can be much larger than 63.

Section 8 actually writes “63 + overflow + lane” for the pass's cost, so the
unbounded term is present in the design. Its required measurement, however,
is a pass over 63 entries. `what_the_poll_costs` without an armed retirement
cannot establish the latency or amortized cost of this path. “No trace” does
not mean “no substantial work on the mutator.”

**Required correction:** give the retirement path an explicit work/amortization
bound over *all* lanes it scans. The choices include bounded retirement with
persistent progress, a death-directed structure, or a trigger that accounts
for the population scanned; each needs its actual free-path cost evaluated.
Do not select one solely from the existing 63-entry probe.

**Acceptance:** keep R below threshold while independently varying a large
mostly-live deferred population. Count visits per returned death over repeated
arming, measure armed-poll tails and candidate-free cost, and check eventual
retirement. The pass must not hide repeated whole-lane scans behind the idle-
poll result.

## F3. Parts ship before the mechanism that makes their longer grant affordable

**Location:** snapshot lines 74–92; package v3 section 11, commits 3–5.
**Confidence:** high on the increased work; its wall-clock cost is unmeasured.

Today one grant's trace shares one arena budget across the batch
(`src/cycle/worker.rs:1903–1939`). S65.5 resets to the watermark after each
part, giving each part a fresh B. Package section 7 explicitly allows up to
K times a part's work under one grant. Recall arrives only in S65.6; the
withheld-stack triggers arrive only in S65.7.

**Counterexample at the S65.5 commit:** K disjoint closures each fit B but
their union does not. A mutator asks for its token during the first part.
The collector now finishes K parts before releasing. A mutator that continues
freeing instead also withholds returns for this longer grant, with no M-based
recall. These costs are paid by the mutator even though the extra tracing
instructions run on the collector.

The package's “parts before recall because recall posts by parts” is a code
dependency, not evidence that this intermediate behaviour meets the owner's
performance rule. A zero-root mutator census after the grant does not measure
waiting or retained memory during it.

**Required correction:** keep multi-part execution disabled until recall and
its stack triggers are active, or land the dependent mechanism as one coherent
change with internal development checkpoints. Another temporary bound needs
to preserve the pre-change grant behaviour and be tested as such.

**Acceptance:** at every independently accepted commit, request the token in
the first of K substantial disjoint parts and exercise sustained frees under
the same grant. Compare wait work and withheld memory against the recorded
baseline. Do not defer this comparison to the final N-on-N rig.

## F4. Optional live-list entries cannot be the sole parking-mark clear

**Location:** snapshot lines 94–104; package v3 section 2, answers 6 and 8,
and sections 5 and 7.
**Confidence:** high.

Answer 8 closes the old recurring-mark defect by saying that a successfully
retried root belongs to its live core, is in the list, and therefore has its
parking bits cleared by the stamp. Answer 6 and section 5 permit the opposite:
the list has a per-grant L-block cap, allocation can fail, and a completed
part can receive no stamp list at all. Stamping a subset is safe for pruning,
but it does not establish that every successful root's parking mark is cleared.

**Counterexample:** x was parked with epoch tag e. At e+1 its closure fits the
retry and is live. Earlier parts have exhausted L, or the list allocation
fails, so x receives `ReadLive` but no stamp entry. Its old parking mark stays.
If later opportunities also omit its entry, at e+4 the two-bit tag matches
again and the collector skips x before opening the part. The recurring skip
that answer 8 says was fixed remains possible.

**Required correction:** separate the mandatory disposition of parking marks
on successfully judged roots from optional stamping of non-root live members.
Roots already remain named by P, so the correction need not require an
unbounded member list. Specify what happens on allocation refusal and cap
exhaustion; do not silently make the optional live list mandatory.

**Acceptance:** park x, advance the epoch, successfully retry with list
allocation refused and separately with L already consumed, then drive through
four epoch tags. The obsolete mark must not suppress the later attempt.

## F5. `cap 0` needs a transition protocol for requests already standing

**Location:** snapshot lines 117–120; package v3 sections 3 and 11, commit 9.
**Confidence:** high that the stated acceptance criteria omit a reachable path.

The package says the elder remains alive but `serve` is not called under
`cap 0`. `serve` is not the only route to a batch. A round first calls
`Standing::checkpoint` (`src/cycle/worker.rs:992`), which can call
`serve_the_grant` directly (`src/cycle/worker.rs:1718`). The elder's standing
list lives for the thread's lifetime; its existing withdrawal guard runs on
list destruction, not on an arbitrary cap change (`src/cycle/worker.rs:664–675`
and `Standing::drop`).

**Counterexample:** at positive cap the elder leaves a request on a sleeping
mutator. The embedder sets cap to zero. The mutator wakes and consents. If
checkpoints still run, the collector can trace despite the new mode. If all
serving checkpoints are simply skipped while the elder remains the clock,
the resulting `COLLECTOR` grant has no specified releaser; a later mutator
take can wait indefinitely.

**Required correction:** define the linearization and draining policy for
positive-to-zero and zero-to-positive changes. Cover existing `REQUESTED`,
granted-but-unserved `COLLECTOR`, active traces, posted results, and siblings.
Withdraw standing requests, release or recall grants according to that policy,
and preserve the epoch clock and registry unlinking invariants. It is reasonable
for an already active trace to finish or be recalled; the API must say which.

**Acceptance:** switch modes with an unanswered request, a newly consented
grant, an active trace, and P already posted; repeat with a sibling involved.
Assert no stranded token or linked record, no newly admitted trace after the
defined boundary, intact P, continuing clock turnover, and successful restart.

## F6. The pre-`cap 0` rig needs its own in-line control mode

**Location:** snapshot lines 117–120; package v3 sections 11 and 14.9.
**Confidence:** high.

The step requires measurements first and only then lifting the clamp and
adding threshold/merge collections. But the referenced Astra experiment
explicitly requires a control with no background collector and equivalent
reclamation (`dev/S64-GC-IMPROVEMENT-ANALYSIS.md:502–519`). Package section
14.9 itself names the in-line-at-threshold mode as a dependency on commit 9.
The current setter clamps zero to one (`src/cycle/worker.rs:422–424`).

Merely stopping collectors after S65.10 is not the control: it leaves no
normal threshold/turnover collection to replace their work. Throughput would
then be compared with a different memory-reclamation obligation.

**Required correction:** split construction of the experimental control from
public activation. Before measuring, provide a test-only mode that executes
the intended clock, threshold, merge, and request-draining behaviour, or build
the complete mode behind a disabled switch. Measure that implementation, then
enable the public zero-cap behaviour. Name this dependency in S65.12.

**Acceptance:** equal workload, bounded comparable retained memory and reclamation
latency in all arms; per-thread physical-core placement, not process-wide
pinning. A run that simply accumulates garbage is not an admissible baseline.

## F7. Updating the normative RFC is part of each behavioural step

**Location:** snapshot lines 111–115, compared with S65.2 and S65.5–S65.9.
**Confidence:** high.

The plan requires each step to be one commit, but defers all RFC changes to
S65.11. Meanwhile S65.5 changes P's order, S65.6 adds the recall protocol,
S65.8 adds the list handoff, and S65.9 assigns the parking bits. The current
RFC still says “in R's order” (`../rfc/model/gc/rc-cycle.md:746`) and reserves
bits 20–23 (`../rfc/model/classes.md:45`). The plan header declares the RFC
authoritative, and `dev/WORKFLOW.md`, “Documentation follows the logic, in the
same commit”, requires descriptions to move with behaviour.

**Required correction:** make the corresponding RFC amendments acceptance
conditions of their implementing steps, coordinating the two repositories.
S65.11 can remain a final consistency review. If intermediate commits are
intended to be unpublished checkpoints, say so and apply F3's activation rule;
do not describe both them and the stale RFC as independently accepted states.

## What this review does not reopen

- The accepted ownership split, whole-R pressure/exit collection, and separate
  `Unwalked` treatment by `BatchForm` are retained.
- F2/F3 of the lane Critic are the accepted amendment: the merge counter and
  doubling K only on a filled clamp. The rejected `OVERDUE` construction is
  not treated as a requirement of S65.3.
- Bounded live stamping, mutator-owned byte 6, and the block/run return hooks
  remain the basis. F4 concerns the interaction with optional list entries.
- Candidate-only live cores and components beyond `B_max` have explicitly
  accepted limitations; their mere existence is not a new finding.
- L, M, M_b, M_c, D, N, X and the batch turnover count are unmeasured starting
  values. This review supplies no substitute timings or tuned constants.

## Review evidence and handoff

The snapshot's S65 body was compared with the active `PLAN.md` section. The
review followed the relevant source paths for token taking/release, checkpoints,
array/object cell enumeration, arena reset, and queue compaction. It also
checked the proposed constraints against the accepted decision and the prior
Critics' dispositions; older rejected designs are not the baseline.

Only this report was added. Findings belong in the authoritative `PLAN.md`
and the package contracts when accepted; this review does not edit either.
No claim of a red test, passing suite, measured regression, or implemented fix
is made.
