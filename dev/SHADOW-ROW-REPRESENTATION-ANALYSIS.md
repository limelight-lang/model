# Shadow rows: representation and measurement analysis

Date: 2026-09-12. Scope: S40.3's experiment and S40.2's flat/chunked
representation decision in [PLAN.md](../PLAN.md).
Reviewed model source: the tree after S40.1's pruning arm closed on 2026-09-12
(the day's work lands squashed over `ea51cbe`, so the tree is inside that commit).
`PLAN.md` had pre-existing working-tree edits; this analysis does not alter them.

Status: source analysis and proposed experiment. This document changes no
collector behavior, closes neither step, and reports no new hardware timing.
Amended 2026-09-12 by S40.5: §3.1 specifies the chunked form, and the replay
of the census through both forms is `cycle::census::replay`, its figures in
[BENCHMARKS.md](BENCHMARKS.md), 2026-09-12 (S40.5).
Arithmetic below is derived from the current implementation or from an
explicitly stated hypothetical chunk layout. Existing benchmark observations
remain in [BENCHMARKS.md](BENCHMARKS.md); their scope must be preserved.

## Recommendation

Decided 2026-09-12 (S40.2): the flat representation stays, and the chunked
form of §3.1 is not adopted without a built candidate, whose stage is
Edmond's to open; the figures for and against it and what would reopen the
question are `DECISIONS.md`, "the flat row array stays". The recommendation
as written before that decision follows.

Keep the current flat representation pending a comparison with a concretely
specified chunk representation. Build a phase-aware structural census, then
compare the two implementations in an optimized driver without unit-test
instrumentation. Decide from workspace demand, completion under a fixed memory
budget, and execution cost on the same workloads.

A table of reserved row bytes against distinct written cache lines explains
the flat representation. It cannot alone select its replacement. In particular:

- Slot density does not determine group occupancy or the winning representation.
- The recorded two-byte chunk directory needs an implementable addressing scheme.
- The old sparse-load comparison covers mark/scan, not today's full live commit.
- A release unit-test binary retains substantial per-dispatch test work.

## 1. The built structure and its consumers

[shadow.rs](../src/cycle/shadow.rs) reserves one allocation per touched block:

```text
24-byte RowArray header | padded u32 rows | group initialization bitmap
```

Each row contains two color bits and thirty working-count bits. The row region
is reserved whole but arrives dirty. The bitmap is cleared at reservation;
each group of eight rows is cleared only at that group's first meeting.
`ensure_row` initializes a newly met row from its refcount. `mark` subtracts
internal references; `scan` colors rows from residual counts and propagates
liveness. Saturated counts remain conservatively live.

The ordinary full path in [collect.rs](../src/cycle/collect.rs) is:

```text
open workspace and detach roots
  -> mark -> scan -> construct membership
  -> maturation of live rows
  -> if membership is nonempty: initial exact validation
  -> if confirmed: destructors, revalidation, possible reclamation
  -> disposition and window close -> return overflow workspace
```

Maturation reuses live rows' working counts as traversal/component indexes.
Membership enumeration and membership probes also consume the representation;
the decision is not confined to mark's decrement instruction.
`shadow::for_each_of_color` tests all `G` group positions and reads rows in
initialized groups. A chunk implementation that scans its directory still
pays an `O(G)` enumeration term; fewer allocated rows do not by themselves
make membership walks `O(T)`. Price any separate initialized-group list needed
to achieve the latter, including its maintenance.

The pressure path is a separate arm: it harvests listed members and releases
trace workspace before teardown. Its lifetime and capacity constraints must
not be inferred from an ordinary collection with an artificially small budget.

Row addresses must remain stable until the window ends:
[WorklistEntry](../src/cycle/stack.rs) retains a direct `*mut u32`.
Large entities are a separate population: their single row is in their own
block header, with only a touched-list prologue allocated from scratch.
Retained rows are indexed by survivor-list positions, not ordinary size-class
slots. Report these populations separately.

## 2. Memory model: group occupancy is the relevant variable

For one array-bearing source block, let:

- `R` be the size of its row index space;
- `G = ceil(R / 8)` be the number of possible groups;
- `T` be the number of groups actually initialized;
- `V` be the number of met rows;
- `d` be bytes per entry of a proposed chunk directory.

The flat request is exactly:

```text
M_flat = 24 + 32*G + ceil(G/8)
```

The arena grants `align_up(M_flat, 8)`. Its discarded block tails are additional
costs, not part of this request.

A preliminary chunk model is:

```text
M_chunks = 24 + d*G + 32*T + A
```

`A` denotes addressing tables and other representation overhead. This formula
does not yet include per-allocation alignment or abandoned arena tails. The
shared 24-byte header is an assumption of this candidate model, not a proof
that a complete chunk implementation needs no extra header fields.

For an index space of 256 rows and 32 met rows:

| Placement | T | Flat request | Hypothetical chunks, d=2 and A=0 |
| --- | ---: | ---: | ---: |
| 32 consecutive rows, starting at a group boundary | 4 | 1,052 B | 216 B |
| One row in each group | 32 | 1,052 B | 1,112 B |

Both have slot density `V/R = 12.5%`. Their memory winners differ. This is a
counterexample to using slot density alone, even below the recorded 29% crossing.
Use `T/G` and actual allocation geometry, with `V/R` as contextual information.

The recorded `1 - 1/sqrt(2)` crossing can be reproduced by assuming independent
per-slot visitation with probability `p`, two-byte directory entries, and
omitting bitmap and other overhead:

```text
E[T]/G = 1 - (1-p)^8
2 + 32*(1-(1-p)^8) = 32
p = 1 - 1/sqrt(2) = 0.292893...
```

This supplies an explicit model under which that number follows; it does not
establish that the historical benchmark used this distribution. The number is
not a universal collector threshold. Sequential allocation, correlated graph
reachability, and retained placement need not satisfy independence. Including
the flat bitmap changes even this idealized crossing slightly.

## 3. Define chunk addressing before pricing it

The [RFC](../../rfc/model/gc/rc-cycle.md) records eight-row chunks behind
two-byte directory entries. The current [arena](../src/cycle/arena.rs) grows
through separately obtained 64-KiB blocks; they are not one contiguous address
range. A two-byte entry is therefore not automatically a pointer to a chunk.

Specify before implementation:

1. What an entry encodes: offset, table index, or another handle; how absence
   is represented; which bases/tables reconstruct the row address.
2. The representable range and behavior when it is exhausted, including a
   full trace exceeding any small handle space.
3. Where directory and chunk memory come from, what alignment they require,
   and how growth across arena blocks works without moving existing rows.
4. The complete load dependency chain for first and repeated row lookups.
5. Enumeration of initialized groups, membership probes, cleanup, and
   publication ordering on every allocation failure.

Count all supporting storage in `A`. A table lookup must appear in the
instruction/access model. Constraining chunks to one allocation region needs
a capacity proof. Reserving a whole contiguous region must be charged even
when most of its groups remain unused.

The known invariants remain: no global allocation in collection scratch paths,
stable row pointers, absorbing saturation, and no published pointer into memory
that an abort returns. This work evaluates representation, not new GC semantics.

### 3.1 The specified chunked form

Answered on 2026-09-12 (S40.5), against the tree of `6151b2c`. What follows is
a candidate for S40.2 to refuse or to open a stage for; nothing of it is built,
and the replay in `cycle::census::replay` prices it from the census alone.

**The directory.** One allocation per touched block, out of the collection's
arena at the block's first touch, in the place the flat form's `RowArray`
takes and threaded into the touched list the same way:

```text
+0   block         the block header this directory belongs to
+8   next          the touched list, newest first
+16  row_count     the index space, the bound a row index is checked against
+20  population    which word the sweep owes a null
+24  continuation  the next directory of this block's chain, null until the
                   bump has left this directory's arena block
+32  entries       one u16 per group of eight rows, G of them
```

Its size is `dir_bytes(G) = align_up(32 + 2 G, 8)`: 32 bytes for a large
entity (`G = 0`, the prologue the sweep needs and nothing behind it), 96 at
class 256 (`G = 32`), 160 at class 128 (`G = 64`), 288 at class 64
(`G = 128`), 544 at class 32 (`G = 255`) and 1,056 at the smallest class of
16 bytes (`G = 510`, 4,080 slots), which is outside the design's four classes
and the matrix. The 24-byte prologue of the flat form gains the `continuation`
word, so `A` holds 8 bytes per directory before anything else.

**The directory is written whole when it is placed.** The arena hands memory
over dirty, so the header, the `continuation` word and all `2 G` entry bytes
are zeroed at placement, as the flat form zeroes its bitmap in `shadow::init`;
an entry read before that clearing would be a stale chunk's rows and would
place a row up to 524,280 bytes past the directory. The clearing is the
directory's first-touch write: `32 + 2 G` bytes, 542 at class 32, against the
flat form's 24-byte prologue and `ceil(G / 8)`-byte bitmap, 56 at class 32.
A continuation is cleared the same way when it is placed. So on every block
the chunked form writes more at the block's first touch than the flat form,
by `2 G − ceil(G / 8) + 8` bytes, and its saving is in the bytes reserved and
not in the bytes written; the replay counts both.

**1. What an entry encodes.** Zero is absence: the group has not been met in
this collection, which is the reading the flat form's clear bitmap bit gives.
A non-zero entry `e` places the group's chunk at `directory + 8 e`. The unit
is eight bytes because the arena grants on eight-byte boundaries and the chunk
is granted by the same bump as the directory, so the distance is a multiple of
eight; a 32-byte unit would need an alignment the arena does not give. The row
of index `i` is then `chunk(i / 8) + 4 (i mod 8)`, and the bases that
reconstruct it are the directory's own address and the entry: no table.

**The chunk.** 32 bytes, eight rows, zeroed when it is placed, which is the
group init the flat form performs on the group's first touch, done on memory
the group owns rather than on a span of a reserved array. A row keeps its
address until the arena resets: a chunk is never moved, and a continuation
adds a directory rather than relocating anything, so `WorklistEntry`'s
`*mut u32` stays valid as it does today.

**2. The representable range and its exhaustion.** An entry addresses the
directory's own arena block: the payload is 65,280 bytes, so a chunk placed in
the same block as its directory is at most 65,248 bytes away and `e` is at most
8,156, inside `u16` with the rest of the range unused. The placement rule
below makes that the only case, so the entry width is never exhausted. What is
exhausted is the arena block, and the answer to that is the **continuation**:
when a chunk is to be placed and the bump has left the block of the chain's
last directory, the arena places a continuation directory in the block under
the bump, as one request with the chunk (`dir_bytes(G) + 32`), links it from
the last directory, and records the entry there. A full trace therefore has no
handle to run out of; it pays one continuation per directory per arena growth
that falls between two of its group first touches, bounded by
`min(T − 1, growths)` per block.

**3. Where the memory comes from, alignment, growth.** Directory, chunk and
continuation are all grants of `TraceScratchArena::alloc`, eight-aligned, out
of the workspace and then out of the blocks the bump draws, so the funding
order, the refusal on both paths and the reset's hand-back are exactly the flat
form's. The first touch of a block requests the directory and its first chunk
together, `dir_bytes(G) + 32`, since a first touch always meets one group; so
does a continuation. A directory thus always holds at least one chunk in its
own block, and a chunk is placed alone only when the block under the bump is
the chain's last directory's and at least 32 bytes remain in it. Growth across
arena blocks moves nothing: the chain grows at its tail, in the new block, and
every row placed before it keeps its address. The largest row-side request is
`dir_bytes(G) + 32`: 576 bytes at class 32 and 1,088 at the smallest class,
against the flat form's 8,216 and 16,408, which is what bounds the tail an
arena growth abandons on the rows' account; the chains' 4,160-byte segments
are the same in both forms and are the larger request wherever a chain draws,
so the tail bound of a whole collection is the segment's and not the
directory's.

**4. The load chain.** Both forms begin at the block's collector line, one
acquire load of the shadow word (`memory::heap::block_shadow`), and both load
`row_count` from the array's or the directory's header for the bound check.
The flat form's bitmap byte is behind `row_count` as well — its offset is
`24 + 4 × padded(row_count) + group / 8` (`shadow::groups`) — so the chain
to the initialised test is shadow, `row_count`, byte, three deep; the row's
address is `array + 24 + 4 i`, known at depth two, and the row load depends on
the byte's test only by control, which a predictor covers. The chunked form's
row address is `directory + 8 e + 4 (i mod 8)`, a data dependency on the
entry, three deep and predicted by nothing: that is the further dependent
load the rfc records, restated as the depth at which the row's address is
known, two against three. On a group's first lookup the flat form finds the
bit clear, sets it and zeroes eight rows in place; the chunked form finds the
entry zero and then does what the flat form's first touch does not — the
bump's grant (the cursor and the remaining count moved), the test whether
the block under the bump is the chain's tail directory's, the walk to that
tail, the zeroing of the 32 bytes granted and the entry store; on a
continuation, a whole directory cleared and linked first. A row whose group
was placed in a continuation costs two loads more per hop (`continuation`,
then the entry there), and only the groups met after a growth stand there.
`find_initialized_row`, the scan's read-only twin, walks the chain on a zero
entry and answers absent at its end, so an absent group costs one entry load
per directory of the chain instead of one bitmap byte.

**5. Enumeration, probe, cleanup, refusal.** `for_each_of_color` reads every
entry of every directory of the chain and, for each non-zero one, the eight
rows of its chunk up to `row_count`: `G × chain` entry loads and `8 T` row
reads, against the flat form's `G` bit reads and `8 T` row reads, so the
per-group term the hardware arm measured at class 32 stands in both forms. The
membership probe is `find_initialized_row` as in 4. The cleanup is the flat
form's to the instruction: the sweep nulls the shadow word of every touched
block through the touched list, which threads through the directories, and
chunks and continuations die with the arena's reset; no chunk is referenced
from the heap. The publication order on a refused allocation keeps the flat
form's rule that nothing outside the arena points at memory an abort returns:
the directory is enrolled before the shadow word is stored, a refused first
touch stamps nothing; a refused chunk leaves its entry zero, since the entry is
written after the grant; a refused continuation leaves the tail's
`continuation` null for the same reason. Every non-null pointer written by
this form names arena memory, and the arena returns it after the sweep has
nulled the only heap-side pointer into it.

**`A`, in full.** Per directory, 8 bytes over the flat prologue plus the
rounding of `32 + 2 G` to eight; per continuation, `dir_bytes(G)`; per chunk,
nothing, 32 bytes being a multiple of eight; no table, no per-arena state. So
`M_chunks = dir_bytes(G) + 32 T + C × dir_bytes(G)`, with `C` the
continuations, and the two placements of §2 read 224 and 1,120 bytes against
the flat form's 1,056-byte grant, at `C = 0`. The bytes written at first
touch are `dir_bytes(G)` per directory and continuation and 32 per group met,
against the flat form's `24 + ceil(G / 8)` and 32 per group met.

**What the form does not change.** The saturation rule, the colour codes and
the row word; the retained population, whose index space is the survivor
list and whose groups are eight consecutive positions of it; the large
entity, whose row stays in its own block header behind a prologue-only
directory; and the collector line, whose shadow word points at the first
directory as it points at the array today.

## 4. Why the old six-versus-zero calculation is insufficient

[density::tests::collect](../src/cycle/density/tests.rs) calls `trace_batch`,
reads its rows, and closes the window. It does not execute the ordinary commit.
The historical sparse measurement and its hypothetical chunk arithmetic must
remain labeled as that trace-only scope.

Today's ordinary commit calls `stamp_live_components` even when the candidate
membership is empty. In [maturation.rs](../src/cycle/maturation.rs), an open
vertex pushes a `Finish` frame and its outgoing edge frames; descent into an
unvisited child additionally leaves a `Post` frame. Component vertices stand
on a separate stack until their strongly connected component closes.

For a simple live directed ring of `n` met vertices, one counted outgoing edge
per vertex, successful unpruned descent, and no additional met vertices:

```text
peak work frames       = 2*n
peak component entries = n
segment bytes          = 64 + 256*16 = 4,160
stack reservation      = 4,160*(ceil(2*n/256) + ceil(n/256))
```

At `n = 381` this is three worklist segments and two component segments,
or 20,800 bytes. The worklist segments are reused across phases; do not add
another mark/scan worklist to this maximum. An outside keeper is not itself
met merely because it owns an incoming edge; a fixture tracing through a keeper
or containing extra outgoing edges must account for them separately.

The optimistic old chunk model for 381 source blocks of class 256 gives:

```text
source-block slots = 255; groups = 32; one met group per source block
chunk row storage = 381*(24 + 2*32 + 32) = 45,720 B
rows plus stacks  = 45,720 + 20,800      = 66,520 B
base bump capacity                       56,960 B
```

Thus a full successful descent cannot fit even this optimistic representation
in the base bump. At least one overflow block is needed for that execution.
Exact draw counts depend on the implemented chunk layout and grant order.
This is a source-derived capacity bound, not a new execution measurement.

For flat arrays, the 1,052-byte request is a 1,056-byte arena grant: 381 arrays
consume 402,336 grant bytes rather than 400,812 request bytes. Abandoned tails,
stacks, and block headers must not disappear in a comparison of row requests.

Record both the end-of-scan workspace and the full-collection maximum. A
maturation allocation refusal abandons maturation, which is distinct from a
mark/scan refusal that aborts the trace; report these outcomes separately.

## 5. Measurement quantities and their limits

| Quantity | Meaning | What it cannot establish |
| --- | --- | --- |
| Row requests and aligned grants | Logical reservation by the row representation | Bytes written or cache misses |
| Initialization write bytes | Header/bitmap setup and first group initialization | All later row stores |
| Total semantic row-store bytes | Sum of row writes, including repeated writes | Machine store instructions or DRAM traffic |
| Distinct written line addresses | Union of address ranges written, divided into cache lines | Fill counts, evictions, or writeback volume |
| PMU events | Hardware events during a specified execution interval | Attribution to rows alone |
| Overflow draws and refusals | Actual manager interactions and collection outcomes | A probabilistic failure rate without a workload model |

`shadow::written_bytes()` currently accounts for the prologue and bitmap at
reservation, then 33 bytes per initialized group: one bitmap-byte store and
32 bytes of zeroing. It omits the first row initialization from refcount,
subtraction stores, recoloring, and maturation-index writes. Preserve its
existing contract and label it initialization work; do not rename that reading
as full-trace writes.

If total semantic row stores are needed, count their actual writers in the
structural build. Final row contents cannot recover repeated stores. Saturation
and repeated scan classification prevent a general derivation from `V` and
one inferred edge count alone.

Distinct written row lines can be reconstructed from initialized groups:
each contributes its entire 32-byte range, not merely its met row words.
Add the header and bitmap ranges for an array-wide figure, and the actual
header-row locations for large entities. Form one address union across arrays;
adjacent allocations can share a cache line. State the line size and separately
label any distinct-page count. These address sets do not reconstruct all reads
or their temporal order. Snapshot traversal itself belongs outside PMU runs.

## 6. One report, multiple observation boundaries

Add test-only `cycle::census` beside `density`, with one report assembled from
phase snapshots and event counters. A single snapshot after the ABI call is
too late: rows have gone, peak stacks have emptied, and returned blocks no
longer identify their former consumers.

| Boundary | Reading |
| --- | --- |
| Before workspace open | Existing physical holdings, queue state, initial funding |
| After mark | Attempted root records, trace status, mark dispatch count |
| After scan and before maturation | V, colors, saturation, internal-edge census, G/T, row ranges |
| During commit and cleanup | Phase maxima, validation work, funding, abandonment and returns |
| After all collection cleanup | Ending, freed entities, remaining holdings and queue records |

Each phase snapshot has an explicit presence/completeness tag. On mark/scan
refusal, capture a partial footprint before `sweep_rows` clears the touched
list; only initialized group storage is readable. A partial mark is not an
exact internal-edge census, and a partial scan has no complete classification.
If workspace never opened, report that outcome and absent phase snapshots,
not a successful zero-sized trace. An early return after scan may retain a
complete scan snapshot while later phases remain absent. Preserve allocation
and cleanup counters through all these endings.

`trace_batch`'s second return value counts root records attempted, including
the root whose mark refused allocation. It is not automatically a count of
successfully expanded live vertices or distinct roots. Report detached records,
attempted records and distinct root identities separately where needed.
Unique-root extraction belongs in the structural harness.

Read the internal-edge census before maturation: live counts become traversal
indexes afterward, as [density.rs](../src/cycle/density.rs) explicitly requires.
Define `E` precisely as graph/internal edges or as phase-specific edge visits;
neither a row-dispatch count nor the whole collection's visits is interchangeable
with the other. Live rings have zero proposed members and skip exact validation.
Distinguish revalidation-stage entries from actual `validate_component` calls:
`Revalidation::revalidate` returns without a second exact pass when no destructor
ran. Count actual invocations, their member/refcount walks, cell walks, and
membership probes. Debug-only premise checks are a separate structural-build
quantity, not part of release validation cost.

Instrument successful aligned grants, failed requests, successful growth,
discarded tails and block returns. A failed growth leaves the current tail
available and must not count it as abandoned. For one arena lifetime, verify:

```text
base bump capacity + sum(successful overflow payload capacities)
  = aligned granted bytes + abandoned tails + final remaining bytes
```

Track current and peak worklist entries on successful pushes/pops, including
the early successful push path and cleanup. Count retained segment capacity
separately. A one-comparison-only claim needs a demonstrated O(1) current-depth
reading; existing segment counts alone are insufficient. Existing maturation
component high-water can be reused.

Separate funding (ordinary pool, critical reserve) from role (queue base,
workspace base, overflow) and consumer (rows, worklist, component stack, drops,
listed members, queue records). Blocks may serve several consumers. Never sum
independently observed consumer peaks and present them as a simultaneous peak.
Manager counters have transition-based publication; reconcile their residues
and persistent bases rather than treating a snapshot delta as a peak.

Counters and final report ownership must survive early returns and cleanup.
For nested collections, use scoped identities or explicitly restrict a fixture
to nonnested execution; resetting shared TLS must not erase an outer report.
No measurement helper may allocate through the collector's own allocation path.
Keep instrumentation out of production builds and register new TLS in the
existing first-touch census. Do not fix the counter count at an arbitrary ten.

## 7. Workloads that distinguish the hypotheses

Keep the proposed sizes `2, 16, 256, 381` and ordinary classes
`32, 64, 128, 256`. Keep dense, one-per-block, and retained placement, but add
targeted contrasts rather than every Cartesian combination:

| Contrast | Question |
| --- | --- |
| Registered live ring | Reproducibility, normal trace, maturation and deferral |
| Same ring after removing the external keeper | Exact validation, reclamation and withheld returns |
| Unreachable ring with at least one executed no-op destructor | The second physical exact-validation pass |
| Same R/V with clustered versus dispersed groups | Effect of T/G at fixed slot density |
| Fully reached ordinary blocks in each design class | The dense extreme: all meaningful groups initialized |
| Same vertices with more counted edges | Cost of repeated directory lookups |
| Same placed vertices/edges, different traversal order | Locality sensitivity |
| Retained blocks with all versus few survivors reached | Retained group occupancy and lookup cost |
| Actual pressure entry with controlled available memory | Pressure lifetime, harvest limits and completion |
| Ordinary entry with controlled overflow funding | Representation-dependent allocation and safe refusal |

Use separate fixtures or rebuild outside the measured region for collections
that reclaim their graph. Boundary cases around observed segment/block growth
thresholds complement the four requested sizes. Counter calibration must also
exercise otherwise-zero events such as overflow and exact validation.

For the full ordinary-block control, use `n = slots_per_block(class)` and
verify actual placement and `V = R`, `T = G` in the intended blocks. The class
32/64/128 counts are 2,040/1,020/510, beyond the original maximum of 381.
Arrange the keeper separately so it does not occupy an intended ring slot.
One retained block at full visitation is not a substitute for this ordinary
control because retained indexing follows a different lookup path. These are
additional endpoint cases, not a multiplier on the whole matrix.

All-member registration in an ordinary ring prevents pruning of its members. Assert
the actual root set, zero pruning, and expected met vertices rather than assume
repeated calls still trace the same graph. For other graphs, stabilize and
report maturation/epoch state explicitly.

The existing retained fixture is specifically different: only its heap holder
is registered. The retained ring is reached through that holder. Full commits
age its unregistered members; with a pinned epoch, a subsequent mark prunes
the entry once its age reaches the threshold. Re-offering the holder does not
undo this. Its old trace-only repetitions did not execute maturation.

Use two explicitly distinct retained protocols:

- For natural evolution, start with fresh retained members, force the holder's
  re-offer before each of eight full collections, and report each collection's
  pruning and footprint separately. Assert the expected transition; do not
  compare post-pruning `V/E/G/T` against the first full trace as equal. This
  studies maturation under a forced collection schedule, not its occurrence
  frequency in an application.
- For a fixed full-trace representation comparison, restore every fixture
  member's maturation stamp to a specified age-zero state before each measured
  collection, through an explicitly benchmark-only setup hook outside the
  interval. Re-offer the holder there too. Keep the threshold and the timed
  collector algorithm unchanged and verify zero pruning in the structural
  companion. This is an artificial full-trace workload. The stamp stores warm
  entity headers; state that initial condition and apply identical preparation
  to both representations. It is not a cold-cache result. The hook belongs to
  the driver support described below and must not introduce a check on each
  production row lookup.

An epoch pin alone is insufficient. In the ordinary-library PMU build the
test-only epoch pin is absent: record the actual epoch/commit progression and
keep comparison batches at matching schedule positions. Age-zero restoration
prevents stale mature stamps from pruning regardless of their previous epoch.

Manually re-offering a deferred lane before each collection creates a forced
trace workload. Its re-offer frequency is a fixture input, not an observation
of production scheduling. Time re-offer separately or label an interval that
includes it. The normal poll's automatic epoch-driven re-offer needs its own
behavioral calibration.

Test memory limits after the base is acquired as well as first-open refusal.
Report trace refusal, maturation abandonment, teardown refusal, and successful
freeing independently. Fewer acquisition calls are fewer refusal sites, not a
measured failure probability. Assert cleanup and queue preservation on failure.

## 8. Separate structural and hardware experiments

`cargo test --release` still defines `cfg(test)`. In
[row::resolve_edge_target](../src/cycle/row.rs), an ordinary test-gated `assert!`
calls `stands_where_a_block_can` on every dispatch. That function walks the
pool's region registry and may consult the large-entity registry. It survives
release optimization, as do test counters. A repeatable measurement of that
binary is not a clean estimate of production row-lookup cost.

Use:

- A structural build with census, detailed event counting and fixture checks.
- An optimized benchmark driver linked to the ordinary library, without
  `cfg(test)` or per-edge counters. Any necessary fixture-control hook must
  have explicit scope and no instrumentation on the timed hot path.

Share workload construction and expected topology between them. Build the two
row representations from the same revision/configuration except for the
representation selector. Preserve the same GC semantics and correctness tests.

Treat eight collections as a structural repeatability check, not a statistical
sample-size guarantee. Measure first workspace acquisition separately. For the
warm arm, establish stable relevant state, then choose a repetition count that
makes interval-control overhead small relative to useful work and yields
stable estimates. Repeat independent batches and report dispersion.

For `perf stat --control=fifo`, start disabled with `--delay=-1`, use an
acknowledgment FIFO, and wait for enable/disable acknowledgments. Measure an
empty control interval to bound instrumentation overhead; acknowledgment does
not make the interval cost-free. Keep setup, census walks and output outside
the interval. See the [perf stat manual](https://man7.org/linux/man-pages/man1/perf-stat.1.html).

Pin the benchmark worker to one CPU and verify the counted thread scope.
Record CPU/PMU identity, binary revision and event encodings. Check
`time_running/time_enabled`; split event sets into repeatable runs when they
cannot run together. Multiplexed values are scaled estimates. Generic
`cache-misses` is not automatically equivalent to `LLC-load-misses`, and the
proposed load-miss events do not measure all write traffic. See
[perf_event_open](https://man7.org/linux/man-pages/man2/perf_event_open.2.html).
Availability on the cited WSL2 machine remains a claim to verify at execution;
this source review does not establish current PMU support.

Use flat -> chunks -> flat controls in one sitting, following
[BENCHMARKS.md's Method](BENCHMARKS.md#method). A -> A alone estimates baseline
repeatability; it does not measure the alternative. Establish tolerances from
current repeated controls, reject disturbed comparisons, and distinguish a
difference below resolution from equality. PMU event noise is not governed by
one universal percentage; exact structural invariants have no statistical noise.

## 9. Execution order and decision record

1. Specify chunk layout/addressing, failure behavior, and all storage overhead.
   Done in §3.1 (S40.5).
2. Implement the phase-aware census and calibrate it against small fixtures,
   saturation, refusals, and segment boundaries. Keep runtime behavior unchanged.
   Done: `cycle::census` (S40.3).
3. Replay the observed allocation sequence in an arena-placement model. First
   reproduce flat grants, alignment, tails and draws; only then estimate chunks
   using their actual allocation events and order. A byte-total division is not
   a packing proof. Done: `cycle::census::replay` reproduces the flat grants,
   tails and draws of every load to the byte and replays the chunked form in the
   loads' own order with a bracket over every order (S40.5); the order of group
   first touches across blocks is the one event the census does not record, and
   the bracket is what stands in for it.
4. Implement a comparable chunk candidate and verify row semantics, stable
   pointers, enumeration and cleanup. Run scoped correctness checks before timing.
5. Execute structural and hardware arms on identical workload definitions;
   measure normal and memory-constrained outcomes with their stated boundaries.
6. Record the decision in `DECISIONS.md`, with raw measurements and commands in
   `BENCHMARKS.md`. State the workload range in which the rejected form wins.

The decision table needs at least:

```text
representation, population/placement, V, E, G, T,
row requests/grants, total workspace peak, overflow draws by source,
trace/commit outcome, freed members, instructions and cycles per collection
```

Additional cache events explain observed differences; they do not replace
execution cost or successful reclamation. Normalize by collection first, then
by clearly defined vertex/edge counts for interpretation. Do not average
different populations or failure outcomes into one density or speedup.

No universal winner follows without a workload distribution or a declared
priority between memory-constrained completion and execution cost. Publish the
tradeoff range if neither form dominates. This analysis recommends obtaining
that evidence; it does not authorize a representation switch or invent an
acceptable slowdown threshold.

## 10. Independent critic and author responses

An independent read-only critic reviewed this document against the source on
2026-09-12. The critic confirmed the main memory formulas, the missing chunk
addressing specification, the release-test instrumentation problem, and the
`2*n`/`n` stack maxima for the restricted ring. In particular, the 20,800-byte
stack reservation and the 66,520 > 56,960 capacity comparison stood. This was
source review, not a runtime measurement.

| Finding | Author response and disposition |
| --- | --- |
| C1, substantial: retained repeats do not preserve the traced graph merely by pinning the epoch and re-offering the holder | Accepted. Section 7 now distinguishes the holder-only registration and gives natural-evolution and artificial full-trace protocols. Stamp restoration is outside timing and its cache warming is explicit. |
| C2, required for refusal experiments: a missing post-scan snapshot can hide already initialized rows | Accepted. Section 6 requires tagged partial pre-sweep snapshots, forbids incomplete-mark internal-edge interpretation, and distinguishes absent phases from measured zero. |
| C3, required for validation calibration: revalidation can skip the physical exact pass | Accepted. Section 6 separates stage entries, actual validations, walks and probes; section 7 adds an executed-destructor calibration case. |
| C4, bounded improvement: sizes at most 381 miss the fully occupied ordinary-block endpoint in several classes | Accepted. Section 7 adds verified full-block controls, with the keeper placed separately, without expanding every arm. |

No reported finding was rejected. The responses narrow unsupported claims and
make the experiment executable; they do not turn its predictions into results.
The author also made the full-directory enumeration cost explicit and added
the reviewed source revision.

The critic rechecked the revised sections 6, 7 and this response record and
closed C1-C4, finding no remaining concrete error in those changes. The closure
accepts the analysis as an experiment design; implementation and measurement
remain future work.

Document checks: local links resolve and `git diff --check` passes. An abstract
replay of the specified ring frame events agrees with the `2*n` maximum at
`n = 2, 16, 256, 381`; it is an arithmetic cross-check rather than independent
execution of the Rust collector. No collector test run or PMU measurement is
claimed for this documentation change.
