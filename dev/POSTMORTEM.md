# Postmortem

Serious mistakes only: ones that cost real time, broke something that
worked, sent work down a false path, or happened twice. An entry
without a root cause is useless, so every entry states why the mistake
was possible and why it was not caught.

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
