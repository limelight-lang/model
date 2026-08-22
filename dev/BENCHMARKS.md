# Benchmark log

Every measurement lands here, not in chat and not only in a commit
message. Negative results are recorded too — "tried it, it did not
pay" saves the next attempt and is usually worth more than a win.

`benches/RESULTS.md` holds the crate's headline
comparison against other allocators. This file holds the *change log*:
what was tried, measured, and accepted or rejected.

## Method

```
cargo bench --bench standard -- our_heap    # larson, rptest — Heap
cargo bench --bench alloc                   # arena vs bumpalo/malloc/mimalloc
```

**Measure both arms back to back in one session, against a freshly
taken baseline:**

```
git stash push src/...
git checkout <base>
cargo bench --bench standard -- --save-baseline fresh our_heap
git checkout main && git stash pop
cargo bench --bench standard -- --baseline fresh our_heap
```

**The rule, stated plainly: performance is only ever measured live,
before and after, in one sitting.** A stored baseline records numbers,
never the conditions they were taken under, so comparing against one
taken earlier reports `code change + machine drift` as if it were the
change alone. Never compare against a baseline taken earlier in the
day. See the 2026-07-20 entry in `POSTMORTEM.md` for what that cost.

**Run A, then B, then A again.** The second measurement of the *same*
code is the control. If the two A's disagree, the run is void — all of
it, however good B looked. This is the only check needing no prior
history, and the only one that catches load arriving mid-session, which
the back-to-back rule does not.

**Throw away the first run.** It is systematically slow: measured at
854 µs against ~700 µs for later runs of the *same binary* — 22% off,
reproducibly. Cold caches, cold branch predictors, a cold frequency
state. Treat the first measurement as warm-up and discard it. Several
comparisons were wrecked by using it as arm A.

**A band of "normal" values is less useful than it sounds.** One was
recorded here as 749–769 µs for larson and 1.89–1.93 ms for rptest, and
a day later the same code ran at 686–730 µs and 1.72–1.78 ms — the band
had captured a warm machine and was quietly biasing every later
comparison against it. If a band is kept it has to be re-taken cold, and
it is only good for spotting gross contamination (a value 50% out), not
for validating a few-percent delta.

**Know the noise floor before believing a delta.** Repeated runs of an
identical binary, first discarded, still disagree by **1.5–3%** on this
box, and an occasional run comes back with an interval 12% wide. Nothing
smaller than that is resolvable here, no matter what p-value criterion
prints.

**Build both arms before measuring either.** `git stash` touches file
mtimes, so `cargo bench` recompiles between arms and each measurement
then starts on a machine still busy from its own build. That alone
produced a monotonic "improvement" across three arms that was pure
recovery from the preceding compile. Build first, measure after:

```
git stash push src/...
cargo bench --bench standard --no-run
cp "$(ls -t target/release/deps/standard-*.exe | head -1)" bench_A.exe
git stash pop
cargo bench --bench standard --no-run
cp "$(ls -t target/release/deps/standard-*.exe | head -1)" bench_B.exe
./bench_A.exe --bench --save-baseline A1 our_heap
./bench_B.exe --bench --save-baseline B  our_heap
./bench_A.exe --bench --save-baseline A2 our_heap
```

**`--bench` is required when running the binary directly.** Without it
criterion runs each benchmark once as a smoke test and prints `Success`
instead of timings — a silent no-op easily mistaken for a real run,
because cargo normally supplies the flag.

**Noise signature.** A large change of similar size across benchmarks
that stress different things is machine noise, not a result. These runs
happen on a dev box with an IDE running; differences around 1–2% are
near the resolution limit, so a result that small is worth repeating
before it is believed.

**Know when the box is unmeasurable.** After hours of compiles, Miri
runs and benchmarks, everything can settle 40–80% outside the bands
above — thermal, most likely. At that point the two control A's diverge
by ~10% and nothing smaller is resolvable. Stop and measure another
day; no statistic rescues a measurement taken then.

**Do not record per-variant figures in code comments** unless they came
from a back-to-back run. Direction ("this alternative measured slower")
is safe to write down; a number that will not reproduce is worse than
no number, because the next person will trust it.


## 2026-08-22 — the counted pair against its working set: 4 ns hot, 88 ns at a million entities, and a spread of seven

S5.4b of `rfc/dev/PLAN.md`:
`memory::barrier::tests::what_a_counted_pair_costs_when_headers_miss::measure_cold_pair_cost`,
`cargo test --release --lib -- --ignored measure_cold_pair_cost --nocapture`,
11th Gen Intel i7-11700K, 8 cores, L2 4 MiB, L3 16 MiB, WSL2. Eight runs of
the whole probe, each run 15 timed rounds per arm after a discarded warm-up,
median of the rounds.

The question is what a store costs when the two foreign headers the pair
touches are out of cache. `what_a_store_costs_by_working_set` does not answer
it and says so: its cold half warms every child header before the timer
starts. This probe varies the child population instead and subtracts a plain
arm that makes the same scattered read of the value vector and the same slot
write without touching a count, so the difference is the pair.

| children | pair, ns/store, eight runs | median |
|---|---|---|
| 1 | 3.5 3.6 3.9 4.1 4.2 4.3 5.8 6.4 | **4.1** |
| 64 | 3.7 3.8 3.9 4.2 5.5 6.0 6.2 7.1 | **4.9** |
| 4 096 | 4.0 4.0 4.2 5.0 5.8 5.9 7.1 7.8 | **5.4** |
| 65 536 | 13.0 18.8 27.6 28.4 43.1 54.6 66.5 91.1 | **35.8** |
| 1 048 576 | 27.2 48.9 57.7 76.8 99.8 115.3 182.5 185.4 | **88.3** |

**The instrument agrees with the one already here at the narrow end.** The
existing figures put the counted publish at 2.74–2.82 ns and `drop_ref` of
the displaced value at 0.85, so an overwriting store costs 3.6–3.7 ns where
the headers are warm. The narrow end of this probe reads 4.1. The bare
`retain_release_nonfinal` bench at 1.84–1.87 is not the comparable figure:
it carries neither the category test nor the slot write.

**The direction is settled and the magnitude is not.** Every run is monotone
in the working set, and the pair at a million entities is an order above the
pair at a thousand. The spread at that end is a factor of seven run to run,
so 88 is a median over a heavy tail, not a figure to plan against. Fifteen
rounds do not resolve a distribution this shape; a run that wants the
magnitude needs more rounds, pinned frequency, and pre-faulted pages.

**What it decides.** `rfc/model/gc/gc-horizon-v2/questions.md` node N puts
the crossover of the deferred regime against today's counted lowering at 1 to
40 elided stores per live entity per pass, and says the answer turns on the
share of counted stores whose headers miss, estimated there at ~80 ns and
marked unmeasured. The estimate is inside the measured range and near its
median. The store path therefore carries real money at large working sets,
and node N's low end of the crossover is the one in force for a heap that
does not fit cache.

**What it does not decide.** The population here is one class of empty
objects allocated consecutively and addressed by a scattered cursor, which is
the worst case for the header, not an observed PHP heap. The plain arm also
degrades — 0.45 to 15.6 ns — because its own read of the value vector misses,
and real code has the value in a register from a previous load, so the
absolute store cost at the wide end is above what a program pays. Only the
difference is read here. The horizon's elisions apply on top of all of it:
what a proven owned slot never pays does not appear in any of these figures.


## 2026-08-16 — what an epoch parks, in counts that repeat exactly

S26.5: `collector::tests::the_epoch_as_a_whole::measure_parked_memory`,
a count instrument — the stepped epoch runs no collector thread, and
debug and release print identical figures, so the clock's noise floor
does not apply. One 16-byte class, deaths landed in two arms: victims
born before the epoch, killed after the snapshot; victims born and
killed mid-epoch, after the walk.

| deaths per arm | parked after arm 1 | after arm 2 | at close, before the flush checkpoint | after it |
|---|---|---|---|---|
| 100 | 100 | 200 | 200 | 0 |
| 1 000 | 1 000 | 2 000 | 2 000 | 0 |
| 10 000 | 10 000 | 20 000 | 20 000 | 0 |

The formula is read directly: **parked records = deaths while the
epoch is in flight, one for one, regardless of when the victim was
born** — the churn-times-duration bound of the 2026-07-27 correction,
with the live heap nowhere in it. Bytes are records times the class's
slot (16 here), by the probe's construction rather than by header
reads. The memory returns not at `close` but at the owning thread's
next checkpoint, which the table's last two columns exhibit — and
which is what the design means by "the memory returns at the thread's
first death or poll". The wall-time translation stays the reader's
arithmetic: records/ms at an assumed churn rate times the epoch
figures of "fresh brackets on one HEAD".

The mid-born arm parking one-for-one is also the measured case for the
unbuilt young-free exemption (`rfc/BACKLOG.md` via `rfc/model/gc/rc-walk.md`,
"Deferred physical release"): an entity born and dead inside one epoch
is in no snapshot row, and every record the second arm adds is a
record that exemption would remove.

## 2026-08-16 — AArch64 reads the header with plain loads and stores

S26.6, an inspection rather than a measurement: `rustup target add
aarch64-unknown-linux-gnu`, `cargo rustc --release --target
aarch64-unknown-linux-gnu -- --emit asm`, and the generated code read.
`ll_retain` is `ldr w8,[x0,#4]` (the 4-byte flags half), a branchless
gate through `ands`/`tst`/`ccmp`, and the counter as
`ldr`/`add`/`str` — three plain accesses where x86-64 has one `incl`,
which is the architecture's memory model, not a cost claim.
`ll_release` matches, with the death test fused through `orr`/`cbz`
and the handshake byte an `ldrb`. No `ldxr`/`stxr` pair, no LSE
atomic, no `dmb` anywhere on either path.

The crate's 84 atomic sites all live where the design puts them: the
collector (`walk`, `epoch`'s ack and attend), the block pool's
cross-thread structures, the buffer arena's remote free, the intern
table, and `Arc` — none on the counted hot path.

**The cost half of `rfc/model/gc/rc-walk.md` open question 2 stays
open**: instruction identity is confirmed, but no ARM hardware exists
here to pair the instructions with a clock, and the x86 lesson of
2026-07-27 (a 3x effect invisible in instruction choice) is exactly
why identity is not a cost claim.

## 2026-08-16 — store and lifecycle canaries: the slot allocator beats malloc on its own cycle

S26.3, in the same canary binary and under the same protocol (three
passes by three process runs, rotation, acceptance re-run). Wide set,
hot, and one finding before any figure: **no canary arm can reach the
store barrier, because every barrier ABI entry resolves a context and
the C ABI has no door that constructs one.** The counted-publish
comparand therefore stays with the in-lib probe, cross-instrument,
which both instruments' measured zeros now permit for effects this
size.

| arm | figure | spread |
|---|---|---|
| plain 8-byte pointer store | 0.33 ns | 0.326–0.341 across nine cells |
| `ll_create_die` (ReferenceBox, 24 B, full factory + teardown) | 3.4–4.0 ns steady | first printed pass of every process 14.2–15.3; spikes to 44–52 in later passes |
| `malloc` + three-word init + `free` (24 B, glibc) | 6.4–6.9 ns | run-to-run drift 6.37 → 6.93 |

**The counted publish costs ≈ 2.4 ns over a plain store** — in-lib
`heap → heap` hot reads 2.74–2.82 on this HEAD against the canary's
0.33, and what that buys is the retain, the category test and the COW
door: the semantics, not overhead looking for a cure.

**The entity lifecycle in its steady mode runs ≈ 2x under glibc's
malloc/free on the same 24 bytes** — 3.4–4.0 against 6.4–6.9 — with
the factory contract, kind-dispatched teardown and slot recycling
inside the figure. Two honest asterisks. The first printed pass of
every process reads 14–15 ns and settles to 3.4–4.0 from the second
pass on, a per-process warm-in the unprinted warm-up pass does not
absorb, cause unresolved — and the criterion `create_release_die`
(16.5 ns, a classed object with the constructed hook, not a box) sits
suspiciously near that slow mode, so the two entities' paths should
not be conflated until someone separates entity kind from mode. And
1–2 rounds in 15 spike to 44–52 ns in every pass, an excursion the
median absorbs and the maxima report.

## 2026-08-16 — fresh brackets on one HEAD, and the 2.78 contradiction dies of staleness

S26.2: every figure the case document will quote, re-taken on this
HEAD in per-bracket sessions, each with its own resolution line. And
the cross-instrument contradiction S26.1 held its pair row on — 1.98
through a real call against 2.78 in-crate — resolves the way
`dev/POSTMORTEM.md`'s oldest entry warns: **the 2.78 was July's figure
of July's code.** S24.2's narrow accessors landed in between. On
today's HEAD the criterion pair reads 1.84–1.87, and the canary's 1.98
sits 0.11–0.14 *above* it — the ABI call, in the direction the
declared bias requires. The pair row is unblocked for the case.

The criterion bracket (`benches/lifecycle.rs`, A → B → A2 with both
binaries built first; A/A2 are the resolution lines):

| bench | rc-walk (A / A2) | rc-trace (B) |
|---|---|---|
| retain_release_nonfinal | 1.867 / 1.838 ns (1.6 %) | 2.252 ns |
| create_release_die | 16.93 / 16.45 ns (2.9 %) | 14.27 ns |
| harness_skeleton (new arm) | 0.330 / 0.340 ns | 0.335 ns |

The pair under rc-walk now measures 18 % below rc-trace — in July it
was 4 % — and the create+die tax holds at +15–18 %, the July band. The
new `harness_skeleton` arm prices the bench's own iter-and-black_box
machinery at 0.33 ns, refuting the harness-term account of the
contradiction before the stale-baseline account settled it: net of
each instrument's skeleton the two pairs sit at ≈ 1.52 (in-crate) and
≈ 1.63 (through the call).

The store-direction bracket on this HEAD is this file's entry "the
null sweep bounds the instrument" (same day, five rc-walk and three
rc-trace runs). The epoch probe, three runs, 100 000 entities, steady
rounds 1–3 per run: singletons 3.23–4.06 ms (32–41 ns per entity),
chain 7.2–10.8 ms (72–108 ns per entity), the third run reading
systematically high on chain — the machine, not the shape, since its
singleton rounds sit inside the band. Resolution on this instrument is
the range, roughly ±15 % around the middle, and the July "45–78 ns"
generalized two shapes this grid keeps apart.

## 2026-08-16 — the pair against its canaries: 1.8–2.2 through the ABI, 0.55 bare, 11.6 atomic

S26.1: `bench-external/canary/pair_canary.cpp`, the first canary probe —
naive C++ loops beside the same operation through the real C ABI, one
binary, five arms, acceptance by `accept.sh` re-run after every rebuild.
Protocol: an unprinted warm-up pass, three printed passes of 15 rotated
rounds, and three runs of the process, because passes inside one process
share one draw of page placement. Staticlib from HEAD `6c733c5`,
rc-walk, rebuilt immediately before linking. Hot figures only — the
probe has no eviction mode — and the working-set axis varied nothing
here: both sets are L1-resident, and the arms read the same at 1 and
at 64 children, one disturbed set-1 cell aside (naive at 0.63, 14 %
over its band). The grid below is the wide set, nine cells (three
passes by three runs).

| arm | figure | spread across the grid |
|---|---|---|
| shipped pair, `ll_retain`/`ll_release` | 1.77–2.20 ns | three modes ≈ 0.21 apart; 1.98 the most common |
| its duplicate (instrument zero) | 1.98 ns | 1.979–1.984 in eight cells, 1.77 once |
| bare non-atomic inc/dec + branch | 0.55 ns | 0.552–0.560 |
| `std::shared_ptr` copy/drop | 11.57 ns | 11.566–11.586 |
| loop skeleton | 0.35 ns | 0.352–0.359 |

**The instrument's zero is a mode, not a jitter band.** An ll-shaped
arm sits a whole pass in one of three states about 0.21 ns apart —
same binary, same data, the mode flipping between passes and between
process runs while the bare arms never move. Worst same-arm spread
across cells is 0.43 ns, worst arm-to-arm disagreement inside one pass
0.22, so a difference under ≈ 0.4 ns between ll-shaped arms is
unresolved on this instrument. The bare arms' repeatability (0.01 over
nine passes in three processes) places the mode in the ll arms; whether
it sits in their code or in the boxes' cache placement — data the bare
arms never touch — this probe does not decide.

**The atomic figure was a mislabel until review caught it.** The first
build read `shared_pair` at 4.59 ns; the disassembly showed `lock addl`,
and the number still was not the atomic pair — glibc branches the
counter ops on `__libc_single_threaded`, and a process that never
spawned a thread priced the fast path. One spawned-and-joined thread at
startup moved the arm to 11.57. Both figures are real and both are
worth having: 4.6 is the scope pattern in a process glibc knows is
single-threaded, and 11.6 is the same arm once a second thread exists.
Neither is pure atomics — the arm carries its own null-check destructor
branch and `_Sp_counted_base` dispatch — so the row prices the
`shared_ptr` scope pattern, and 11.57 − 1.98 is not "what atomics
cost". What separates either figure from the shipped pair is what
`rc-walk`'s single-mutator, non-atomic counter refuses to pay.

**The bracket, stated with its biases.** The counted pair through a
real ABI call sits at ≈ 3.6x a bare inc/dec that no shipping runtime is
(the bare arm has no flags test, no immortality gate, no null path —
it is "what this loop costs", not "what naive RC costs") and at ≈ 0.17x
the atomic scope pattern a multi-threaded ARC pays. The call bias runs
against our arms — the production route inlines through merged bitcode —
so the first ratio is an upper bound on this box.

**One contradiction is open, and the pair row stays out of the case
document until it closes.** The in-crate criterion bench put the pair at
2.78 ns (2026-07-27, another HEAD and another harness); this probe reads
1.98 through a call the criterion figure does not cross. Same nominal
operation, inverted by more than the ABI bias story allows — so one of
the two instruments is mismeasuring the pair, and S26.2's fresh
same-HEAD bracket owns the answer.

## 2026-08-16 — the failed store-forward is the stall itself, not the log serialized behind it

S25.2, answered by the rule the plan registered before the run, on
Δ = wide − narrow per direction. The wide binary is the two-line revert
of `e9e43b2`'s accessors — `header_flags` and `header_refcount` back on
`mutator_load_header` — built on a scratch branch and discarded; the
narrow binary is HEAD `b18f6d2`. One debug run of the wide probe first
(asserts held), then three runs per binary, alternated in one session.
Wide set, hot passes, medians per pass:

| direction | wide | narrow | Δ per pass |
|---|---|---|---|
| `heap → heap` | 5.50–5.93 ns | 2.63–2.93 ns | **2.77–3.19, ≈ 3.0** |
| `heap → arena` | 5.52–6.27 ns | 3.29–3.81 ns | 2.08–2.77, ≈ 2.45 |
| `arena → arena` (control) | 2.69–2.97 ns | 2.53–2.87 ns | −0.01–0.18 |

The control writes and reads no counter, so its Δ bounds the
cross-binary layout term at ≤ 0.18 ns — fifteen times under the effect.

**The verdict, by the rule's first arm.** Δ(`heap → heap`) ≈ 3.0 sits at
the 3.28 anchor on a direction with no log at all: the stall costs its
full price with nothing serialized behind it, so it is intrinsic to the
failed forward, and the latency account of the 2026-07-27 trap — a
stall that hides under independent work and costs only on the critical
path — is wrong in general. The wide load over a fresh narrow store
behaves as a throughput penalty of ≈ 3 ns per store on this box. What
survives of the latency account is a residue: Δ(`heap → arena`) runs
≈ 0.6 ns under Δ(`heap → heap`) in the same session, so about a fifth
of the stall does overlap the log's independent work — a minority, not
the mechanism. The 2026-07-27 and S24.2 figures (10.2 ns on the pair,
4.82 → 1.53 on `heap → arena`) stand; their account moves from
"latency exposed by the dependent chain" to "penalty paid per
occurrence, partially maskable".

## 2026-08-16 — the null sweep bounds the instrument, and rotation settles the statistic

The probe gained the two controls the S25.1 Critic round demanded: a
null sweep — the sweep's two loops at the same `k` with both owners on
the GC heap, whose slope is zero by construction — and an arm order that
rotates with the round index, so no arm inherits the cache and pool
state of the same neighbour every round. Each arm now prints its
minimum, median and maximum. Protocol: one debug run first (the drain
asserts all held), both release binaries built before any run, three
runs per build alternating rc-walk and rc-trace, then two more rc-walk
runs to separate drift from spread. HEAD `acddfd0` plus the probe
change; nothing below is comparable with the 2026-08-15 table — the
instrument changed.

**The instrument's zero is measured now, not argued.** At the wide set,
hot, the null slope reads −0.036 to +0.027 ns per record across five
runs: the two-loops-two-slots term the sweep was suspected of carrying
is absent where the record is priced. It is real elsewhere — 0.05–0.20
cold, and 0.08–0.39 at the single-child set, where the dependency chain
is longest — so the record's figure is quoted from the wide hot point
and nowhere else.

**One record costs ≈ 0.45 ns with the log cache-hot** — the sweep's
median-based slope over five runs reads 0.36–0.52, median 0.45, net of
a null within ±0.04. Cold the slope reads 0.64–0.71 against a null of
0.05–0.20, so ≈ 0.5–0.6 net. The direction difference
`heap → arena` − `heap → heap` reads 0.61–0.79 and still disagrees with
the sweep; the rule registered on 2026-08-15 stands and the slope is
the answer.

**The statistic question is closed: the fixed order was the
inflation.** Under rotation the median-based hot slope lands at
0.36–0.52 — where the *minimum* statistic sat on 2026-08-15 (0.38,
against the median's 0.72). Per-arm minima sit 2–7 % under their
medians with occasional right-tail maxima, which is the interference
account of spread, not the layout one: the old medians were inflated by
each sweep point warming its successor in a fixed, `k`-monotone order.
Medians stay the quoted statistic; under rotation they and the minima
agree.

**The rc-trace anomaly persists and grew.** On log code that is
byte-identical, the rc-trace build's hot sweep slope reads 1.12–1.32
(one pass read 0.50), its null 0.08–0.13; its `sweep k=0` sits
0.25–0.43 ns *below* its `heap → heap`, where the instrument's zero
requires equality and the rc-walk build delivers it within 0.03; and
its `heap → heap` itself reads 3.9–4.3 ns against rc-walk's 2.74–2.82.
Unresolved, as on 2026-08-15; S25.2's wide-accessor arm measures the
mechanism that could explain it and meets these figures again.

**This session's repetition floor is wider than the last one's.**
`heap → arena` warmed monotonically, 3.35 → 3.58 ns over the first
three runs, and held 3.55–3.58 for the last two; a session's early
runs carry the machine's warm-up, so the directions repeat within
0.5–3.3 % across all five runs and within ~1 % after the plateau. The
S25 criterion quotes 0.1–1.7 % from the previous session; this session
did not reach that floor, and the slope's own spread (±0.08 around
0.45) is the honest uncertainty on the headline figure.

## 2026-08-15 — what the release-at-reset record costs, and the statistic that decides the answer

**Retracted in part the same day, by the Critic round on S25.1, and the
retraction was checked against the run data rather than taken on the word of
the review.** Four of the paragraphs below are wrong, and the reader who
needs a number today should take only the hot one, 0.5 ns, and take it as
bounded rather than measured:

- **The cold figure, 1.2 ns, is withdrawn.** `sweep k=0` and `heap → heap`
  run the same thousand publishes into the same slot of the same holder, so
  their difference is the instrument's zero. Hot it reads 0.05 ns per store;
  **cold it reads 1.22**, which is the whole of the figure the entry claimed.
  The paragraph below quotes that control only in the half where it passes.
- **The `rc-trace` override is withdrawn**, and with it the ground for
  rejecting the tie-break S25 registered in advance. The explanation offered
  — a layout term attaching to `heap → arena` alone, which draws its owner
  from the arena where the other two draw a holder from the heap — requires
  `sweep k=0` to equal `heap → heap` there. It sits 0.61 ns below it. The
  mechanism is contradicted by the larger of the two gaps it was invented for.
- **The carve term biases the slope, not the residual.** A step of 0, c, c,
  2c, 2c against k regresses to a slope of 0.002·c per record and leaves only
  ±0.2·c behind, so the paragraph that puts the carve in the residual has it
  backwards. The residual — 578 ns cold at the wide set, against a fitted
  1456 — is unexplained, and the cold sweep it comes from is not monotone:
  3.87, 4.48, 5.47, 5.48, 5.19 ns per store, with `k` = 1000 cheaper than
  `k` = 500. A least-squares slope over that series is not a marginal cost.
- **The cold half does not evict the child headers.** `median_ns_per_store`
  walks the scratch *after* a round, and the next round's `children()` writes
  all 64 headers before its timer starts, so they are warm in both halves.
  The decomposition below — 0.67 ns on `arena → arena` against 2.10 on
  `heap → heap`, "which writes one" — attributes the cold penalty to a cause
  the order of operations rules out. What the walk does reach is the log's own
  pages, the TLB and the instruction lines.

Two more findings stand against the design rather than against a number, and
both are S25.1's remaining work. The sweep has **no null arm**: its two loops
are two code bodies at two alignments writing two slots in two allocators, so
nothing in the probe bounds what they differ by apart from the log, and a
`null_sweep_round` with both owners on the GC heap would read that term
directly. And the arms run in a **fixed order monotone in k**, with no
eviction between them in the hot half, so each sweep point inherits the cache
state left by the point below it and the pollution grows along the axis being
measured; rotating the arm order by the round index closes it.

**One record costs about 0.5 ns with the log cache-hot**, under `rc-walk` at
the 64-child working set, and the two instruments bound rather than resolve
it: over two sittings the sweep's slope reads 0.72 and 0.44 and
`heap → arena` minus `heap → heap` reads 0.54 and 0.48, against a null-pair
error of 0.05 in the same half. The cold figure is withdrawn above. S25.1.

**What the record is:** `Arena::log_release_at_reset` appends the child's
address to the arena's release-at-reset log when a `GcHeap` child is
published into a `RequestArena` owner, and the reset owns one release per
record. The child's own category cannot isolate it — `ll_retain` returns
early exactly where the log stays silent — so both instruments vary the
owner. The new `heap → heap` direction takes the same counted retain from the
same allocator and appends nothing; the sweep varies only `k`, how many of a
region's 1000 stores name an arena owner rather than a heap one, inside one
timed region. Neither figure is a subtraction across arms that differ in the
retain, which is what the 0.45 ns of S24.2 was.

The probe is
`memory::barrier::tests::what_a_store_costs_by_working_set::measure_store_cost`,
`#[ignore]`d and run explicitly. The library is unchanged; only the probe
moved.

```
cargo test --release --lib --no-run                         # rc-walk
cargo test --release --lib --no-default-features --no-run   # rc-trace
<each binary> --ignored --nocapture measure_store_cost      # five runs, alternating
```

Median of fifteen rounds per shape after one discarded round, 1000 publishes
per round, nanoseconds per store, median over the surviving runs of five:

| shape | set | walk hot | walk cold | trace hot | trace cold |
|---|---|---|---|---|---|
| arena → arena | 1 | 3.00 | 3.60 | 3.54 | 4.46 |
| arena → arena | 64 | 2.82 | 3.49 | 3.69 | 4.29 |
| heap → arena | 1 | 3.57 | 5.33 | 5.17 | 6.69 |
| heap → arena | 64 | 3.57 | 5.58 | 5.26 | 6.92 |
| heap → heap | 1 | 3.27 | 3.96 | 4.67 | 5.65 |
| heap → heap | 64 | 2.99 | 5.09 | 4.68 | 5.55 |
| arena → heap | 1 | 3.54 | 4.80 | 4.26 | 5.98 |
| arena → heap | 64 | 3.46 | 5.00 | 4.23 | 6.58 |
| sweep k=0 | 64 | 3.04 | 3.87 | 4.17 | 5.32 |
| sweep k=250 | 64 | 3.12 | 4.48 | 4.32 | 5.71 |
| sweep k=500 | 64 | 3.25 | 5.47 | 4.66 | 6.37 |
| sweep k=750 | 64 | 3.25 | 5.48 | 4.85 | 6.39 |
| sweep k=1000 | 64 | 3.42 | 5.19 | 5.07 | 6.88 |

**The statistic decides the answer, and it decides it by a factor of two.**
Taking the fastest of an arm's fifteen rounds instead of their median halves
the record: measured against each other in one sitting, five runs each and
the binaries alternated, the wide-set hot figure is 0.38 ns per record from
the minimum and 0.72 from the median, and the cold figure 0.65 against 1.42.
The minimum is the tighter statistic — its slope spans 0.105 ns across five
runs where the median spans 0.276 — and it is the wrong one, because the
rounds of an arm do not differ only by interference. Each round allocates its
children and its log segments afresh, so each meets a different layout, and
the fastest round is the luckiest layout rather than the least-disturbed one.
A program pays the typical layout.

**Hot against cold is 0.7 ns per record**, and cold is what a request pays:
it writes a record into a line it never revisits, whereas a second timed
round of the same shape finds the line the reset's own drain has just read.
The cold half walks 32 MiB of scratch untimed between rounds — larger than
this box's 16 MiB L3 — which takes the child headers out of cache along with
the log. That costs 0.67 ns per store on `arena → arena`, whose retain
returns early and writes no header at all, and 2.10 on `heap → heap`, which
writes one; the record's line is what `heap → arena` adds on top. The sweep
does not carry that term: its children are the same across all five points,
so what varies with `k` is the record's line alone.

**Under `rc-trace` the sweep and the direction difference disagree by about
2x**, 1.12 against 0.67 ns hot and 1.56 against 0.89 cold, where under
`rc-walk` they agree. The log's code is the same in both builds, so at most
one of the two `rc-trace` readings can be the record. The endpoints say which
half is out of line: under `rc-walk` the sweep at `k` = 0 sits within 0.02 ns
of `heap → heap` and at `k` = 1000 within 0.01 of `heap → arena`, while under
`rc-trace` the same endpoints sit 0.61 and 0.19 below them. What differs
between the two instruments is one allocation — the sweep and `heap → heap`
each draw a holder from the GC heap and `heap → arena` draws its owner from
the arena instead, so the children of that one arm meet a different heap
layout. Why that costs 0.6 ns under one build and 0.02 under the other is
unresolved. S25.2 measures the same two directions and will meet it again.

This overrides the tie-break S25 registered in advance, which named the slope
the answer whenever the two instruments disagree. That rule was written
before either had been run against the other; the cross-build identity of the
log's code outranks it, and here it is the `rc-trace` slope that the identity
refuses.

**A per-record figure carries a fraction of a segment carve.** A log segment
holds `LOG_SEG_RECORDS` = 500 records and is carved from the arena's own
bump, so `k` records cost `1 + (k - 1) / 500` carves and the sweep's five
points stand at 0, 1, 1, 2 and 2 of them. That is a step and not a slope, and
it is part of the probe's reported residual: 64 ns per region hot and 578
cold at the wide set under `rc-walk`, against a 380 to 1320 ns effect across
the sweep. The term moves with `STORES`.

**This probe's floor is far worse than the S24 brackets' 0.1 to 1.7 %, and
the run count went from three to five.** One run in five arrived grossly
contaminated — every figure of a block 1.8x to 2.7x high, the box being
shared with interactive work — and those blocks are voided by the Method's
gross-contamination test rather than averaged in. What survives still spans
0.22 to 0.98 ns on the wide-set hot slope of five runs. The two hot passes
that bracket the cold one, which are the Method's A-B-A control, agree to
20 % on that slope and to 4 % on the direction difference. Nothing here is
resolvable below ±0.3 ns per record, and a quieter machine would settle both
the `rc-trace` disagreement and the sweep's curvature.

**Three changes to the probe were forced by the measurement.** The arms are
interleaved round by round, so the block pool's LIFO state and any drift are
common mode across shapes. Every timed loop is bounded at run time through
`black_box`: with `STORES` visible to the optimizer, `sweep k=0` and
`heap → heap` ran the same thousand publishes into the same slot and
disagreed by 0.11 ns per store. And each working set is reported three times,
hot, cold and hot again, because the cache mode cannot be interleaved into
the arms — an arm's cache state is made by whatever ran before its round.

**What it corrects.** The premise S25 opened on — that the record is a
minority of S24.2's 0.45 ns and the rest is retain and codegen — is refused:
the record alone is 0.5 ns hot, which is that whole figure, and 1.2 cold.
The `rc-trace` subtractions of 0.66 and 0.71 ns from the S24.2 entry are
within the range the direction difference reads there, 0.67 hot and 0.89
cold, so those two were not the contaminated pair the stage assumed.

## 2026-08-15 — one header read for the store path: refused, and the branch discarded

**Merging the barrier's two flag reads into one moves nothing, so it does not
land.** S24.3 predicted this before the work and the gate it set — more than
4 % of the probe's per-store figure on the arena→arena and heap→arena
directions — is what refused it.

The change was the cheap half only: `store_category_barrier` reading the
flags once and answering both the category and the COW question from that
word, with no signature touched. Measured on S24.1's probe, before → merged →
before, nanoseconds per store at the 64-child working set:

| direction | A1 before | B merged | A2 before |
|---|---|---|---|
| arena → arena | 1.115 | 1.116 | 1.115 |
| heap → arena | 1.562 | 1.602 | 1.536 |
| arena → heap | 2.438 | 2.260 | 2.392 |

**The reason it cannot move those two rows is structural, and it is what the
step should have seen first.** The second read sits inside
`if new_cat == RequestArena && owner_cat != RequestArena` — the escape
branch — which neither of the gated directions takes. Only arena→heap
executes both reads, and that is the row this probe cannot resolve: its
controls disagree by 2 %, and across today's brackets by up to 10 %.

So the merge is left out, with the invasive half — twins carrying a flags
snapshot through `ll_retain` and `escape_gain` — never written. What would
justify reopening it is an instrument that resolves the escape direction to
better than a percent, and none exists on this box.

## 2026-08-15 — the barrier's header reads go narrow: a heap store into an arena costs a third of what it did

**`heap → arena` falls from 4.82 to 1.53 ns per store under `rc-walk`, and
the escape direction loses its working-set effect entirely.** What changed is
two accessors: `header_flags` and `header_refcount` load four bytes instead
of the whole word, matching the narrow stores `refcount_store` makes. That is
the rule already written on `refcount_load`, applied where the store barrier
reaches it. S24.2.

Both figures came from the probe of S24.1, before → after → before in one
sitting, medians of five rounds, nanoseconds per store:

| direction | set | A1 before | B after | A2 before |
|---|---|---|---|---|
| arena → arena | 1 | 1.115 | 1.109 | 1.107 |
| arena → arena | 64 | 1.106 | 1.117 | 1.113 |
| heap → arena | 1 | 4.818 | 1.562 | 4.820 |
| heap → arena | 64 | 4.816 | 1.566 | 4.815 |
| arena → heap | 1 | 4.732 | 2.209 | 4.660 |
| arena → heap | 64 | 3.115 | 2.478 | 2.475 |

The controls agree to 0.1 % on every row but the last, where they disagree by
26 % — the machine's load was 1.29 when A1 was taken. That row is the one
this bracket does not resolve; the rest are far outside the floor.

A second bracket, after the change, against the other configuration:

| direction | set | rc-walk A1 | rc-trace B | rc-walk A2 |
|---|---|---|---|---|
| arena → arena | 64 | 1.092 | 1.508 | 1.091 |
| heap → arena | 64 | 1.538 | 2.172 | 1.531 |
| arena → heap | 64 | 2.193 | 1.992 | 2.406 |

**`rc-walk` is now cheaper than `rc-trace` on both counted directions** — 28 %
on the plain publish and 29 % on the logged one — where the logged one was
2.2x dearer this morning. The escape direction is the one the controls leave
soft: the two A's differ by 10 %, so all that can be said is that the two
builds are within about that of each other, `rc-trace` ahead.

**One defect in two shapes is the best explanation of both gaps, and it is
not an established one.** The 3.7x on the escape arm was a wide load over the
*previous* store's narrow write, which is why spreading the stores over 64
children removed it. On `heap → arena` the same overlap sits **inside one
store** — `ll_retain` writes the counter half and `store_category_barrier`
read all eight bytes immediately after — so no working set can break it, and
the narrow read removes the whole 3.3 ns.

What the figures do not settle is why that intra-iteration stall was not
hidden. Sixty-four independent iterations of a 13-instruction loop should
absorb a latency of ten-odd cycles behind each other, and they absorbed none
of it: 4.818 at one child and 4.816 at 64. Two readings survive that. Either
a failed forward is a throughput cost rather than a latency one, replayed and
consuming issue slots, in which case this file's latency framing of the
2026-07-27 trap is the wrong picture in general; or something else in that
arm was the limiter and the wide load stood in series with it — the log's
cursor is a read-modify-write of one location on every iteration, and it is
the one loop-carried chain `heap → arena` has that the escape arm has not.

Two cheap arms discriminate, and both are owed: one that keeps the wide load
but moves `ll_retain`'s counter store after the barrier's read, and one with
an `Immortal` heap child, whose retain writes nothing at all while the log
append stays. Until one of them exists, the mechanism above is the leading
explanation and the numbers are the fact.

**`arena → arena` did not move**, as it should not: the retain returns early
on a non-`GcHeap` category, so that path writes no counter and never had an
overlap to lose.

What stays wide is `header_pair`, and `ll_cow_separate` moved onto it,
because one load beats the two it was making and nothing narrow precedes it
there. It buys no coherence the split reads lacked: the collector's one claim
on a published header is the epoch byte, which that predicate does not read,
so both shapes answer identically in every execution.

Verification: the full gate green in both GC configurations, three threaded
runs each, plus `hash-folding` and both `debug-journal` legs; Miri clean over
`refcount::`, `memory::barrier::` and the COW paths in both configurations.

## 2026-08-15 — the store path in the shape lowering emits: the escape's gap is the working set

The escape direction costs **4.68 ns per store over one child and 2.48 over
64** under `rc-walk`, which is the whole of what the harness's unexplained
3.7x was: a chain through one header line, not a cost a program pays. The
other two directions are flat across the same working sets, and `rc-trace`
is flat everywhere. S24.1.

The instrument is a probe inside the lib rather than a bench, because a
bench is a separate crate and reaches every micro-op through a call:
`memory::barrier::tests::what_a_store_costs_by_working_set::measure_store_cost`,
`#[ignore]`d, run explicitly. The library is unchanged from `8062713`; the
probe lands with this entry.

```
cargo test --release --lib --no-run                         # rc-walk
cargo test --release --lib --no-default-features --no-run   # rc-trace
cp <each test binary> <dir>/probe_walk , <dir>/probe_trace
<dir>/probe_walk  --ignored measure_store_cost --nocapture   # discarded, then A1
<dir>/probe_trace --ignored measure_store_cost --nocapture   # B
<dir>/probe_walk  --ignored measure_store_cost --nocapture   # A2
```

Median of five rounds per shape, the first round discarded inside the probe,
1000 publishes per round, nanoseconds per store:

| direction | set | A1 rc-walk | B rc-trace | A2 rc-walk |
|---|---|---|---|---|
| arena → arena | 1 | 1.110 | 1.544 | 1.109 |
| arena → arena | 64 | 1.110 | 1.540 | 1.112 |
| heap → arena | 1 | 4.811 | 2.217 | 4.815 |
| heap → arena | 64 | 4.816 | 2.247 | 4.815 |
| arena → heap | 1 | 4.647 | 1.995 | 4.704 |
| arena → heap | 64 | 2.487 | 2.244 | 2.470 |

The two controls agree to 1.2 % on the worst row and to 0.3 % on the rest,
which is tighter than the criterion harness reaches on this box.

**The escape's working-set effect is the reading.** One child against 64
costs 1.88x under `rc-walk` and nothing at all under `rc-trace` (1.995
against 2.244, the wide set marginally dearer). That is the signature the
narrow-store trap predicts: `escape_gain` increments the counter half with a
4-byte store, and the next store's `header_category` reads all eight bytes,
which cannot take the value out of the store buffer. Spread the stores over
64 children and consecutive stores touch different lines, so nothing waits.
A program filling a thousand slots with a thousand children is the second
shape, and there `rc-walk` is 10 % dearer than `rc-trace` on this direction
rather than 2.3x.

**`arena → arena` has no such effect in either build, and that is a check on
the explanation rather than a null result:** the retain returns early on a
non-`GcHeap` category, so that path writes no counter at all and has nothing
for a wide load to overlap.

**`heap → arena` is 2.2x dearer under `rc-walk` and flat across working
sets**, so this gap is not the stall. What it prices is the counted retain
of a heap child plus the release-at-reset record, both of which `rc-trace`
pays more cheaply. This is the figure S24.2 has to move, and the one that
says what the arena's logging costs in the shape compiled code has: 3.7 ns
per store over the cheapest publish, against the 3.85 the harness reported
through a call boundary.

**Corrected by S24.2, the entry above:** those 3.7 ns were the header's own
stall and not the log. With the accessors reading narrow, the same
subtraction is 1.538 − 1.092 = 0.45 ns, and the sentence above stands only as
a description of the code as it was on this date.

**The loops carry no call on the path they take.** The innermost timed loop
is 13, 12 and 13 instructions under `rc-walk` and 25, 25 and 27 under
`rc-trace`, none of them containing a `call`. What remains in the enclosing
region is cold and reached by a forward branch that returns into the loop:
`store_category_barrier`'s COW copy path and the log's `grow_log`.

**Not comparable with the harness table below.** A different binary out of a
different profile, so no delta is drawn between the two, and the figures are
not tabulated together. What the shapes differ by is structural: the harness
pays an indirect call per store and publishes into one child, and this probe
does neither.

## 2026-08-15 — the store barrier's three directions, and the arena's logging inside them

A release-at-reset record costs **3.9 ns per store** under `rc-walk` and
under 1 ns under `rc-trace`, which is the first measured figure the arena's
write-side logging has ever had (S22). The log's segment allocation stays
below this box's noise floor at both batch sizes, so it is bounded rather
than priced.

**Corrected the same day by S24.2:** the 3.9 ns was almost all the header's
own stall — a wide load over the counter half `ll_retain` had just written —
and not the record. Read narrow, the same difference is 0.45 ns, which is
what agrees with the 0.64 and 0.13 this entry measured under `rc-trace`,
a build that changes nothing about the log. **Do not quote the 3.9.** The
rest of this entry stands: it describes the code of that hour and the harness
it was taken on.

Commit `598706c`, harness `benches/barrier.rs`, both GC configurations,
i7-11700K (16 threads, 7.9 GB, WSL2), idle at load 0.24 when the session
started. Both binaries were built and copied out before either was
measured, so no compile sits between two arms:

```
cargo bench --bench barrier --no-run                        # rc-walk
cargo bench --bench barrier --no-default-features --no-run  # rc-trace
cp target/release/deps/barrier-<hash> <dir>/barrier_walk    # and barrier_trace
<dir>/barrier_walk  --bench --save-baseline discard   # first of the session, thrown away
<dir>/barrier_walk  --bench --save-baseline A1
<dir>/barrier_trace --bench --save-baseline B
<dir>/barrier_walk  --bench --save-baseline A2
<dir>/barrier_trace --bench --save-baseline B2        # repeat, outside the control bracket
```

Every run went through an RSS guard that kills at 1200 MB; the peak was
24–26 MB, which is the harness's per-region arena reset holding (S22.1,
and the machine it took down before that reset existed).

Criterion's median per timed region, in µs:

| arm | A1 rc-walk | B rc-trace | A2 rc-walk | B2 rc-trace |
|---|---|---|---|---|
| `store_arena_into_arena_x1000` | 6.1640 | 7.0834 | 6.1487 | 7.5451 |
| `ref_store_arena_into_arena_x1000` | 7.0349 | 8.0677 | 6.9874 | 8.1058 |
| `store_heap_into_arena_x1000` | 10.157 | 7.7191 | 9.8715 | 7.6793 |
| `store_heap_into_arena_x100` | 1.0151 | 0.78913 | 1.0033 | 0.79438 |
| `store_arena_into_heap_x1000` | 6.3440 | 7.0295 | 6.3602 | 7.7217 |
| `category_barrier_arena_into_heap_x1000` | 5.1920 | 1.4011 | 5.2218 | 1.4194 |

**The control holds:** the two A's differ by 0.25 % to 2.8 %, inside the
1.5–3 % floor of this box (Method), so the run is not void. B2 was taken
after the disassembly below, with the load at 0.86, and it repeats B to
within 1.3 % on the four arms that log or escape while the two
arena-source publish arms drifted 6.5 % and 9.8 %. Every cross-configuration
statement here is therefore made only where the two B runs agree.

Per store, rc-walk being the two A's averaged and rc-trace the B run, in ns:

| what the arm prices | rc-walk | rc-trace |
|---|---|---|
| `store_box`, arena into arena: retain's early return, category test, write | 6.16 | 7.08 |
| `ref_store`, same direction: the publish plus `drop_ref` of the displaced entity | 7.01 | 8.07 |
| `store_box`, heap into arena, 1000 per region: one release record per store | 10.01 | 7.72 |
| `store_box`, heap into arena, 100 per region | 10.09 | 7.89 |
| `store_box`, arena into heap: the first store logs the escapee, the rest count it | 6.35 | 7.03 |
| `store_category_barrier` alone on that direction: the bookkeeping, no slot written | 5.21 | 1.40 |

**What the release-at-reset record costs is the heap→arena arm minus the
arena→arena one: 3.85 ns per store under `rc-walk`, 39 % of that store.**
The difference prices the record together with the retain it owes, because
the arena→arena arm's retain returns early on a non-`GcHeap` category and
the heap→arena one does not (the harness's module doc says the same). Under
`rc-trace` the same difference is 0.64 ns in B and 0.13 ns in B2, so
nothing tighter than "under 1 ns" is claimed there. **The 3.85 was corrected
by S24.2 the same day: it was the header stall the retain left in front of
the category test, and the record's own cost is 0.45 ns — which is why the
`rc-trace` figures beside it were an order out and should have been read as
the warning they were.**

**The escape direction costs 0.19 ns per store over the cheapest publish**,
which is inside the floor and is what the design predicts: only the first
store of a region appends an escapee record, and the other 999 increment the
hold-count in the entity's own header. `ref_store` costs 0.85 ns more than
the bare publish under `rc-walk` and 0.99 under `rc-trace` — the `drop_ref`
of the entity the slot displaced.

**The log's segment is not resolvable by these two batch sizes.** One
4016-byte segment is carved out of the arena's own bump every
`LOG_SEG_RECORDS` = 500 records (`memory/arena.rs`), and the harness resets
the arena per region, so a 1000-store region draws two segments and a
100-store region one: 0.002 against 0.01 segments per store, a factor of
five in the amortised share. The per-store cost differs by +0.006 ns and
+0.16 ns across the two `rc-walk` runs — a sign change between them — and by
+0.17 ns and +0.26 ns across the two `rc-trace` runs. All four sit at or
under the floor, so what follows is a bound and not a cost: at 100 stores
the segment adds at most about 0.3 ns per store, under 3 % of the store.
Resolving it would need an arm that draws a segment per store, and no such
arm exists — the growth branch is `#[cold]` behind a count of 500 by
construction.

**The two configurations disagree on the escape bookkeeping by a factor of
3.7, and the emitted code does not explain it.** `escape_gain` is the same
sequence of 38 instructions in both binaries up to its refusal tail, the
only difference being the absolute address of identical relative targets
(`objdump`), and the calling loops differ in one place: under `rc-walk` the header is read as a relaxed
atomic load of the whole 8-byte word, twice, since an atomic load is not
common-subexpression-eliminated, while `rc-trace` reads the 4-byte flags
half once and reuses it for both tests. Two extra loads from a hot line do
not cost 3.8 ns.

**The mechanism is this crate's own recorded trap, not a new guess.** The
8-byte load overlaps the 4-byte `incl (%rsi)` that the previous iteration's
`escape_gain` stored into the counter half, and a load wider than an
overlapping store cannot be forwarded from the store buffer. That is the
trap of 2026-07-27 in this file ("the narrow mutator lands"): keeping the
word load over a fresh narrow store measured the retain/release pair at
10.2 ns against 2.78, three times worse, and the rule drawn from it — narrow
stores demand narrow loads — is written on `refcount::refcount_load`.

**The barrier breaks that rule today.** `escape_gain` writes both halves of
the header with plain 4-byte stores, while `header_flags` and
`header_refcount` load the whole word through `mutator_load_header` and
throw the half they did not want away; `header_pair` is the one caller that
needs both. `ll_retain` already reads narrow (`flags_load`, `refcount_load`),
which is why the retain/release path is fast and the escape bookkeeping
beside it is not.

What stays unmeasured is only whether that stall is the whole of this
particular gap: `perf` on this WSL2 kernel exposes no counters, so
`ld_blocks.store_forward` cannot be read, and the answer comes from making
the loads narrow and re-measuring rather than from a probe. Until then the
`rc-trace` figure in that row must not be quoted as "the escape is four
times cheaper without `rc-walk`".

**A second caveat on that row, and it applies to every arm here.** Each
timed region hammers one entity, so the counter store and the header load
of consecutive stores fall on the same line and form a loop-carried
dependency. Compiled PHP storing a thousand different children into a
thousand slots has no such chain, and the stall pipelines away behind
independent work. The row therefore bounds what the mixed widths can cost
in the worst shape, and a probe over a working set of many entities is what
would say what they cost in an ordinary one.

**What these numbers are not:** the cost of a store in compiled PHP. Lowering
emits these micro-ops specialized to the slot's kind and to a constant
`owner_cat` and inlines them, while the harness calls them across a boundary
the optimizer keeps — visible in the disassembly above as an indirect call
per store. The comparison between arms is what the reading is for.

## 2026-08-10 — the block kind becomes an atomic: the clock cannot resolve it, the disassembly can

The word at offset 0 of every block header took an atomic type and left
the private half of the two headers that have one (S11.9): the collector
reads it for every block of every region while the owner holds a `&mut`
over the surrounding struct, and that borrow was a data race Miri
reports. Owner-side reads are relaxed loads now, writes go through
`store_block_kind` as before. `size_class` moved out with it, being the
second word the collector reads.

**The wall clock answers nothing here, and the reason is the box rather
than the change.** Six runs, the two arms alternating, `cargo bench
--bench standard -- our_heap`:

| scenario | base (9a65197) | after |
|---|---|---|
| `larson_5k_slots_20k_rounds` | 774.9 / 793.6 / 803.5 / 1021.5 µs | 791.6 / 804.2 / 821.6 / 1007.6 µs |
| `rptest_10k_blocks_40_iters` | 2.170 / 2.197 / 2.256 / 2.276 ms | 1.962 / 2.171 / 2.211 / 2.239 ms |

Identical code varied by 32 % across those four larson runs, and the two
benchmarks point in opposite directions: larson's median is 1.8 % higher
after the change, rptest's 1.6 % lower. The effect, whatever it is, is
under the noise floor of a shared development box, and the interactive
work beside it is the floor (`Method`, above).

**So the claim is made where it can be checked — in the code the
compiler emitted.** `ll_heap_alloc` and `ll_heap_free`, the two exported
entries `Heap::alloc` and `Heap::free` inline into, are 104 instructions
in both arms and differ by **one**:

    movaps %xmm0,(%rsi)   ->   movups %xmm0,(%rsi)

A relaxed load or a release store of an aligned `u32` is a plain `mov`
on x86-64, which is why nothing else moved; the one difference is not an
atomic at all but an alignment proof the compiler lost when the private
half started at offset 8 instead of 0, and an unaligned 16-byte move
costs nothing on an aligned address on any x86-64 since Nehalem.

**Verdict: accepted, at parity.** The instruction counts are equal, the
timings are not resolvable, and what was bought is the removal of a data
race on every allocation under `rc-walk`.

**And the general claim rests on structure rather than on that
disassembly**, which is the Sage's ruling of the same day, asked because
one build of one compiler over one benchmark shape is thin evidence for a
path meant to be inlined into compiler-generated PHP code. What an atomic
load forecloses — duplication, merging, hoisting out of a loop, elision —
each needs a **second** read of the same location, at a provably equal
address, with no intervening write the compiler cannot see through. No
such site exists here or can arise: `ll_free` and `Heap::free` each load
once, from an address derived fresh from the incoming pointer, and feed
the result straight to a branch. Successive frees carry different
pointers, and the free-list push stores through raw pointers into the
same block, so alias analysis would refuse to merge a *plain* load across
it just as readily. Atomicity closes nothing that was open, and
`Heap::free`'s `size_class` read lands on a header line its own push has
already dirtied.

Ruled out with it, and not to be reproposed: reading the word
non-atomically on the ground that the owner is its only writer (no
codegen to gain, and it puts back the mixed-access subtlety the change
removed, outside what the Miri test defends); caching either word
anywhere (a lookup keyed by block costs more than one L1 load from a
dirty line); giving `rc-trace` a plain `u32` of its own (its store is
already relaxed, the codegen is identical, and it would split the header
type across builds and take the instrument off one of them); and
realigning the private half to win back the `movaps`.

## 2026-08-08 — the journal's record sites: counted in the IR, and the clock's answer is owed

The sites of `dev/design/debug-modes.md` §9.5 landed behind the
`debug-journal` feature (S5.2). §9.6 makes two claims about their cost and
only the first is settled here.

**Claim one — off means absent — is settled, and counted rather than
timed.** A site is a load of the enabled mask and a branch, and the clock
on this box cannot resolve fourteen of those against a 750 µs benchmark.
The count is over the emitted release IR, one module per arm
(`cargo rustc --release --lib -- --emit=llvm-ir`, `codegen-units = 1`):

| | mask loads | IR lines |
|---|---|---|
| default build | 0 | 55 043 |
| `--features debug-journal` | 34 | 55 758 |

The one load the default build does contain is inside `enabled_kinds`,
the module's own public accessor, and no runtime path reaches it — the
number above excludes it. Fourteen sites are written by hand; 34 is what
inlining makes of them, a site copied into each caller it is inlined
through.

**Claim two — the cost with the feature on and the kind disabled — is
owed, not claimed.** It is one relaxed load of a line nobody writes and
one predictable branch, on the allocation and the death paths, and this
crate does not accept "probably negligible". The measurement to make is
the ordinary two-arm one, `lifecycle` and `standard`, default build
against `--features debug-journal` with the default mask, back to back.
Until it exists the feature belongs in a development build only, which is
what §9.6 already says and what `Cargo.toml`'s note repeats.

## 2026-08-05 — growing a long-lived buffer off the bump top: no resolvable difference

`buffer_ensure_longlived` now extends a payload in place when it is still
the last chunk the buffer arena bumped, instead of allocating a new one,
copying and freeing the old. The request arena has had this since it was
written; the long-lived side did not (`rfc/model/memory/buffers.md`).

**It works, and the clock cannot see it.**

| arm | `append_256x16_gcheap` |
|---|---|
| A — reallocate and copy | 1.4285 µs |
| B — extend in place | 1.4355 µs |
| B, repeated | 1.3693 µs |
| A, control | 1.4046 µs |

Criterion defaults, release, rc-walk, one sitting, first run discarded
(1.503 µs). `append_256x16_arena` beside it moved 1.2707–1.3024 µs across
the same four runs and is not affected by the change either way.

**The run is void by this file's own rule and would be void whatever it
showed:** two runs of the *same* arm B disagree by 4.6%, which is wider
than the 1.5–3% recorded elsewhere here and wider than any effect being
looked for. Nothing separates the arms.

**What replaces the measurement is a count.** `string::tests::`
`an_append_loop_moves_its_payload_once` counts payload moves over 256
appends of 16 bytes: **one with the in-place path, nine without it.**
Eight of those nine are a copy of everything written so far — 16, 32, 64
… up to 2 KiB — and they are gone. That the wall clock does not show it
means the copies were never what the loop spent its time on: 256 calls
each doing a 16-byte `memcpy` plus the append's own bookkeeping dominate.

**Accepted anyway**, on three grounds that need no measurement. It
removes work rather than adding any, so the arm that cannot be resolved
is the one doing less. It removes a payload free from the growth path,
and a payload freed during a collector epoch has to park
(`memory::deferred_free`) — a payload that never moves has nothing to
park. And it removes the fragmentation that hole reuse would produce on
this shape: holes never coalesce here, so a moving append loop leaves a
chain of them at 64, 128, 256 bytes, each too small for the step that
follows.

**Where it would show, unmeasured:** a bigger payload, where the copies
grow linearly while the per-append work does not. Nothing in this crate
builds one yet.

## 2026-08-04 — first string benchmark: a baseline, not a comparison

`benches/strings.rs`, new. The crate had no string or hash benchmark at
all, which is why `PLAN.md` records the bump-top growth optimization as
blocked on measurement rather than on design, and why the rapidhash port
landed with its cost unstated.

**Nothing is compared here.** These are first numbers for a harness that
did not exist; there is no arm B and no A→B→A. They are recorded so that
the next change to hashing or to append has something to be measured
against, taken in one sitting on an otherwise idle box, criterion
defaults, release, rc-walk (the default configuration).

| benchmark | median |
|---|---|
| `hash/8` | 3.59 ns |
| `hash/16` | 3.57 ns |
| `hash/17` | 4.39 ns |
| `hash/64` | 5.44 ns |
| `hash/113` | 10.16 ns |
| `hash/1024` | 52.6 ns (18.1 GiB/s) |
| `new_inline_gcheap_hash_die` | 17.1 ns |
| `append_256x16_arena` | 1.295 µs |
| `append_256x16_gcheap` | 1.419 µs |

An earlier pass, before the harness checked creation and append for
refusal, gave 3.71 / 3.64 / 4.34 / 5.42 / 10.39 / 53.6 ns, 16.7 ns, and
1.311 / 1.418 µs. Every one of those agrees with the table above inside
this box's noise floor — but the two passes were **not** run back to back,
so that agreement is not a measurement of what the checks cost. It is a
reason not to have bothered measuring it.

What the shape says, and it is only what the numbers say: the hash is
flat from 8 to 16 bytes, which is the reference's single short arm doing
the same work for both; 17 bytes costs a step more, which is the 16-byte
cascade opening; and 113 is where the bulk loop starts and the per-byte
rate settles.

**The two append arms are not a measurement of the bump-top gap**, and
the 8% between them must not be quoted as one. They differ in the
allocator serving the payload *and* in how it is reclaimed — an arena
reset against a refcount death. The pair exists so the optimization can
be measured as `append_256x16_gcheap` against itself, with the arena arm
beside it as the control that should not move.

**Three defects in the harness itself, found by running it and by being
asked about it, all fixed before these numbers were taken.** The
create-and-hash arm first built arena strings without ever resetting, so
it grew the arena for the whole run — gigabytes at criterion's iteration
counts, and a 60% spread that looked like machine noise. The append arms
first leaked their payloads for the same reason. Both now reclaim inside
the timed region. A benchmark that allocates without bound measures the
allocator's behaviour under a condition no program produces.

The third: nothing checked for refusal. `ll_string_new` and
`ll_string_new_dynamic` return null when memory runs out or the 4 GiB cap
is reached and `ll_string_append` returns false, and three sites
dereferenced or ignored them — so the harness would have crashed on the
one condition it is most likely to reach, instead of reporting it.

**A fourth number was taken and thrown away**, which is worth recording
because the rule that catches it is easy to forget: a re-measurement run
while a Miri job occupied the box gave 1.42 and 1.78 µs for the two
append arms. Nothing about the code had changed by 25%. Load arriving
mid-session is what the A→B→A control exists for
(`POSTMORTEM.md`, 2026-07-20), and here there was no control — only the
knowledge that the box was busy.

## 2026-07-28 — dense census in the epoch walk: 2–3× on the walk step

`walk_edges`' child test was one `HashMap<address, row>` lookup, and
building that map was a full pass over the walked set. Replaced with
the dense census: a per-slot `u32` row array laid out from the block
snapshot (prefix sums per block, virgin tails included), filled by
pass 1 as rows are recorded; pass 2 finds a child's block by the
64 KiB alignment mask + binary search over the snapshot's sorted
payloads, its slot by one narrow division, and rejects interior
addresses by the remainder — the exact-key match the map gave for
free, now pinned by a verified-failing regression
(`an_edge_into_a_slot_interior_is_dropped`).

`measure_epoch_cost` probe, release, rc-walk default; A → B → A, round
0 (allocate-black stamping, no walk) discarded, medians of rounds 1–3;
control A re-run agreed with the first A throughout:

| scenario | walk A (HashMap) | walk B (census) | speedup |
|---|---|---|---|
| 100k singletons | 3.42 ms | 1.22 ms | 2.8× |
| 100k chain | 5.19 ms | 2.52 ms | 2.1× |
| 10k singletons | 0.35 ms | 0.17 ms | ~2× |
| 10k chain | 0.48 ms | 0.33 ms | ~1.5× |

Per entity the walk drops from ~34 ns to ~12 ns (rows only) and from
~52 ns to ~25 ns (one edge each). The cost moved to `snapshot`: the
slot array's allocation + `u32::MAX` fill, ~2–5 µs → ~15–90 µs at
100k — two orders below the walk win, and collector-side. `judge` and
the mutator are untouched.

**Verdict: accepted.** Possible further squeeze on record, unmeasured:
replace the per-edge division with a precomputed reciprocal multiply,
and the binary search with a region-indexed table — both matter only
if the walk shows up again in profiles.

## 2026-07-28 — the batched-checkpoint split: within noise

The split (`rfc/model/gc/rc-walk.md` "Batched releases", amendment
2026-07-28) replaces the one pre-run `ll_gc_checkpoint` with
`ll_gc_checkpoint_ack` before the run and a full `ll_gc_checkpoint`
after it, in both the batched bench shape and `ll_release_vector`.
Correctness-driven (pre-run pickup = the phase-lock shape); measured
only to confirm the extra trailing test costs nothing.

`cargo bench --bench lifecycle -- batch_64` / `-- vector`, rc-walk
default build, 64-object iterations. First pass discarded as warm-up
per protocol; arms then old → new (medians):

| arm | old (single pre-run) | new (split) |
|---|---|---|
| batch_64_batched_release | 1.54 µs | 1.53 µs |
| batch_64_plain_release (control) | 1.49 µs | 1.51 µs |
| factory + vector release | 1.61 µs | 1.45 µs |
| reserved + vector release | 1.44 µs | 1.36 µs |

**Verdict: accepted — no measurable cost.** Batched and control within
the 1.5–3% floor. Both vector arms measured *faster* with the split
(5–10%); direction only, single pass, not claimed as a win — the
warm-up pass had already produced similar vector numbers for the new
form, so the honest statement is "not slower".


## 2026-07-27 — bulk operations vs per-object: reservation wins ~12–15%, vector release within noise

The 2x2 on 64-object batches (`cargo bench --bench lifecycle -- bulk/`,
rc-walk default build, destructorless 16-byte class, two runs; the
box drifted ~5–9% between runs after a day of benching, but the
*ordering* held in both):

| arm | run 1 | run 2 |
|---|---|---|
| factory create + loop release | 1.30 µs | 1.19 µs |
| **reserved create** + loop release | 1.10 µs | 1.04 µs |
| factory create + **vector release** | 1.26 µs | 1.23 µs |
| reserved + vector | 1.15 µs | 1.15 µs |

- **Cell reservation beats the factory by ~12–15%** on the whole
  64-lifecycle (~2–3 ns per object): one `ll_entity_reserve` call and
  a stamp per object against an allocator entry per object.
- **Vector release measures within noise of the loop** — expected
  after the narrow mutator: the per-object release path has nothing
  left to amortise except the ~1 ns checkpoint test, which was already
  sub-noise in the batch_64 comparison. Its value is code size and the
  manager's future freedom (prefetch, block-sorted frees), not
  present-day speed.
- Caveat: after the virgin tail of the class's block is exhausted
  (~32 iterations here), reservations serve from the free list —
  non-contiguous — so this measures the *call-shape* win, not the
  locality win; the locality claim stays unmeasured until a consumer
  with real pointer-chasing exists.

## 2026-07-27 — the narrow mutator lands: retain/release reach parity with rc-trace (and past it)

Implementation of the rfc's narrow-mutator amendment (same day, after
the adversarial review returned SOUND WITH CONDITIONS — see
`DECISIONS.md`). Clean A → B → A after the Miri run finished; the
retain/release controls came back identical (2.777 / 2.777 ns).

| bench | rc-walk before | rc-walk narrow | rc-trace |
|---|---|---|---|
| retain_release_nonfinal (pair) | 3.33 ns | **2.78 ns** | 2.89 ns |
| create_release_die | 19.1–19.7 ns | 19.3–20.7 ns¹ | 17.0 ns |
| batch_64 plain vs batched | −1.1 ns/death | within noise² | — |

- **The counted hot path now pays nothing** — rc-walk's retain/release
  pair measures *below* rc-trace (no candidate machinery at all,
  narrow 4-byte loads and store). −17% against the word-RMW version.
- ¹ The die-cycle keeps its ~+15%: the death-branch checkpoint and the
  parking branch are the residue, as designed; its A/A2 controls
  disagreed by 7%, so quote the range, not a point.
- ² With the death branch this cheap, the batched form's advantage is
  no longer resolvable on this box; the ABI stays for lowering.
- **Trap for the record**: the first narrow-store attempt kept the
  8-byte word load and measured the pair at **10.2 ns — 3x worse**: a
  wide load over a fresh narrow store defeats store-to-load
  forwarding. Narrow stores demand narrow loads (comment on
  `refcount::refcount_load`).
- A contaminated session preceded the clean one: benches run while
  Miri ground in the background produced 16% control divergence —
  discarded whole.

## 2026-07-27 — rc-walk mutator tax measured on the factory lifecycle; epoch cost per entity

The two gaps the architecture review named: the object-lifecycle path
had never been timed (all benches ran `ll_malloc`), and the
collector's epoch cost had never been measured at all. Taken the same
day the checkpoint moved to `ll_release`'s death branch and rc-walk
became the default build.

**Mutator side** — new `cargo bench --bench lifecycle` (create →
constructed → release-to-zero → die through the public ABI, 16-byte
class, no destructor). A (rc-walk) → B (rc-trace,
`--no-default-features`) → A-control, plus two more rc-walk runs to
settle one load-spiked arm:

| bench | rc-walk | rc-trace | delta |
|---|---|---|---|
| create_release_die | 19.1–19.7 ns | 16.8 ns | **+14–17%** |
| batch_64, plain `ll_release` | ~1.25 µs | ~1.07 µs | +17% |
| batch_64, `ll_gc_checkpoint` + `ll_release_batch` | ~1.15–1.20 µs | ~1.06 µs (same fn) | — |

- The full rc-walk tax on a create+die lifecycle is **~2.5–2.9 ns**
  (+14–17%): checkpoint test + relaxed header atomics + the parking
  branch together. It rides the death branch only — the factory
  allocation itself carries no test since the 2026-07-27 move.
- The batched form saves **~1.1 ns per death** (~5% on a 64-death
  run) — consistent with the checkpoint test costing ~1 ns each.
  In the rc-trace build the two arms measure identical, as expected
  (same function).
- Caveat: feature switching forces a rebuild between arms, so the
  build-both-binaries-first rule could not be followed literally; the
  bias direction (a post-build run measures slow) works *against* the
  winning rc-trace arm, so the delta is a floor, not an artefact.
  One rc-walk `batched` arm read 1.385 µs and its A-control voided
  it; three further runs clustered at 1.15–1.20 µs.

**Where the mutator tax comes from** (same session, follow-up): a
`retain_release_nonfinal` bench (retain + release pair on one live
object, count never zero) measures rc-walk ~3.3–3.55 ns vs rc-trace
2.87 ns — **+0.5–0.6 ns per pair**. Disassembly of both `ll_release`
builds pins it: rc-trace's fast path is a 4-byte flags load plus a
narrow `decl [mem]`; rc-walk must load the whole 8-byte header word,
decrement in a register, clear the condemned byte (`movabsq` mask +
and/or) and store the full word back — the protocol's
"every retain/release clears the byte in the word it already loaded"
turned a one-instruction decrement into a load-modify-store chain.
Notable: on steady churn rc-walk does **not** win — rc-trace's
candidate machinery after the first buffering is a single masked test
(`testl` + skip). Decomposition of the 2.7 ns lifecycle tax:
~1.1 ns checkpoint test + ~0.5 ns word-protocol on the two header ops
+ ~1 ns spread over factory/free (kind release-store, parking branch).
Optimisation lead, unmeasured: narrow relaxed byte ops (4-byte
refcount decrement + 1-byte condemned clear) instead of the word RMW —
needs the rfc's two-header-bytes reasoning re-checked first.

**Collector side** — new probe
`cargo test --release --lib -- --ignored measure_epoch_cost
--nocapture` (stepped epoch on a live set that stays live; round 0 is
the allocate-black stamping pass, rounds 1–3 are the mature-heap
steady state):

| live set | shape | epoch total (steady) | per entity |
|---|---|---|---|
| 10 000 | singletons | ~430–530 µs | ~45 ns |
| 10 000 | chain (1 edge/entity) | ~750–830 µs | ~78 ns |
| 100 000 | singletons | ~4.9–5.1 ms | ~50 ns |
| 100 000 | chain | ~7.4–8.1 ms | ~78 ns |

- Scaling 10k → 100k is **linear** — no superlinear term in walk,
  judge or the census `HashMap` at these sizes.
- The split: walk ≈ 70%, judge ≈ 25%, handshake + snapshot + close ≈
  µs-scale noise. Any future collector-side optimisation (dense
  slot-indexed census instead of the `HashMap`, cursor hints at ack)
  should attack walk first and be re-measured against these numbers.
- Verdict: **recorded as the baseline**; nothing rejected. The
  economics question that stays open is the epoch *duty cycle* —
  trigger thresholds still need real workloads (Phase D).

## 2026-07-26 — the rc-walk build vs default; an atomic bump cursor rejected

First measurement of the `rc-walk` configuration against the default,
after step 3 commits 1–6 (relaxed-atomic retain/release, the deferral
branch in `ll_free`, kind stores, the swept field/flag stores).
`cargo bench --bench standard -- our_heap`, warm-up run discarded,
A → B → A-control:

| arm | larson | rptest |
|---|---|---|
| A default | 794 µs | 1.99 ms |
| B rc-walk, bump as relaxed atomic | **904 µs (+14%)** | 2.61 ms (+31%) |
| B rc-walk, bump plain (isolation) | 807 µs | 1.94 ms |
| B rc-walk, final (no cursor snapshot) | 784 µs | 1.94 ms |
| A control | 800 µs | 2.14 ms |

The rptest column is **void** by the control rule (the two A's
disagree by 7%); larson's A's agree and its delta is real. The entire
regression was the one `bump += 1` turned into a relaxed-atomic store
for the collector's cursor snapshot — isolated by reverting exactly
that line. Verdict: **rejected**. The cursor is not read at all now:
commissioning zeroes every entity-block slot header, so the walker
scans whole blocks and virgin slots skip on the occupancy test — the
extra scan is collector-side, which is the side rc-walk always pays
on. With that, the rc-walk build measures **within noise of default**
on the raw-heap path (784 µs vs 794–800 µs).

## 2026-07-26 — free-list link moved to slot bytes 8–15 (rc-walk step 1): **within noise**

**Commit:** this one. Windows box, release, `cargo bench --bench
standard -- our_heap`, stash-swap A→B→A in one sitting, first run of the
session discarded as warm-up.

**What changed on the measured path:** `FreeSlot::next` moved from slot
bytes 0–7 to 8–15 (same cache line; one addressing offset in
alloc-pop/free-push), `Heap` gained a cold `block_kind` field, `refill`
gained one branch (entity-only zero pass, not taken by the raw heap).
The entity population itself is off this benchmark's path entirely.

| scenario | A1 (base) | B (new) | A2 (control) |
|---|---|---|---|
| larson (median) | 665.1 µs | 667.7 µs | 665.0 µs |
| rptest (median) | 1.6485 ms | 1.6784 ms | 1.6500 ms |

A1 ≈ A2 (0.01% / 0.1%): the run is valid. B vs A: larson +0.4%,
rptest +1.8% — both inside this box's 1.5–3% noise floor (see Method),
so **no resolvable difference**. Matches the prediction that bytes 8–15
ride the same line the slot write already owns
(`rfc/model/memory/heap-slot-allocation.md` argument unchanged).

**Verdict: accepted.** Re-check rptest if a future session shows a
drift in the same direction.

---

## 2026-07-21 — `buffer_candidate` taken out of `ll_release`

**Commit:** this one. **Evidence:** release IR, not a timing run — see
the note above about what this box can and cannot resolve.

**What changed:** the cycle collector's candidate buffering was fully
inlined into `ll_release`: thread-local access, `RefCell` borrow, `Vec`
push with its growth and panic paths, two `alloca`s. `ll_release` is the
most frequent operation in the runtime and the work is needed at most
once per object per collection. It now tests the buffered bit itself —
from the flags word it already holds — and calls an `#[inline(never)]`
`buffer_candidate` only when there is something to record.

| function | before | after |
|---|---|---|
| `ll_release` | 169 IR lines, 21 calls | 38 IR lines, 1 tail call |
| `alloca` in `ll_release` | 2 | 0 |

**Verdict: accepted on IR.** No timing claim is made: the effect is a
smaller hot function and a cold tail, the same shape as `Heap::alloc`'s
split, and this box cannot resolve differences of that size.

**Correction to the audit that prompted it.** The finding read "the
early-out `buffer_candidate` sits behind a call boundary, so every
non-zero decrement pays a call plus a reload of flags" (translated from
the Russian it was written in). That was not true: LLVM had inlined the
callee, so there was no call — the cost was the opposite, the whole
buffering machinery sitting in the hot function. Hoisting the test and
forcing the callee out of line is what the finding should have said.

---

## 2026-07-21 — `free`'s cold tails (H11): **no measurable difference**

**Committed** (`1824392`), on structural evidence rather than a timing
win — and this entry keeps that distinction rather than dressing one up
as the other.

**Measured 2026-07-21, bracketed A→B→A→B→A with pre-built binaries and
the first run discarded. The answer is "no measurable difference":**

| | A (no split) | B (split) |
|---|---|---|
| larson | 706.71, 696.36 µs | 714.51, 693.45 µs |
| rptest | 1.7584 ms, (one run disturbed) | 1.7846, 1.7528 ms |

Runs of the *same* binary differ by 1.5–3%, which is the size of the
effect being looked for. The instrument is coarser than the thing
measured. That is not "no effect" — it is "not resolvable here", and the
two must not be confused.

The change stands on the IR evidence below, and on the absence of a
regression.

**What was established, from release LLVM IR (`cargo rustc --release
--lib -- --emit=llvm-ir`), which no machine load can distort:**

| | before | after |
|---|---|---|
| `Heap::free` as a standalone function | 201 IR lines, real call from `ll_free` | **0** — fully inlined |
| `Heap::alloc` as a standalone function | 0 — fully inlined | unchanged |

So the audit's claim is confirmed exactly: `free` was not inlining while
`alloc` was, and marking its tails `#[cold] #[inline(never)]` fixes
that. Note `free`'s doc *already claimed* the split — the doc was ahead
of the code.

Two variants were built, both achieving the inline:

- **both tails cold** (`retire_empty` + `relink_unfull`) → `ll_free`
  body 92 IR lines
- **`retire_empty` only** → `ll_free` body 129 IR lines, with
  `relink_unfull` inlined

**Only one tail is `#[cold]`, deliberately.** `retire_empty` is
genuinely rare — a block reaching zero live slots. `relink_unfull` is
`#[inline(never)]` **without** `#[cold]`, because `#[cold]` asserts to
LLVM that a branch is rare, and a workload churning blocks across the
full ↔ has-room boundary takes it constantly. Both variants achieve the
inline; only one of them makes a claim that could turn out false, so the
one that stays is the one that claims less.

**Four earlier attempts on 2026-07-20 were void** — load arriving
mid-session, a recompile between arms, and finally a box 40–80% off with
the control runs 10% apart. One of them suggested marking
`relink_unfull` cold is a large regression on rptest. That reading came
from a contaminated run and was never reproduced; it is recorded as the
reason for caution, not as a finding.

---

## 2026-07-20 — block pool free list behind a mutex (H4)

**Commit:** `0ee8f77`. **Machine:** dev box, Windows, release profile,
IDE running. **Base:** `549c469`.

**What changed:** the lock-free Treiber stack of free blocks replaced
by a `Mutex`-guarded chain, removing a data race in `pop_global` and
the ABA tag with it.

| scenario | lock-free | mutex | change |
|---|---|---|---|
| larson_5k_slots | 749.04 µs | 752.59 µs | noise |
| rptest_10k_blocks | 1.8994 ms | 1.8996 ms | none (p=0.12) |

criterion reported −2.1% on larson at exactly p=0.05, with the interval
nearly touching zero while the point estimates moved the *other* way.
Read as noise, not a win — a change estimate that disagrees with the
raw midpoints is not a result.

**Verdict: accepted.** The lock costs nothing measurable, which is what
a cold, batched path should show. Taking it was never expected to be
faster; it was expected to be free, and it is.

---

## 2026-07-20 — block header split into private / shared halves

**Commit:** `ee89de0`. **Machine:** dev box, Windows, release profile,
IDE running. **Base:** `6b8b28c`.

**What changed:** `HeapBlockHeader` split into four `repr(C)` structs so
the owner's `&mut` no longer covers the atomics other threads read.
Layout: line 0 = hot private fields + `owner`; line 1 = `remote_free`
alone; line 2 = cold `owned_*` links.

**Scenario:** `larson_5k_slots_20k_rounds/our_heap` (multi-threaded,
worker churn) and `rptest_10k_blocks_40_iters/our_heap` (block churn
across the full ↔ has-room boundary).

| scenario | before | after | change |
|---|---|---|---|
| larson_5k_slots | 768.71 µs | 750.72 µs | **−3.03%** (p=0.00) |
| rptest_10k_blocks | 1.9268 ms | 1.9254 ms | **−1.54%** (p=0.02) |

**Verdict: accepted.** Faster on both, while removing a real data race
and the `remote_free` false sharing.

### Rejected variants (same change, different layout)

Both were measured against a stale baseline, so only their direction is
trusted, not their magnitude — which is exactly why they are recorded
as direction only.

- **`owner` moved out of line 0**, onto the isolated line with
  `remote_free`. Slower: every local `free` reads `owner` to test
  ownership, so the check started costing a second line. Rejected.
- **`next`/`prev` moved out of line 0**, onto their own line. Slower:
  `link`/`unlink`/`relink_unfull` run on every full ↔ has-room
  transition, which `rptest` does constantly. Rejected.

The rule both point at: the header owns the block's whole reserved
256-byte line (`LINE_SIZE`), so there is no reason to evict anything
hot from line 0. Only the contended field needs isolating.

Architectural conclusion: see `DECISIONS.md`, 2026-07-20.
