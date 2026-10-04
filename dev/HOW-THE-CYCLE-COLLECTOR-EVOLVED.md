# How the cycle collector evolved

Material for an article, asked by Edmond on 2026-10-04: the path of
decisions from PHP's synchronous cycle collector to a collector that judges
and frees garbage on its own thread, with the figures that moved each step.
Every figure is a measurement cited to its journal entry, or marked as an
estimate. Run results are appended at the end as they come.

## 1. The starting point: PHP's synchronous collector

PHP counts references immediately and collects cycles with the synchronous
Bacon–Rajan algorithm when its 10,000-entry root buffer fills
(`dev/RESEARCH.md`, "why the Recycler did not become a mainstream
collector"). The whole cycle collection runs on the request's thread: the
pause is the collection. Its known pathology is trial deletion walking the
live data reachable from the candidates — Composer once disabled PHP's
collector for it.

The runtime kept PHP's model where it matters — immediate, per-thread,
non-atomic counts (copy-on-write reads exact counts; destructors run when a
count reaches zero) — and set out to move the cycle collection off the
request's thread.

## 2. `rc-cycle`: one collector, trial deletion over shadow state (August 2026)

On 2026-08-26 the earlier collectors were deleted and `rc-cycle` became the
one design (`dev/DECISIONS.md`, "what the old collectors left behind is
deleted…"; `rfc/model/gc/rc-cycle.md`). Its two choices that everything
later builds on:

- **Shadow rows.** Trial deletion never writes the heap: each entity met gets
  a row initialised to its count, and each edge followed subtracts one from
  the target's row. The owner's counts stay the owner's.
- **Only the owner frees.** A sliding-view log (Levanoni–Petrank) was refused
  for its per-store soundness cost (`rfc/model/gc/cycle/questions.md`, Y1),
  and the collector-side free was withdrawn (Y5).

## 3. A collector thread that finds, and a mutator that judges (September)

- **A collector thread** born lazily, rounding on a timer (2026-09-15/19).
- **The trace token** (2026-09-15/17): the collector requests, the mutator
  consents at its next free or poll, and while a collector holds it the
  mutator withholds every memory return, so the collector may hold any
  address.
- **The collector finds, the mutator judges** (2026-09-23): the collector's
  verdicts are proposals; the owner validates exactly under its own token
  and frees. A recall lets a mutator that needs its token stop the collector.
- **Deferred lanes** (2026-09-18 on; waits of 1, 3 and 7 epoch turns by a
  root's count of live readings, 2026-09-28/29): a root read live is not
  re-traced at once.

## 4. Web loads and the proof-epoch collector (late September – 3 October)

- **The web loads** (2026-09-27 on): `web-arena` and `web-heap`, a request
  server's memory shape, replaced the synthetic rings as the deciding loads.
- **No budget on the collector's work** (Edmond, 2026-09-30): "не
  ограничивать работу коллектора числом, а ограничивать ситуацией" — the
  part budgets and the rows ceiling went; a grant ends on the mutator's
  situation (the withheld-return marks, the take).
- **The collector writes the maturation stamps itself** (2026-09-30) and
  **posts the set it proved unreachable** (2026-10-01): the owner validates
  that set alone instead of walking from roots.
- **The epoch turns by proofs** (2026-10-03): when the collector's work
  reaches a ratio of what its stamps cost to prove (2, then 4 by Edmond's
  ruling).
- **HG leaves the tree; the proof-epoch collector is the default build**
  (2026-10-03). On `web-heap` its garbage was 71 MB on the mean against
  461 MB for plain D (`dev/BENCHMARKS.md`, "S67.6").
- **The owner's fast path** (2026-10-03): over a set whose rows all read
  zero the owner skips the scan; its longest pause on `web-heap` fell from
  117–173 ms to 67–103 ms (`dev/BENCHMARKS.md`, "S67.12").

## 5. The measurement that changed the question (4 October)

Edmond asked: the same roots collected by one thread, against the split.
Measured on one box in one run (`dev/BENCHMARKS.md`, "one garbage ring, one
thread against the split"):

| members | one thread | collector | owner's pause | collector + owner |
| ---: | ---: | ---: | ---: | ---: |
| 4,000 | 0.55–0.61 ms | 0.28–0.33 ms | 0.41–0.42 ms | 0.70–0.75 ms |
| 40,000 | 5.5–5.9 ms | 2.4–2.7 ms | 4.2–4.5 ms | 6.8–7.2 ms |
| 400,000 | 62.4–66.7 ms | 26.3–28.3 ms | 47.6–51.2 ms | 75.1–77.5 ms |

The split moved only a quarter of the pause off the mutator, for a fifth
more work in all: the owner re-traced and tore down the whole set. An
earlier comparison of the owner's 26 ms with reference counting's 9.6 ms
cascade over an acyclic tree had compared different things and was
withdrawn. An independent review (Fable) called the design mediocre for its
purpose — pause — and good for memory; the remaining weakness was named:
the owner's validation of a large garbage set.

## 6. Three designs, three rounds each (4 October)

Each went through a Critic, a revision, a second Critic and the Sage
(Fable).

- **The logged verdict.** The owner keeps validating, but cheaply: a
  per-slot record of destroyed edges plus a counts sum replace the re-trace.
  Round 1 broke the first version (a stopped batch's set is unproven; a
  header bit dies with slot reuse); round 2 found that counts read at
  different times let a local move between members (a cursor ahead of the
  trace) — fixed by the counts sum; the Sage proved the pair sound, reduced
  the record to destructions only, and estimated the owner's pause at
  25–30 ms. It did not answer the request: the mutator still judged.
- **Recycler over counts (R2).** What Edmond asked for: the collector judges
  and frees alone. Counts stay immediate; while a window is open, every count
  write and every pointer store tags header byte 7 with the window number,
  and an untagged white set is garbage. Round 1: ARC-cancelled
  retain/release pairs move edges with no count write, so pointer stores tag
  their holder; the collector's free must leave registered members dead in
  place; its own remote frees would recall it. Round 2: the scan re-read
  the heap and could whiten a live entity — it now runs over the edges the
  mark recorded; deaths with owner-side effects (weak cells, arena children)
  go to the owner as typed actions; the destructor-bearing part of the set
  goes to the owner with every edge into it. The Sage proved it sound under
  four conditions and estimated the owner's pause at 0.1–1 ms, for
  ≈ +1–3 % mutator CPU — the per-store cost Y1 had refused.
- **The Recycler as published (R1).** Deferred object counts, uncounted
  locals, the collector owning counts. First dropped on a "destructors run
  on the spot" constraint that Edmond had never set (Y2 rules the opposite:
  PHP promises no instant) — the error is in `dev/POSTMORTEM.md` — then
  reworked: copy-on-write kinds (arrays, strings, boxes) keep exact immediate
  counts, which `rfc/model/values.md` already requires; a conservative scan
  of Rust frames; logged Rust-side retains; parked mutators. The Sage found
  it sound after two fixes but deferred it: about the same pause as R2, for
  1–3 collector cores and ≈ 60 % of the lowering rewritten; its real prize,
  uncounted object locals, can be measured on its own.

## 7. What the literature said

- **Why the Recycler did not spread** (`dev/RESEARCH.md`): its 2.6 ms pauses
  cost a spare CPU per ~3 mutators, an atomic exchange per pointer store and
  collector work many times mark-sweep's; its concurrent cycle collector
  rescanned candidates and had a liveness race (Paz, Bacon et al.); its
  successors kept deferred counting but found cycles by tracing; only the
  synchronous algorithm shipped (PHP, Firefox, Nim). Most of its cost bought
  concurrent counting, which per-thread counting avoids.
- **Who built the hybrid** (`dev/RESEARCH.md`): nobody, as far as found —
  owner-written immediate counts with a concurrent collector thread judging
  over shadow state appears new. Closest: Firefox's incremental collector
  (the same "touched during the collection means live" rule, in one thread),
  Samsara (a concurrent Rust collector over immediate RC). Lessons: an object
  dying mid-scan must not whiten what it pointed to (Firefox bug 1023758);
  CPython 3.14's incremental collector cut the longest pause from 26 to
  1.3 ms but grew memory up to 5× and was reverted; Kotlin/Native dropped its
  concurrent RC cycle collector for races and long pauses.

## 8. The decision (4 October)

Edmond approved R2, built as an arm beside the default build
(`dev/DECISIONS.md`, "the collector judges and frees a garbage set by window
tags…"; `dev/design/recycler-over-counts.md`). The gate: mutator CPU within
+3 % of the default build on `web-heap`, the longest owner pause under 5 ms
in every cell, held garbage no worse, refusals under 10 % of batches.

## 9. Results

(Appended as the runs land.)
