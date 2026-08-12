# Workflow

How work is done in this crate. Not how the code is built
(`ARCHITECTURE.md`, still unwritten), not what was decided
(`DECISIONS.md`) — the routine that is the same for every task.

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
cargo test --lib -- --test-threads=4      # three times (rc-walk, the default)
cargo test --lib --no-default-features -- --test-threads=4    # three times (rc-trace)
cargo build --release
cargo build --release --no-default-features
```

The three threaded runs are not ceremony: several defects here only
appear under contention, and one flake took three runs to surface.

The width is capped at 4 because the development box is shared with
interactive work (decision 2026-08-03). That weakens the gate: the two
flakes on record were found at 16 and at 32 threads (`POSTMORTEM.md`,
`heap.rs`, `buffer_arena.rs`), and a narrower run reaches those
interleavings less often. A wider run on a machine that can spare the
cores stays worth doing before a release; it is no longer the
per-commit gate.

Since 2026-08-04 there is a second build-time axis, `hash-folding`
(`src/hash/seed.rs`). It does not run as a full third leg of the gate: it
touches no header bit, no GC path and no threading, so the interleavings
the three threaded runs exist to find are the same on both sides of it.
What it does need is one run of its own, because the two arms take
different `cfg` branches and each carries a test the other does not:

```
LL_HASH_SEED=<any> cargo test --lib --features hash-folding -- --test-threads=4
```

Run it once, in the default GC configuration. Without `LL_HASH_SEED` it
does not fail, it does not compile: a folding build with no seed is
refused by a `const` assertion in `hash/seed.rs`, because
`cargo build --features hash-folding` runs no tests and an artifact from
such a build hashes identically to every other one.

Since 2026-08-08 there is a third build-time axis, `debug-journal`
(`src/journal/kinds.rs`), and it needs **more** than one run rather than
less. Without the feature the record sites are not compiled at all, so
their bodies are never name-resolved and a site that does not build is
invisible to every command above. With it they are on the allocation and
the death paths, which is where a site on a path §9.7 forbids shows up as
an abort rather than a failure. So both GC configurations run again with
it:

```
cargo test --lib --features debug-journal -- --test-threads=4
cargo test --lib --no-default-features --features debug-journal -- --test-threads=4
```

Three times each, for the reason the two above are run three times: what
this axis adds is a re-entry into the allocator from inside itself.

Both GC configurations run because GC strategy selection is a build-time
feature (the two collectors claim the same header bits — see the
feature's note in `Cargo.toml`). Since 2026-07-27 **rc-walk is the
default build**; rc-trace is `--no-default-features`. Both must be
green. Strategy-bound tests are gated
to their configuration (`cfg(not(feature = "rc-walk"))` on rc-trace
tests); that gating is selection, not muting — the default runs keep
executing them.

Run each test command **as its own command and read its result line**;
never pipe into a filter that can swallow a failure and let a commit
through on a red suite (that happened once — see `ll-next-todo`'s
flake note, 2026-07-27).

## Formatting

`rustfmt.toml` governs, and the tool decides — nothing here is formatted
by hand. The catch is that **`rustfmt` is not installed on the default
`stable` toolchain on this box**, so `cargo fmt` fails outright rather
than reporting a clean tree, and it is easy to read that failure as "no
formatting step exists here". It does:

```
cargo +1.94 fmt
cargo +1.94 fmt --check      # what to run before a commit
```

The crate went unformatted from `3dd2d2a` (which added `rustfmt.toml`)
until 2026-08-04 for exactly that reason. Re-formatting it was one
mechanical commit touching 23 files; keeping it formatted costs nothing,
and letting it drift again means the next such commit hides real changes
inside it.

## Bugs first

**A known bug is fixed before new work starts.** Not queued behind the
interesting task, not "after this lands" — first. That includes a
flaky test: a suite that fails sometimes is a suite nobody reads, and
the next real failure hides inside the noise everyone has learned to
re-run past.

It also includes bugs found in passing, in code the current task never
meant to touch. Silently working around one — a check that swallows it,
a hardcoded value, a comment noting it — is not allowed. If it is
genuinely unclear whether something *is* a bug, or whether fixing it
drags in a larger change, ask; do not decide it quietly in either
direction.

## Naming: write the word

A name is clear and states what the thing is. Short is good, but it is
the second goal, not the first — a name that is short by dropping
letters has bought nothing and sold the word.

**Abbreviations are avoided, effectively always.** `InterfaceEntry`,
not `IfaceEntry`; `interface_count`, not `iface_count`;
`declaration_index`, not `decl_idx`. Four saved characters cost every
future reader a small act of decoding, and there are many more readings
than writings.

The exceptions are names that are *already* the word in this domain,
where expanding them would be the unclear choice:

- established terms — `vtbl`, `itable`, `refcount`, `rc`, `gc`, `abi`,
  `tls`, `id`;
- what the thing is called outside our code — a C ABI symbol, a field
  mirroring a published structure, a term the RFC defines;
- conventional locals whose scope is a few lines — `i`, `n`, `ptr`,
  `cls`, `b` for a block in a five-line loop.

When in doubt, write the word. A long name in a hot function costs
nothing at runtime.

## Documentation follows the logic, in the same commit

**When behaviour changes, the documentation describing it changes with
it — in the same commit, not later.** "Later" does not happen; the
change ships, the comment stays, and it is now a lie that reads as
authority.

This is not a style preference here. This crate's comments are the
record: they carry invariants, measurements, and alternatives that were
tried and rejected. That is the codebase's most valuable asset, and it
is worth exactly as much as its accuracy. A stale comment is worse than
no comment, because a missing one prompts you to read the code while a
wrong one stops you.

Concretely, when a change lands, check:

- the doc comment on the function or type touched;
- the **module** doc, which usually states the contract that just moved;
- `docs/memory-manager.md`, which `memory/mod.rs` declares this module
  implements;
- `README.md`, if a number or claim it quotes has moved;
- `dev/` — a decision goes in `DECISIONS.md`, a measurement in
  `BENCHMARKS.md`, a new trap in `POSTMORTEM.md`, and `INDEX.md` gains
  a line for anything new to find.

Evidence this rule was earned, all found in one session: `free`'s doc
described a cold-tail split the code did not have; the module doc
promised automatic thread-exit reclamation that existed only on Windows;
`refill`'s doc described initializing a bitmap two lines above code
saying it allocates nothing; `README.md` quoted benchmark numbers its
own results file had superseded. Each was believed, and two of them
misdirected real work.

**When a document has drifted past patching, do not quietly rewrite
history.** Move it to `docs/history/<name>-<date>.md`, mark it
superseded at the top with what is known to be wrong in it, and write
the new one. The reasoning in an old design document usually outlives
its accuracy, and deleting it throws away the record of what was
considered.

## Comments: the contract in the code, the argument in a document

A comment carries the contract and the facts the code cannot state. The
argument behind it goes into a document: the measurement, the
alternative that was tried and refused, the protocol the code obeys. The
comment names that document's section and stops there.

**What stays in the code.** The contract at every declaration another
module can reach, which is what the caller gets, what each parameter
means, what is refused and what the caller must not do. The facts an
expression cannot carry: units, ownership, the invariant that holds
here, what a zero or a `None` means. The reason for a line that a reader
who does not know it would undo. And the cross-reference of the form
"change this, change that too", written by name.

**What leaves.** A retelling of the line below it. A second copy of an
argument that already stands in this file or in the module doc. A
closing sentence that lands the point instead of adding a fact. The
history of the change, which git already holds: "now", "used to",
"since the fix".

**Where the argument goes.** To `rfc/` when it is about the model or the
protocol, which is what the runtime guarantees, what a walker may
observe, how two threads agree; that design is normative and outlives
any rewrite of this crate. To `docs/` when it is about how this crate is
built, which is the layer, the data structure, the allocation path;
`docs/memory-manager.md` is the pattern, and `memory/mod.rs` declares it
normative for that module. A dated decision still goes to
`dev/DECISIONS.md` and a trap to `dev/POSTMORTEM.md`, but those are the
record rather than a target a comment points at.

**How a reference is written.** By file and named section:
`rfc/model/gc/rc-walk.md`, "Deferred physical release". Never by a
number that gets reissued or removed, which rules out a `PLAN.md` stage
(closed stages are deleted from that file), a dated `dev/` entry, a line
number, and an item number from a list that has since been rewritten.
When the section a comment needs does not exist yet, write it and give
it a name.

**The test before leaving a comment in place.** Cover it and read the
code. What you can no longer answer is what the comment is for; the rest
was a retelling.

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
    --target x86_64-unknown-linux-gnu --lib -- --test-threads=2
```

**Cap the threads and time the run from outside.** The harness defaults
to one thread per core and each Miri thread carries its own view of the
interpreted heap; on this box (7.8 GiB, 2 GiB swap) that default took the
machine down on 2026-08-09. Two threads held, at a load around 2. And the
`finished in …s` line is Miri's own clock rather than the wall's — two
runs over different trees reported 154.41 s and 154.59 s while each cost
tens of minutes — so a Miri run is timed by `time` or by the shell, and a
figure quoted from its output says nothing about how long it took.

**Run it in slices.** `array::` alone is about an hour at two threads,
which is longer than any foreground command should hold the box, and a
background run outlives the session that started it with nobody to stop
it. Take a submodule at a time — `array::entry`, `array::table`,
`array::element`, `array::entity` — each under a `timeout`.

Three things about the command itself are load-bearing:

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

## Loom

Loom explores the executions the C11 model permits, which is how an
ordering defect is exhibited on a box whose hardware reorders nothing.
One model exists, `src/array/version_bracket_model.rs`:

```
RUSTFLAGS="--cfg loom" cargo test --lib version_bracket
```

It is outside the commit gate, and the dependency is gated the same way
(`[target.'cfg(loom)'.dev-dependencies]`), so an ordinary build neither
resolves nor compiles it.

Two limits decide what a loom model is worth here. Loom replaces every
atomic, cell and thread with its own types, so code that allocates, holds
raw pointers or reaches thread-locals cannot run under it — a model is a
hand-written **copy of the protocol**, and it drifts from the code
silently unless someone keeps the two in step. And loom's own README
records gaps in its model, load buffering among them, so a green run is
weak evidence while a red one exhibits an execution. Write the model so
that the defective configurations stay pinned as `should_panic` tests:
that is the half of the run that proves something.

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
