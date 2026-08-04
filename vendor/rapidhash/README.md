# rapidhash

Upstream copy of the reference implementation, kept here so the Rust port
in `src/hash/rapidhash.rs` can be checked against it constant for
constant. Nothing
in the crate compiles this header; it is read by people and by the vector
generator (`vendor/rapidhash/generate_vectors.c`).

- Source: <https://github.com/Nicoshev/rapidhash>
- Version: rapidhash V3 (the string in the file's own banner)
- Commit: `e04c9f35fa5a11c8c11de0f7cc1bdad38978d429`, authored 2025-09-29
- Fetched: 2026-08-04
- `sha256(rapidhash.h)`:
  `de0a6acd5e7901470f348c6de4634be076f39c4dc190e02e724eaff52040baac`
- Licence: MIT, `LICENSE` beside this file

`rapidhash.h` is unmodified. Replacing it means re-running the generator
and committing the new table — the port is defined by this file, not by
the port's own tests.

**That the file here is the one the table came from is checked**, by
`hash::tests::the_vendored_reference_is_the_one_the_table_came_from`,
which pins the crate's own hash of the header rather than the sha256
above (computing sha256 would mean carrying an implementation of it for
one assertion). The sha256 identifies the upstream file; the test's
digest identifies the local one. Updating the header means changing
both, plus `seed::FUNCTION_REVISION`.

## Why a copy and not a dependency

The hash is a **compiler/runtime contract**: `ll-model` computes a
string's hash at run time and the compiler will fold the same value for a
literal key. A divergence of one constant between the two produces no
crash — only lookups that miss. A pinned copy is what makes the two sides
comparable at all; a version range would let them drift apart between
builds.

## Test vectors

The author publishes no test vectors for V3: the repository holds
`rapidhash.h`, `secret.h`, `bench/`, `collisions/` and `old_version/`,
and nothing else. The vectors this crate tests against are therefore
generated from this header rather than quoted from upstream —
`generate_vectors.c` compiles it, hashes a fixed input list under fixed
seeds and prints the table that lives in `src/hash/vectors.rs`.

Running it needs a C compiler; running the crate's test suite does not.

```
cc -O2 -o /tmp/generate_vectors vendor/rapidhash/generate_vectors.c
/tmp/generate_vectors > src/hash/vectors.rs
```

## What is ported and what is not

`rapidhash_internal` — the general-purpose variant, with the header's own
defaults: `RAPIDHASH_COMPACT` (no unrolled 224-byte loop) and
`RAPIDHASH_FAST` (`rapid_mum` overwrites rather than xors). The
`rapidhashMicro` and `rapidhashNano` variants are not ported; they differ
from it only above 16 bytes, and choosing between them is a measurement
this crate has not made.

The port reads every multi-byte word little-endian on every target, which
is what the reference does — its big-endian arms byte-swap so that the
hash of a given input is one value everywhere.
