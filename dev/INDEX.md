# Index

Map of the project for an agent: where to look, so the whole tree does
not have to be read. Pointers only — nothing is explained here, only
located.

## Modules

Knowledge map: `dev/ARCHITECTURE.md` — **not written yet**. Until it
exists, the module docs at the top of each file are the map;
`src/memory/heap.rs` and `src/promote.rs` carry the fullest ones.

## Entry points

- C ABI surface: `src/memory/context.rs` (arena + context),
  `src/memory/stdapi.rs` (`ll_malloc`/`ll_free`/aligned),
  `src/memory/barrier.rs` (`ll_ref_store`), `src/object.rs`
  (`ll_object_new`, `ll_object_die`), `src/refcount.rs`
  (`ll_retain`/`ll_release`).
- Crate root: `src/lib.rs`. Built as `rlib` + `staticlib` for the
  C++/LLVM layer.
- Tests: inline `#[cfg(test)]` per module, no `tests/` directory.
- Benches: `benches/alloc.rs`, `benches/standard.rs`; external probes
  in `bench-external/`.

## Hot paths

- Allocation: `Heap::alloc` → `ll_alloc`, expected to inline fully,
  cold tails split with `#[cold] #[inline(never)]`.
- Local free: `Heap::free`, including the `owner` check.
- Store barrier: `ref_store` / `ll_ref_store`.
- Arena bump: `Arena::alloc` → `ll_arena_alloc`.

Measured by `cargo bench --bench standard -- our_heap` (larson,
rptest); headline comparison in `RESULTS.md`, change log in
`dev/BENCHMARKS.md`.

## Layout contracts (pinned by tests)

- Block header halves and cache lines: `memory::heap::tests::`
  `block_header_halves_are_laid_out_as_the_design_requires`.
- `RcHeader` 8 bytes at offset 0: `refcount::tests::`
  `header_is_8_bytes_at_offset_zero`.
- `Value` 16 bytes, fixed offsets: `value::tests::`
  `box_is_16_bytes_with_fixed_offsets`.

## Key decisions

`dev/DECISIONS.md` — arena handle as a raw pointer; trailing inline
data through raw pointers; block header split by access rule; cold
concurrent structures take a lock rather than a CAS loop; Miri against
a UNIX target. All 2026-07-20.

## Diagrams

`dev/design/` — none yet.

## Traps

`dev/POSTMORTEM.md` — benchmarking against a stale baseline
(2026-07-20).

Also worth knowing before touching this crate:

- Formal-UB defects here all pass `cargo test`. Only Miri sees them,
  and only against a UNIX target — see `dev/WORKFLOW.md`.
- Miri is blind to leaks here (`-Zmiri-ignore-leaks` is mandatory) and
  runs in permissive provenance in the pointer-heavy modules, so a
  clean run is not proof there.
- The block header is a tagged union shared with the pool's
  `BlockHeader`: `kind` must stay at offset 0, and the pool's `next`
  overlays the heap's `used`.

## Conventions

`dev/WORKFLOW.md` — branches, commits, the required verification
sequence, test rules, Miri invocation.

Not obvious from the code: `AUDIT.md` and `.idea/` are deliberately
untracked and must stay so — this repository is public and the audit
lists unfixed defects. Design lives in the separate `limelight-lang/rfc`
repo and is kept in sync with behaviour changes.
