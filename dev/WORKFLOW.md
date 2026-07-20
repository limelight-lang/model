# Workflow

How work is done in this crate. Not how the code is built
(`ARCHITECTURE.md`), not what was decided (`DECISIONS.md`) — the
routine that is the same for every task.

These rules are written down from established practice in this
repository, not invented. Where a rule was inferred rather than stated,
it is marked *(inferred)* — correct it rather than follow it blindly.

## Branches and merging

Work lands **directly on `main`**. No PR unless one is explicitly
asked for. There is no review gate, so the verification below is the
gate.

## Commits

Message format: `area: short summary`, then a body explaining **why**.
The body is the valuable part — this crate's history is used as a
record of rejected alternatives and measured trade-offs, not as a list
of edits.

One commit is one coherent change. Where two fixes are causally
dependent (the second cannot be observed until the first lands), they
go together rather than as a broken intermediate commit. *(inferred —
this one is reasoning from practice, not a stated rule)*

## Verification (required before every commit)

```
cargo test --lib
cargo test --lib -- --test-threads=16     # three times
cargo build --release
```

The three threaded runs are not ceremony: several defects here only
appear under contention, and one flake took three runs to surface.

## Tests

**Every fix needs a regression test verified to fail on the bug.**
Verify it by temporarily reverting the fix, confirming the test fails,
then restoring. A test that was never seen failing proves nothing.

For formal-UB fixes there is no such test under `cargo test` — the bug
passes it by construction. There the Miri run *is* the regression test,
and the same discipline applies: see it report the violation before the
fix, and see it silent after.

**Never mute, skip, weaken or delete an existing test to go green.** A
failing old test is a signal: either the change broke behaviour, or the
contract genuinely moved. Those cannot be told apart silently — ask.

The one sanctioned exception is an environment that physically cannot
express what the test checks, marked narrowly and still running
everywhere else, e.g. `#[cfg_attr(miri, ignore = "...")]` on the three
dispatch-table tests, which compare function identity that Miri does
not model. Assertions stay untouched.

## Miri

Miri is the only tool that can see the formal-UB class of defect here;
all of them pass a normal `cargo test`.

```
MIRIFLAGS="-Zmiri-ignore-leaks" cargo +nightly miri test \
    --target x86_64-unknown-linux-gnu --lib
```

Three things about that command are load-bearing:

- **UNIX target.** The Windows TLS fast path is inline `asm!`, which
  Miri cannot execute. Against a UNIX target the crate's existing
  portable `thread_local!` path is selected, so no source change is
  needed. The consequence: Miri does not exercise the Windows TLS path.
- **`-Zmiri-ignore-leaks`.** The thread heap, the immortal region and
  the block pool intentionally live until process exit. The cost:
  **Miri is blind to leaks here**, so leak-shaped defects need another
  method.
- The bench-only `mimalloc` dev-dependency is gated behind
  `cfg(not(miri))` in `Cargo.toml`, because it compiles C for the
  target and no cross toolchain exists on a Windows box.

Known limits: the crate's integer-to-pointer casts put Miri into
permissive provenance in the most pointer-heavy modules, so a clean run
is weaker evidence there than elsewhere. Tree Borrows cannot run at all
until those casts go away — it requires strict provenance.

**Tests that exercise reentrancy must keep one raw pointer per arena
and per context and reuse it**, which is the shape generated code
actually has. Taking a fresh `&mut` per call retags and invalidates the
pointer `set_current_context` parked in TLS, producing a failure that
is an artefact of the test, not of the runtime.

## Benchmarks

See `BENCHMARKS.md`. The short version: this crate is
measurement-driven, so no hot-path change lands on reasoning alone, and
both arms are measured back to back in one session.

## Files that must stay untracked

- `AUDIT.md` — the audit findings report. **This repository is
  public**; a list of unpatched defects must not be published. Nothing
  in `dev/` may reference an unfixed finding either. Fixed ones are
  fine, and are described in commit messages.
- `.idea/`

## Related repository

Design lives in `limelight-lang/rfc` (`e:/limelight/rfc`) and is kept
in sync when behaviour changes — e.g. `model/memory/arenas.md`,
`model/memory/arena-reset.md`, `model/gc/strategies.md`,
`runtime/object-lifecycle.md`.
