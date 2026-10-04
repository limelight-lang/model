# The web loads: `web-arena` and `web-heap`

The rig's two loads that stand for a request server's memory, built in
`src/cycle/worker/tests/the_web_loads.rs` and driven by
`src/cycle/worker/tests/the_rig.rs`, and the protocol the default build was
chosen by (`dev/BENCHMARKS.md`, "S67.6"). [F] marks a figure a cited source
measured, [A] an assumption, [code] a figure of this crate.

## Why the rig's loads decide no scheme (the loads' Critic, 2026-09-27)

- The paced wait calls `ll_gc_maybe_collect` every 1 ms
  (`worker/tests/the_rig.rs`, the paced loop), so a rig mutator is idle and
  consenting nearly all the time: `garbage-25` is under 0.5 % busy,
  `live-churn` 2–8 % (estimated from its instructions). No mutator waited for
  its token in S65.28 or S65.32, so the 10 % share rule was read over zero
  waits. A server worker blocked in `accept` or on a database read makes no
  poll (`rfc/model/gc/rc-cycle.md`, an owner asleep).
- `garbage-25` and `registered-ring-live` exercise fate "a" and a live set of
  47–63 roots, which costs the same in every scheme
  (`dev/OPERATION-COUNT-BY-SCHEME.md`, 3.2); `deferred-then-dead`'s last free
  is X. The long-iteration gate is below its own noise: `garbage-0`, with no
  collector, read 17 against 3.
- `live-churn`'s result is set by `FALLBACK_INTERVAL_MIN` and
  `BATCHES_PER_EPOCH`, neither measured, and by one ratio T/E ≈ 1.25.
- Rings of 6 disjoint members, against a corpus whose median candidate
  closure is 381 of 381 objects, every root reaching the service container
  (`rfc/model/gc/cycle/questions.md`, the corpus); the loads with overlap
  never start a collector.
- `standing_bytes` is one sample of a sawtooth, in 128-byte members with
  nothing hanging from them.
- The metric is the mutator's instructions only; the collector's CPU and the
  cache misses of header touches are printed and not scored.
- Every death in the churn loads is silent (the keeper's edge lands on the
  registered root); every death in the garbage loads registers a root.
- The loads register at most about 16k roots a second a mutator.

## The loads

Two variants, because where a request's memory lives decides the load.

**`web-arena`**, the design's main path: request objects live in the
request's arena, which no collection scans (`rfc/model/memory/arenas.md`).
A heap reference stored into an arena container is logged for release at
the reset (`Arena::log_release_at_reset`), and the request ends in
`promote::arena_reset_full`, which promotes the escapes and runs the release
log; a bare `Arena::reset` drops escaped objects with their blocks and is not
used.

**`web-heap`**, the fallback where escape analysis fails: the request graph
is in the GC heap.

### Common to both

| parameter | value | basis |
| --- | --- | --- |
| mutators | 6, on six physical cores | one worker per core (Octane's default) [F]; two cores left for the collector and the box |
| collectors | cap 1 on a seventh physical core, and cap 4 on the seventh and eighth with their SMT siblings | 4 is the crate's default [code] |
| requests in flight per mutator | 1 | PHP's worker model [F]; the async variant of 64 is not in the deciding run |
| arrivals | open-loop Poisson per mutator at the λ that keeps a worker busy or waiting 60 % of the wall, found per load by a pilot of best D and fixed across the arms (S67.7, ruled 2026-09-30); 0.6 / (E[CPU] + E[waits]) where the build is free | [A] |
| request CPU | lognormal, median 4 ms, σ 1: the mean is 6.6 ms | [A] |
| blocking I/O waits | 2 a request, lognormal, median 3 ms, σ 1: 9.9 ms a request | [A] |
| random draws | seeded per mutator and repeat, the same sequence in every arm | the paired comparison below |
| during waits and between requests | the mutator sleeps and makes no poll | the first finding above |
| polls inside CPU phases | every 50 µs of the synthetic CPU | [A] |
| cache lookups per request | 20, keys drawn Zipf (s = 1) over 4 N keys | [A]; sets the registered population with N |

The synthetic CPU is a counted loop of a calibrated number of instructions
a turn, k, measured once per binary as the rig calibrates its ten million
turns; the scored figure subtracts turns × k and reads no counter around a
spin, so the polls inside a CPU phase, where the frees and the takes
happen, count as the runtime's work (round 2, finding 1).

### Long-lived state per mutator

- A core of 5,000 objects in one strongly connected graph [A]. The corpus's
  381 objects are the whole heap after a boot and one request
  (`rfc/model/gc/cycle/questions.md`, the corpus), a framework's heap and not
  a request's; 5,000 is an assumption for an application with its services.
- An LRU object cache of N values, each 10 objects with a back-edge and one
  edge into the core [A], the cache's map hanging from the core, so a trace
  from a core root reaches the N × 10 resident objects. The cache is filled
  to N at setup with keys drawn from the same Zipf. A hit is a non-final
  decrement that registers the value once; a miss builds the value in the GC
  heap and evicts the least recently used, which dies by a non-final
  decrement on its cycle: silently where its root is a candidate already,
  registering a root otherwise. Eviction is by LRU alone, no TTL; the hit
  rate is whatever the draw gives and is reported. N ∈ {40k, 150k}, the
  resident set a trace from the core meets growing with it. Discord's scan
  of a large LRU cache is the published pathology [F].
- 1,000 sessions of 20 objects with one cycle each and one edge into the core
  [A]; an idle lifetime of 1,440 s (PHP's `session.gc_maxlifetime`) [F] does
  not fit a cell, so sessions are replaced at 1 % of requests [A].

### `web-arena`, a request

- A median of 10,000 objects, lognormal σ 1 [A], in the mutator's `Arena`; they register
  nothing (`refcount`, an arena object never enrols).
- 300 heap references stored into arena containers and logged for release at
  the reset [A]. Every release at the reset is non-final: the core and each
  cache value are cycles, so a release on one registers its root or finds it
  a candidate already.
- Escapes at the reset: a session write on 30 % of requests [A]; cache
  values are built in the heap at a miss and do not escape. A promoted
  survivor keeps its block as a retained block, which the gates count in
  blocks, not in object bytes.

### `web-heap`, a request

- Objects: lognormal, median 10,000, σ 1 [A], against Discourse's 38,363 new
  objects on its front page [F]; sizes 50 % 64 B, 35 % 128 B, 15 % 512 B,
  a median of about 1.4 MB a request and a mean of about 2.5 MB [A].
- A context cycle of 8 objects the request tree hangs from, fan-out 8
  (Express's `req.res`/`res.req`, Octane's `clone $this->app`) [F]; 0–3 ORM
  collections of 10–100 entities with child-to-parent back-edges [A];
  closures 45 % of the objects, each with an edge to the context or the core
  [A] (the corpus's 179 closures of 381 objects are a boot heap's, 47 %); 1 %
  of payload objects hold an edge into the core [A].
- Registrations: 10 % of objects get a non-final decrement at a uniform point
  of the request's life [A]; Shahriyar et al. report 99 % of objects never
  exceeding a count of 2 for many DaCapo benchmarks [F], which bounds the
  share from above and does not set it. The context root registers at the
  start.
- At the request's end the context's last external reference lands on the
  registered root in 50 % of requests, a silent death, and on an unregistered
  member otherwise, which registers a root [A].

### Cell length

96 s of load (twelve X) after 20 s of warm-up (two and a half X, so the
window's edges fall between X turns), and a 12 s drain that polls every
1 ms as the rig's drain does today: the waits' last free came 8.4–8.6 s
after the stop on the rig (`dev/BENCHMARKS.md`, "S65.42"). The warm-up is
discarded by wall clock: every counter the rule reads is read at its end and
at the stop and the difference scored, and the peak garbage is reset at its
end.

## The protocol (fixed before any run, 2026-09-28)

- **Arms.** Four: D and HG, and the best build of each as S65.42 left it
  (S65.43's release not adopted, `dev/DECISIONS.md`, 2026-09-29). Deciding is best D against best HG; D and HG are
  reported. H is not an arm (the stage's scope amended to D and HG).
- **Primary metric.** Instructions a request, the six mutators' runtime
  instructions (the loop's, less the synthetic spin's) and the collector
  threads' together, both read by per-thread counters over the scored
  window. Instructions, not cycles: their spread on the rig was 0–3.2 %
  against 8–36 % for CPU time.
- **Latency.** The excess of each request is its wall from arrival less its
  drawn CPU and drawn waits; the seeded draws pair the requests across arms,
  and the gate reads the p99.9 of the paired difference, best HG's excess
  less best D's request by request. The excess from the start of service is
  reported beside it.
- **Gates**, each against best D, each with an absolute floor so a near-zero
  baseline cannot flip it: mean garbage from the integral at most 1.10 × +
  64 KiB; peak garbage at most 1.25 × + 256 KiB; retained blocks at the stop
  less those the live session writes hold at most 1.10 × + 4 (S67.5's
  Critic, finding 2: the live writes hold about 977 blocks a mutator in
  every arm); the paired difference's p99.9 at most 1 ms;
  `token_wait_longest_us` at most 2 × + 1 ms. One gate is absolute: every
  cell's garbage freed inside the drain, and a cell cut by the rig's garbage
  ceiling fails it. Each gate reads the median of the five repeats.
  `withheld_by_an_entry_mean_bytes` and `ledger_peak_bytes` are reported,
  not gated.
- **Deciding cells.** `web-arena` at N = 40k and 150k and `web-heap`, each at
  cap 1 and cap 4: six cells. `deferred-live-large` and
  `live-churn-dies-by-count` run as controls and count toward nothing; the
  guards `garbage-25`, `registered-ring-live` and `garbage-0` are dropped,
  since they cost the same in every scheme.
- **Repeats.** Five a cell, the arms' order rotated. Each arm's figure is
  the median of its five; the spread is best D's (max − min) / median over
  its five, and the tolerance of a cell is max(3 %, twice that spread), the
  A/A reading.
- **Verdict.** HG is kept and D dropped only if best HG wins at least four of
  the six deciding cells by more than the tolerance, loses none by more than
  it, and fails no gate best D passes; otherwise HG is dropped. A tie drops
  HG: at the same price D is the scheme with less machinery (`PLAN.md`,
  Fog). The rule does not try to resolve a difference below 3 %; five
  repeats resolve the 3 % it asks about. An absolute gate that best D fails
  too is put to Edmond beside the verdict rather than waived.
- **Second verdict, the waits** (Edmond, 2026-09-29: "так строй"). The same
  run decides whether the surviving scheme's default build takes its best
  build's features: best against plain of that scheme, by the rule above
  with plain in best D's place — the best build is kept only if it wins at
  least four of the six cells by more than the tolerance, loses none and
  fails no gate plain passes; a tie keeps the plain build. The instructions
  over the loop and the drain together are reported beside the metric,
  since the waits move teardown into the drain (`dev/BENCHMARKS.md`,
  "S65.43"). The two verdicts together name the default build.
- **Void cells.** A cell is void where the other processes' CPU, the box's
  busy time from `/proc/stat` less the rig process's own from
  `/proc/self/stat`, averages above half a core over the cell; a void cell is
  re-run after the last repeat, once.
- **Length.** 3 loads × 2 caps × 4 arms × 5 repeats = 120 deciding cells of
  about 2.3 min each, and 80 control cells of 25 s: about 5.2 h. The best
  builds are S65.42's.
- **Instruments owed** (S67.2–S67.5 and S67.7; the sleeping mutator built by
  S67.2): the sleeping mutator; arrivals with a
  per-mutator queue and latency from arrival and from service; the counted
  spin and its calibration; the cap and the web placements taken from the
  environment in `dev/tools/arms.sh`; the void reading;
  per-thread instruction counters on the collector threads; garbage in bytes
  by size, mean and peak; retained blocks; deaths split into silent,
  completed and born garbage; the warm-up's counter snapshot; a count of the
  distinct cache values registered; the X turns against the batch turns.
- **Unswept assumptions that can decide.** Lookups per request, the poll
  interval, the request CPU, the edges into the core — how many, the share
  of closures that point there (one in two) and their targets (Zipf over the
  core) — the releases per request and the escape rates. They are reported with the verdict and
  decide nothing; the two first named, lookups per request (5, 80) and the
  poll interval (10 µs, 250 µs), are run one at a time on the deciding cells
  only if Edmond asks after the verdict, about 5 h more.
- **Placement at cap 4.** The collectors take both spare cores, so the box's
  own work lands on the mutators' SMT siblings; that moves cycles and
  latency, not the instructions the verdict reads.


## Sources

- ircmaxell, "What About Garbage?" (Composer and the PHP GC)
- Scrutinizer, Composer GC improvement; Blackfire, the PHP GC's impact
- Tideways, the PHP GC threshold; the PHP manual, "Garbage Collection"
- Discord, "Why Discord is switching from Go to Rust" (the LRU scan)
- Twitch, "Go memory ballast"; Instagram, "Dismissing Python Garbage Collection"
- Laravel Octane: the documentation, `Worker.php`, `CollectGarbage.php`, `octane.php`
- Express issue #2560 (`req.res`, `res.req`)
- Sam Saffron, "Demystifying the Ruby GC"
- Shahriyar, Blackburn, Frampton, "Down for the Count?" (ISMM 2012)
- Jones and Ryder, Java object demographics; NG2C, middle-lived data in servers

