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

**Honest methodology**: every contender runs the *same* workload — N
allocations of 40 bytes, each **written to** (the header a real object
pays), then **reclaimed**. The arena pays `reset`, bumpalo pays `reset`,
malloc/mimalloc pay per-object frees. Nobody measures allocation over
untouched memory, and nobody hides the cleanup.

Per allocation, with reclamation (dev box, noisy — read as ratios):

| Contender | Per alloc | vs arena |
|---|---|---|
| **arena** | ~0.82 ns | 1.0× |
| **arena + `reserve`** | ~0.81 ns | ~1.0× |
| bumpalo (best Rust bump allocator) | ~1.26 ns | ~1.5× slower |
| mimalloc (fast malloc) | ~4.4 ns | ~5.4× slower |
| system malloc (OS default) | ~34 ns | ~41× slower |

The arena is ~1.5× faster than bumpalo and ~5.4× faster than mimalloc —
the honest fast-malloc rival, and the comparison worth quoting. Not the
"50×" an earlier, flawed benchmark suggested: that number came from
comparing against the OS allocator. Full data, caveats, and what these
numbers do and don't prove:
[`benches/RESULTS.md`](benches/RESULTS.md).

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
