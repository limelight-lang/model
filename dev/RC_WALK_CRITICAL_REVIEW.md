# rc-walk critical review

**Reviewed:** 2026-08-16
**Repository state:** `c654ac9` (`main`)
**Scope:** the implemented collector and its runtime protocol, not only the
RFC design.

## Executive assessment

The mathematical core of rc-walk is strong and deliberately conservative.
Roots are derived from `RC - IN`; uncertain observations postpone collection
instead of freeing early; Phase 3 rechecks the concurrent snapshot; and Phase
4 repeats an exact test on the mutator before destruction. No obvious path to
freeing a live object was found while the documented single-mutator and epoch
protocol invariants hold.

The implementation is not yet a production collector, however. Its weakest
parts are integration, progress, collector memory consumption, deferred-free
growth, and enforcement of the invariants on which raw-pointer validity
depends. The correctness design is materially more mature than the production
shell around it.

## Findings

### 1. No production driver or automatic collection

`collector::run_epoch` implements the intended blocking driver, but no
production collector thread spawns it and there is no ABI trigger or measured
trigger policy. Tests are currently the only driver.

Consequently, selecting the default `rc-walk` feature does not by itself make
cycle collection run in the shipped runtime. Unless a future integration calls
the stepped API, unreachable cycles accumulate indefinitely.

**Severity:** high for production readiness; known incompleteness rather than
a hidden correctness defect.

**Relevant code:** `src/collector.rs`, `run_epoch` and its preceding comment.

### 2. Epoch progress has no bound

The collector waits for handshake acknowledgement and verdict drain with
spin/yield loops. A mutator acknowledges or drains only when it reaches one of
the designated checkpoints: a final release, poll, teardown exit, or explicit
batched checkpoint.

A mutator in a long computation, syscall, FFI call, pure-allocation loop, or a
workload containing only non-final reference operations can therefore stall an
epoch indefinitely. `yield_now` also consumes scheduling attention without
providing a timeout, abort path, or useful diagnosis.

**Severity:** high. This is an accepted design limit, but it becomes an
operational failure mode once epochs run automatically.

**Recommendations:** use an event/condition-based wait; expose epoch age and
checkpoint latency; permit a safe abort before verdict publication; define an
explicit fairness contract for generated code and FFI boundaries.

### 3. Deferred memory is unbounded in epoch duration

Every relevant free during an active epoch parks physical release. The useful
bound is therefore:

```text
parked memory ~= allocation churn rate * epoch duration
```

It is not bounded by the live heap at epoch start. Since epoch duration itself
has no bound, neither does the transient retained memory. Large entities and
array storage make a record-count watermark insufficient; retained bytes must
be measured.

A thread that exits while an epoch is active can also leak its parked backlog
until process exit. One record can pin a 64 KiB block or an entire large run.

**Severity:** high.

**Relevant code:** `src/memory/deferred_free.rs`; `docs/memory-manager.md`,
"Deferred free".

**Recommendations:** track parked bytes; abort an unposted epoch at a hard
watermark; solve exiting-thread handoff; implement and prove the proposed
young-free exemption.

### 4. The free path allocates memory

`deferred_free::park*` lazily allocates a `Vec<Parked>` and grows it with
ordinary `push`. Collector phases also allocate large `Vec`, `HashMap`, and
message payloads without a fallible or budgeted path.

This is dangerous precisely when collection is triggered by memory pressure:
freeing an object can require more memory. The release profile uses
`panic = "abort"`, so allocator failure in this machinery can terminate the
process instead of recovering memory or abandoning the epoch conservatively.

**Severity:** high for an OOM-capable runtime.

**Recommendations:** use a pre-reserved/chunked ledger, preferably funded from
an emergency reserve; make collector metadata allocation fallible; abort safely
before verdict publication when the budget is exhausted.

### 5. Per-epoch graph metadata is heavy

The collector keeps several arrays per walked entity and a 32-byte `Edge` per
recorded edge. `judge` then creates a second `(u32, u32)` edge list, multiple
marking arrays, CSR storage, and `Vec<Vec<u32>>` adjacency for every entity.
Phase 3 creates another `Vec<Vec<u32>>` to distribute edges among components.

An empty `Vec` is commonly 24 bytes, so one million walked entities cost about
24 MiB merely for the empty undirected-adjacency shells, before their contents,
the original edges, census, rows, flags, versions, and component data. On a
large graph the collector can consume hundreds of megabytes of temporary
metadata while the mutator's freed memory is simultaneously parked.

**Severity:** medium to high, workload-dependent.

**Relevant code:** `src/walk.rs::garbage_components`,
`src/collector.rs::judge`, and `Epoch::recheck_and_post`.

**Recommendations:** keep the graph in flat CSR arrays; avoid the `Edge` to
pair copy; replace per-node and per-component nested vectors; reuse a budgeted
epoch arena across collections.

### 6. Phase 4 can create an unbounded mutator pause

Discovery and preliminary judgement can run off-thread, but confirmed
components are drained on the mutator. Drain performs a current-graph trace,
allocates a membership map, installs guards, updates weak references, executes
user destructors, possibly verifies again, severs edges, and performs cascading
release and teardown.

A large component therefore produces an unbounded checkpoint pause. User
destructors make the tail even less predictable. The claim that collector work
is off the mutator thread must be qualified: candidate discovery is off-thread;
final verification and reclamation are not.

**Severity:** medium to high for latency-sensitive workloads.

**Relevant code:** `src/walk.rs::drain_confirmed`.

**Recommendations:** measure p95/p99 drain duration and component sizes; devise
a resumable drain protocol or explicitly cap acceptable component latency.
Simply stopping midway is not safe under the current guard/sever discipline.

### 7. Soundness-critical state transitions use `debug_assert`

Non-nesting epochs, acknowledgement-before-snapshot, acknowledgement-before-
recheck, and no-close-with-outstanding-verdicts are partly enforced with
`debug_assert!`. Those checks disappear in release builds.

Violating these rules is more serious than producing bad statistics. Closing
the deferred-free window while verdict messages still contain raw pointers can
permit slot reuse and invalidate the identity guarantee. Opening nested epochs
also conflicts with the single global activity bit and handshake state.

The manually stepped `Epoch` exposes several `pub(crate)` methods, so invalid
call sequences are constructible as the crate evolves.

**Severity:** medium today because the sole intended driver orders operations
correctly; high as an integration hazard.

**Recommendations:** represent phases with typestate, or return checked
`Result`s and retain mandatory release-mode assertions for every invariant on
which pointer validity depends.

### 8. The acyclic-class optimization is not implemented

The design allows classes proven unable to participate in cycles to be omitted
totally from the walk. Current code deliberately does not take this path because
the compiler does not yet supply the flag.

This preserves correctness but enlarges the census, graph, epoch duration, and
parked backlog. The omission matters more because the collector is whole-heap
and metadata-heavy.

**Severity:** medium for efficiency, none for correctness.

**Relevant code:** `src/walk.rs`, the Phase 1 comment in
`collect_cycles_inner`; the concurrent census likewise has no acyclic skip.

### 9. Active mutation can repeatedly acquit garbage

Any relevant count, edge, or movable-storage change between WALK and FILTER
acquits the whole weakly connected candidate component. This is the correct
conservative direction, but a frequently changing neighbour can repeatedly
delay otherwise stable garbage. Weakly connected grouping increases the blast
radius of one changed edge.

Current `EpochStats` reports aggregate acquittals but not why they happened,
their component sizes, or how many epochs an eventual collection required.

**Severity:** medium; a possible collection-starvation and observability issue,
not premature reclamation.

**Recommendations:** record acquittal reason (count, cell, storage version,
corpse, Phase-4 exact test, resurrection), component size, and retry age.

### 10. The protocol is intrinsically single-mutator

The design uses one global handshake bit and acknowledgement count, one global
verdict queue without owner routing, and non-RMW reference counts. Under the
documented single-mutator contract this is coherent.

It cannot be generalized to multiple mutators by merely adding locks. One
thread could acknowledge another's publication obligation or drain a component
owned by another thread. Actors or parallel mutators require per-domain epoch
state, owner-addressed queues, explicit transfer rules, and a new refcount
contract.

**Severity:** architectural constraint, not a current bug.

## Strengths worth preserving

- `RC - IN` derives roots without stack scanning.
- Unwalked regions conservatively act as root sources.
- Allocate-black delays judgement of new entities.
- Concurrent uncertainty normally costs recall, not safety.
- Phase 3 rechecks counts, cell contents, cell shape, and movable-storage
  versions.
- Phase 4 trusts no collector verdict and recomputes exact component liveness.
- The corpse rule handles ordinary death between posting and drain.
- Weak cells are cleared before destructors.
- Destructor resurrection is followed by a guard-discounted recheck.
- Logical death is separated from physical reuse, preserving slot identity.
- The stepped state machine enables deterministic danger-case tests.

## Recommended order of work

1. Connect a real trigger and collector thread, with end-to-end telemetry.
2. Add parked-byte accounting, an epoch age limit, and safe pre-verdict abort.
3. Remove unbounded allocation from deferred-free and other recovery paths.
4. Enforce identity and epoch-state invariants in release builds.
5. Replace nested graph structures with flat, reusable, budgeted storage.
6. Measure and bound Phase-4 mutator latency.
7. Add detailed acquittal and retry telemetry.
8. Implement compiler-provided acyclic classification.

## Verification note

Targeted collector tests could not be executed in this review environment.
The checked-out source uses let-chain syntax rejected by the installed
`rustc 1.87.0`, so compilation fails before the tests run. The repository
should pin its required toolchain (for example with `rust-toolchain.toml`) or
state the minimum supported Rust version explicitly.
