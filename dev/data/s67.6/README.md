# S67.6 run data: D, HG, best D and best HG on the web loads

The raw data of the run recorded in `dev/BENCHMARKS.md`, "S67.6", 2026-10-03,
11:00–13:30 UTC. The algorithm of the best D build is
`dev/design/the-best-d-collector.md`.

## The box and the arms

A cloud container with four cores and no PMU. Two mutators pinned to CPUs 1
and 2, one collector (cap 1) pinned to CPU 3, CPU 0 left to the box. Arms,
test binaries of the rig built at `a72693d`
(`cargo test --release --lib --no-run [--features …]`), by SHA-256 prefix:

| arm | features | binary |
| --- | --- | --- |
| D | — | `bb9cb27d439d` |
| HG | `hold-by-generation` | `3694c3caef63` |
| bestD | `wait-by-readings` | `3c29855bc740` |
| bestHG | `wait-by-readings`, `hold-by-generation`, `death-check-back-off` | `263af54b4a5a` |

Each cell is one process: `dev/tools/arms.sh <out> 5 web` with
`ARMS="D HG bestD bestHG" CAPS=1 WEB_MUTATORS=1,2 WEB_COLLECTORS_CAP1=3`
and `WEB_LOADS="web-arena-40k:31.08 web-arena-150k:31.04 web-heap:46.78"`
(load : mean interarrival in ms); 116 s with a 20 s warm-up, then a 12 s
drain; five repeats, the arms' order rotated by one each repeat. The
repeat number seeds the request draws, so request *i* of a mutator is the
same request in every arm at one repeat, and differs between repeats.

## Files

- `cells.csv` — one row per cell, 60 rows: `arm`, `repeat`, then the rig's
  line (`src/cycle/worker/tests/the_rig.rs`, `fields`). The columns the
  analysis reads: `iterations` (requests in the loop),
  `mutator_cpu_in_the_loop_us`, `collector_cpu_at_the_stop_us`,
  `collector_cpu_us` (with the drain), `web_garbage_mean_bytes`,
  `web_garbage_peak_bytes`, `web_garbage_at_the_drain_end_bytes`,
  `web_retained_blocks_at_the_stop`, `web_retained_blocks_of_live_writes`,
  `token_wait_longest_us`, `arrival_latency_p999_ns`, `last_free_us_max`,
  `window_turnovers_by_proofs`, `window_turnovers_by_x`,
  `other_cpu_cores` and `void` (a cell is void past half a core of other
  processes' CPU; none was).
- `requests.tar.xz` — `web-requests/<arm>-cap1-<load>-<repeat>.csv`, every
  request of the scored window: `mutator`, `index`, `arrival_ns`,
  `start_ns`, `end_ns`, `cpu_ns` (drawn CPU), `waits_ns` (drawn waits).
  `dev/tools/paired_excess.py a.csv b.csv` pairs two arms' files of one
  repeat and prints the p99.9 of the excess difference.
- `aa/` — the A/A check after the run: best D twice at repeat 1 on
  `web-arena-40k` and `web-heap`; `bestD-<load>-{a,b}.csv` are the requests,
  `<load>-{a,b}.rig.csv` the rig's header and line.
- `pilots/` — the pilots of best D, 50 s with a 10 s warm-up, that set the
  interarrivals: (mean service + mean drawn wait) / 0.6.
- `analyse.py` — the medians, the gates and both verdicts of S67.1's rule
  with CPU in place of instructions. Run from this directory:
  `tar -xJf requests.tar.xz && python3 analyse.py .`

## What the protocol could not do here

No instruction counters (CPU time instead), no cap-4 cells (they need four
spare cores), three deciding cells instead of six, and a latency gate of
1 ms below the box's own A/A excess (31.7 and 120 ms). See
`dev/BENCHMARKS.md`, "S67.6".
