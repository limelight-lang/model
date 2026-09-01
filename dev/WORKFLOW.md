# Workflow

How work is done in this crate. Not how the code is built
(`ARCHITECTURE.md`), not what was decided
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
cargo test --lib -- --test-threads=4      # three times
cargo build --release
cargo bench --no-run
```

**One configuration since 2026-08-26.** The GC axis went with the two
collectors: there is no `rc-walk` feature and no `rc-trace` default, so
the matrix below has lost its second leg. `cargo bench
--no-run` joined the gate at the same time, because `cargo test --lib`
builds no bench target and `benches/lifecycle.rs` imports the GC ABI —
without it a deleted export is found by a release, not by a commit.

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
different `cfg` branches and each carries a test the other does not.
The two are `hash::seed::tests::where_the_seed_comes_from::`
`the_process_seed_comes_from_the_operating_system` off and
`a_folding_build_hashes_under_its_own_seed` on, so the arms list the
same number of tests and a count tells them apart in neither direction
(diffed byte for byte on 2026-08-26; both arms list 576 on 2026-09-01):

```
LL_HASH_SEED=<any> cargo test --lib --features hash-folding -- --test-threads=4
```

Run it once. Without `LL_HASH_SEED` it
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
an abort rather than a failure. So the suite runs again with it:

```
cargo test --lib --features debug-journal -- --test-threads=4
```

Three times, for the reason the run above is run three times: what this
axis adds is a re-entry into the allocator from inside itself.

Run each test command **as its own command and read its result line**;
never pipe into a filter that can swallow a failure and let a commit
through on a red suite. That happened once, on 2026-07-27, and again on
2026-08-29 through a `grep "^test result"` in a shell chain
(`dev/POSTMORTEM.md`).

## Formatting

`rustfmt.toml` governs, and the tool decides — nothing here is formatted
by hand. **The authority is `+1.94`:**

```
cargo +1.94 fmt
cargo +1.94 fmt --check      # what to run before a commit
```

One toolchain is named because two can disagree on a tree neither has
seen. `stable` carries rustfmt 1.9.0 as of 2026-09-01 and reports this
tree clean, so a `cargo fmt` without the toolchain is not the failure it
once was — it is a second opinion, and the gate takes the pinned one.

The crate went unformatted from `3dd2d2a` (which added `rustfmt.toml`)
until 2026-08-04, when `stable` had no rustfmt at all and the failure
read as "no formatting step exists here". Re-formatting was one
mechanical commit touching 23 files; letting it drift again means the
next such commit hides real changes inside it.

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
`dev/DECISIONS.md` and a trap to `dev/POSTMORTEM.md`, and code may cite
either by the entry's title rather than by its date.

**How a reference is written.** By file and named section:
`rfc/model/gc/rc-cycle.md`, "Cycle teardown". Never by a
number that gets reissued, which rules out a line number, a dated `dev/`
entry, and an item number from a list that has since been rewritten.
When the section a comment needs does not exist yet, write it and give
it a name.

**How a debt is written.** A comment that states a capability is absent
names the `PLAN.md` step that builds it: "buffer chunks do not yet park for a
worker trace; S38.3 builds that window". Build order is in the plan and
nowhere else, and a stage number is never reissued, so a stale citation
dangles rather than misleads — the recoverable failure of the two. The
number is a pointer and not the content: the sentence says what is
absent and what the step builds, so it stays readable when the number
goes dead. A stage is never cited as history — what a closed stage did
belongs to git, or to a journal entry cited by its title.

When a stage's section leaves `PLAN.md`, or a debt it carries moves to
another stage, **the same commit sweeps the number**: the capability
exists now, so every comment naming it states the contract that replaced
the absence, or goes. Unlike the four passes below, this one a grep can
make end to end — pull every `S[0-9]+` token out of `src/`, `benches/`,
`docs/`, `dev/INDEX.md`, `dev/ARCHITECTURE.md` and this file, and resolve
each against the `##` sections still in `PLAN.md`; an unresolved number is
debris. `docs/` joined that list on 2026-08-27, when a sweep run from a
stage that had added citations of its own found seven dangling sites
there — four dead numbers across four documents — and none anywhere else:
the earlier runs swept the code and the maps and left the layer documents,
which carry the same kind of forward claim. Two of the seven cited a
closed stage as history, which the rule below forbids outright, so they
went to the journal entry holding the fact rather than to a live number;
the other five named a stage the plan had deleted that week. The maps are swept with the code because they carry the same
kind of forward claim; `dev/DECISIONS.md`, `dev/POSTMORTEM.md` and
`dev/BENCHMARKS.md` are not, an entry there naming the stage of its own
day being a record rather than a pointer. Grep the bare number, because
punctuation hides one: `(S36.2)`, `S36-2`, `marked S36.2`.

**An `S<n>` in this tree is this plan's, and a debt the `rfc` plan owns is
never written as one.** The two plans number independently and both delete
closed stages, so `rfc`'s live S8 and this plan's deleted S8 are the same
token; a sweep run here reads the citation as debris and the next one deletes
it. A debt that belongs over there is cited the way any other cross-repository
fact is — by the `rfc` document and its named section, which is where the
question stands anyway. Found on 2026-08-27, when S34.1's comments named
`rfc/dev/PLAN.md` S8.5 in four places and the sweep reported a dead stage.

An
`#[expect(dead_code, reason = "…S36.2")]` is the self-reporting form of
the same debt — the attribute goes unfulfilled the moment the caller
arrives.

Why the ban on a `PLAN.md` stage was lifted rather than enforced:
`dev/DECISIONS.md`, "a comment names the plan step that owes it, and the
stage's deletion sweeps the number".

A **bolded lead-in** counts as a named section, quoted exactly:
`rfc/model/gc/rc-cycle.md`, "Where the shadow count lives". These
documents state most
of their rules that way, and a citation of the enclosing heading instead
sends the reader to a page rather than to the sentence. What decides the
form is which one a search finds — so the rule that follows from it is
that a lead-in cited from code is never reworded in the document without
the citation moving with it.

**The test before leaving a comment in place.** Cover it and read the
code. What you can no longer answer is what the comment is for; the rest
was a retelling.

## Checks a grep cannot make

A deletion leaves debris a name search does not find, because the debris no
longer carries the name. Four passes found what a grep for `rc-walk|rc-trace`
missed when the two collectors went (2026-08-26), and each is cheap enough to
re-run after any deletion of a module, a document or a feature:

1. **Every document a comment cites, resolved against the tree it names.**
   Pull each `` `rfc/…md` `` out of `src/`, `benches/`, `docs/` and `dev/` and
   test the file exists. Three of the four dead documents this found —
   `retained-block-walk.md`, `walk/questions.md`, `gc-horizon-v2/questions.md`
   — do not contain the deleted strategy's name at all. `dev/tools/linkcheck.php`
   in `rfc` does not cover this: it reads only `rfc`, and only bracketed links.

   **What a hit means depends on where it stands.** A dated journal —
   `DECISIONS.md`, `BENCHMARKS.md`, `POSTMORTEM.md` — names the document of
   its own day, and so does a note under a deletion banner. Those hits are
   records and stay; a hit anywhere else is debris. That is the distinction the stage
   sweep above draws, and it is drawn for the same reason.
2. **Every module path a comment cites, resolved against the modules that
   exist.** `` `walk::` ``, `` `collector::` ``, `` `epoch::` ``. A rename
   leaves nine of these behind and no build reports one, since a comment is
   not code. An intra-doc link (`[`crate::epoch`]`) is the exception —
   `cargo doc` does report it — so run that too.
3. **Orphan files:** every `.rs` under `src/` that no `mod` declares. A test
   file whose `mod` line was removed compiles nowhere and still reads as a
   live test.
4. **The upward edges, re-enumerated rather than patched.** Resolve every
   `crate::…` path in production code against `dev/ARCHITECTURE.md`'s layer
   map. Patching that table by hand removes the edges you remember and leaves
   the ones you do not: the 2026-08-26 run found three edges into `cells` the
   table had never listed, and one listed edge that was doc links with no call
   behind them.

On 2026-09-01 pass 3 returned empty, pass 1 returned seven hits — every one
inside a dated journal or a deletion banner, which is where a pointer at a
deleted document belongs — and pass 2 returned one: an unresolved intra-doc
link at `src/cells.rs`. A count is not the check; the question is which sites,
and the passes name them.

## Tests

**Every fix needs a regression test verified to fail on the bug.**
Verify it by temporarily reverting the fix, confirming the test fails,
then restoring. A test that was never seen failing proves nothing.

For formal-UB fixes there is no such test under `cargo test` — the bug
passes it by construction. There the Miri run *is* the regression test,
and the same discipline applies: see it report the violation before the
fix, and see it silent after.

**A door that names one representation is found by flipping the
factory's default.** Where two representations sit under one tag — the
array's vector and ordered hash today, `Map` and the typed vector later —
the suite stays green while a door reaches for one of them by name,
because the factory hands out the other. Flip the stamp, run the suite,
read the failures as the inventory of such doors, and revert the flip
until the step that owns it. What this found, and why no static
check reports it, is `dev/DECISIONS.md`, "flipping the factory's stamp is
how a representation-blind door is found".

**Run it on both sides of a change that moves the tag.** A flip finds
only what the tag it flips discriminates: before the string's layout
became a kind code, the flip exercised no kind dispatch at all, because
every string still carried one code. Running it again after the fold is
what drove the second code through the death switch, the COW separation,
the escape copy and the reset's traceability test.

**A flip can abort rather than fail.** A test that writes at a fixed
offset writes into a different field in the other representation, and a
corrupted pointer reaching a free aborts the process — which hides every
failure after it, so the inventory reads as short rather than as
truncated. Read the tail of the run for `SIGABRT` before trusting the
list, skip the offending test, and run again.

**A flake that appears only under load is reproduced by making the
load.** Build the test binary with `--no-run`, pin it to two cores with
`taskset -c 0,1` at `--test-threads 4`, and run two spinners on the same
cores. The census flake of 2026-08-06 failed 3 in 30, 7 in 40, 6 in 40
and 9 in 40 that way, and 0 in 60 after the fix under the same load
(`dev/POSTMORTEM.md`, "an entity killed at refcount 1").

**Never mute, skip, weaken or delete an existing test to go green.** A
failing old test is a signal: either the change broke behaviour, or the
contract genuinely moved. Those cannot be told apart silently — ask.

The one sanctioned exception is an environment that physically cannot
express what the test checks, marked narrowly and still running
everywhere else: `#[cfg_attr(miri, ignore = "...")]` stands on eleven
tests today — six dispatch-table ones comparing function identity, which
Miri does not model, four that read a source file, and one that spawns a
child process, both of which its isolation refuses. Assertions stay
untouched.

**A test that spawns a process or reads a file carries that attribute in
the commit that adds it.** Miri stops at the first error, so such a test
does not merely skip itself — every test after it in the run is never
executed, and the run reports a failure whose cause looks like the
subject rather than the harness (`dev/POSTMORTEM.md`, 2026-09-01).

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

**What a whole-suite run costs.** No figure for the single configuration
has been taken. What is measured is `cycle::` on 2026-09-01: 86 passed, 0
failed, 1 ignored, 387 s on Miri's clock against 10 m 33 s of wall. The
two-configuration figures of 2026-08-08 went with the configurations
themselves. One test dominating
a run has taken it from a quarter of an hour to 28 minutes on its own
(`dev/POSTMORTEM.md`, 2026-08-08). A stage therefore closes on a targeted
run over the modules it touched, and says which ones; whether a
whole-suite run belongs at every stage or only before a release is
Edmond's, and open.

**Run it in slices.** `array::` alone is about an hour at two threads,
which is longer than any foreground command should hold the box, and a
background run outlives the session that started it with nobody to stop
it. Take a submodule at a time — `array::entry`, `array::table`,
`array::element`, `array::entity` — each under a `timeout`.

**The region carve has an arm of its own, and it is why Miri runs at
all.** `memory::os::map_aligned` cuts an aligned span out of an oversized
mapping by unmapping the head and the tail, which POSIX allows and Miri's
`munmap` shim does not model — it reports "incorrect layout on
deallocation" and ends the run. Since the first `BlockPool::get` of any
test carves a region, every test that allocates was unrunnable under Miri
from 2026-08-26, when the pool started taking its memory from the
operating system, until 2026-08-29. Under `cfg(miri)` the trim does not
happen; a table in `os.rs` remembers where each untrimmed mapping starts,
and `unmap` hands back the whole of it, which is the exact-layout
deallocation the shim does accept.

**What that costs, stated rather than assumed: an apron.** The live
mapping is `bytes + align` where the crate's object is `bytes`, so every
region and every large run is flanked by up to 64 KiB of mapped, readable
memory that does not belong to it. An access just past the end of a
region — an off-by-one in a block walk, a header written one stride too
far — lands outside a mapping on Linux and inside a live allocation under
Miri, so the run comes back clean. Miri remains the instrument for edge
overruns *inside* a region and is not one for overruns *past* it.

**Why the table is there rather than a no-op unmap:** with a no-op, an
unmap would stop being an unmap and every question about returned memory
would leave Miri's view. With it, `munmap` runs on the whole mapping and
the shim accepts it — which the `memory::large_entity` run of 2026-08-29
shows, five tests through the unmap path with neither an "incorrect
layout" report nor the panic `unmap` raises when the table has no entry
for a pointer.

**Three tests claim Miri as their whole regression, and whether any of
them still exhibits its defect is unverified.**
`promote::tests::the_reset_reads_no_corpse`'s
`a_large_survivor_killed_by_the_drain_is_not_read_by_the_reconcile` and
its two neighbours guard the reset window against reading a large run
after it was unmapped, and their doc comments say `cargo test` passes the
defect by construction. They run under Miri again since 2026-08-26: on 2026-08-29 the reconcile one was
run under Miri with `reset_window::park_large` returning false — a build
whose window parks nothing, which its neighbour's comment names as the
condition — and it passed, in 176 s. Either the mutation is not the one
those comments mean, or a second half of the arrangement is missing.
Re-arming them is nobody's step yet.

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

**A test keeps one raw pointer per object and reuses it**, which is the
shape generated code actually has. Taking a fresh `&mut` per call retags
and invalidates every raw pointer taken before it, producing a failure
that is an artefact of the test rather than of the runtime.

Reentrancy is where this was first paid — a fresh `&mut` per call
invalidates the pointer `set_current_context` parked in TLS — but the rule
is not about reentrancy. On 2026-08-27 it caught S34.1's queue tests,
which took `&raw mut header`, released through `&mut header`, and then
read the flags back through the first pointer: three lines, one local, no
TLS and no arena. **Anything a test holds a raw pointer to is released,
retained and read through that pointer**, never through a borrow of the
binding beside it.

## ThreadSanitizer

ThreadSanitizer sees the one class Miri cannot reach here: a plain field
read beside the collector's atomic store into the same header. Miri sees
the mixed-size access and not the plain-against-atomic race; TSan is the
other way round.

```
RUSTFLAGS="-Zsanitizer=thread" cargo +nightly test -Zbuild-std \
    --lib --target x86_64-unknown-linux-gnu -- --test-threads=1 a_free_running
```

**`-Zbuild-std` is not optional.** Without it the build fails outright on an
ABI mismatch — the flag changes the ABI, and `core` in the prebuilt sysroot
was compiled without it. `rust-src` must be installed for the nightly
toolchain.

What a report looks like, from the run that validated this on 2026-08-15:
the write side is `atomic_store::<u8>` (`collector_stamp_epoch`) and the
read side is `RcHeader::memory_category` under `object::object_constructed`,
which is the defect `6e5d137` fixed, put back for the occasion. Restoring the
fix returned the run to silence. Both halves are the discipline this file
asks of every fix: see the instrument report it before, and be silent after.

Two things about the run itself. A clean run of that test takes under a
second, and a run that reports takes a minute and a half — nearly all of it
symbolization, so a slow run means a finding rather than a slow test. And the
window is thin: the mutator churns only while four epochs run, so a clean run
is weak evidence and a report is strong evidence, the same asymmetry loom has.

It is outside the commit gate. Run it when a change touches header access,
the collector's own writes, or anything else two threads reach.

**Since 2026-08-26 the run has no test.** `a_free_running_mutator_survives_`
`concurrent_epochs` lived in `collector::`, which went with `rc-walk` and
`rc-trace`, so the command above now selects nothing and reports silence for
that reason rather than for the good one. Nothing in the crate pairs a live
collector with a mutator today. The debt belongs to `PLAN.md` S38.0, which is
where the second thread arrives; until it lands, a change to header access is
verified by reading the sources, by
`refcount::tests::who_may_read_a_header`, and by Miri, which sees the
mixed-size access but not the plain-against-atomic race.

## Loom

Loom explores the executions the C11 model permits, which is how an
ordering defect is exhibited on a box whose hardware reorders nothing.
Two models exist, four cases each — `src/array/version_bracket_model.rs`
for the array table's version bracket and `src/journal/ring_model.rs` for
the journal ring's, which is the same bracket read the other way round:

```
RUSTFLAGS="--cfg loom" cargo test --lib version_bracket
RUSTFLAGS="--cfg loom" cargo test --lib ring_bracket
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
  public**; a list of unpatched exploitable defects must not be
  published, and nothing in `dev/` may reference an unfixed audit
  finding. Fixed ones are fine, and are described in commit messages.
  The boundary (ruled 2026-08-18, over `dev/RC_WALK_CRITICAL_REVIEW.md`):
  an architectural debt the code already names in its own comments — a
  missing driver, an unbounded backlog — may be tracked and cited; what
  stays out is any finding the code does not admit to openly.
- `.idea/`

## Related repository

Design lives in `limelight-lang/rfc` (`/home/edmond/limelight/rfc`) and is kept
in sync when behaviour changes — e.g. `model/memory/arenas.md`,
`model/memory/arena-reset.md`, `model/gc/strategies.md`,
`runtime/object-lifecycle.md`.
