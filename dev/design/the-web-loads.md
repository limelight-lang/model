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
| polls inside a request's build | after every `BIRTHS_AN_ADVANCE` births (`POLL_STRIDE`, 4,072), as a compiled build loop polls on its back-edge | the Sage, 2026-10-05 |
| the draw of the next plan | inside a blocking stretch: the harness's bookkeeping, already subtracted from the window, touches no runtime entity | the Sage, 2026-10-05 |
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

## `web-heap-arrays`, S68's array-bearing load

`web-heap` with part of its tree, its ORM collections, its sessions and a
result set a request held by arrays: the array-bearing load S68's gate
names (`PLAN.md:123-131`), which the rig did not have (`PLAN.md:350`).
`web-heap` places objects of three sizes only (`SIZES`,
`the_web_loads.rs:224`; paths under `src/cycle/worker/tests/` unless named)
and chains a collection through segment objects, so no array is born.
Specified 2026-10-12 after two rounds of the Critic and two of the Sage, and
built the same day in `the_web_loads/in_arrays.rs`. [inf] marks a claim
inferred from the code, not run.

### What changes against `web-heap`

Everything of `web-heap` stands unless named here: the long-lived state at
N = 150,000, the drawn count and sizes, the closures, the 10 %
registrations, the CPU and the silent end. A third `Variant`,
`HeapArrays(ArrayLoad)` (`the_web_loads.rs:210`), draws what is new from a
stream of its own, `Purpose::Arrays` = 9 (`:263`), so `Variant::Heap`'s
path and `web-heap`'s pinned plan digest
(`a_heap_plan_folds_to_its_pinned_digest`) do not move. The plan lists
every array as a placement (holder, slot, kind), born straight after its
holder (`Request::hang_the_arrays`).

- **Array holders.** A tree object holds its children through a vector in
  its first child slot with `ARRAY_HOLDER_PERCENT` = 50 % [A, set so that
  arrays are 10 % or more of the garbage made, the validity test below]:
  the children it would hold in its slots are appended to the vector at
  their births. The tree, its fan-out and its sizes are `web-heap`'s.
- **ORM collections.** The head's slot 1 holds a vector of its entities,
  each with its edge back to the head in slot 0 (`ExtraEdge::BackTo`): the
  ring head → vector → entity → head in place of the segment chain. The
  counts stand, 0–3 collections of 10–100 entities (`draw_the_orm`); the
  segments' positions go back to the tree.
- **The result set.** One vector a request at a tree position drawn
  uniformly, its rows the next `RESULT_ROWS` = 15 tree positions in build
  order (Laravel's default page, `Model::$perPage` [F]; one page a request
  [A]), clamped to the positions left.
- **Sessions.** A session's head holds in slot 0 a string-keyed hash in
  place of the chain: the session's other 19 objects under keys, each
  holding the head in its slot 0, and an integer under `t`; the core edge
  stays in slot 1. Keys are a per-mutator table of 1,024 long-lived strings
  built at setup (`make_the_keys`); a string never registers
  (`src/refcount.rs:473-482`).
- **The session write**, the request's last step, as PHP saves a session at
  the script's end [A]: `SESSION_WRITE_PERCENT` = 30 % of requests write,
  `SESSION_APPEND_PERCENT` = 50 % of those [A] a new object of a drawn size
  under the hash's next key, the rest, and every write to a hash at
  `SESSION_ENTRIES_MOST`, the request number into `t`. The new object is in
  the directory's bytes before any later step can replace the session, and
  is reported born at the request's end (`Ended::born`). At setup each
  session takes its prior appends, a geometric count of mean 15
  (`PRIOR_APPENDS_END`, the model of `SESSION_WRITE_STEADY_SHARE`).

### Element writes

Every element write moves the child's creation reference into the array,
through `array::testing::push` on a vector or `testing::raw_insert` under a
fresh key on the hash (`src/array/testing.rs:177-196`), the key's reference
retained for the table before the insert; or stores an integer. These are
the crate's test-only writes to the representation, not the element layer's
`write_through`, which a program's stores take; both arms run the same path,
and under B the mutable view tags the array as the holder
(`src/array/entity.rs:184-193`). No release follows a write, so a write
registers no root and `Request::advance_at_most` counts the events it
counted for `web-heap`. Every write is asserted, since a refused one would
lose its reference. No write overwrites or unsets a counted element; every
array is held by one slot at a count of 1; none is shared, nested or holds
itself.

A body of at most `BLOCK_PAYLOAD` = 65,280 B
(`src/memory/block_pool.rs:30-34`) is a buffer-arena chunk, a larger one an
OS-direct run, and an array with an OS-direct body is ineligible for the
collector's frees (`collector_frees::eligible`). The lengths stay below it:

| constant | value | basis |
| --- | --- | --- |
| `VECTOR_MOST` | 2,048 elements | [code]: 16 B a `Value`, capacity 8 doubling: 2,048 is 32 KiB, 2,049 grows to 65,536 B (const assertions in `in_arrays.rs`) |
| `SESSION_ENTRIES_MOST` | 1,024 entries | [code]: 32 B an entry and 4 B a slot at 2,048 slots: 40,960 B |

A holder's vector holds at most `FAN_OUT` = 8, a collection 100, the result
set 15.

### What the rig reports

- **Garbage by class.** `CLASSES` = 4: 64, 128 and 512 B objects and
  `ARRAY_BYTES` = `size_of::<LLArray>()` = 112 B arrays (`CLASS_BYTES`,
  `the_web_loads.rs:229-235`). `Plan::bytes`, `held_by_size`, `Ended`,
  `Advanced` and `Garbage` answer the four, an array's 112 B born at the
  step that creates it; `web_garbage_mean_bytes_112` and
  `web_garbage_peak_bytes_112` read the fourth. `LongLived::new` asserts
  that the 112 B class holds arrays only (a key is a 24 B string).
- **Bodies**, reported apart and not added (the Sage), by test-only
  counters a thread at `body_alloc` and `body_free`
  (`memory::routing::body_bytes`), each with its OS-direct share:
  `web_body_bytes_granted`, `_given_back`, `_os_direct_granted`,
  `_os_direct_given_back`; under B the bodies the collector posts remote,
  `frees_body_bytes_posted`; the sessions' live bodies,
  `web_body_bytes_live_at_the_stop`; and `web_body_bytes_dead_at_the_stop`,
  the outstanding less the posted and the live.
- **Validity, every cell.** `web_array_share_percent`: array entities and
  bodies made over the garbage made with the bodies, both sides counting
  bodies; a cell under 10 % is not array-bearing and is reported so. B only:
  `frees_arrays` over `frees_members`, the arrays among the members the
  collector freed.
- **Refusals.** `tag_sets_touched_in_an_array`, the sets
  `tag_sets_touched` counts whose touched member is an array; the split by
  the origin of the batch's root is deferred. Reporting only (the Sage; the
  denominator is Edmond's, `PLAN.md:348-350`).
- **Counts.** `web_session_writes`, `web_slow_requests`,
  `web_large_result_sets`, `web_array_bytes_made`, `web_body_bytes_made`,
  and `frees_not_ineligible`.

### The cells

`web-heap`'s protocol as S68 reads it (`dev/BENCHMARKS.md:13-18`): A, the
default `gc-checkpoint`, against B, `gc-window`
(`dev/design/gc-kinds.md:9-12`); `dev/tools/arms.sh <out> 5 web` with
`ARMS="A B" WEB_MUTATORS=1,2 WEB_COLLECTORS_CAP1=3 CAPS=1`, two mutators
on CPUs 1–2 and cap 1 on CPU 3; 116 s with a 20 s warm-up and a 12 s
drain (`arms.sh:91-99`); five cells an arm, interleaved, the order rotated
a repeat (`:107-114`); a void cell run again once (`:17-19`). The
interarrival comes from a pilot by this document's rule (the arrivals row,
"Common to both"): one A cell at `web-heap`'s 46.78 ms, then
46.78 ms × `arrival_busy_share` / 0.6 (`BUSY_SHARE`) [inf: the share
scales as the inverse of the interarrival below saturation]. `WEB_LOADS`
runs `web-heap` at the same interarrival beside it, interleaved
(`arms.sh:15`, `:120`), as the control: the arrays' effect is B's margin
over A on `web-heap-arrays` less B's margin over A on `web-heap`.

| S68 clause (`PLAN.md:127-129`) | column (`the_rig.rs`) | met when |
| --- | --- | --- |
| mutator CPU within +3 % of the default build | `mutator_cpu_us`, the load's own, A against B; no microbenchmark (the Sage) | B's median ≤ 1.03 × A's |
| longest owner pause under 5 ms in every cell | `disposal_longest_cpu_us` and `frees_longest_application_cpu_us`, the thread's CPU time (`CLOCK_THREAD_CPUTIME_ID`) of the longest disposition and of the longest application of the collector's frees, beside the walls `disposal_longest_us` and `frees_longest_application_us`; S68.13 closed reading the pause so (`dev/DECISIONS.md:25-45`) | each B cell's CPU times under 5,000 µs; the walls and A's `verdict_collection_longest_us` reported |
| held garbage no worse | `web_garbage_mean_bytes`, `web_garbage_peak_bytes`, the 112 B class included; `web_garbage_at_the_drain_end_bytes` | B's median ≤ 1.10 × A's + 64 KiB (mean) and 1.25 × A's + 256 KiB (peak), the floors of "Gates" below; 0 at the drain's end |
| Δ-refusals under 10 % of batches | `tag_sets_touched` over `batches` | under 0.10 in each B cell |

`tag_sets_touched` counts over the whole process and `batches` from the
cell's start to its end (`take_outcomes`) [inf]; the ratio is read over
that span, as on `web-heap`.

**Side loads, outside the gate.** Two loads of their own, read by the same
columns and reported beside the gated one, so that their requests mix into
no gated cell; the gated `web-heap-arrays` and the `web-heap` control have
neither. Both draw from `Purpose::Arrays`.

- `web-heap-arrays-large`, `web_large_result_percent` 5 [A] (the Sage): a
  request so drawn gives its result set `LARGE_RESULT_ROWS` = 4,000 rows
  [A], capacity 4,096, a 64 KiB body, the smallest past the block payload.
  The rows are leaves, with no extra edge and no free slot, and the
  vector's position is drawn uniformly in [0, tree − rows], so the
  OS-direct body seeds a set of its own rows and little else (the Critic);
  read by `frees_not_ineligible` and the OS-direct body columns.
- `web-heap-arrays-slow`, `web_slow_percent` 2 [A] (ruled
  2026-10-12): a request so drawn waits first uniform in 150–300 ms
  (`SLOW_WAIT`) [A] instead of the lognormal, past the window's young cut
  of 100 ms (`dev/DECISIONS.md:47-52`), so its roots can be offered while
  it sleeps and its later births append into vectors born before the wait
  [inf].

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
- Laravel, `Illuminate\Database\Eloquent\Model::$perPage` (15)
- Express issue #2560 (`req.res`, `res.req`)
- Sam Saffron, "Demystifying the Ruby GC"
- Shahriyar, Blackburn, Frampton, "Down for the Count?" (ISMM 2012)
- Jones and Ryder, Java object demographics; NG2C, middle-lived data in servers

