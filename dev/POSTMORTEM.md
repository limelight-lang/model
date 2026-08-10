# Postmortem

Serious mistakes only: ones that cost real time, broke something that
worked, sent work down a false path, or happened twice. An entry
without a root cause is useless, so every entry states why the mistake
was possible and why it was not caught.

---

## 2026-08-10 — an atomic field does not survive a `&mut` over the struct

**What happened.** `Heap::alloc` takes `&mut (*block).private` on every
allocation, and the block's `kind` — which the collector reads for every
block of every region — was the first field of that struct. The repair
ruled on was to give the word an atomic type, on the stated ground that a
retag does not descend into an `UnsafeCell`. The type landed, every
`.kind` in the crate moved with it, and Miri reported the same race at
the same line. The second repair was the real one and the crate had
already made it once: the word left the struct the borrow covers.

**Why it was possible.** The interior-mutability exemption is real and it
belongs to **shared** references. A `&mut` asserts uniqueness over its
whole range whatever is inside it, so an `UnsafeCell` buys nothing there.
Both halves of that sentence are true and only one of them was recalled.

**Why it was not caught earlier.** The premise was checked against the
rule as remembered rather than against the tool: the fix compiled, the
suite was green, and the tree looked finished. What caught it was running
Miri again after the change instead of assuming the change had done its
job — the same discipline `dev/WORKFLOW.md` states for formal-UB fixes,
see the violation before and the silence after. The silence is the half
that is easy to skip.

**The rule.** A word two threads touch does not stay inside a struct
somebody borrows exclusively, whatever its type. `dev/DECISIONS.md`,
2026-07-20 already said this for `BlockShared` and `BlockRemote`: making
it a type rule was the only option that cannot be violated again. The
atomic type is still right — it says what the word is, and it is what
makes the write side legal — but it is not what makes the borrow legal.

---

## 2026-08-04 — an assertion about a global flag, read twice, from two threads

**What happened.** `cargo test --lib -- --test-threads=4` aborted on
about one run in twelve — five times in sixty, measured — always with
the same signature: `flush runs only between epochs`, from
`deferred_free::flush`, inside
`collector::tests::a_free_running_mutator_survives_concurrent_epochs`.
A `debug_assert` failing in a function that cannot unwind aborts the
process, so the whole suite died and took its passing tests with it.

**Root cause.** Check-then-act across threads. `flush_due()` reads the
process-global activity bit and returns; the caller then calls
`flush()`, which asserted the same bit was still clear. Between the two
reads the collector thread runs `Epoch::open`, whose first statement
raises that bit. The assertion states an invariant that holds at the
moment of the check and is not owed for the length of the call — and
nothing in the protocol ever promised it would be.

**What was decisive.** A temporary probe rather than more reading: one
thread flipping the bit, another performing the same two reads, counting
the disagreements. 1,194,869 of 10,188,918 checks — one in nine. That
turned "narrow race, probably" into a measurement, and it took a minute.

**Why it was not caught.** The window is real but the path into it is
rare: it needs parked memory, a checkpoint, and an epoch opening in the
same breath. Nothing but the free-running stress test produces all
three, and it is the newest test in the crate. The assertion had also
been true of every single-threaded caller since it was written.

**The fix, and the shape of it.** `flush` now returns zero and leaves
the backlog alone when the bit is set, because an epoch that opened in
that window has not been acked by this thread — `Epoch::open` raises the
bit before requesting the handshake, and the snapshot waits for the ack
— so nothing has read those slots and the backlog is free to wait for
the next checkpoint. The regression test is deterministic: park, open an
epoch under the call, assert nothing was recycled.

**The general lesson.** An assertion is a claim about an invariant, and
an invariant over shared mutable state has to name the window it holds
in. `debug_assert!(!active())` inside a function whose caller already
checked `active()` is not a second opinion — it is the same read, taken
later, with a race in between.

## 2026-07-21 — a test oracle read global state and blamed the runtime

**What happened.** `many_threads_freeing_into_one_owner_lose_no_slots`
failed about one run in twenty at `--test-threads=32`, and never in
thirty runs alone. Its assertion says "the owner lost track of a slot
freed from another thread" — a lost cross-thread free, the worst defect
this allocator could have. Half a session went into reading the MPSC
push and drain paths looking for the race.

There was no race. The oracle summed `used` over every block the heap
owns, and some of those blocks were **adopted**. A block reaches the
abandoned list precisely because it still holds live objects when its
thread exits, so adoption hands a heap live slots belonging to a thread
that is gone. The oracle read another test's leftovers as this test's
lost frees.

**Root cause.** The oracle was written to replace `blocks_out`, whose
stated problem was exactly this — "it is shared, so another test's
block returning late moves it in either direction". The replacement
counted a different global and inherited the same fault. **A test that
reads process-global runtime state has to say which part of it is
attributable to the test**, and neither instrument did.

**Why it was not caught.** It passes alone, in every ordering the
default thread count produces, and its failure names a plausible real
bug in the most suspicious subsystem in the crate. A flake that accuses
something real is worse than one that looks like noise: it sends the
reader into the code it names.

**What was actually decisive.** Not reading the code — instrumenting
it. One temporary print in `adopt` showed blocks arriving with `used`
of 1, 3 and 147, and the failing assertion reporting exactly the
inherited count. The reasoning pass before it had produced four wrong
hypotheses about the CAS loop.

**Rule this leaves.** When a concurrency test fails intermittently,
establish *what it counts* before investigating *what it accuses*.

---

## 2026-07-20 (second) — the fixed benchmark rule was still not enough

**What happened.** With the stale-baseline rule from the entry below now
in force, four consecutive attempts to measure H11 each produced a
confident, statistically significant, wrong answer. criterion reported
`p = 0.00` and `Performance has improved` on runs that were measuring
the machine.

The failures were each of a different kind, and each defence only
caught the previous kind:

1. Load arrived mid-session, so both arms were in one sitting — the
   rule below was satisfied and did not help. Caught only by noticing
   the absolutes were far outside their historical band.
2. The `git stash` between arms changed file mtimes, so `cargo bench`
   recompiled and every measurement began on a machine still busy from
   its own build. This produced a *monotonic* improvement across three
   arms — the shape of a machine recovering, not of a code difference.
3. Running pre-built binaries directly without `--bench` made criterion
   smoke-test each benchmark once and print `Success`. A silent no-op
   that looks like a completed run.
4. By the last attempt the box was 40–80% outside its bands with the
   two control measurements of identical code 10% apart — unmeasurable,
   most likely thermal after hours of compiles and Miri runs.

**Root cause.** The first entry fixed *one* way a comparison can be
invalid and I treated the problem as solved. The general fault is
different: **a benchmark has no built-in way to say "I was not valid",
so validity has to be measured too, not assumed.** A p-value describes
whether a difference is random; it says nothing about whether the
difference came from the code.

Contributing: I stated a mechanism for the intermediate result
(`relink_unfull` is hot in `rptest`, so marking it cold deoptimizes it)
that was plausible and might even be true. Plausibility is what stopped
the checking — the same failure as in the entry below, which is why this
is a repeat and not a new lesson.

**What changed so it cannot repeat.** `BENCHMARKS.md` now carries three
independent validity checks rather than one rule: control repetition
(A → B → A, run void if the A's disagree), absolute values checked
against a recorded per-benchmark band before any delta is read, and
both arms built before either is measured. Plus the `--bench` trap and
a stated condition for declaring the box unmeasurable and stopping.

The H11 change was **not committed on that evidence**, because the rule
it would have violated is the one this crate exists to keep. It landed
later, on a valid measurement that showed no difference outside the
noise floor, and was kept for the shape of the code rather than for a
number (`1824392`; H11 in `BENCHMARKS.md`).

---

## 2026-07-20 — benchmarked against a stale baseline, believed the numbers

**What happened.** While measuring the block-header split, three
comparisons were run against a criterion baseline captured earlier in
the same session. They reported, in order: `+10.8%` on rptest, then
`+2.4%`, then `+51.8%` on larson and `+56.0%` on rptest simultaneously.

The last pair is what exposed the problem. Two benchmarks that stress
different things — one multi-threaded worker churn, one single-threaded
block churn — degraded by nearly the same amount, from a change that
moved two pointer fields within a cache line. No physical mechanism
produces that. Re-measuring the identical build back to back against a
fresh baseline gave `−3.03%` and `−1.54%`.

**Root cause.** A saved criterion baseline records *numbers*, not the
*conditions* they were taken under. On a dev box with an IDE and
background work, machine state drifts within minutes, so every
comparison against an older baseline silently reports
`code change + machine drift` as if it were the code change alone. The
mistake was treating a stored baseline as a controlled A/B when it is
only a stored number.

Two things let it through:

- No sanity check on the *shape* of the result. A large, uniform change
  across unrelated benchmarks is a machine artefact by construction,
  and that should have been the first question asked, before any
  explanation of the mechanism was attempted.
- The intermediate figures were plausible. They agreed with a
  reasonable cache-line argument, and agreeing with theory made them
  feel confirmed rather than untested. A number that matches a
  prediction is the easiest kind to stop questioning.

**What it cost.** About an hour, and one wrong intermediate conclusion
— that the first layout cost 10.8% on rptest — that drove an extra
redesign round. The final layout is genuinely better and is measured,
but the two rejected variants were never verified properly, so only
their direction can be claimed.

It also came close to doing lasting damage: those unverified
percentages had already been written into code comments as measured
facts. In a crate whose comments are used as a record of rejected
alternatives, a false number would have been trusted by whoever read it
next. They were removed before the change landed.

**What changed so it cannot repeat.**

- `BENCHMARKS.md` now specifies the method: both arms measured **back
  to back in one session** against a freshly taken baseline, with the
  exact `git stash` / checkout sequence.
- A stated noise signature: a large change of similar magnitude across
  benchmarks that stress different things is machine noise, not a
  result, and a 1–2% difference on this box is near the resolution
  limit and must be repeated before it is believed.
- A rule that per-variant figures may not be written into code comments
  unless they came from a back-to-back run. Direction is safe to
  record; an unreproducible number is worse than none.

---

## 2026-08-06 — an entity killed at refcount 1

**What happened.** `walk::tests::census_counts_objects_and_their_edges`
failed at roughly 5 in 30 under load, and stayed unexplained for a
session. The cause was two array tests calling `ll_entity_die` on
entities whose refcount was still 1. The slot reaches the free list
carrying its old header, and that word is the occupancy test both
process-global enumerators apply, so every later census in the process
read those freed slots as live entities — until the allocator handed
them back out, at which point the count stopped growing and an
unrelated test on another thread failed.

**Why it took so long.** The failure has no local symptom. The test
that commits the mistake passes, on every run, in both configurations;
what fails is a different test, in a different file, on a different
thread, after enough allocation to reuse the slot. The first diagnosis
went the other way round — a live entity leaving the walk — because a
count that fails to grow looks the same from the outside whichever end
the error is at, and the walk is the side with a documented quiescence
requirement it does not get in a parallel suite.

The measurement that settled it was cheap and could have been made on
day one: record the *addresses* both censuses yield, not only the
totals, and print the header word each census read at an address the
two disagree about. The two sets turned out to be identical, which
killed the entire "an entity left the walk" family of hypotheses in one
reading.

**What changed so it cannot repeat.**

- `stdapi::ll_free` asserts in test builds that an entity slot arrives
  with a refcount-0 header. Killing at 1 is now a failure in the test
  that does it, at the moment it does it.
- The census test keeps its drift report: on a mismatch it names the
  addresses that came and went and the block state behind each
  (`heap::describe_slot`).
- For an object the defect is worse than an over-count, and that is the
  part to remember: the free-list link is written at bytes 8-15, where
  the class pointer was, so a walk that believes such a slot follows a
  free-list link as a `*const Class`.

## 2026-08-08 — a test that dominates the Miri run stops the Miri run

`a_deep_array_tears_down_without_the_machine_stack` built and tore down
20 000 nested arrays. Natively it costs 0.04 s, which is why the number
looked free. Under Miri it had not finished after eighteen minutes, and
three whole-suite runs launched against the same commit were all killed
by their timeout — one of them after the commit message had already said
Miri was running against it.

**What it costs.** Miri is the only tool that sees the formal-UB class of
defect in this crate (`dev/WORKFLOW.md`), and its whole-suite run is how
a stage's verification closes. A single test three orders of magnitude
above the others does not fail the gate; it makes the gate not finish,
which reads exactly like nobody having run it. The stage-end review found
it by measuring; the killed runs were dismissed as environment until
then.

**The depth was also larger than its own argument.** The test's comment
justified 20 000 levels by a per-level stack budget, but the drain spends
a fixed frame and one list entry: what the test demonstrates is a total,
not a margin per level. 2 000 levels on a 64 KiB stack proves the same
thing and was seen aborting the process with the list forced to refuse.

**What changed so it cannot repeat.** The depth is 2 000, the stack is
64 KiB, and the test carries `#[cfg_attr(miri, ignore = "…")]` with the
reason and with what covers the same code under Miri instead. Before
quoting a Miri run in a commit message, read the log to its result line —
a killed run leaves an empty file, and an empty file is not a green one.

## 2026-08-09 — a private teardown for an entity the barrier had published

`element::box_element` allocates the reference box, fills it, publishes
it through `store_category_barrier` and only then inserts it into the
entry. The refusal arm after that insert tore the box down with
`destroy_unpublished`, whose contract is "an entity at count one that no
slot has ever named": it releases and calls `ll_entity_die`
unconditionally. For an arena array the publication had already written
a release-at-reset record naming that box, so the arm freed a cell the
reset would release again.

**What made it invisible.** The arm is unreachable — `Table::insert`
returns before it can allocate when the key is present, which is the only
way `box_element` reaches it — and it carries `debug_assert!(false)`
saying so. A branch nobody runs is a branch nobody reads either, and the
teardown it called was the correct one for the *other* refusal arm twenty
lines above, where the box is genuinely unpublished. The two arms differ
by one call that happens between them.

**The rule the arm broke.** A publication is undone by `drop_ref`, never
by a release: `drop_ref` mirrors the category barrier — it skips the
release for a heap entity displaced from an arena container, because the
log record owns that release — and any private teardown re-derives that
mirror and gets it wrong. The repair is one call.

**What to check when writing one.** Ask what has happened to the entity
between its factory and this line, not what the function is called: the
same pointer at count one is "never published" before a barrier call and
"published" after it, and the type says nothing. Found by the stage-end
review of S6, 2026-08-09.
