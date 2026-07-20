# Benchmark log

Every measurement lands here, not in chat and not only in a commit
message. Negative results are recorded too — "tried it, it did not
pay" saves the next attempt and is usually worth more than a win.

`RESULTS.md` in the repository root holds the crate's headline
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

**Check absolutes against the known band before reading any delta.**
The same code has been measured many times, so the normal range is
known:

| scenario | quiet-machine band |
|---|---|
| `larson_5k_slots_20k_rounds/our_heap` | 749–769 µs |
| `rptest_10k_blocks_40_iters/our_heap` | 1.89–1.93 ms |

A point outside its band describes the machine, not the code, and no
delta computed from it means anything. Update the bands when the code
legitimately moves them.

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

---

## 2026-07-20 — `free`'s cold tails (H11): **measurement inconclusive**

**Not committed.** The code change is two attributes; the evidence that
it is *correct* is solid and machine-independent, but the evidence that
it is *faster* could not be obtained, so it stays out of the tree.

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

**Why no verdict.** Four measurement attempts, all void: two were
contaminated by load arriving mid-session, one by the recompile between
arms, and the last found the box 40–80% outside its bands with the two
control A's 10% apart. An intermediate reading suggested marking
`relink_unfull` cold is a large regression on `rptest` — plausible,
since that benchmark churns blocks across the full ↔ has-room boundary
constantly, so the tail may not be cold *in that workload*. **That
hypothesis is unverified and must not be quoted as a finding.**

**Next time, on a quiet machine:** measure three arms — none cold,
`retire_empty` only, both cold — with the control repetition above. The
question is not whether `free` should inline (IR says yes) but whether
`relink_unfull` is cold enough to deserve the attribute. `#[cold]` is an
assertion to LLVM, and if it is false the branch is deoptimized on a
path that is actually hot.

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
