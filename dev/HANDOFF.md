# Handoff — 2026-10-08 (cloud session), the optimisation round of both kinds

The state where the session of 2026-10-08 stopped. It is consumed by the next
session and replaced whole by the next handoff; what outlives it is in
`PLAN.md` and the journals, and `PLAN.md` outranks it.

## State
- /home/user/model: `main` = `origin/main` at the optimisation round's close.
  /home/user/rfc: `BACKLOG.md` carries the compiler tasks for the ownership
  mark (de55f74, d563b14).
- PLAN.md outranks this file. Open steps: S68.8, S68.9, S68.11, S68.12,
  S68.13 (gate failed, open for Edmond), S68.14.

## Done this session (2026-10-07/08)
- Both kinds named and kept: `gc-checkpoint` (default) and `gc-window`.
- `gc-checkpoint`: rounds at 1,024 or 50 ms standing; collector born at the
  first thread's start; one-pass free of a set garbage whole; the drain
  queues the outside children (owner collections on `web-heap` 24.2 → about
  20 s, teardown 7.8 → 3.0–3.5 s).
- `gc-window`: the record's append by cursor, one dispatch a met mature
  edge (instructions −10 %, time inside the spread).
- Measured and not taken: close shortcut, drain prefetch, the counts' sum
  ceiling, the stamping walk (all in `dev/BENCHMARKS.md`, 2026-10-08).
- Ownership mark: Edmond ruled the compiler clears it; the Critic's rules
  R5a–R5e in rfc `BACKLOG.md`. Two red runtime tests kept outside the repo.
- DECISIONS: a small optimisation stays when no slower than main (Edmond,
  2026-10-08 15:57).

## Next
- The collectors' local tuning is spent: the remaining cost is the drain's
  pointer chase and the prune's header read. Edmond chooses the direction
  (asked 2026-10-08): S68.14's measurements, S68.8's runs and article, or
  the compiler-side work that lets fewer objects reach a collector.

## Working notes
- Push: `cd` first; hash by `git diff origin/main..main | sha1sum | cut -c1-8`;
  `TAS_REVIEWED=<hash> git push origin main`; then
  `TAS_REVIEWED=da39a3ee git push origin main:refs/heads/claude/exciting-tesla-y0c10j`.
  The hook rejects "moved/now/was/previously/used to" in added comment-marked
  lines (markdown lines with `*` or `#` count).
- Release test binaries: `cargo test --release --lib [--no-default-features
  --features gc-window] --no-run --target-dir target/s611_a|s611_b`; arms in
  target/arms68 (Asw/Bsw = gated sweep, A13/B13 = cb0faf1).
- This VM has no PMU ("no PMU driver, software events only"): perf cannot
  count instructions. Callgrind counts them; its cache model has no hardware
  prefetcher, so its miss shares overstate address-ordered passes.
- Compare arms only within one session: `web-heap`'s mark moved 6 % between
  two sessions of the same build on 2026-10-08.
- Rig arms: `ARMS_DIR=target/armsX ARMS="M W" WEB_MUTATORS=1,2 CAPS=1
  WEB_COLLECTORS_CAP1=3 WEB_LOADS="web-heap:46.78" dev/tools/arms.sh
  target/armsX/web.csv 3 web`; binaries from `cargo test --release --lib
  --no-run --target x86_64-unknown-linux-gnu` with a PIE relocation model.
- The full unit suite must run in debug (a release run fails
  `a_second_init_on_a_started_thread_is_refused`, a debug_assert test).
