# ll-model

Limelight runtime data model: the memory manager, the object model and the
cycle collector.

Implements the design from the [rfc](https://github.com/limelight-lang/rfc)
repository (`model/`, `runtime/`), which is authoritative and is read before
code is written. This crate contains runtime *mechanics* only — PHP standard
library functions (`print`, string functions, …) do **not** live here; they
belong to the future stdlib repository.

## What is here

- **The memory manager** (`src/memory/`): 64 KiB blocks carved out of OS
  regions, a per-thread small-object heap in mimalloc's shape, a request arena
  with promotion at reset, buffer arenas for out-of-line payloads, large
  entities one per allocation, and the reserves that keep the store barrier
  and a collection from failing on a refused block. `docs/memory-manager.md`
  is the normative description.
- **The object model**: the 8-byte `RcHeader` (`src/refcount.rs`), the
  16-byte value box (`src/value.rs`), classes, objects, strings, interpolated
  templates, arrays (a mixed vector and an ordered hash), reference boxes,
  weak references and static blocks.
- **The cycle collector** `rc-cycle` (`src/cycle/`): candidates registered
  from the release path into a per-thread ring, trial deletion over shadow
  rows, exact validation by the owning thread, finalization and reclamation
  in one commit, and a collector thread that traces a mutator's candidates
  under a trace token and posts verdicts back. The design is
  `rfc/model/gc/rc-cycle.md`.

Where things are: `dev/INDEX.md`. How the modules fit and what each may not
know: `dev/ARCHITECTURE.md`, drawn in `docs/architecture.md`. What was
decided and why: `dev/DECISIONS.md`. What was measured: `dev/BENCHMARKS.md`.
The traps: `dev/POSTMORTEM.md`. The routine every change follows:
`dev/WORKFLOW.md`. The work in progress: `PLAN.md`.

## Development loop

The gate before every commit is `dev/WORKFLOW.md`, "Verification":

```sh
cargo test --lib -- --test-threads=8                       # three times
LL_HASH_SEED=1 cargo test --lib --features hash-folding -- --test-threads=8
cargo test --lib --features debug-journal -- --test-threads=8   # three times
cargo build --release
cargo bench --no-run
cargo doc --no-deps --document-private-items               # no warning
cargo +1.94 fmt --check
python3 dev/tools/citations.py                             # no miss
```

Every test is a unit test and reads crate-internal state; there is no
`tests/` directory. Miri, ThreadSanitizer and loom run outside the gate, in
the slices `dev/WORKFLOW.md` names.

## Benchmarks

```sh
cargo bench
```

`benches/` holds the allocator comparisons (`alloc.rs`, `standard.rs`), the
store barrier (`barrier.rs`), the object lifecycle (`lifecycle.rs`), strings
(`strings.rs`) and the value box (`value.rs`). A change to a hot path lands
with both arms measured in one session, and the record is `dev/BENCHMARKS.md`;
`benches/RESULTS.md` is the headline allocator comparison. The noise floor of
the development box is 1.5–3 %, so an effect smaller than that is not
claimed here.

The allocator comparison, read as ratios (Rust 1.87, `x86_64-pc-windows-msvc`,
2026-07; per allocation of 40 bytes, written to and reclaimed):

| Contender | Per alloc | vs arena |
|---|---|---|
| **arena** | ~0.82 ns | 1.0× |
| **arena + `reserve`** | ~0.81 ns | ~1.0× |
| bumpalo | ~1.26 ns | ~1.5× slower |
| mimalloc | ~4.4 ns | ~5.4× slower |
| system malloc | ~34 ns | ~41× slower |

Every contender runs the same workload — the arena pays `reset`, bumpalo pays
`reset`, malloc and mimalloc pay per-object frees. What these numbers do and
do not prove: [`benches/RESULTS.md`](benches/RESULTS.md).

## LLVM IR export

The runtime's hot paths must inline into compiled PHP code
([rfc/runtime/implementation-language.md](https://github.com/limelight-lang/rfc/blob/main/runtime/implementation-language.md)).
The crate is built with `codegen-units = 1` in release, so it emits one LLVM
module:

```sh
cargo rustc --release --lib -- --emit=llvm-ir,llvm-bc
# -> target/release/deps/ll_model-*.{ll,bc}
```

Merging with compiler-generated IR:

```sh
llvm-link php_generated.ll ll_model-*.bc -o combined.bc
opt -O2 combined.bc -o final.bc
```

Verified on 2026-07 with Rust 1.87 / LLVM 20.1 on `x86_64-pc-windows-msvc`:
`ll_retain` and `ll_release` emit as small IR functions, `llvm-link` merges
the crate's bitcode with hand-written IR, and after `opt -O2` `ll_retain`'s
body inlines into the calling function. One gotcha: the inliner refuses a
callee whose `target-features` are not a subset of the caller's. Rust emits
`"target-features"="+cx16,+sse3,+sahf"` (baseline x86-64); generated PHP IR
must carry matching `target-cpu` / `target-features` attributes on its
functions, or nothing from the runtime will inline. The LLVM tools of the
right version ship with `rustup component add llvm-tools`.

## Naming

Module and identifier names are full, readable words (`refcount`, not `rc`);
abbreviations only where they are the established term of the domain (`gc`,
`tls`, `abi`). The vocabulary follows `rfc/dev/GLOSSARY.md`, and three guard
tests in `src/cycle/tests/` fail on a retired word.
