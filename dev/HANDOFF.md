# Handoff — 2026-09-30 (night), S67.9

The state where the session of 2026-09-30 stopped. It is consumed by the next
session and replaced whole by the next handoff; what outlives it is in
`PLAN.md`, `dev/plans/S67.md` and `dev/DECISIONS.md`.

## Where the work stands

`PLAN.md` S67.9, the collector's progress guarantee, is open and first, before
S67.6. The design is revision 3 with the held-stack ruling (`dev/plans/S67.md`,
section "S67.9 — The collector's progress guarantee"), amended today by two
rulings and one proposal awaiting Edmond's answer. Nothing of the design is
built.

Read from "runs R0–R3 2026-09-30" to the end of that section first.

## Done today

- Runs R0–R3, recorded in `dev/plans/S67.md` with their figures. Every cell
  was void (2.2–14.0 cores of other CPU); the logs were not kept.
  - R0: under a 2-ms stubbed trace R is read at 2.90× (two mutators) and 3.13×
    (six) its registration rate, so G5's gate holds and the round keeps its
    sleep; 65.8 and 41.0 batches a second a mutator.
  - R1: a complete walk of `web-heap-150k`'s state takes 167 arena blocks,
    26.5M positions, 202 ms; the scan equals the mark in positions.
  - R2: at K = 1,024 on `web-arena`, the first regions finish before the
    crossing in 93.5 % and 90.3 % of the batches; the blocks stack crosses at
    exactly 16, and holds up to 796 at the release; first-region proposals
    drive the owner's collections over P through the whole state (up to
    26.6M positions, 1.05 s).
  - R3: a full live list, 130,560 stamps, costs 1.7–2.5 ms at a take and up
    to 5.1 ms inside `ll_free`.
- Ruled by Edmond: no rows ceiling and no runtime memory limit
  (`dev/DECISIONS.md`, 2026-09-30, "the collector's trace has no rows ceiling,
  and the runtime sets no memory limit of its own"); a limit, if one comes, is
  the memory manager's. A constant fitted to a rig load was refused
  (memory `no-constants-fitted-to-a-test-load`).
- Two Critics recorded in the notes: the ceiling removed (nine findings; the
  allocator's refusal is unreachable under Linux overcommit), and the
  collector writing the stamps (no data race; four conditions accepted).

## Open questions for Edmond, in order

1. Adopt into revision 3: the collector writes the maturation stamps itself,
   once a batch after its trace and before the release, and the live list
   goes (`dev/plans/S67.md`, "Critic 2026-09-30, the collector writing the
   stamps").
2. The epoch's length: the model proposed turning the epoch on events — a
   complete walk finished in it, and R's entries at its start all read —
   instead of 64 batches or 8 s. Not answered.
3. Then, from revision 3's list: B_owner, Q2 after the switch cell, Q1.

## Uncommitted (main = origin = `19897d1`)

- `dev/plans/S67.md`, `dev/DECISIONS.md`, `dev/RESEARCH.md`, this file.
- The rig's test-only instruments for R0–R3: `LL_RIG_STUB_TRACE_MS`,
  `LL_RIG_BATCH_BLOCKS`, `LL_RIG_BATCH_ROOTS`, `LL_RIG_FIRST_REGIONS`, the load
  `web-heap-40k`, and the columns for cadence, the widest part, positions by
  ending, withheld returns, owner positions over P and stamping
  (`worker.rs`, `worker/testing.rs`, `mark.rs`, `gc.rs`, `live_list.rs`,
  `live_list/testing.rs`, `deferred_slot_reuse.rs`, `the_rig.rs`, and the
  thread-local guard's list in `where_the_first_touch_happens.rs`).
- `the_rig.rs`'s saturating `standing_bytes`, a rule-3 fix of the previous
  session.
- The red `the_progress.rs` with its line in `tests.rs` and the fixture edits
  in `the_ceiling.rs`: three cases red by design, rewritten at build step (c).
  Edmond asked to push everything; whether the red cases go in is asked and
  not answered.

`cargo test --lib -- --test-threads=8` twice and once with `debug-journal`:
1,277 and 1,291 passed, only the three `the_progress` cases red;
`cargo doc --no-deps --document-private-items` prints no warning. The rest of
the gate (a third threaded run, `hash-folding`, `cargo build --release`,
`cargo bench --no-run`, `cargo +1.94 fmt --check`) was not run.

## Next

Answer the open questions one at a time; then build step (a) of revision 3,
with what today added to the build: the worklist without leaves (the Critic's
finding 4), the critical reserve kept for the scan (5), giving the arena's
memory back at the reset, and the runs owed for findings 6 (a 150-ms epoch on
`web-heap-150k`) and 7 (the reset timed in its two parts).
