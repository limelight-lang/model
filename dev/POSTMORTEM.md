# Postmortem

Serious mistakes only: ones that cost real time, broke something that
worked, sent work down a false path, or happened twice. An entry
without a root cause is useless, so every entry states why the mistake
was possible and why it was not caught.

---

## 2026-07-21 — a test oracle read global state and blamed the runtime

**What happened.** `many_threads_freeing_into_one_owner_lose_no_slots`
failed about one run in twenty at `--test-threads=32`, and never in
thirty runs alone. Its assertion says "the owner lost track of a slot
freed from another thread" — a lost cross-thread free, the worst defect
this allocator could have. Half a session went into reading the MPSC
push and drain paths looking for the race.

There was no race. The oracle summed `used` over every block the heap
owns, and some of those blocks were **adopted**. A block reaches the
abandoned list precisely because it still holds live objects when its
thread exits, so adoption hands a heap live slots belonging to a thread
that is gone. The oracle read another test's leftovers as this test's
lost frees.

**Root cause.** The oracle was written to replace `blocks_out`, whose
stated problem was exactly this — "it is shared, so another test's
block returning late moves it in either direction". The replacement
counted a different global and inherited the same fault. **A test that
reads process-global runtime state has to say which part of it is
attributable to the test**, and neither instrument did.

**Why it was not caught.** It passes alone, in every ordering the
default thread count produces, and its failure names a plausible real
bug in the most suspicious subsystem in the crate. A flake that accuses
something real is worse than one that looks like noise: it sends the
reader into the code it names.

**What was actually decisive.** Not reading the code — instrumenting
it. One temporary print in `adopt` showed blocks arriving with `used`
of 1, 3 and 147, and the failing assertion reporting exactly the
inherited count. The reasoning pass before it had produced four wrong
hypotheses about the CAS loop.

**Rule this leaves.** When a concurrency test fails intermittently,
establish *what it counts* before investigating *what it accuses*.

---

## 2026-07-20 (second) — the fixed benchmark rule was still not enough

**What happened.** With the stale-baseline rule from the entry below now
in force, four consecutive attempts to measure H11 each produced a
confident, statistically significant, wrong answer. criterion reported
`p = 0.00` and `Performance has improved` on runs that were measuring
the machine.

The failures were each of a different kind, and each defence only
caught the previous kind:

1. Load arrived mid-session, so both arms were in one sitting — the
   rule below was satisfied and did not help. Caught only by noticing
   the absolutes were far outside their historical band.
2. The `git stash` between arms changed file mtimes, so `cargo bench`
   recompiled and every measurement began on a machine still busy from
   its own build. This produced a *monotonic* improvement across three
   arms — the shape of a machine recovering, not of a code difference.
3. Running pre-built binaries directly without `--bench` made criterion
   smoke-test each benchmark once and print `Success`. A silent no-op
   that looks like a completed run.
4. By the last attempt the box was 40–80% outside its bands with the
   two control measurements of identical code 10% apart — unmeasurable,
   most likely thermal after hours of compiles and Miri runs.

**Root cause.** The first entry fixed *one* way a comparison can be
invalid and I treated the problem as solved. The general fault is
different: **a benchmark has no built-in way to say "I was not valid",
so validity has to be measured too, not assumed.** A p-value describes
whether a difference is random; it says nothing about whether the
difference came from the code.

Contributing: I stated a mechanism for the intermediate result
(`relink_unfull` is hot in `rptest`, so marking it cold deoptimizes it)
that was plausible and might even be true. Plausibility is what stopped
the checking — the same failure as in the entry below, which is why this
is a repeat and not a new lesson.

**What changed so it cannot repeat.** `BENCHMARKS.md` now carries three
independent validity checks rather than one rule: control repetition
(A → B → A, run void if the A's disagree), absolute values checked
against a recorded per-benchmark band before any delta is read, and
both arms built before either is measured. Plus the `--bench` trap and
a stated condition for declaring the box unmeasurable and stopping.

The H11 change was **not committed on that evidence**, because the rule
it would have violated is the one this crate exists to keep. It landed
later, on a valid measurement that showed no difference outside the
noise floor, and was kept for the shape of the code rather than for a
number (`1824392`; H11 in `BENCHMARKS.md`).

---

## 2026-07-20 — benchmarked against a stale baseline, believed the numbers

**What happened.** While measuring the block-header split, three
comparisons were run against a criterion baseline captured earlier in
the same session. They reported, in order: `+10.8%` on rptest, then
`+2.4%`, then `+51.8%` on larson and `+56.0%` on rptest simultaneously.

The last pair is what exposed the problem. Two benchmarks that stress
different things — one multi-threaded worker churn, one single-threaded
block churn — degraded by nearly the same amount, from a change that
moved two pointer fields within a cache line. No physical mechanism
produces that. Re-measuring the identical build back to back against a
fresh baseline gave `−3.03%` and `−1.54%`.

**Root cause.** A saved criterion baseline records *numbers*, not the
*conditions* they were taken under. On a dev box with an IDE and
background work, machine state drifts within minutes, so every
comparison against an older baseline silently reports
`code change + machine drift` as if it were the code change alone. The
mistake was treating a stored baseline as a controlled A/B when it is
only a stored number.

Two things let it through:

- No sanity check on the *shape* of the result. A large, uniform change
  across unrelated benchmarks is a machine artefact by construction,
  and that should have been the first question asked, before any
  explanation of the mechanism was attempted.
- The intermediate figures were plausible. They agreed with a
  reasonable cache-line argument, and agreeing with theory made them
  feel confirmed rather than untested. A number that matches a
  prediction is the easiest kind to stop questioning.

**What it cost.** About an hour, and one wrong intermediate conclusion
— that the first layout cost 10.8% on rptest — that drove an extra
redesign round. The final layout is genuinely better and is measured,
but the two rejected variants were never verified properly, so only
their direction can be claimed.

It also came close to doing lasting damage: those unverified
percentages had already been written into code comments as measured
facts. In a crate whose comments are used as a record of rejected
alternatives, a false number would have been trusted by whoever read it
next. They were removed before the change landed.

**What changed so it cannot repeat.**

- `BENCHMARKS.md` now specifies the method: both arms measured **back
  to back in one session** against a freshly taken baseline, with the
  exact `git stash` / checkout sequence.
- A stated noise signature: a large change of similar magnitude across
  benchmarks that stress different things is machine noise, not a
  result, and a 1–2% difference on this box is near the resolution
  limit and must be repeated before it is believed.
- A rule that per-variant figures may not be written into code comments
  unless they came from a back-to-back run. Direction is safe to
  record; an unreproducible number is worse than none.
