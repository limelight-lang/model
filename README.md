# ll-model

Limelight runtime data model: classes, memory manager, GC.

Implements the design from the [rfc](https://github.com/limelight-lang/rfc)
repository (`model/`, `runtime/`). This crate contains runtime
*mechanics* only — PHP standard library functions (`print`, string
functions, …) do **not** live here; they belong to the future stdlib
repository.

## Development loop

Code in Rust, tests in Rust:

```sh
cargo test
```

Tests compile to a native executable and run directly — the ordinary
Rust loop. The LLVM IR export below is a *second artifact of the same
code*, verified separately.

## Benchmarks

```sh
cargo bench
```

**Honest methodology** (`docs/memory-manager.md`, Test Plan): every
contender runs the *same* workload — 1000 allocations of 40 bytes **plus
reclamation**. The arena pays its `reset`, bumpalo pays its `reset`,
malloc pays its per-object frees. Nobody measures allocation while
hiding the cleanup.

Per 1000 allocations of 40 bytes, with reclamation
(Rust 1.87, x86_64-pc-windows-msvc, dev box — figures noisy, treat as
orders of magnitude):

| Contender | Time / 1000 | Per alloc |
|---|---|---|
| **arena + `reserve`** (compiler batch hint) | ~0.65 µs | ~0.65 ns |
| **arena** (plain) | ~0.8 µs | ~0.8 ns |
| bumpalo (best Rust bump allocator) | ~1.3 µs | ~1.3 ns |
| system malloc (`Box`) | ~44 µs | ~44 ns |

Reading: our arena beats `bumpalo` roughly 2× (the `reserve` hint hoists
the limit check out of the loop), and both bump allocators are ~50–65×
faster than a general-purpose allocator doing real per-object frees —
which is the whole point of the request-arena design, not a trick of the
benchmark. The malloc column is honest: its cost is the per-object free
that the arena replaces with one `reset`.

## LLVM IR export

The runtime's hot paths must inline into compiled PHP code
([rfc/runtime/implementation-language.md](https://github.com/limelight-lang/rfc/blob/main/runtime/implementation-language.md)).
The crate is built with `codegen-units = 1` in release, so it emits one
clean LLVM module:

```sh
cargo rustc --release --lib -- --emit=llvm-ir,llvm-bc
# -> target/release/deps/ll_model-*.{ll,bc}
```

The emitted module contains only this crate's functions (no std baggage
as long as the hot paths stay dependency-free). Merging with
compiler-generated IR:

```sh
llvm-link php_generated.ll ll_model-*.bc -o combined.bc
opt -O2 combined.bc -o final.bc
```

### Verified in practice (Rust 1.87 / LLVM 20.1, x86_64-pc-windows-msvc)

- `ll_retain` / `ll_release` emit as small, clean IR functions
  (the whole module: ~60 lines).
- `llvm-link` merges the crate's bitcode with hand-written
  "PHP-compiler-style" IR without issues.
- After `opt -O2`, `ll_retain`'s body **fully inlines** into the calling
  function — cross-language unified code works.
- **Gotcha**: the inliner refuses to inline a callee whose
  `target-features` are not a subset of the caller's. Rust emits
  `"target-features"="+cx16,+sse3,+sahf"` (baseline x86-64); generated
  PHP IR must carry matching `target-cpu` / `target-features`
  attributes on its functions, or nothing from the runtime will ever
  inline.
- The LLVM tools of the exact right version ship with
  `rustup component add llvm-tools` — no separate LLVM install needed
  for this check.

## Layout

- `src/refcount.rs` — common refcounted header (`RcHeader`),
  retain/release fast paths, memory-category and flag bits
  (per `rfc/model/classes.md`, `rfc/model/values.md`).

Module naming convention: **full, readable words** (`refcount`, not
`rc`) — abbreviations only where they are the established term of the
domain (`gc` is fine, everyone reads it as garbage collector).
