# Handoff — 2026-10-04, S65 and S67 closed

The state where the session of 2026-10-04 stopped. It is consumed by the next
session and replaced whole by the next handoff; what outlives it is in
`PLAN.md` and the journals.

## Done

- S65 and S67 closed whole (rule 23.1.3): their sections left `PLAN.md`, the
  notes files `dev/plans/S65.md` and `dev/plans/S67.md` were deleted, and what
  they held alone was carried to the journals first: `dev/DECISIONS.md`,
  `dev/BENCHMARKS.md` and `dev/POSTMORTEM.md`, each under "… the S65 and S67
  stage notes held, carried at the stages' close". The notes stand in git at
  `410b856`.
- The proof-epoch collector is the default build; the epoch's ratio is 4 and
  the embedder's to set (`ll_gc_set_epoch_ratio`).

## Open

- No stage is active; the next is drawn from `PLAN.md`'s backlog.
- Owed to Edmond: whether the release-test-build case of a debug assertion's
  abort is gated to debug assertions; the ratio on loads whose live core dies.
