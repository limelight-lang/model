# Postmortem

Serious mistakes only: ones that cost real time, broke something that
worked, sent work down a false path, or happened twice. An entry
without a root cause is useless, so every entry states why the mistake
was possible and why it was not caught.

---

## 2026-08-29 — a liveness assertion a starved reader cannot satisfy, and a `grep` that let it through

**What happened.** `array::table::tests::what_a_walker_reads_while_the_storage_`
`is_released::disposing_hands_out_no_state_the_array_never_had` failed once in
an ordinary run and then 18 times in 30 under the load `dev/WORKFLOW.md`
prescribes — two spinners on the two cores the run is pinned to, four test
threads. What failed was not the safety assertion, which held every time, but
the one below it: "the reader saw only one side of the release, so it raced
nothing". The reader is a spinner on another core and the arrangement it has to
catch is a window of two stores; starved of a core it can run through a whole
pass of 4096 rounds on one side of the release. The pass now repeats until both
sides have been seen, up to 32 of them, which leaves both assertions exactly as
they were and only gives the arrangement more chances: 0 failures in 30 under
the same load.

**Why it was possible.** A test that asserts a race *was observed* has a
liveness clause, and a liveness clause is a scheduling assumption. This one was
written against a machine with cores to spare — the module doc records seven
runs reporting 13 to 275 mixed readings, all on such a machine — and the
assumption went in unnamed. The safety clause has no such problem: it holds
whether the reader runs or not.

**Why it was not caught.** The commit gate runs four threads on sixteen cores
and never starves anything, and the load method exists in `WORKFLOW.md` for
reproducing a flake rather than for finding one. Nothing runs it routinely, so
a test whose liveness clause needs a core is green until the day the box is
busy.

**The second half, and it is worse.** The failing run was let through to
`origin` by a shell chain of the form `cargo test --lib 2>&1 | grep "^test
result" && git push`: the suite printed "FAILED", `grep` matched that line and
exited 0, and the push ran on a red tree. `WORKFLOW.md` already forbids this in
those words — "never pipe into a filter that can swallow a failure and let a
commit through on a red suite" — and records that it happened once before, on
2026-07-27. It is now twice. The rule needs no amendment; what it needs is that
the exit status of a test command is the thing read, and a pipeline's status is
its last command's.

## 2026-08-29 — a draw re-entered itself through the journal, and a `Cell` has no refusal to give

**What happened.** S34.8 put the escrow's floor draw at the head of
`ll_thread_init`, which made it the thread's first `BlockPool::get`. That `get`
raises `KIND_BLOCK_COMMISSIONED`, and a thread's first record runs
`ll_thread_init` from inside `journal::ring_for_writing` — so the inner call
reached `draw_floor` while the outer one was still inside its own `get`, drew a
block, and installed it. The outer call then wrote its own block over the cell.
One 64 KiB block stranded per registered thread, in the `debug-journal` build,
which is the build turned on to investigate memory. Measured on a copy of the
crate: two blocks out across a thread's life where one was expected, and one
after the repair — the one being the retired ring, which the registry keeps by
design. The Critic round on the step named it; `draw_floor` now reads the cell
again after the `get` returns and hands the surplus block back.

**Why it was possible.** The two memory reserves are safe from the same
re-entry, and by accident rather than by design: `reserve::replenish` and
`critical::replenish` hold a `RefCell` borrow across the draw, so the inner
call takes the `Err(_)` arm and gives up. The queue holds its state in bare
`Cell`s, chosen because the enrolment write is the hottest path in the runtime
and a borrow flag on it buys nothing — and a `Cell` has no refusal to give. So
"a pool draw is re-entrant through the journal" was a rule enforced by one
module's synchronisation choice and written down nowhere.

**Why it was not caught.** No test counted blocks across a whole thread's life
under `debug-journal`; that arm's tests count records. The ordinary arm cannot
see it at all, the record sites not being compiled. And the accounting tests
that do count blocks bracket a live thread, where the floor is out on both
sides and cancels.

**The same shape was latent next door.** `queue::replenish` took its cell index
before the draw and wrote at it afterwards, which under the same re-entry is a
write one past the end of a two-cell array rather than a leak. It was
unreachable only because `reserve::replenish` ran first and absorbed the
thread's first record; the floor draw moving to the head of `ll_thread_init` is
exactly what would have made it reachable. Both now re-read after the draw.

## 2026-08-27 — a block kind was read as proof of which heap a child lives in

**What happened.** S32.0's edge-to-row dispatch branches on a block's kind, and
its large-entity arm took `BLOCK_KIND_ENTITY_LARGE` and its `_RUN` twin as proof
that the child is a GC-heap entity. It is not proof. `arena::alloc_entity` hands
an entity past one block payload to `large_entity::alloc` — the same allocator
`heap::entity_alloc` uses — so a `RequestArena` entity carries `_RUN` exactly as
a heap one does, and the block header records no category. A ring closed through
such a child would be condemned, and the teardown would free a run the arena's
reset still holds in its `Log::Larges` and frees again. The Critic round on the
step named it; the arm now reads
`MemoryCategory::from_flags(mutator_flags(child))`.

**Why it was possible.** The other two populations make the kind sufficient: an
entity block and a retained block hold collected-heap entities alone, so two arms
out of three taught the wrong rule. And `large_entity`'s two kinds read as a
population rather than as a shape — the module's own predicate is called
`is_large_entity`, which is true and says nothing about whose heap.

**Why it was not caught earlier.** The step's own external test built its arena
case from a one-property class, far under `BLOCK_PAYLOAD`, so it landed in a
`BLOCK_KIND_ARENA` block and exercised the arm that was already right. The
fixture could not produce the state its doc claimed to pin. Nothing else could
catch it: no collector calls the dispatch yet, so the defect had no run to fail
in.

**The rule.** A block kind answers what the memory is shaped like, never which
heap owns it, wherever one allocator serves two categories. The category bits are
the answer, and they stay right across a reset — promotion rewrites a surviving
run's category in place and deliberately leaves its kind alone, because
restamping it would send a multi-megabyte run to the 64 KiB pool. Written into
the normative design at `rfc/model/gc/rc-cycle.md`, "A large entity's block kind
does not say which heap it belongs to".

---

## 2026-08-27 — an acquire load was credited with excluding a stale value

**What happened.** `collector_load_block_kind` was introduced with a paragraph
saying a relaxed load "would let the reader see the kind of a block whose size
class is still whatever the previous tenant left". Acquire does not buy that. It
orders the writes that preceded the paired release relative to the value read; it
puts no age limit on the value itself, so a reader may legally see an older store
and then take a relaxed load beside it from a later commissioning. The paragraph
was rewritten to say what the acquire does buy — the commissioning that
accompanies the value — and to name the mechanism that actually excludes a
recycled block: the parking rule, which keeps a block from emptying and reaching
the pool while a trace is in flight (`rfc/model/gc/rc-cycle.md`, "Death while
enrolled"). Nothing parks today; `PLAN.md` S36.2 builds the window.

**Why it was possible.** The pairing is real and the ordering it provides is
real, so the sentence reads correctly at speed. What it did was attribute the
safety of the whole read to the half of it that a keyword provides, which is the
attractive mistake: the keyword is in the code and the parking rule is in another
document and another stage.

**Why it was not caught earlier.** No test separates the two claims, and none
can: acquire and relaxed produce identical behaviour on x86 for this access, and
the design's second reader thread does not exist yet. It was caught by the Critic
reading the paragraph against the memory model.

**The rule.** An ordering comment says what the ordering buys and names the
separate mechanism that covers the rest. A claim that a memory ordering excludes
staleness is wrong on every ordering C11 has.

---

## 2026-08-27 — a scripted rewrite keyed on a field name converted a second type's field

**What happened.** S31.7 routed 187 fixture accesses of `RcHeader.flags` through
a `#[cfg(test)]` shorthand, by a scripted rewrite matching the field name. Four
of the sites it converted read `(*cls).flags` on a **class descriptor**, not on
a header. `Class.flags` is at offset 0 and `RcHeader.flags` at 4, so each of the
four began reading the descriptor's first word as a flags word four bytes further
on. Two tests went red at once, which is what named it; the repaired sites call
`Class::flags_of`.

**Why it was possible.** `flags` is the obvious name for a bitfield, and two
types in this crate carry one. A rewrite keyed on the spelling cannot tell them
apart, and neither can the header guard, which is why the guard's own doc calls
the spelling its weakest instrument. The pointer form `(*x).flags` hides the type
of `x` at the site: nothing local says whether `x` is a header or a descriptor.

**Why it was not caught earlier.** It was caught immediately, by the suite, which
is the part worth recording: the two types' fields sit at different offsets, so a
wrong conversion reads a different word rather than the same word by another
route, and the read is wrong in a way an assertion sees. Had the offsets matched,
all four sites would have compiled, passed and been wrong only under a collector
that does not exist yet.

**The rule.** A rewrite keyed on a field name is scoped by the type before it
runs — list the sites, resolve the receiver of each, and convert the ones whose
receiver is the type meant. A field name is not an address.

---

## 2026-08-26 — an atomic read needs write provenance, and `&raw const` does not carry it

**What happened.** Taking `RcHeader`'s fields private moved twelve fixture
reads of a stack-built header from `a.refcount` to
`entity_refcount(&raw const a)`. The suite passed in all three configurations,
three runs each. Miri stopped the first of the twelve: `refcount_load` casts to
`*const AtomicU32` and dereferences it, which retags SharedReadWrite because an
atomic holds an `UnsafeCell`, while a pointer made by `&raw const` grants
SharedReadOnly. `&raw mut` fixes all twelve.

**Why it was possible.** `*const T` in the signature reads as "a read needs no
write rights", and for a plain read that is true. An atomic access performs the
retag its cell type demands rather than the one the operation looks like. Every
production caller passes a pointer descended from an allocation, which carries
write provenance already, so the crate had never exercised the read-only case
and the signature had never been tested against it.

**Why it was not caught earlier.** No `cargo test` run separates the two, which
is the same blindness that made the header rule need a source guard in the
first place. It surfaced because the stage ran Miri slice by slice instead of
trusting a green suite — the discipline `dev/WORKFLOW.md` states for formal-UB
work, applied to a change that looked like a rename.

**The rule.** A pointer handed to any accessor in `refcount` carries write
provenance. In a fixture that means `&raw mut` over a local header; everywhere
else it is free, an allocation's pointer having it already. The accessors say
so at the declaration.

---

## 2026-08-18 — an allocation moved earlier re-aimed four refusal tests, and their counters could not see it

**What happened.** The COW copy gained a presized storage chunk in
`array::entity::new_empty_copy`, which made it the copy's first request
to the buffer arena. Every test that forces a refusal on that path now
stopped there instead of where it aimed: the work list, the association,
the nested destination and the escapee giveback in
`who_owns_a_key_reference.rs`. All four stayed green. Their instrument is
a count of heap slots taken and given back, and a copy that takes one
slot and returns it leaves the free list looking exactly like a copy that
takes two and returns two. The branch each was written for — a teardown
handing back a child the copy had already published — had no test at all
for the length of that change, and the suite said nothing.

**Root cause.** A forced-refusal test names its branch in prose and
proves it with a resource count, and the count is blind to *which*
allocation was refused. The two facts are independent, so the prose can
go stale while the assertion still passes. `dev/POSTMORTEM.md`,
2026-08-13, records the same class from the other side — a test that
never proved a refusal happened at all — and the answer there, a refusal
counter, does not separate two refusals on one path either.

**Why it was not caught.** Nothing reads the pairing of a test's stated
branch to the allocation it actually refuses; only a mutant does. The
Critic round on the change found it by instrumenting the refusal branch
and printing which call site fired.

**The repair.** `buffer_arena::SERVE_BEFORE_REFUSING` — a grace count the
injection spends before it starts refusing, so a test serves the
allocations that precede its subject and refuses the one it names. Each
of the four tests now asserts both numbers, one served and one refused,
and each was re-verified by a mutant that reddens it. Where the refusal
had to follow a publication, the fixture moved it onto the entry's key
rather than its element, the key's crossing being the one that still
allocates. The unit of the grace is the injection point rather than the
request, because `buffer_ensure_longlived` consults it and then calls
`buffer_alloc_longlived_payload`, which consults it again; that is
stated at the flag.

**The rule.** When an allocation moves earlier on a path, list every test
that forces a refusal on that path and re-verify each by mutant. Reading
them is not enough: they are green either way.

## 2026-08-18 — a fixture's search width, paid once natively and thirty-two times over under Miri

**What happened.** The flood ladder's tests forge families of keys whose
index slots agree, by searching candidate names until enough of them
land in one slot. The width was thirteen bits — one slot in 8192 — so
33 names cost about 270,000 candidates, each formatted and hashed. That
is milliseconds natively and the module runs in 0.01 s. Under Miri the
same module outran a 50-minute slice at two threads, then a 90-minute
one, then a per-test run of an hour and a half that reached six tests of
fourteen. Three runs were killed and produced no evidence at all.

**Root cause.** The width was chosen as a round number rather than from
the table the fixture builds. Those tables hold about 40 entries over
128 slots, so eight bits carry the family with a doubling to spare, and
every bit above that doubles the search for nothing.

**Why it was not caught.** The module was written and run natively,
where the cost is invisible, and its Miri slice was owed at the stage
close rather than run beside it. A cost that only an interpreter can
feel is not felt by the author.

**The repair.** Eight bits, named `AGREEING_BITS`, with the table's slot
count asserted against it in both forging fixtures — a family that no
longer shares a slot now fails loudly instead of silently proving
nothing. The module then ran under Miri in 272.84 s of its own clock,
four minutes of wall, 14 tests clean.

**The rule.** A fixture that searches states the width it needs and
asserts it against the structure it builds. Constants like `0x1FFF`
chosen for roundness are paid by every environment that reads them
slowly.

## 2026-08-15 — a hypothesis written where a measurement of ours already stood

**What happened.** A benchmark arm came back 3.7x apart between the two GC
configurations while the function it measured compiled to the same 38
instructions in both. The gap went into `dev/BENCHMARKS.md` as an unmeasured
hypothesis, with the note that `perf` on this kernel has no counters to test
it. It was not a hypothesis: the mechanism was this crate's own recorded
trap, measured on 2026-07-27 at 3x on the retain/release pair, with the rule
drawn from it written on `refcount::refcount_load` — a wide load over a fresh
narrow store defeats store-to-load forwarding. Two greps would have found
either.

**Why it was possible.** The disassembly answered the question I asked — are
the two builds' instructions different — and I stopped at its answer instead
of asking what the crate already knew. The journals are searched when a
question is opened, and this question felt closed: I had a mechanism in hand
that fit the numbers, and a mechanism that fits is what stops the search.

**Why it was not caught.** Nothing catches it. An entry can only be checked
against the file it sits in by someone who has read the rest of that file,
and the entry it contradicted was three months back in the same document.
The review that found it was asked a different question — whether the plan's
steps were sound.

**What follows.** Before writing "hypothesis" or "unexplained" about a
performance figure, grep `dev/BENCHMARKS.md` and `dev/POSTMORTEM.md` for the
mechanism and read the doc comment of the primitive involved; this crate
keeps its arguments there by rule, so the search is two commands. The cost of
the omission was not the wrong number alone — it was a stage planned around
finding an explanation that was already written down.

---

## 2026-08-15 — four header reads that no test could see

**What happened.** `object_constructed`, `ll_default_dispose` twice,
`array::entity::needs_separation` and a test fixture read a published entity
header as a plain field while, under `rc-walk`, the collector stamps a byte
of that same word from its own thread. A data race, formally undefined, and
harmless in every execution to date because the byte read back lies outside
every field those callers test.

**Why it was possible.** `RcHeader`'s fields are `pub` — the layout is a
contract the compiler shares — so nothing in the type system distinguishes
the helper from the field. The rule lived in doc comments and in the head of
whoever wrote the line; `object_constructed` reads the category sixteen lines
above a comment stating that rule for the write on the next line.

**Why it was not caught.** No instrument here could. `cargo test` passes by
construction. Miri cannot run the only test that pairs a live collector with
a mutator: that test is ignored under it because the design's mixed-size
atomics are rejected outright. The class was found by a review reading the
callers of an accessor that had just changed.

**What follows.** Two instruments, both built the same day.
`refcount::tests::who_may_read_a_header` reads the crate's sources and fails
on a direct field read outside `refcount.rs`, which is aimed at inattention
and says so — a rename or a local evades it. ThreadSanitizer is the real one:
it reports plain-against-atomic, does not share Miri's model gap, and was
validated by putting the defect back and watching it report
(`dev/WORKFLOW.md`, "ThreadSanitizer").

## 2026-08-13 — a forced-refusal test that never proved the refusal

**What happened.** Both regression tests for the reset's pin were written
against `buffer_arena::FORCE_REFUSE_LONGLIVED`, saw the defect, and were
committed. The review that followed took the flag away on paper and
traced them again: both still pass. The carry succeeds, no pin is taken,
and the block reaches the very state each test asserts — retained with
two live occupants in one, free with no index in the other. They were
regression tests that can stop reproducing without failing.

**Why it was possible.** Each test asserts on state the refused entity
does not own — a block's kind, the presence of an index — and that state
has two roads to it. The refusal is upstream of the assertion by the
whole of a reset, and the entity that would carry the proof is dead by
the time the test looks: the survivor whose payload was refused is torn
down inside the reset, so its block pointer cannot be read afterwards and
a post-reset `pinned_payloads` reads zero whether the fix worked or the
refusal never happened.

**Why nothing caught it.** The suite was green and the tests had each
been seen failing for the right reason before the repair, which is the
crate's own standard and was met. What the standard does not cover is a
test that reaches the right answer by a road the change did not build:
seeing it fail once proves the assertion is reachable from the defect,
not that the assertion is reachable only through the mechanism under
test. The two sibling tests written the same day do carry the guard —
`pinned_payloads(block) == 1` and a block pointer that did not move — and
neither guard transfers to a test whose subject dies inside the reset.

**The rule.** A test that forces a refusal proves the refusal happened,
in the same run, by an observable of its own:
`buffer_arena::REFUSALS`, read before and after and asserted as an exact
delta. A proxy that the refusal *could* have happened is not that proof.

## 2026-08-13 — a harness that measures a configuration the design does not use

**What happened.** The first set of index measurements for the array
table — chains against a control-byte index — was published and then
withdrawn whole. An independent review found six defects in the harness
across two passes. Two decided the result: every table size was a power
of two, so the open-addressed index ran at load 0.5 rather than the
0.875 it exists for, and the deletion rule truncated the probe sequences
of unrelated keys, losing live entries. The numbers that stand are the
second set, and they stand for integer keys only. It cost a day.

**Why it was possible.** The harness was written from the data structure
rather than from the design point. A load factor is not a property of
the code under test, it is a property of the workload the test builds,
so nothing in the index's own source says which load it is being asked
about — and the number the harness produced was a real measurement of a
configuration nobody would ship.

**Why nothing caught it.** Both arms ran the same harness, so the defect
was symmetric in appearance and asymmetric in effect: it moved one index
off its design point and left the other on it. The result looked like a
clean comparison and reproduced.

**What changed.** `rfc/model/arrays-hashtable.md` states each index's
design load beside the comparison, names the equal-N basis, and records
what was measured and what was refused as unquotable. The general rule
this is the third instance of is the entry of 2026-07-20 (second):
validity is measured, not assumed.

## 2026-08-13 — an entity nobody named yet is still an entity, and at count 1 nothing reclaims it

**What happened.** Every refusal path out of `array::entity::separate`
released the copy's children and disposed its storage, and left the
destination entity itself allocated. It stays a GC-heap entity at
refcount 1 that no edge names, which by the derived-roots corollary is a
computed root: no later pass reclaims it, in either configuration. One
refused escape copy leaked one entity slot for the life of the process.

**Why it was possible.** The refusal path was written from the storage
outward — release what was retained, free what was allocated — and the
destination is neither. It was allocated before the loop that the refusal
unwinds, so it is invisible from inside that loop, and the `rfc`
sentence describing the unwind stopped at the storage too, so the
document confirmed the omission instead of catching it (`rfc`, amended
2026-08-12).

**Why nothing caught it.** Miri runs here with `-Zmiri-ignore-leaks`,
which is mandatory in this crate, so this class has no automatic
detector at all. A leaked entity also has no local symptom: the test that
leaks passes, and the cost surfaces as an unrelated count in an unrelated
test, or nowhere.

**What changed.** `object::destroy_unpublished` is the one door for an
entity no slot ever named, and its doc block names its callers. A refusal
test measures the giveback three ways — a lower bound, an upper bound and
a positive control — because a slot count cannot say a slot was *taken*.

## 2026-08-13 — the walk was written out per caller, and paid for it twice before it was centralized

**What happened.** Where a kind keeps its counted children was written
out again in every operation that needed it, so one layout was known in
five places. Two bills arrived before the strides were merged into
`object::for_each_counted_cell` and `walk::trace_cells`. The interpolated
template moved its value count from the class to the instance, and three
separate walkers had to learn it; the third was found by review rather
than by the suite, and a walk that strides an object's slots without
knowing it leaks instead of crashing. The array was wired into the child
walkers and not into the sever, so a confirmed-garbage ring of two arrays
was un-freeable in both configurations until `144b318`.

**Why it was possible.** Each copy was correct when written, and a new
kind or a moved field is a change to the layout rather than to any of the
callers, so nothing in the type system connects them. The sever is the
copy that gets forgotten, because it runs only on the collector's path
and only for garbage, so its absence produces a leak and not a failure.

**Why nothing caught it.** An empty default arm reads as a deliberate
skip. The suite tested each walker against what it already knew, and the
one differential test that would have compared them did not exist until
the strides were merged.

**What changed.** One stride, two zero-sized readers over it, and a
differential test asserting that on a quiescent heap both readers yield
the same child set for every walked kind.

## 2026-08-13 — two arenas split by size, and the table asked the one that does not

**What happened.** A request-arena table asked `Arena::alloc` for a
storage whose size comes from the program. That call asserts above a
block payload, so the 1025th element of a request array killed the
process by abort in a release build, the profile not unwinding. Found by
an independent review, not by a test.

**Why it was possible.** Both arenas split by size, and neither split
belongs to the table: the buffer arena's is
`buffer_arena::buffer_alloc_longlived_payload`, and the request arena had
no counterpart, so the table's call site was the only place the split
could have been written and the only place nobody would think to write
it. The two entry points that do exist, `Arena::alloc` and
`Arena::alloc_large`, each assert against the half of the range the other
serves, which reads as thoroughness and behaves as a trap for a caller
holding a program-sized number.

**Why nothing caught it.** No test allocated an array storage over one
block payload. The threshold is 1024 elements at eight bytes, which is
larger than every fixture and smaller than any real array.

**What changed.** `Arena::alloc_body` makes the split once, its doc block
states the failure mode, and `buffer::buffer_ensure` and the routing layer
go through it.

## 2026-08-11 — a doc written from a test's name inherits the test's false promise

**What happened.** S9.4 grouped every test in the crate into named
submodules, each carrying a doc saying what the group pins. The docs were
written from the tests' names and their own doc comments, which is the
cheapest source and reads as the authoritative one. The Code Reviewer
then found sixteen statements the tests do not support, and nine of them
were not my invention: the test's own name or doc had made the promise
and the group doc had repeated it one level up. `removing_shortens_the_
chain_rather_than_leaving_a_marker` inserts sixty-four dense keys, each
alone in its slot, and builds no chain at all; `a_weak_referenced_object_
held_by_a_static_notifies_at_thread_exit` destroys its weak cell before
the thread ends, which clears `HAS_WEAK_REFERENCES`, so no notification
can fire.

**Why it was not caught.** A name is a claim nobody re-derives. The
grouping pass read three hundred and fifty tests in a day, and reading a
body against its name costs an order of magnitude more than reading the
name — so the pass took the names, and the suite stayed green because a
test that measures nothing measures nothing quietly. The counts, which
were this step's acceptance test, are blind to it by construction: they
count tests, not what the tests reach.

**The rule.** A doc that summarises other code is written from the code,
not from that code's own description of itself. Where the summary is over
tests, the source is the assertions: a claim with no assertion under it
goes into the doc as absent, or the test is repaired first. And the
finding runs the other way too — a group doc that cannot be written from
the assertions has found a test whose instrument does not measure its
claim, which is worth more than the doc. Nine such tests are S12.

## 2026-08-11 — a probe shows the assertion fires, not what it covers

**What happened.** S12.6 wrote six tests over production arms the suite
had never entered, and each was seen failing under a probe of the code it
guards before it was committed. The Critic then found two of the six
measuring less than their names claim, and both defects survive the probe
that was run. `a_refused_destructor_record_fails_the_construction` spends
the arena's block, drains the thread reserve and forces the pool, then
asserts only that `object_constructed` answers false; a
`track_destructor` returning false on `Arena::remaining()` passes it and
the other 424 tests, while refusing every destructor-carrying
construction in the last four kilobytes of every block.
`escape_log_survives_segment_growth` counts the records the drain
delivers, and growing the chain one record late — `count ==
LOG_SEG_RECORDS` written as `>` — writes at index 500 of a `[usize; 500]`
and reads that same value straight back, so all 1137 records arrive
exactly once over a clobbered entity header.

**Why it was not caught.** A probe answers one question: does this
assertion fire when the arm I aimed at breaks. It says nothing about the
arms nobody aimed at, and the probe is chosen by the same reading of the
code that wrote the assertion, so the two share a blind spot. The first
test's probe removed `object_constructed`'s `return false`, which is the
propagation rather than the cause; the second's dropped the chain link,
which a count does see.

**The rule.** A refusal establishes several conditions at once, so the
test repeats the call with only the forced condition lifted and asserts
that it succeeds: that assertion, and no other, names which condition
caused the refusal. And a boundary inside a container is read from the
container's own shape, never from a count of what came out of it — the
count is built from what the test supplied, so it measures the round trip
and not the container.

**What changed so it cannot repeat.** Both tests carry the second
instrument, and both were probed again with the defects above: each is
then the only failing test in the suite. `force-oom-names-one-allocator`
in memory carries the positive-control half, since `FORCE_OOM` is where
this crate forces refusals.

## 2026-08-11 — the guard checked a different limit from the call below it

**What happened.** `stdapi::ll_alloc_large` guarded its OS-direct branch
with `checked_add(LINE_SIZE)` and a checked block round-up, both carrying
a comment about caller-controlled ABI input, and then unwrapped
`Layout::from_size_align`. The checked pair fails near `usize::MAX`;
`Layout` fails at `isize::MAX`. Everything between the two — the whole top
half of the range — walked past the guard into a panic, and a panic
crossing `extern "C"` aborts, where the module's contract is null.

**Why it was not caught.** The guard reads as complete. Two checked
operations and a comment naming the hazard are what a reviewer looks for,
and the site had both; nothing on the line says which limit was being
guarded against. The test beside it,
`huge_size_overflow_returns_null_not_underallocation`, uses `usize::MAX`
and `usize::MAX - 100` — chosen to exercise the wrap, and passing for the
same reason the defect survived. The correct shape was twenty lines away
the whole time, in `immortal_alloc_run`.

**The rule.** A guard is verified against **the limit of the call it
protects**, never against the widest type in the expression. Write the
limit down: `usize` arithmetic and `Layout` disagree by a factor of two,
and so do `isize`-based length caps, `u32` index caps and every ABI that
takes a size as a signed word. The band between two limits is where a
caller that lost a sign lands, and a test that picks its input from the
type's maximum never reaches it.

## 2026-08-10 — cutting the duplicate left the copy that overreached

**What happened.** The comment pass over `src/memory/` found `Heap::refill`
stating the same "no side allocation, no per-slot initialization" property
twice: once in its doc comment, once in its body after the entity arm's
zero pass. The body copy was deleted as the retelling. It was the scoped
one — it sat *after* the loop, so a reader met it as a statement about
what remained — while the doc copy claims the property for the whole
function, and `refill` runs a pass over every slot of an entity block.
Deleting one of two made the survivor a false contract, and a nearby
comment saying "the property **below** is measured" now pointed at
nothing.

**Why it was possible.** Two comments saying the same thing are not
interchangeable. Each one's scope is its position, and position is exactly
what a diff of the text does not show. The pass compared the sentences and
not what stood between them.

**Why it was not caught earlier.** The suite cannot see it and neither can
`fmt`: a comment has no failing state. What caught it was the stage's own
Code Reviewer, one round, reading the function rather than the diff.

**The rule.** When two comments carry one claim, decide which is scoped
before deciding which is surplus, and read what stands between them. If
the survivor has to be widened or narrowed to stay true, the cut is not a
cut — it is a rewrite, and it is the doc comment that is usually wrong.

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
`collector::tests::the_epoch_as_a_whole::a_free_running_mutator_survives_concurrent_epochs`.
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

**What happened.** `walk::tests::what_the_walk_enumerates::census_counts_objects_and_their_edges`
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

## 2026-08-11 — a test measured a block's tail and called it a code path

`string::tests::the_payload_and_who_frees_it::an_append_loop_moves_its_payload_once` asserted that 256
appends move the payload exactly once: allocated on the first append,
extended in place after that. It failed about one run in thirteen at
`--test-threads=16` and never at the gate's width of four, and the count
it reported was 2.

**What it was really measuring.** A rotation adopts an abandoned block
before it takes a fresh one, because a block with no owner has nobody to
collect the frees posted into it (`dev/DECISIONS.md`, 2026-08-05). So a
test thread's arena can begin bumping in the few kilobytes another
thread left, and a payload doubling past that tail is copied once
however well the in-place path works. Measured from a failing run: the
payload was allocated in a block with 3280 bytes free and copied when it
grew to 4096, into a block with 61184. Sixteen threads make abandoned
blocks plentiful; four rarely do.

**What made it look like a code-path assertion.** The property the test
is for — the string path reaches `BufferArena::try_grow_in_place` — is
real and was measured when it was written: one move with the in-place
path against nine without it. But `try_grow_in_place` has two
conditions, adjacency and room, and the test asserted the outcome of
both while only one of them is the string path's. `test_guard()` does
not close the gap: it serialises the block pool, not the tails other
threads abandon.

**What changed so it cannot repeat.** The test asks the arena for the
room before each append and counts a move against the path only when the
block could have held the growth, which is the second condition written
out. It also asserts that at least one growth was served in place, so a
run where the room never sufficed fails loudly instead of passing with
nothing measured.

**The shape to watch for.** An assertion on a count that a shared
allocator produces is an assertion about every other thread's history.
Before writing one, ask which of the conditions behind the number belong
to the code under test — and make the test establish the rest, or stop
asserting on them.

## 2026-08-12 — a guard taken twice in one test, and the configuration that hid it

S14.2 added `block_pool::test_guard()` to the top of
`barrier::tests::publication_before_teardown::a_collecting_destructor_
cannot_see_the_slot_it_is_being_removed_from`, which already took one
thirty lines further down its body. The guard locks a
`std::sync::Mutex`, which is not reentrant, and `let _g` shadowing does
not drop the first binding, so the second call waits on a lock the same
thread holds. The suite stopped there and stayed stopped: no failure, no
output past that test name, every thread in `futex_wait` with no CPU
time accruing.

**Why the scan that placed it did not see the existing call.** The step
looked for tests that allocate without the guard, and it read the head
of each `#[test]` body. This test takes its guard after two `static`s
and an `extern "C"` destructor body, past where the reading stopped.

**Why it stayed invisible for a day.** The test is
`#[cfg(not(feature = "rc-walk"))]`, so it is compiled only into
rc-trace, and the tree had been verified in the default configuration
alone: `cargo test --lib -- --test-threads=4`, green, 440 tests. The gate
runs both configurations for exactly this reason, and the gate had not
been run since the edit.

**The shape to watch for.** A lock taken by a helper that tests call by
habit is taken more than once as easily as not, and the second call is
silent at the source: it reads as an ordinary line. Before adding one,
grep the whole test body rather than its head, and read a hung run as a
lock held by the same thread until the wait is ruled out.

---

## 2026-08-12 — a ring is a block, and a thread's first record decides when it is taken

**What happened.** `memory::arena::tests::the_logs_the_reset_reads::
reset_hands_destructors_and_recycles_blocks` asserts that an arena built
after a reset draws back the block the reset recycled, and it failed 1 in
35 at eight threads under rc-trace with `debug-journal`. `BlockPool::put`
pushes the block onto the thread cache, closes the borrow, and then raises
`KIND_BLOCK_DECOMMISSIONED`; a thread whose first record is that one
allocates its ring inside the site, through `ll_malloc`, and a ring is one
pooled block. So the draw takes the block pushed a few instructions
earlier and the arena below is served the next one. A 300-run leg at eight
threads found the same site disturbing
`put_then_get_reuses_without_new_region`,
`an_overflowing_cache_keeps_half_and_flushes_the_rest` and
`emptied_noncurrent_block_returns_to_pool`.

**Why it was not caught.** Which record is a thread's first is no property
of the test that suffers for it: the enabled mask is process-wide, and
thirty call sites in the suite move it. A test thread that initialises
while the mask is 0 journals nothing at `ll_thread_init`, so it reaches
its first site later, in the middle of what the test measures. At the
gate's width of 4 that overlap is rare enough for the suite to read as
green, and the first two instances were repaired one at a time in
`heap`'s tests without the shape being named.

**The per-test remedy is what those two used, and it does not scale — it
feeds the defect.** Quieting one test's sites widens the window in which
*another* thread initialises without journaling, so each application makes
the next test likelier to meet its first record late. Measured on this
crate: three tests repaired that way, and the next 600-run leg surfaced
five more, three of them tests that had never failed before.

**The rule.** `block_pool::test_guard` takes this thread's ring before the
test body runs (`journal::take_ring_for_test`, `debug-journal` builds
only), so the draw lands where nothing is measuring and no site inside a
test that holds the pool's lock can allocate a ring. That closed every
instance at once — 1200 runs at eight threads across both GC
configurations, no failure, including four tests nobody had repaired. The
repair belongs to the fixture rather than to the pool: the draw is what
the runtime really does, and a pool that served the journal from anywhere
but the cache would be a worse allocator bought for one assertion.

**A count every thread moves is not an instrument, and the fixture cannot
help it.** `blocks_out`, the registry's live and retired totals and
`regions_carved` are moved by threads that hold no pool guard —
`single_live_slot_churn_keeps_its_block` read `blocks_out` one high
6 times in 600 runs, `a_refused_ring_is_not_asked_for_a_second_time` read
a ring another thread had retired as the ring it was refused, and
`the_retired_list_keeps_the_newest_and_drops_the_oldest` demanded that
only its own oldest be evicted while another thread's retirement pushes
the bound down its list too. Each now measures what it is about: this
thread's block cache, which the pathology fills at the free it denies; the
refusal count, which only guard-holding tests move; and the *order* of
eviction, which is the guarantee — the rings that go are a prefix of the
order they were retired in, however many go.

**A `mark` writes into the window it opens.** It takes the rings an
eviction left for a live thread to free and frees them after capturing the
cursors, so the decommission of a freed ring's block falls inside the
window; the pool then hands that same block to the next `get`. So the
round-trip test reads its trip as the tail of what its ring holds for the
address rather than as the whole of it. Moving the frees above the capture
does not fix it and cannot: the frees would then fall inside the window
the *end* mark closes.

---

## 2026-08-12 — moving an argument out of a comment deletes it, unless someone opens the section

**What happened.** S15.5 shortened 37 comment blocks by moving their
argument to a document section the comment then names. Seven of those cuts
took a fact the named section does not carry: why `exit_guard_armed` takes
`try_with` and what a panic inside a TLS destructor costs, why the weak
table is sound without a lock, the whole-struct store an atomic field
cannot defend against, the retained index's one lock, that a `LARGE_RUN`
returns to the OS on free, what can refuse a publish, and what blocks
sharing a chain walk between `Table::get` and `insert`. The stage's Code
Reviewer found all seven by opening each cited section and reading it
against the deleted lines; the suite could not, the tests being green
throughout.

**Why it happened.** Each cut was made by a worker that had verified the
section held *the block's argument* — and it did. What the section did not
hold was the one sentence beside that argument which the code site needs:
the argument is the design, the sentence is the local obligation. The two
read alike in the diff, and the verdict form the survey produced ("move to
X §Y") does not separate them.

**The rule.** A cut toward a document is verified sentence by sentence,
not block by block: for each line leaving the code, name the sentence in
the cited section that now carries it. Where none does, the line stays,
whatever the block's verdict said. A pass of this kind ends with a review
that opens every cited section — the only instrument that sees a fact
which now exists nowhere, since the compiler and the suite see nothing at
all.

---

## 2026-08-12 — a test that dies with `FORCE_OOM` raised reports the next test's crash

**What happened.** S7.6's two new tests were run together for the first
time. The first panicked on an `expect` between `FORCE_OOM.store(true)`
and `FORCE_OOM.store(false)`; the second then died of SIGSEGV, and the
run printed `FAILED` for the first with no message and the signal for the
second, the process having died before the panic's line was flushed. The
panic's text appeared only when that test was run alone. Unwinding does
not restore a fault-injection flag, and both flags are process-global
statics while `block_pool::test_guard` serializes only the tests that take
it — so the party that meets an allocator refusing everything is every
test running concurrently in the binary, not merely the next one on this
thread. The first null one of them did not check was the crash.

**Why it happened.** The panic was itself correct — the exhaustion loop
had found no slot to take, because the thread had no warm entity block
and the raised flag stopped it from drawing one, so the loop returned
empty and the `expect` fired. The setup error and the reporting error
compounded: the real defect was one line of ordering, and what it showed
was a segfault in a different test.

**A third rule, and the same day paid for it twice.** A fault-injection
flag is scoped to what it forces. `gc::FORCE_BUFFER_REFUSAL` was a
process-global static forcing a **thread-local** candidate buffer, and
one test raised it while `the_candidate_buffer::forgetting_a_candidate_
keeps_the_moved_one_findable` ran beside it on another thread: three of
that test's four candidates were refused, and it reported "buffered in
order" with one address where four belonged. It failed once in ten
rc-trace runs at four threads and not at all in the eighty-six after the
flag became thread-local. The lock was not the missing piece and could
not have been: `block_pool::test_guard` serializes the tests that take
it, the raiser took it and the victim did not, and a victim cannot be
expected to know which global a stranger raises.

**The rules, and they are two.** A fault-injection flag is raised through
a guard that lowers it in `Drop`, never by a bare store: keeping
assertions outside the window answers only the panics a test author
writes, and the ones that matter are the crate's own `debug_assert`s
firing inside it on exactly the regression the test exists to catch. And
an exhaustion loop under a raised flag needs its first block drawn
**before** the flag goes up: with the pool refusing, an allocator with no
warm block serves nothing, and "exhausted" and "never started" are the
same empty vector.
