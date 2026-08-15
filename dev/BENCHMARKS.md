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


## 2026-08-15 — the store barrier's three directions, and the arena's logging inside them

A release-at-reset record costs **3.9 ns per store** under `rc-walk` and
under 1 ns under `rc-trace`, which is the first measured figure the arena's
write-side logging has ever had (S22). The log's segment allocation stays
below this box's noise floor at both batch sizes, so it is bounded rather
than priced.

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
nothing tighter than "under 1 ns" is claimed there.

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
not cost 3.8 ns. **Hypothesis, unmeasured:** the 8-byte load overlaps the
4-byte `incl (%rsi)` that the previous iteration's `escape_gain` stored into
the counter half, a load wider than an overlapping store cannot be forwarded
from the store buffer, and the resulting stall — 10 to 15 cycles, 2.8 to
4.2 ns at this clock — is the size of the gap. Nothing here forces the
question: `perf` on this WSL2 kernel has no counters, so
`ld_blocks.store_forward` cannot be read. Until it is, the `rc-trace` figure
in that row must not be quoted as "the escape is four times cheaper without
`rc-walk`", and the same suspicion covers every `rc-walk` header read that
follows a narrow counter store on the same line.

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
