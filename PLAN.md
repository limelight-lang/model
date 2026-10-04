# Plan

Destination: `ll-model` is the runtime the compiler links — the memory
manager, the object model and the cycle collector built to the `rfc`'s
design and calibrated on parameterized test heaps, each reading naming its
parameters.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/rc-cycle.md`, `model/gc/cycle/questions.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`. The `rfc` repository carries its own plan at
`dev/PLAN.md` for work that lands in the specification rather than in this
crate. The destination's last mile, the compiler that links this crate, is
outside this plan: `rfc/BACKLOG.md`, "The big one", and the front end in
`limelight`.

Updated: 2026-10-04 · Active: none; the next stage is drawn from the backlog
below.

Review 2026-09-29, the last; its text and the closed stages' summaries are in
`git log -- PLAN.md`.

**Closed stages are deleted whole** (rule 23.1.3), and what outlived each of
them is in the journals: `dev/DECISIONS.md` for a decision and its reason,
`dev/POSTMORTEM.md` for a trap, `dev/BENCHMARKS.md` for a measurement,
`dev/INDEX.md` and `dev/ARCHITECTURE.md` for the map. Deleted so far: S4
through S67. A number is never reissued, and the prose sections below the
active stage are the backlog stages are drawn from.

**Every cycle-GC step has one review gate, and the Sage is the escalation**
(Edmond, 2026-09-10 and 2026-09-12: the Critic first, the Sage only for what
the model cannot answer itself). The pre-change baseline the step would erase
— operation count, manager-allocation budget, cache working set, lifetime and
refusal model — is recorded in the step before the first edit; a red test is
seen failing; the Critic reviews the repair and its mutations before the step
is checked, and its findings are recorded in the step. One broad review does
not waive a later step's gate. The rest of the routine — the gate, Miri in
slices, the citation checks — is `dev/WORKFLOW.md`.

## Fog

A line here is an unresolved question rather than a step: it carries no
criterion, and it leaves when it gets one or when it is ruled on.

- **Whether a survivor list should prefer a block this reset has already
  retained.** `Arena::alloc_preferring` tries the described block's tail, the
  reset's current block, then a fresh pool block. A bump arena's survivor
  blocks are normally full, so the common answer is the second, and that
  block — which may hold no survivor of its own — is then retained and held
  until the last survivor of every block whose list it carries has died; the
  third answer holds 64 KiB drawn at reset time for the same span. Preferring
  the tail of a block the reset is keeping anyway would cost nothing extra in
  bookkeeping and would build pairwise dependencies between retained blocks,
  which `release_emptied` already recurses through. Raised by the Critic of
  2026-09-13 over the survivor-list grouping, and priced nowhere.

- **What a process-wide `pthread_key` would buy the reserve draw.** The
  thread-locals of this crate that carry drop glue — the census test
  `critical::tests::where_the_first_touch_happens` lists them — register a
  TLS destructor at their first touch whose failure ends the process rather
  than reporting (`dev/DECISIONS.md`, "what the first touch of a thread-local
  with drop glue may cost"). `ll_thread_init` touches every one, so the death is
  deterministic in place; the class itself is still there for a thread that
  never runs init. A guard on a key taken once at process start, where a
  failure is reportable, would remove it if `pthread_setspecific` allocates
  nothing per thread — which nobody has read, on any target. Named when the
  reserve's first touch was decided on 2026-08-29 and priced nowhere since.

- **Whether a root read live should leave every queue until it is
  decremented again.** Variant S (2026-09-26): the owner clears the candidate
  bit of a root read live and writes the epoch into byte 6; a later
  registration whose stamp is the current epoch goes to the deferred lane
  without a trace, an older one is traced. A live root nobody touches then
  costs neither thread anything, where the lane and the chain re-trace it
  every epoch (70,000 roots a mutator on `deferred-live-large`); a ring that
  dies in the stamped epoch is found at the next turn, as today. It amends
  `rc-cycle.md`'s rule that the maturation prune never spares a queue root,
  and the claim that a ring whose external reference goes registers a root is
  unproved: a case of that claim is its first check, and the run of
  `dev/BENCHMARKS.md`, "S65.28 D against H on the repaired build" does not
  measure it.

- **How long a completed death behind a live entry holds its slot.** In R
  it waits for the retirement pass below 64 entries, or for the collector's
  batches to reach it above; behind a chain entry, under the proposal, for
  the next grant that posts anything, 32 such deaths, or its block's expiry.
  The rig's tally (`queue::withheld_by_an_entry`, `cfg(test)`) counts deaths
  withheld by any entry, not only those behind a live one; the load
  `live-churn-dies-by-count` makes it non-zero, and splitting it by the kind
  of entry is what would give the question a figure. Edmond, 2026-09-26: "the
  memory of the object itself is held until the collector comes again — this
  needs thought".

- **The names of 2026-09-26.** R, P, the deferred lane and the chain, the
  arms' letters and the constants N and D each read two ways in this plan;
  Edmond chose role names that day (CandidateQueue, TraceResultQueue,
  EpochQueue by owner), which wait for `rfc/dev/GLOSSARY.md` and a rename
  stage of their own.

## Cross-cutting (every stage)

- The old collectors are reachable at `archive/pre-rc-cycle` and nowhere else.
  Nothing is copied back without a decision entry.
- Every fix carries a regression test verified to fail on the bug
  (`dev/WORKFLOW.md`, Tests); the gate, Miri in slices and the citation
  checks are `dev/WORKFLOW.md`'s, the review chain is the header's.
- A claim about speed is a measurement or it is not made; a bench does not
  cross the C ABI — ABI-entry work is shown by IR or asm (`dev/BENCHMARKS.md`).
- Every byte owned for cycle collection comes from the memory manager and is
  identifiable there as GC memory; production collection paths use no
  allocator-owning Rust container. The deny run is
  `cycle::collect::tests::what_a_collection_asks_the_allocator`, the ledger
  `memory::gc_metadata` (`dev/DECISIONS.md`, "GC memory is counted once, and
  the block kind is the split").
- `dev/ARCHITECTURE.md` is the crate's knowledge map and moves with behaviour
  like any other document (`dev/WORKFLOW.md`).

## Then: arrays as a performance problem

Opened 2026-08-07; the representation is built (`dev/DECISIONS.md`,
2026-08-07 and 2026-08-11). The compaction threshold, `EQUAL_HASH_LIMIT` and
`CHAIN_LIMIT` stand on borrowed numbers until a machine resolves an effect
under this box's 1.5–3 % noise floor (`rfc/model/arrays-hashtable.md`,
"Open").

- [ ] **The string-key cancellation threshold.**
  done: the control-byte index against the chain at N of 56, 512, 4,096 and
  28,672 on string keys of realistic length, the deletion margin beside
  them, judged by `rfc/model/arrays-hashtable.md`, "Open" (1.5× on both
  lookups, margin not worse); the default changes or is recorded as kept,
  figures in `dev/BENCHMARKS.md`.
- [ ] **Whether an escalation raises an operations-visible signal**
  (`rfc/model/arrays-hashtable.md`, "Open").
  done: a decision with its reason in `dev/DECISIONS.md`, or a line saying
  the rfc answers it.

## The vocabulary

The rename closed 2026-09-02; three guards in `src/cycle/tests/` hold it
(`dev/DECISIONS.md`, "the vocabulary is held by three guards, one per
surface"). The cross-repository remainder is the `rfc` plan's step "Rewrite the
documents and the crate to the glossary".
Residues with no owner:

- [ ] **`promote` keeps `corpse`** for the glossary's *torn-down entity*,
  under an exemption in both metaphor guards.
- [ ] **`shadow::groups`, `group_bit` and `group_bytes` were never put to the
  glossary** (`dev/DECISIONS.md`, "an uncovered term is a gap rather than a
  local ruling").
- [ ] **The comment guard reads 14 words and one two-word term of the
  audit's mapping.** `refused` in its retired sense is not among them;
  `colour` stands 100 times in comments against the US-spelling rule (count
  of 2026-09-24).
- [ ] **Neither metaphor guard refuses a stale exemption.** The identifier
  guard has a test that fails on a file which has stopped offending; the name
  and comment guards have none.
- [ ] **`cycle::deferred_slot_reuse` outgrew its name.** `ActiveTrace` lives
  there and owns the scratch arena, takes the candidate batch and hands out
  rows, while the module header still describes the slot-return window alone.

## Beside the hashtable: the memory categories

The region reset gates the rename.

- [ ] **Rename the memory categories** in the rfc, the documents citing them
  and the crate, once the region reset decides what a region's contents
  carry (`dev/DECISIONS.md`, "`LongLived` goes out of use, and its rename
  waits for a mechanism"). `LongLived` is marked out of use on the enum.
- [ ] **The region reset.** What a region owns, when it resets, how the
  owner's death reaches its entities, and what promotion across a region
  boundary is (`rfc/model/memory/regions.md`). Until it exists
  `ll_string_new_dynamic` refuses `LongLived`, since nothing would reclaim
  such a string. Blocked on design.

## What is left of the old phase lists

Carried from the phase lists deleted with the 2026-07-24 snapshot; checked
against the code on 2026-09-24.

- [ ] **A3's factory half.** `factory(ctx, category)` takes no class, so it
  needs per-class generation; until then the generic path is
  `ll_object_new(ctx, class, category)`, and `ll_default_dispose` stands in
  for a generated `dispose`, which `rfc/runtime/object-lifecycle.md`'s "Only
  the GC reads layout as data" waits on. `clone`, `deep_clone`,
  `thread_clone` and `thread_move` are reserved for multi-threading.
- [ ] **A7, no zeroing by default** (`rfc/BACKLOG.md`, deferred
  optimizations): `ll_object_new` zero-fills the whole body.
- [ ] **`Lazy` (1) and `Box` (10) have no producer**: Box waits on the FFI
  surface, Lazy on the compiler. `ll_entity_die` routes `LAZY` to
  `ll_object_die`; `Box` reaches its `debug_assert!`.
- [ ] **The critical reserve's third customer.** Who arms a collection is
  settled (`rfc/model/gc/strategies.md`, "Collection requests and triggers").
  The reserve's third customer, a mutator whose gate is closed, is answered
  null until the ABI
  names the runtime progress operations a reserve would fund
  (`rfc/model/memory/critical-reserve.md`, "Mutator progress while
  collection is unavailable").
- [ ] **The move rule of unique ownership has no home.** The rfc ruling of
  2026-08-23 (`rfc/dev/DECISIONS.md`, "compiler logic leaves this
  repository's scope; a uniquely owned entity is not collected") strikes the
  birth count and makes the move rule compiler business owed a home outside
  `rfc`; this line holds it until that home exists.
- [ ] **Pure destructors, the P0 runtime step.** A specialized dispose for a
  class with no `__destruct`, and a raw-sever arm in reclamation for a
  component with no pending destructor (`rfc/model/gc/pure-destructors.md`,
  "P0 gains available without compiler work"); no ruling and no compiler
  needed, but the analysis predates `rc-cycle` (that document's status), so
  the step is revalidated against it first. The compiler tiers wait on the
  compiler.
- [ ] **Three `promote` tests claim Miri as their whole regression**
  (`dev/WORKFLOW.md`, Miri, "Three tests claim Miri as their whole
  regression").
  done: each seen failing under Miri against the mutation it names, or its
  comment corrected.
- [ ] **Strategy 1, the typed vector.** No producer, so the 1 → 2 transition
  waits on one (`dev/DECISIONS.md`, 2026-08-13).

## Residual / carried-over items

- [ ] **The `rfc` lags the default build** (Edmond, 2026-09-29: "в долг
      запиши"). `rfc/model/gc` has no `NOTHING_PROPOSED`: form D's batch that
      proposed nothing releases to it (`cycle::collect`'s `dispose_of_p`),
      where the `rfc` describes the release to `POSTED` alone; and
      `rc-cycle.md`'s section on the live list listed every row a part left
      live, where the code left the part's own root out since 2026-09-27. The
      live list went on 2026-10-02, and its section with it; form D's release
      is owed now that the default build keeps D (`dev/DECISIONS.md`, "HG
      leaves the tree").

- [ ] **The review's cuts of 2026-09-26** (pass 1 over `89bb0dc..49b544a`,
  with the comment reviewers' findings before the push): production —
  `worker::batch` 148 lines (the chain's preamble, the shares, the sort,
  the trace's timing and the tail as functions), `cells::trace_cells_until`
  87 at depth 4 (the array's arm out), `compaction::compact` 81,
  `worker::trace_in_parts` 80 at depth 4 (retry-or-defer out),
  `serve_the_grant` 60 (its release guard at module level, one
  `released_byte(posted, proposed)` shared with `verdicts::testing`),
  `mutator_record::take_record` 59, `token::take_recalling` 57 at depth 4,
  `ll_gc_maybe_collect` 56, `collect_under_pressure` 56,
  `make_returns_withheld_under_a_foreign_trace` 54, `Standing::checkpoint`
  54, `block_pool::put` 56; `RecordChain::check` at depth 5 and its
  `(front, tail)` pair read in five places (a `span` helper);
  `for_each_met_root` and `shadow::for_each_met_row_where` at depth 4;
  one-implementation `GrantsBehind`, `PartsOutcome`, `ChainPosts`; forwards
  `chain::peek_the_ready_part`, `commit_the_ready_part`,
  `shadow::for_each_live_met_row`, `take_under_pressure`, `dispose_of_p`,
  `Reader::unread_at_most`; `chain::DEATHS_TO_POST`, read only by its own
  assertion. Tests — `the_rig`'s `fields` 321 lines, the calibration 216,
  `a_mutator` 165 at depth 4, its 19 loads spelled field by field (a base
  and `..BASE`); the traced-batch bracket written 14 times and the token-wait
  stamp 4 times (two helpers); `what_a_grown_k_costs` at depth 5; the
  fixtures `what_a_batch_without_a_proposal_owes` copies from
  `what_the_byte_arms` (to `collect/tests.rs`); the chain's `ignore` reason
  written 25 times; `arms_table.py` and `two_arms_table.py` in parallel
  copies; whether `what_a_take_costs`'s short-take retries still meet a
  short take now that P's room is read off the reader's front, unverified.

- [ ] **Garbage behind few roots signals no collector.** A collector is born
  by the poll's signal on a filled block of R, 8,135 entries, so a load that
  registers one root an iteration holds its garbage until memory runs short
  or the thread exits: `one-large-root` stood at 392 MB with none freed in
  the rig's calibration (`dev/BENCHMARKS.md`, "S65.19 the rig's figures"), and
  another run read 790 MB/s standing over three mutators. Done when a bound on
  the bytes standing before a collector is signalled is built with a case, or
  a ruling in `dev/DECISIONS.md` names the pressure path as the bound with the
  figure (the plan review's Critic of 2026-09-26, F6).

- [ ] **The review's cuts of 2026-09-21** (pass 1 over `c58d49f..128cefc`):
  `memory::heap::ll_thread_init` at 59 code lines (the unstarted life's
  unwind and the heap-funding tail become functions);
  `cycle::collect::collect_under_pressure` at 51 (the tail after
  `freed += taken` becomes `after_a_round`); depth 3 in
  `static_block::run_thread_exit_teardown` (`pop_the_newest` lifted out of
  the loop), `cycle::testing::traced_from_a_collector_thread` and
  `the_metaphors_the_comments_still_carry::retired_in`; six `#[test]` bodies
  at depth 3, one of them `critical::tests::where_the_first_touch_happens`
  with the fifth copy of a directory walker.
  done: each under its threshold or recorded as kept with the reason.
- [ ] **The review's cuts of 2026-09-23** (pass 1 over `128cefc..89bb0dc`):
  `Standing::checkpoint` at depth 4 (one record's pass becomes a function
  with early returns, the kept grant's serve another); `epoch::of_record` and
  `Standing::take_backlogged` inlined into their one caller each; in tests,
  32 bodies over 50 lines (largest `the_siblings`' birth-and-handover case,
  130, and `when_the_turnover_reoffers`' mature-member case, 128) and six at
  depth 3–4, most cut by shared fixtures: a ring deferred behind a keeper
  (four copies), a keeper's release-and-die pair (eleven), `keeper_class`
  (five files), the sleepers' start and end in `under_stress.rs`, the three
  hooks forwarding to one `OneShot`, the two `EmbeddersInterval` guards.
  done: each under its threshold or recorded as kept.
- [ ] **A case that expects a debug assertion's abort fails in a release
  test build.** `memory::heap::tests::a_thread_outside_its_life::a_second_init_on_a_started_thread_is_refused`
  reads the child exit 0 under `cargo test --release` (2026-10-03, with and
  without the owner's fast path); the gate runs the debug build alone. Whether
  it is gated to debug assertions is Edmond's (no muting without him).
- [ ] **A born sibling under the cap's flip has no case.** The per-slot case
  of the cap set under work stands in for it
  (`worker/tests/the_cap_set_under_work.rs`): a sibling birthed by a backlog,
  the cap set to zero under its round, its own checkpoint withdrawing its list
  and the elder ending it.
- [ ] **The turn's ratio is 4 "for now".** Edmond set it on 2026-10-03
  (`dev/DECISIONS.md`, "the epoch's ratio is 4, and the embedder's to set")
  over `web-heap` and `web-arena-40k` alone; 3, 5 and 6, `web-arena-150k` and
  the ring loads whose live core dies (`live-churn`, `deferred-then-dead`) are
  unread at it, a run of 2, 3, 4 and 6 over them put to him. Y9's
  minimum-over-stamped-members question changes no prune at `k = 1` and
  reopens with any `k` above 1
  (`mark::tests::the_pruned_share_against_a_survival_rate` under
  `pin_threshold`). `template.rs` builds its entity without reading the
  class flags, so a template class marked acyclic would miss `ACYCLIC_GATE`
  — conservative, left until one is marked.
- [ ] **Garbage whose own component outlasts every grant** is freed only by
  the owner's collections over R whole, under pressure and at the exit: a stop
  posts its root read live or unwalked and keeps no continuation. The
  alternatives go to Edmond (`dev/design/the-proof-epoch-collector.md`, §9).
- [ ] **Where the stop level stands.** `STOP_MARKS` = 2 marks raise the stop
  (27,074 withheld returns at 7.1 ms, up to 369 blocks, on `web-arena`;
  `dev/BENCHMARKS.md`, "readings the S65 and S67 stage notes held"), and
  whether a grant needs a bound in blocks touched beside it is Edmond's.
- [ ] **The epoch to cut work, three ideas after the full walk** (Edmond:
  "сначала полный обход, лучше потом"): an epoch a heap region turned round
  robin ("Это самое простое"; a Critic first, then R4's cell); re-checking only
  the suspects — a verdict read live that hit a prune — a layer at a time, which
  repairs the chain of rings a batch boundary reads live (a case of 2K + 1
  rings owed); keeping the stamps of blocks where nothing registered, sound
  only with the second.
- [ ] **A validation across polls behind a barrier on the increment** would
  bound the owner's pause over a posted set, which the fast path only shortens;
  a mutator cost, Edmond's to rule.
- [ ] **The stamps' walk could stop at a wind-down raised during it**, not at
  one standing before it (`dev/DECISIONS.md`, "rulings the S65 and S67 stage
  notes held").
- [ ] **Cases unbuilt around the posted set and the stamps:** a retained-block
  member of a set; a member dying between the scan and the stamps' walk; the
  walk's reversal on an unwind; a stop inside the roots' meeting loop.
- [ ] **A turn re-offers the lanes, and their batches pay toward the next
  turn** while proving little (a Critic's finding, accepted, not fixed).
- [ ] **A posted set's drops and cuts are counted nowhere**, in the journal or
  on the rig.
- [ ] **Where a collector's `mmap` can fail:** no process-wide word says a
  thread was refused, and the peaks of N collectors add up.
- [ ] **R0's gate, 65 batches a second, fails at six mutators** and was never
  ruled.
- [ ] **A snapshot's read-live raises the lanes' count of readings**; never
  reviewed.
- [ ] **Floating garbage by cause** (the Sage's D7): reported per cause, and the
  traversal threshold `k` = 2 put to Edmond only if die-young garbage
  dominates.
- [ ] **The web protocol never ran whole:** six mutators, cap 4 and
  instruction counts need a box with a PMU and spare cores; the two named
  sensitivity sweeps and a host-steal reading under WSL with it.
- [ ] **Loads the rig lacks:** an old set of 32,768–65,536 roots dying as
  cycles after several epochs, a medium immortal chain under young traffic, a
  recall-heavy shared core.
- [ ] **The review's cuts of 2026-09-29 kept in the default code:** the lane's
  mirror into one `mirror_the_lane`, the lanes as an array, the name
  `SURVIVED_READINGS_MASK`, the rig's figure sets repeated in five places.
- [ ] **Two cases time out on Miri's clock in a long run**
  (`a_completed_batch_stamps_what_its_final_drain_read_before_the_release`,
  `a_wind_down_sends_back_a_root_read_live_only_through_another`): the
  collector's wait for the consent; each passes alone.
- [ ] **A store of one request arena's entity into another's object.**
  `store_category_barrier` keys on category alone, so the pointer is stored
  raw. A destructor inside arena A's reset can reset arena B, whose
  `count_children` meets A's entity as an ordinary child: a non-COW child is
  promoted early (conservative); a COW child's count is raised by B's pass
  while the edge's record is freed with B's log, so A's reconciliation
  subtracts a reference a promoted holder still holds (Critic, 2026-09-13;
  nesting itself is ordinary, `dev/DECISIONS.md`, "one arena is reset once
  at a time on a thread, and a second entry is refused").
  done: a red test resets B inside a destructor of A's reset, or a
  demonstration that the shape is refused, in `dev/DECISIONS.md`.
- [ ] **A retained block's occupant freed from another thread inside the
  reset.** `retained::occupant_freed` subtracts from the low half of a word
  whose high half holds the pins, so a free before the reset has set
  occupant counts borrows from the pins (a `debug_assert`; a lost pin in
  release; with no pin, a wrapped word that makes `has_held_occupants` true
  for good), and the guard, `reset_window::absorbs_retained_free`, reads the
  freeing thread's window. Whether a promoted survivor is reachable from
  another thread mid-reset is unestablished (Critic, 2026-09-13).
  done: a red test from a second thread, or a demonstration of
  unreachability in `dev/DECISIONS.md`.
- [ ] **The twenty-sleeper case has no Miri run.**
  `the_standing_list::twenty_sleepers_stand_and_are_all_served_on_waking` is
  `cfg_attr(miri, ignore)` (killed at 50 minutes, twice); the order of
  service across passes at twenty entries is read by no Miri run. Either
  four sleepers under `cfg(miri)`, or a stand-in that moves the byte without
  a thread. Unpriced.
- [ ] **A round whose walk reads no record carries its checkpoint's
  readings.** `batches_served`, `saw_work` and the backlog fold in
  `read_one_record` and nothing drains them after `for_each_record`, so such
  a round reports no batch and its timer lengthens
  (`Standing::take_saw_work`).
  done: a drain after the walk, and a case over a collector whose records
  are all named elsewhere.
- [ ] **Two arms of the take's measurement are unbuilt**: a pool that sleeps
  and wakes, and the mutator's collection beside a round in flight
  (`dev/BENCHMARKS.md`, "What these arms are not" under the two S64.5
  entries of 2026-09-22). Neither moves a constant alone.
- [ ] **Two residues of the collector's birth.** The guard's case asserts
  the slot's mapping is exactly 2 MiB plus its guard, which an adjacent
  anonymous mapping of up to 64 KiB merging at its end would break (no
  source found in the test binary). A wake landing in the last instant of a
  consent wait is neither spent nor remembered, bounded by
  `FALLBACK_INTERVAL_MAX`. The untested `mprotect` and join failures and the
  unmeasured stack are in `cycle::worker::birth`'s docs.
- [ ] **The harness serves at a 2 s request wait**
  (`worker::testing::HARNESS_REQUEST_WAIT`, a thousand times the crate's
  2 ms), so only the ignored probes exercise the shipped bound;
  `worker::tests::Mutator::start` and `the_siblings`' round wait clear
  `POSTED` by hand, suspending "`FREE` promises an empty P" for those cases.
  A harness at the crate's wait needs consent from a real poll on a loaded
  box; a fixture that disposes pays a collection per batch. Unpriced.
- [ ] **The second heapless arm has no case off Windows.** A heap built and
  then refused by `tls::set` is left heapless by the same two lines as a
  refused allocation (`memory::heap::ll_thread_init`); `refuse_thread_heap`
  reaches the allocation alone and `tls::set` never refuses on Linux, so the
  one case (`…::a_thread_without_a_tls_slot_reports_instead_of_dying`,
  Windows-only) reads three of `a_life_the_allocator_left_heapless`'s six
  promises. The other three need a Windows run.
- [ ] **A root the collector cannot walk circles P and R.** A root sent back
  `Unwalked` — every grant recalled by a mutator freeing past a mark, or a
  pool that refuses every arena of the collector's while the mutator's own
  allocations succeed — is written back into R untraced by the collection
  over P and taken by a later batch, so it and the garbage behind it wait for
  a shortage of memory or the exit; the mutator pays a collection over P per
  turn (`rfc/model/gc/rc-cycle.md`, "The collector's batch"). How often a
  workload reaches it is unmeasured. Whether a collector thread may spend its
  critical reserve and the unbounded spare-cell surplus are recorded in
  `dev/DECISIONS.md` 2026-09-16.
  done: a case that recalls every grant, the turns it takes and what the
  mutator pays per turn, read and recorded, and a ruling on a breaker or on
  keeping the wait.
- [ ] **The collection over P opens a window with no proposal to trace.** A
  P holding only `ReadLive`, `ZeroCount` and `Unwalked` entries still opens
  `ActiveTrace`, traces zero roots and commits an empty membership before the
  disposition, which `retire_candidates_and_dispose_of_verdicts` makes
  without a window (a Critic's finding).
  done: `what_the_poll_costs`-style probe of that collection with and without
  the window, both arms in one session; the window skipped if it pays, or
  the figure recorded as kept.
- [ ] **A weak map, the second death-subscriber kind**
  (`rfc/model/weak-references.md`; Edmond, 2026-09-07: it can wait). Shape:
  a GC-heap entity over `array::Table`, key uncounted and value counted; a
  target's weak-table row becomes a list in its reserved tag bits; the
  displaced value goes to `cycle::reclamation`'s queue inside a teardown
  (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation", step 6)
  and is released inline elsewhere, told apart by a thread-local sink the
  teardown arms. Owed with it: an arena-resident key on the arena's weak
  log, and `HAS_WEAK_REFERENCES` cleared when the last subscriber leaves.
- [ ] **The ladder's refusal has nowhere to go.**
  `InsertOutcome::AdmissionDenied` ends as a null or `false` inside the
  crate, indistinguishable from memory pressure; `rfc/model/maps.md`, "Rung
  three, refusal" raises it as a catchable error, which waits on the
  exceptions work.
- [ ] **The equal-identity trigger's tag test has no test of its own.** In an
  array "the tag equals the incoming string's" names the same set as "not an
  integer key", so the change was verified by reading; `Map` is where the two
  differ, and the test is owed there.
- [ ] **The long-key slot itself.** `strong_hash`'s doc stands in for
  HighwayHash-64 behind a length threshold `rfc/model/strings.md` says is
  unmeasured; blocked on that measurement, with the strings work.
- [ ] **The publication fence's ARM64 price.** `refcount::publish_header`'s
  `fence(Release)` is a `dmb ish` on ARM64, unmeasured, and is measured
  before the first ARM64 build ships (`rfc/dev/DECISIONS.md`, "the
  publication fence lands before its ARM64 price"): `benches/lifecycle.rs`'s
  create/release pair on an ARM64 box, both arms in one session.
- [ ] **The per-process key's Windows randomness source.** `hash/process_key`
  is unix-only, `#[cfg(not(unix))]` a `compile_error!` naming this gap, until
  a session on the Windows box adds the source (`BCryptGenRandom` or an
  equivalent) and runs the gate there. Deferred by Edmond, 2026-08-17.
- [ ] **The collector's birth has run only on Linux.**
  `cycle::worker::birth`'s windows arm (`CreateThread`, `WaitForSingleObject`,
  `CloseHandle`) has run nowhere and cannot build until the key above has its
  Windows source; `pthread_setname_np` is declared for linux and android
  alone; aarch64 checks clean and has run nowhere. What a non-futex unix owes
  the locks is in `dev/DECISIONS.md`, "the collector thread is born by the OS
  entry, on a stack its slot keeps, and is woken by a word".
- [ ] **No ABI entry creates or mounts an arena.** An external caller can
  build an `LLContext` and reach the store barrier, but every `*mut Arena` in
  the crate is made by Rust code inside tests; an embedder needs that entry
  before anything outside this crate exercises the arena paths.

Collection coverage, open since 2026-09-14:

- [ ] **The collection's journal kinds.** `journal/kinds.rs` carries no record
  for a collection's begin or end (`dev/design/debug-modes.md`, §9.5);
  `cycle::collect` records `KIND_EXIT_RESIDUE` alone. A window over a
  collection needs its two ends and which path it took.
- [ ] **A member of a kind other than an object or an array is untested
  through the commit.** A reference box, a template and a class with cells
  outside itself each have an arm in `cells::trace_cells` and
  `cells::sever_cells`, and no case drives one through validation,
  finalization and reclamation as a member.
- [ ] **The refusal path over a component whose member's guard is its last
  reference.** Such a member is freed inside `GuardedComponent::release`, and
  the case that exercises the refusal has no such member.
- [ ] **Two shapes the deny run over a reset inside a collection never
  enters**: an emptied chain — `register` answering true for a block whose
  listed survivors all died inside the reset — and a destructor round that
  escapes something new. Each is one case in
  `cycle::collect::tests::what_a_collection_asks_the_allocator`.

Memory manager, still open. The readings behind the first five are
`dev/RESEARCH.md`, "Memory managers" and "rpmalloc"; each names what would
have to be measured first, and none is measured here.

- [ ] **Batch the cross-thread free, once a workload exists.**
  `Heap::free_remote` and `buffer_arena::post_remote` pay one CAS per item;
  snmalloc pays one per batch. The shape: a bounded thread-local staging
  buffer grouped by block on flush, each group chained through the dead
  slots and pushed by one CAS, flushed at thread exit (`deferred_free.rs` on
  `archive/pre-rc-cycle`); the price is return latency and peak RSS by the
  batch. A per-pair SPSC ring removes the atomic at memory per pair. Order:
  a test heap that frees another thread's objects in bulk, a measurement,
  then this.
- [ ] **Reallocate in place when the class does not change.** `stdapi::ll_realloc`
  allocates, copies and frees on every call, so 40 bytes to 48 costs a block,
  a `memcpy` and a free to move inside one 48-byte slot;
  `stdapi::ll_usable_size` already reads the class size. What comes first is
  a harness: `rptest` in `benches/standard.rs` never reallocates, and nothing
  calls the path while the `#[global_allocator]` install is owed.
- [ ] **Size classes between 8 KiB and one block.** Above `heap::MAX_SMALL`
  everything takes a 64 KiB block. Five classes (10880, 13056, 16320, 21760,
  32640) divide the payload without a tail, hold the worst case to
  1.33–1.5×, and are chosen past `MAX_SMALL` so `CLASS_LUT` stays 514
  entries; cost about 120 bytes per thread and a high block switch rate on a
  two-slot class (`dev/RESEARCH.md`, "rpmalloc"). First a footprint
  measurement in `blocks_out` and RSS past 8192.
- [ ] **The commissioning zero pass: delete it or name its production
  reader.** `Heap::refill` writes eight bytes into every slot of an entity
  block unconditionally, up to 4080 stores at the 16-byte class; the walker
  that needed the invariant, `for_each_entity_slot`, has no production
  caller since `rc-walk` went, while `heap.rs`'s module doc says the pass
  was built for `rc-walk` and outlives it because `rc-cycle` reaches its
  shadow rows by the same arithmetic over the same blocks. Whether a
  production reader still needs a free slot to read as refcount 0 is
  unsettled: either the pass goes with the doc corrected, or its reader is
  named and the zeroed-block flag of `dev/RESEARCH.md`, "rpmalloc" (a flag
  that names the stride it holds for, carried across recycling) is what
  would retire the cost. Refill runs about 0.00003 times per allocation on
  the steady-state benchmarks — a reading, not a measurement.
- [ ] **Return memory to the OS, and cache huge mappings.** A region is an OS
  mapping since `8208815` and `os::unmap` is used by the large-run path, so
  the item needs a reading on a test heap, not a mechanism: rpmalloc decommits free pages
  past a per-type threshold and caches freed huge mappings in a 32-slot cache
  evicted by age; ours never come back and `LARGE_RUN` unmaps on every free.
- [ ] Buffer *K*, the memory-pressure mode thresholds
  (`rfc/model/memory/buffers.md`) and the per-block dense/sparse reset
  threshold (`rfc/model/memory/arena-reset.md`) stand on borrowed numbers; by
  the ruling of 2026-09-19 (second) they are read on a parameterized test
  heap when a stage takes them, the entry naming the parameters.
- [ ] Allocation telemetry layer 2 / debug mode — full design in
  `dev/design/debug-modes.md`, build order its section 10; item 1, the event
  journal, is built, the rest unscheduled, the per-structure breakdown of
  collection's logical bytes (§8, an axis A feature; Edmond, 2026-09-01: not
  in a production build) among it.
