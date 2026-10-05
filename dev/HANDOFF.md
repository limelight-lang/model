# Handoff — 2026-10-05 (cloud session), S68.8–S68.13

The state where the session of 2026-10-05 stopped. It is consumed by the next
session and replaced whole by the next handoff; what outlives it is in
`PLAN.md` and the journals, and `PLAN.md` outranks it.

## State
- /home/user/model: `main` = `origin/main` = `origin/claude/exciting-tesla-y0c10j`
  = `a2d4705`; working tree clean. /home/user/rfc untouched this session
  (`9e79051`, in sync).
- dev/PLAN.md outranks this file. Open steps: S68.8 (runs + article), S68.11
  (owner fallbacks; only Edmond's questions left), S68.12 (pacing; Edmond's
  questions), S68.13 (proposed, design §5f). Closed this session: S68.7,
  S68.10.

## Done this session
- S68.12 touched sets read: 72 % of touched sets hold the context of a request
  still being built (live, correctly refused, a race with the build: the
  external-reference holder of a live request stands in W only tagged, 1,015
  of 1,015). 12.5 % of the sets read are garbage touched at death. A second
  refusal is a set live at its first refusal; ruling 7's text in DECISIONS
  rewritten. Second refusals send 38.6k roots of ended requests to the lanes a
  cell (1.7 % of roots batched). Collector CPU a member freed: 597 ns at 1 ms
  vs 515 at 10 ms.
- 400k-ring probe at e1e74bc: owner's first poll 0.05–0.13 ms in every shape.
- Stretches over standing frees: waits hold 3.9k drops, 36 ms mean; applying
  frees before a stretch rejected by the Critic.
- S68.7 closed: the array view's debug check not built (Critic); the rule's
  text at `as_table_mut` restated as consent points.
- `under_stress` ledger probe fixed: under the feature it counts the owner's
  dispositions with no trace (Edmond's 2026-09-30 rule on tests that pin a
  replaced mechanism). Four test hooks in worker/testing.rs gated by the
  feature (warnings in the default build).
- Allocator: `Heap::alloc_no_block`'s sweep of owned blocks gated
  (`a_sweep_is_due`, `SWEEP_SHARE` = 8; sweeps at a pool refusal too), after
  perf showed `collect_owned` at 34 % (A) / 54 % (B) of ring-load mutator time,
  finding nothing. Two Critic rounds; three tests in
  src/memory/heap/tests/the_sweep_of_the_owned_blocks.rs. Ring-load ops 3–5x
  cheaper in both builds (cells then hit the 1.5 GB ceiling), web-heap
  mutator −2.6–2.9 %, mt_bench within noise. docs/memory-manager.md updated.
- Collector profile once mutators outrun it: no hot spot; ~13 % of B's
  collector in the Δ-test's yielding wait.
- RESEARCH: runtime proofs out of cycle collection (Parkinson/Clebsch/
  Wrigstad ISMM 2024 frozen-SCC counting; Pyrona regions; acyclic bits).
- Artifact for Edmond (Russian, not in the repo):
  https://claude.ai/artifact/PGR9Ztje4meFVtrGWkYjMH — animation of one
  collector batch, figures, unbuilt optimizations.

## Next
- S68.13: Edmond's window the mutator turns (+1 once at a poll when its
  withheld queue reaches a mark or the collector flags its reads done; more
  turns only on the collector's grant). Run the Critic first on the
  memory-order condition (the judging turn must follow the trace's last read),
  then the model, the Critic, the Sage. Nothing built before the ruling.
- Wait for Edmond's answers (PLAN, "Open for Edmond (2026-10-05)"): token
  wait 36 ms; Δ-refusal gate denominator; the array-bearing load; births at
  cap 4; disposition wake.
- Unbuilt optimizations are listed in PLAN under S68.13.

## Working notes
- Push: `cd` first; hash by `git diff origin/main..main | sha1sum | cut -c1-8`;
  `TAS_REVIEWED=<hash> git push origin main`; then
  `TAS_REVIEWED=da39a3ee git push origin main:refs/heads/claude/exciting-tesla-y0c10j`.
  The hook rejects "moved/now/was/previously/used to" in added comment-marked
  lines (markdown lines with `*` or `#` count).
- Release test binaries: `cargo test --release --lib [--features
  recycler-over-counts] --no-run --target-dir target/s611_a|s611_b`; arms in
  target/arms68 (Asw/Bsw = gated sweep, A13/B13 = cb0faf1).
- perf was installed this session from Ubuntu's linux-tools
  (/usr/lib/linux-tools/6.8.0-146-generic/perf); a new container lacks it.
- The full unit suite must run in debug (a release run fails
  `a_second_init_on_a_started_thread_is_refused`, a debug_assert test).
