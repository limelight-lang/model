# The general algorithm: Edmond's ten principles over the recycler

Wave 1 of the rework Edmond set on 2026-10-06 (`dev/DECISIONS.md`, the
entry of that day): the general algorithm on one page, with no code and no
thresholds; then the Critic, Claude's answers, the Sage, and Edmond. Polish,
code, and per-function analysis come in later waves, each on his word. The
detail it rests on is `recycler-over-counts.md` (§2–§5f); where this page and
that one differ, this page is the proposal and that one the accepted design
until Edmond rules.

## The principles (Edmond, 2026-10-06)

1. The mutator alone decides when to open a window.
2. The batch is far larger than 1,024 roots (PHP's buffer holds 10,000;
   perhaps 16k); the size is measured, not chosen.
3. The collector does not start at the window's opening, but once a clear
   delta has formed.
4. The mutator executes several times fewer instructions than in the
   default build.
5. The collector executes more, but does not enter freshly tagged entities,
   unless their count is 0.
6. The mutator's +1 and −1 are optimised and reviewed with care.
7. Neither side waits for the other, except at edge cases.
8. Putting entities into the roots is cheap, and a long-lived entity is not
   put there again and again.
9. The trace is analysed and judged on its own. With window tags the trace
   need not enter fresh tags, no epoch is needed for the judgement, and one
   stage suffices: the set is judged at once.
10. Destructors run on the mutator only: it accepts the collector's proved
    cycles and runs them without judging them again.

## The algorithm

**The mutator.**
- A −1 that leaves a count above zero puts the entity into its root queue R
  once in its life (the buffered bit); an entity read live several times
  waits out longer lanes before it is offered again (P8).
- Every count store and every slot store the trace reads first writes the
  current window's number F into the entity's (or holder's) tag byte, then
  the data with a release (P6). Open: whether a −1 that does not reach zero
  needs the tag (Edmond's hypothesis, below).
- At its own poll, when R holds a batch, the mutator turns its window to F
  and offers the batch's end on its token. Nothing else turns the window
  (P1). The offer is the delta: R's roots, frozen at the frame (P3).
- It does not wait for a take: it goes on, and withholds returns of memory
  until the collector releases the token or a mark withdraws the offer (P7).
- At a later poll it takes the cycles the collector proved and holding
  destructors, and runs their destructors without judging them (P10).

**The collector.**
- It takes an offered batch when it comes round, never before the offer
  (P3), and traces the batch's roots over its own shadow rows.
- It does not expand an entity whose tag equals F unless its count is 0:
  such an entity was touched after the frame, so it and what it reaches are
  live for this batch (P5, P9).
- After the trace, one acquire fence and one tag read a member: a set none
  of whose members carries F is garbage and judged at once, in the same
  batch (P9). No epoch enters the judgement.
- It frees the garbage that holds no destructors and no weak references
  itself; the rest goes to the mutator as proved (P10). It releases the
  token. Neither side waits except where the table names an edge case (P7).

## Where the accepted design and the build stand

Read on 2026-10-06 from main at `4c96740` by three read-only audits, from the
code, not run; the counts are from `dev/BENCHMARKS.md` with their dates.

| P | Accepted design (§5f) | As built | Gap |
|---|---|---|---|
| 1 | mutator offers at its poll; §2.1, §4.7 and §1 still describe the consent and the checkpoint | only the offer turns the window (`offer.rs`, from the poll) | the old sections to rewrite; the collector still gates the offer: none without a standing collector, the standing interval on its round clock, a withdrawal waits its next round, cap 0 turns an offer into an ask |
| 2 | batch ≤ `BATCH_BOUND` 1,024; offer at 64 | as designed; no threshold above 1,024 ever run | the workspace copy caps a batch at 1,780 and a threshold at about 7,100; a 10k–16k batch needs the workspace and the read-ahead reworked |
| 3 | offer at 64 roots, a standing R after 4 s, or merged lanes due; the trace starts at the take | as designed: the take starts at once, at the start of window F | "a clear delta" is defined nowhere (design, PLAN, DECISIONS); the only delta read is the Δ-test after the trace; to rule |
| 4 | — | B's mutator CPU 1.2–1.7× under A's on `web-heap` and the ring loads (2026-10-05); no instruction counts (no PMU) | "several times" not met; per operation B executes more, the gain is the owner's collections leaving the mutator |
| 5 | tag read only at the judgement; the prune at F is an Open item (no room on the row) | the trace never reads the tag | not built; needs a side bitmap or another place for the colour |
| 6 | every count store tagged and released | `ll_retain` 13 → 24 instructions, `ll_release` 46 → 59 (2026-10-04); linked (`benches/lifecycle`, 2026-10-06) the window's read is `mov %fs:0`, `lea`, a byte load, with no call, but the function keeps the frame (three pushes, two pops) the unlinked call needed and splits the `inc`; the window is a plain `thread_local!`, not the heap's fast TLS (`memory/heap.rs`, `mod tls`), so on Windows it pays the module chain the heap's fix removed | candidates: the window beside the heap's fast TLS slot or in `ctx`; read the window before the count; no tag on a −1 to 0; candidate registration out of line; AArch64 unmeasured |
| 7 | the consent and the checkpoint wait gone; returns withheld under `OFFERED`, no blocking | slow paths block on a take (pressure, exit, fire, teardown pass, compaction, reset), up to a whole commit (36 ms read at the 1 ms pace); one batch a mutator at a time, the next waits on the owner's drops; a thread that never polls is never collected | the commit wait and one batch at a time are not edge cases; to rule |
| 8 | buffered bit, lanes 1/3/7 | active in B, but an X turn hands back every lane on a caught-up thread, and B's epoch turns only by X | the lanes do little in B; a refused set's roots also return once |
| 9 | trace → scan → Δ-test → split → post; epoch in use | as designed; the owner re-meets every member of a proved set and confirms by the counts' sum | two stages and the epoch remain |
| 10 | S (destructors, weak) goes to the owner; S if cyclic goes the exact way (§6.15) | the collector tears down C (no destructors) itself; the owner confirms a proved set by the counts' sum and walks it in full when the sum differs (always in debug), and re-traces every unproved set | the confirm and the re-traces are the judging P10 rules out |

## Open for the waves

- **A −1 that does not reach zero, untagged.** Dropped on 2026-10-05 on a
  Critic's two counterexamples without Edmond's word; a Critic and the Sage
  are judging it again with the release kept and a compiler obligation for
  borrowed references.
- **The prune at F during the trace.** A tag read during the trace can only
  send entities to live; the fence and the tag pass over the set after the
  trace remain for soundness, unless an asymmetric barrier replaces them.
- **The batch size.** Measured at 64 and at a 1,024 ceiling only; 10k and
  16k are to be measured once the workspace allows them.
- **The second stage.** What of the owner's recheck and the epoch can go
  once the judgement is immediate and the prune at F is built.

## Wave 1: the Critic's findings and Claude's answers (2026-10-06)

The Critic (opus) attacked "The principles" and "The algorithm", reading the
code, running nothing. Each finding with Claude's answer; the Sage judges
both next, and nothing here is accepted before Edmond rules.

1. **The prune at F keeps a garbage structure forever (liveness).** Stale tags
   are cleared on members of W only; a pruned member never enters W, so a
   stale tag equal to F prunes it again at every attempt. *Accept.* A root
   whose trace pruned anything is posted unwalked with a no-prune bit (the
   second-chance bit's mechanism); its next trace enters everything, W forms
   and its tags are cleared.
2. **"Live for this batch" in the live colour proves liveness.** The live
   colour feeds the read-live posts, the lanes and the stamps, so garbage
   dropped after the frame waits a lane. *Accept.* The prune is a fourth
   outcome, neither live nor member (the side bitmap §5f's Open item asks
   for); a root met only through it is posted unwalked and stamps nothing.
3. **The epoch cannot go.** The judgement reads no epoch already; the epoch
   is the clock of the maturation stamps and the lanes, and a stamp cannot be
   retired by window tags. *Accept.* P9's "no epoch" holds for the
   judgement; the epoch stays as the stamps' clock. Whether the lanes keep
   it is finding 9.
4. **P10 has two readings.** Dropping the validation after destructors is a
   use-after-free (a `__destruct` stores `$this` into a global). Dropping the
   counts'-sum confirm before them buys nothing measurable. *Accept,* with
   the Critic's alternative to rule on: the owner's confirm reads the tags of
   S for F in place of the sum (the window cannot turn while P stands), which
   also lets a weakly held S be proved instead of retraced. The validation
   after destructors stays: it is PHP's semantics, not a second judgement.
5. **P2: a 10k–16k batch is clamped and defeats itself.** P is one block
   (about 8,159 verdicts); the withholding marks (8,192 deaths, 16 blocks)
   recall a long trace, a cut batch goes to the owner and K halves.
   *Accept.* Before P2 is measured, P grows and the marks scale with the
   batch, or a cut batch's work is kept; both are prerequisites, not tuning.
6. **P3: "a clear delta" has two readings.** If it is the offer, it is built.
   If the collector waits after the offer, the withheld returns reach a mark,
   the offer is withdrawn, and no batch is ever taken. *Accept the risk;*
   the meaning is Edmond's to state. Claude's proposal: the delta is defined
   on R (roots never offered, the oldest's wait, lanes due), the take follows
   at once, and no delay after the offer is built before it is measured.
7. **P5: "unless their count is 0" has three readings.** *Accept reading
   (a):* an F-tagged entity at count 0 gets no row and no expansion, as the
   corpse rule does today; expanding a corpse subtracts its released
   children twice.
8. **P9: "a set none of whose members carries F" drops the U split.**
   *Accept.* U, the closure of the touched members, is refused; W − U is
   judged.
9. **P8: the lanes against turns by X.** B turns only by X, so the lanes
   wait 8, 24, 56 s, and a larger offer threshold would release every lane
   at every X turn. *Accept.* Proposal: the lanes count the mutator's own
   offers, with a time floor, independent of the epoch and of the batch
   size.
10. **P7: inherent and removable waits.** Inherent: withheld returns from
    offer to release, a recall's stride, exit and teardown waiting out the
    heap's reads. Removable: the commit-length wait (unproved), one batch at
    a time (needs an "in a posted set" mark first, or a destructor runs
    twice), the withdrawal's wait for the next round (policy), a thread that
    never polls. *Accept.* Proposal for the last: the park
    (`ll_gc_blocking_call`) offers when R is not empty, still the mutator's
    decision, at a point where every reference is counted.
11. **P4 cannot hold per operation.** B's +1/−1 carry A's work plus a tag.
    *Accept the reading, not the restatement:* P4's goal is Edmond's; the
    Critic's "GC-attributable mutator instructions several times fewer" is a
    measurable form for him to accept or reject.

Sound as stated, by the Critic: P1 (but the thread that never polls), P6 as
a goal, P8's first half, P10's first clause, the judgement's fence and tag
read, and the prune's soundness for frees.

## Wave 1: the Sage's judgement (2026-10-06)

The Sage (Fable) read the page, the Critic's findings and Claude's answers
against the design and the code, running nothing.

**Conclusion.** The amended algorithm is sound for what the collector frees,
but not yet live in the sense P8 asks; nine of the eleven findings are
right and Claude's answers hold; two the Sage answers differently.

- **Finding 1, answered differently.** The window advances at every offer
  (`the_next_window`), so a member pruned in F carries a stale F in F+1, is
  expanded and cleared; the repeat is the 1/255 coincidence, not forever.
  The no-prune bit buys nothing; the existing second-chance bit is the
  liveness rule: pruned once, posted unwalked; pruned twice, read live.
- **Finding 2, answered differently.** A member pruned at F leaves its
  out-edges unsubtracted and unrecorded, so a ring's root reads live with
  nothing in the record tying it to the prune; telling it apart needs the
  taint over the raises the design rejected (§5a item 5). Today the same
  ring is refused as U and requeued once; with the prune it waits a lane.
  The prune at F is worse for held garbage than the built U split; its only
  gain, trace work, is unmeasured.
- **Finding 4.** The owner's tag read in place of the sum is sound and
  stronger: a weak upgrade is a count store and tags; the window cannot turn
  while P stands. For an S that is not weakly held the confirm is redundant,
  so P10's first clause can hold literally there.
- **Finding 5.** Scaling the marks with the batch is a trade of P2 against
  P7 to be priced in bytes; a longer trace is a longer window, so the
  refusals grow with the batch as well.
- **Finding 6.** A third reading of "a clear delta": the Recycler's
  deferral, roots old enough that the transient ones died by counting. The
  record scan's read-live count is 2.5 times the heap scan's, unexplained.
- **Finding 9, answered differently.** Not a third clock: the epoch turns on
  this thread's taken batches, with X as the floor; the caught-up rule is
  decoupled from the offer threshold first.
- **Finding 10.** `ll_gc_blocking_call` exists in no source file; an offer
  before a blocking call is a new export. The commit-length wait is one act
  by ruling; its only lever is a second collector.
- **Finding 11.** The gain is the owner's collections leaving the mutator;
  callgrind gives instruction counts where the rig has no PMU.

**Ranked open problems.** P5's prune regresses liveness against the second
chance; `write_through` is the one untagged slot store, closed only by its
tagged count stores; eight-bit windows turn with the offer rate; the offer
is gated by the collector and the drops backlog, so P1's letter needs its
edge cases listed; a lost wake under `OFFERED` can withdraw at a mark round
after round; the owner's second stage stays for S.

**Questions for Edmond** (the Sage's recommendation in brackets).
1. P3's "clear delta": the offer, a wait after it, or roots older than an
   age? (the offer now; measure the age)
2. P5's prune at F: measure its share of the trace's expansions first,
   build it with a side bitmap, or drop it? (measure first)
3. P9: refuse W whole or U only? (U)
4. P10: the owner reads one tag a member before destructors, in place of
   the sum? (yes)
5. P2: the mutator's memory withheld during a 16k batch: marks times 16, or
   a byte bound? (a byte bound, measured)
6. P8's clock: the epoch re-based on taken batches, or a clock on offers?
   (the epoch)
7. P7: a second collector to take while one commits, or cap 1 stands?
   (measure first; cap 1 stands)
8. P4's form: "GC-attributable mutator instructions several times under
   A's", by callgrind? (yes)
9. The untagged −1 not reaching zero: an arm after P6's cheaper wins, or
   not? (after: it saves two instructions where the TLS read and the frame
   cost nine)

**Order of the waves.** Polish: §1, §2.1, §4.7 rewritten to the offer, an
edge-case table for P1 and P7. Measure before code: the share of
expansions at F, corpses and read-live per batch against the oldest root's
age, windows a second and coincidence refusals, owner time by posting kind,
withdrawals at a mark, callgrind counts of `ll_retain` and `ll_release`.
Then code: P6 (the window beside the heap's fast TLS, the frame removed);
P2's prerequisites (workspace and P for 16k, marks in bytes, then 1k, 4k,
16k); P8–P10 (epoch re-based, the sum replaced by the tag read); P5 only if
its measured share is material. Per-function analysis last.

## Edmond's answers (2026-10-06)

- **Q1, P3's "clear delta"** (Edmond, verbatim): «коллектор стартует после
  того как мутатор дал сигнал о том, что он закрыл окно и таким образом
  образовал дельту. Чёткая дельта, это когда в памяти есть отметка 0 и 1. и
  таким образом коллектору не нужно собирать мусор два раза. нет. он идёт и
  собирает сразу. видит 0 - 100% мусор, 1 - ещё нет.» The collector starts
  on the mutator's signal that it closed its window; the delta is a mark in
  memory, 0 or 1; the collector collects at once, in one pass: 0 is
  garbage, 1 is not yet. Read against the build: the signal is the offer,
  which turns the window to F; "1" is a tag equal to F and "0" any other,
  so the marks need no clearing across the heap at each window (the stale
  clear touches members only). "0 is garbage" holds of a member of W not
  reached from a member tagged F (the Critic, below), not of any entity
  and not of every white member.
- **Q2, P5's prune at F** (Edmond: «согласен» with the Sage): its share of
  the trace's expansions is measured before anything is built.
- **Q3, refuse W or U** (Edmond, verbatim): «в наборе - ничего не значит.
  вопрос в том, есть ли кольцо... если коллектор находит кольцо в котоорм
  нет элементов с 1, в таком случае он уничтожает кольцо в любом случае.
  элементы с 1 ждут следующего окна». The unit is the ring, not the set: a
  ring with no member marked 1 is destroyed; members marked 1 wait for the
  next window. Read against the build: U, the touched members and what they
  reach, is refused and W − U freed (`cycle/split.rs`); a ring with no
  member marked 1 that a ring with one refers into waits too, since the
  touched member may be live and hold it.

## The Critic on Edmond's rules (2026-10-06)

Edmond asked that his own rules be judged («мои правила могут быть не
верными!! критик обязан их судить!»). The Critic (opus) took his answers to
Q1–Q3 literally, reading the code, running nothing.

1. **"0 is 100 % garbage" and "a ring with no 1 is destroyed" free live
   objects.** At the frame `G.a → x1`, `x1 ↔ x2`, `x2.z → z1`, `z1 ↔ z2`.
   During the trace the mutator runs `$t = G->a; $t->next->g = $t; G->a =
   null;`: x1 and x2 are tagged F, the trace reads x1's count stale, and
   all four come out white. x1 is live through `$t`, so z1 and z2 are too;
   the ring {z1, z2} carries no 1. The suspect part is what a touched
   member reaches, not a ring: U, the members tagged F and all they reach,
   waits; W − U is freed at once. A ring with no 1 that refers *into* a
   touched ring may be freed. This is the built split (`cycle/split.rs`).
2. **"Destroys in any case" needs three guards.** A weakly held member goes
   to the owner (an upgrade after the reading resurrects it); "destroy" is
   finalize, with the revalidation after destructors kept; S is closed
   under successors, so no destructor reads a part the collector freed. A
   fourth for later: once more than one batch may be in flight, a ring
   posted and not yet taken needs an "in a posted set" mark, or it is
   proved and freed twice.
3. **"1" means two things.** In R1 it is a window's number (below 2 may be
   collected), in R2 "not yet". Read as the window just closed, every root
   carries it (each entered R by a −1 tagged in that window) and nothing is
   collected; read as a bit set on every write and never cleared, every
   entity reads 1 for ever. The test is equality with the frame F of the
   batch's offer.
4. **The tags are read after the trace, not during it.** Read when the
   trace meets an entity, x1 and x2 of finding 1 read 0 before the stores
   that touch them, and live x1 is freed. One acquire fence after the
   trace, then one byte a member, is required and is the cheapest sound
   form.
5. **"Below 2" and "never cleared" do not survive eight-bit windows.**
   After the wrap "below F" refuses every stale tag; tags never cleared
   keep a ring grown across 255 windows refused for ever. The CAS clear on
   members of W, after their reading, is what the rule needs; no clearing
   across the heap is right.
6. **Two preconditions.** The window closes only where every reference is
   counted (§7.16: at a poll); and every count store tags, so an untagged
   −1 conflicts with R2 as stated.
7. **P5, P9, P10 as worded.** P5 literally enters corpses at count 0, the
   opposite of reading (a), which is right. P9's "judge once" holds for
   W − U; U waits a window, a stale coincidence refuses once, S keeps the
   revalidation after destructors. P10 holds before destructors for an S
   with no weak references.

**Sound as stated:** none of R1–R3 word for word. Sound as the build reads
them: the equality test with F, the CAS clear on members only, the fence
then the tag pass, the U split and the S closure.

## Open: the collector kills the weak references itself (Edmond, 2026-10-06)

Edmond (verbatim): «коллектор может узнать есть ли на объект слабые ссылки.
и если они есть... такой же флаг должен быть кажется у объекта да? короче
если есть он сперва должен убить слабую ссылку через CAS + безопасная
операция между потоками и только потом собрать мусор - это надо
продумать!» The collector reads `HAS_WEAK_REFERENCES` (it does at the
Δ-test); where a member has weak references it first kills them by a CAS,
safely across threads, and only then frees. The race to think through:
`$weak->get()` reads the target and then retains it; a kill between the two
retains a freed entity, and a kill after the retain leaves a live entity
whose weak references read null. `get()` and the kill must agree, by the
window's tag or a CAS on `get()`'s side. To the Critic, then the Sage.

## Edmond's answer to Q4, and Q5–Q8 settled by Claude (2026-10-06)

- **Q4, P10** (Edmond: «конечно убираем лишние проверки»): the owner's
  counts'-sum confirm goes for a proved set with no weak references; its
  destructors run at once. The revalidation after destructors stays
  (Edmond: «это верно»). Weakly held sets wait on the open item above.
- Edmond asked not to be asked what follows from his principles and the
  Sage's advice; Q5, Q7 and Q8 are settled by Claude on the Sage's
  recommendation ("settled on the Sage's advice", not Edmond's ruling),
  each open to his overturning:
  - **Q5, P2:** the mutator's memory withheld during a large batch is
    bounded in bytes, measured, not the marks scaled by 16.
  - **Q6, P8:** the lanes keep the epoch as their clock, the epoch turning
    on this thread's taken batches with X as a floor; the caught-up rule is
    decoupled from the offer threshold first.
    Not settled: it keeps the epoch P9 says is not needed (the Critic's
    finding 3), so it goes to Edmond with the counterexample.
  - **Q7, P7:** cap 1 stands; a take while one commits is measured before a
    second collector is considered.
  - **Q8, P4:** measured both ways by callgrind, the mutator's instructions
    in all and those the collection costs it, against A.

### The Critic on killing weak references, and Claude's answers (2026-10-06)

The Critic (opus) read `weak.rs`, `weak/table.rs`, the split, the commit
and the finalization, running nothing.

1. **`get()` loads the target, then retains it: no collector-side CAS or
   fence closes the gap.** The mutator loads X; the collector nulls the
   cell, fences, reads X's tag 0 and frees X; the mutator's retain writes
   into the freed slot and `get()` returns it. *Accept:* the check must sit
   on the mutator's side, after it publishes a claim.
2. **The kill defeats itself wherever it is placed.** Before the Δ-test a
   live member's weak reference reads null for good; after it, an upgrade
   in between is missed; killing member by member and undoing a partial
   kill lets `get()` return null and later the object. *Accept:* the
   condemnation is revocable while pending and final for the whole set at
   one point.
3. **The collector cannot reach the cell, and nulling it strands the
   owner's table row.** The weak table is the owner's thread-local, moved
   in place by its removals and freed at growth; a row left for a slot
   reused by X′ hands `WeakReference::create($x2)` a dead cell. *Accept:*
   the collector condemns the target, never the cell; the owner removes
   the rows and nulls the cells.
4. **The owner's flag write on a weakly held member races the commit's
   `DEAD_IN_PLACE`** (two-byte load and store against the same bytes; a
   double free suspected, not shown). *Accept:* `weakref_die` makes the
   same claim and writes no flags on a dead target.
5. **`WeakMap` has nothing to CAS**: its entries live in the map's storage,
   the owner's. *Accept:* purged by the owner before the splice when
   `WeakMap` is built.

**The Critic's protocol.** Two reserved tag values, DYING and DEAD (windows
wrap at 253). The collector: a verdict word set pending; each weakly held
member of C CAS 0 → DYING (any other value is an upgrade since the test:
abort); one CAS pending → committed for the whole set; then DEAD and the
commit, and a list of killed targets posted with the chains. `get()` and
`weakref_die` claim by a CAS on the tag byte before the retain: DEAD reads
null, DYING vetoes a pending verdict or reads null after a committed one.
Cost: a byte load and a `lock cmpxchg` a `get()` (about 20 cycles,
unmeasured); the owner removes the rows and nulls the cells before it
splices the chains. **A cheaper fallback, no change to `get()`:** the owner
reads the tags of C's weakly held members at its poll (the window cannot
turn while P stands); none F, it nulls the cells and commits C itself.

**Claude's answer.** Both hold as the Critic argues them; the fallback is
the smaller step, the protocol the one that keeps the commit on the
collector. Measured first: `web-heap` produced no weakly held set in six
cells (`tag_sets_weakly_held` 0, 2026-10-06), so neither is built before a
load with weak references shows the share. To the Sage, then Edmond.

### The Sage on killing weak references (2026-10-06)

The Sage (Fable), reading the code, running nothing: the Critic's five
findings are right, finding 1 the decisive one. The DYING/DEAD protocol is
sound with four amendments (the verdict has three states, pending, vetoed,
committed; it is read with an acquire after the DYING read; the abort's
undo cannot be stopped, or the verdict gains a "none" state; the kill list
goes ahead of the chains' splice on every apply path), and rests on one
premise to write into the design: every cell write, slot return and window
turn is the owner's. Its price is a `lock cmpxchg` on every `get()` of
every program and five new cross-thread invariants. The fallback (the owner
reads one tag a weakly held member of C, kills and commits C itself) is
sound and the smaller step. **Recommended:** build neither now (`web-heap`
and S68.5 read no weakly held set; the build handles one correctly as S);
build the fallback when a load with weak references shows such sets, the
protocol only if the owner's commit then shows as a material pause; make
`tag_sets_weakly_held` a gate column. Edmond's wording is right about the
gate, the order (kill, then free, no user code between) and the need for a
CAS; the CAS sits on `get()`'s side, and what is killed is the target's
tag, not the cell. For Edmond to rule.

### Edmond on the compiler's order of retain and release (2026-10-06)

Edmond (verbatim): «компилятор делает не совсем так. когда он видит unset он
понимает, что $c нужно увеличить на +1. и он сперва увеличивает $c, а потом
уже убирает ссылку.» The compiler retains a reference read out of an entity
before it releases the reference it was read through, which is obligation
16b of the untagged −1 (the Sage). Edmond's statement, not yet checked:
the Critic looks for orders that break it (a borrowed read with no unset:
an overwrite, a scope exit, an argument, a return, an elision the compiler
takes where it proves the holder survives), then the Sage. If it holds it
goes into §7 as the compiler's obligation, citing the check.

- **Q9, the untagged −1** (Edmond, verbatim): «я не эксперт и не знаю ...
  стоит ли его делать? ну если пока это мешает - не делай. потом сделаем
  эту оптимизацию». Deferred: not built now, kept as a later optimisation,
  with its conditions (the holder's tag in `write_through`, the compiler's
  retain-before-release under the Critic's check).
- **Q6, the epoch** (Edmond, verbatim): «эпоха нужна... я не уверен втом,
  что она не нужна». The epoch stays, the clock of the maturation stamps
  and the lanes; P9's "no epoch" holds for the judgement only. Settled on
  the Sage's advice, open to Edmond's overturning: it turns on this
  thread's taken batches with X as a floor, and the caught-up rule is
  decoupled from the offer threshold first.
- **Weak references** (Edmond: «решай это сам»): settled by Claude on the
  Sage's advice: nothing is built now; `tag_sets_weakly_held` joins the
  gate's columns, and the fallback (the owner reads the tags of C's weakly
  held members, kills and commits) is built once a load with weak
  references shows such sets.

## Wave 2: the algorithm polished (Claude's draft, 2026-10-06)

The algorithm of wave 1 with every answer and settlement above folded in;
the Critic attacks this draft, the Sage judges it, and only a real fork goes
to Edmond. `recycler-over-counts.md` §1, §2.1, §4.7, §5.9, §6.15, §7.16, §8
and §9 are rewritten to it in the same commit.

**The mutator.**
- A −1 that leaves a count above zero puts the entity into R once (the
  buffered bit). An entity read live again waits out the lanes 1/3/7 before
  it returns to R (P8). The lanes' clock is the epoch, turning on this
  thread's taken batches with X as a floor; the caught-up rule is
  decoupled from the offer threshold (Q6, settled on the Sage's advice).
  One clock: the stamps' epoch and the lanes' are bits of one cell, so the
  stamps retire with it; taken batches are a second trigger beside the
  proofs' price (`SPENT_PER_PROOF`), not a replacement.
- Every count store, the −1 included, writes F into the entity's byte 7,
  then the count with a release; every slot store the trace reads tags its
  holder, then the slot with a release (P6), but for the untagged stores of
  §5f's table: `write_through`, root slots, severs, keys before `set_used`.
  `write_through` stays the open problem the Sage ranked. The untagged −1 is
  deferred (Q9) with its two conditions written down.
- At its own poll, with the gate open and the token `FREE`, when R reaches
  the threshold, or has stood below it for the standing interval, or a lane
  merged into it, the mutator offers: one CAS `FREE` → `OFFERED` carrying
  R's ceiling, then the window turned F−1 → F. Only this turns the window
  (P1); the offer is the clear delta, the frame the collector judges
  against (Q1, P3). The threshold is 64 today; 1k, 4k and 16k are measured
  once the workspace takes them, the memory withheld meanwhile bounded in
  bytes (Q5, P2). The poll does not offer while slices of the collector's
  drops stand to be applied (`gc.rs`).
- It never waits for the take (P7): it runs on and withholds returns of
  memory under `OFFERED` and `COLLECTOR`; a mark reached under `OFFERED`
  withdraws the offer by CAS, without waiting.
- At a later poll it applies the collector's drops and runs the
  destructors of a proved S at once, without judging it again; the
  revalidation after destructors stays (Q4, P10). A weakly held S comes
  unproved and goes the exact way, as today, until a load shows such sets
  (weak references, settled on the Sage's advice).

**The collector.**
- It takes an offer by CAS `OFFERED` → `COLLECTOR` with an acquire, then
  reads F and the ceiling; it never asks and never waits for the mutator.
- It traces the first K roots under the ceiling over its shadow rows and
  records every edge it subtracts. It reads no tag during the trace: the
  prune at F waits for the measured share of expansions it would save (Q2).
- After the trace: one acquire fence, then one tag byte a member of W. U is
  every member tagged F or whose address cannot be read, and everything it
  reaches over the recorded edges; U waits for the next window, W − U is
  garbage, judged once (Q3, P9). U refused a second time reads live and
  waits out the lanes (§4.8's second chance, accepted 2026-10-05), or, with
  an unreadable member, stays in W as a seed of S, posted unmarked: Q3 for
  the first refusal, P8 for the second, since the first attempt cleared
  every stale tag, so a second F is a real write and the root is live at
  that frame (the Sage).
  Stale tags (neither 0 nor F) on members are cleared by CAS; nothing else
  in the heap is cleared.
- Of W − U, S (reached from a destructor, a weak reference, or a member the
  collector does not free: another owner's or a non-slotted block, outside
  cells, a kind `eligible` refuses; closed under successors) goes to the
  owner, proved unless weakly held; C is freed by the collector.
  It releases the token.

**Where the mutator alone does not decide (P1), and where a side waits
(P7).** Read from `cycle/offer.rs` and `cycle/token.rs` on 2026-10-06 and
the audits of the same day, not run.

| case | what happens | proposal |
|---|---|---|
| no collector stands, the elder's birth refused | no offer (`offer.rs`, `a_taker_stands`) | stays: an offer no one can take withholds every return for nothing |
| the token `POSTED` (the last batch's set waits for the owner's poll) | no offer until the poll takes P: one batch a mutator at a time | stays; measured: the time from post to apply, and R's length at each offer |
| slices of the collector's drops stand, the token `FREE` | the poll applies one slice and returns before the offer (`gc.rs`) | stays; measured: the share of polls it holds back |
| no collector thread stands (the first offer, or after the elder unwinds) | the poll spawns one, joining the previous thread (`a_taker_stands`, `birth.rs`): the mutator waits on a collector thread's teardown | an edge case of P7, rare; counted |
| the offer's wake | locks the collector's mutex (`wake_pending`) | stays: a short lock, not a wait on the collector's work |
| the take refused by a reading hold (exit `RETURNING`) | the collector skips the offer; it stands, returns stay withheld | an edge case; measured with the withdrawals |
| an offer withdrawn at a mark | no new offer until a collector's round begins (`WITHDRAWN_AT_ROUND`) | stays: else an untakeable offer is made and withdrawn at every poll |
| the standing interval | read on the collector's round clock, so the poll reads no clock | stays: a ring stands at most one fallback interval late |
| cap 0 | the elder turns `OFFERED` into `ASKED` (`token.rs`) | stays: cap 0 is the switch that hands collection back to the owner |
| the gate closed, the slot-free path | no offer | stays: references there may be uncounted (§7.16) |
| a thread that never polls | offers nothing, its R is never collected | an edge case; an offer before a blocking call is a new export, left for the per-function wave |
| a slow path takes the token under `COLLECTOR` (pressure collection, exit, explicit fire, the teardown pass, compaction, the record reset) | it recalls at the stop level and waits for the collector to stop, up to a whole commit (`take_recalling`) | an edge case by ruling (the commit is one act); measured: how often and how long, per load, before a second collector is weighed (Q7) |
| a mark reached under `OFFERED` | the offer is withdrawn, no wait | none needed |
| the collector | waits for no mutator: the take is one CAS, there is no handshake after it | none needed |

**Polished reading of the principles.** P1: the mutator alone turns the
window and makes the offer; the cases above can keep it from offering,
never make it offer. P7: neither side waits in the steady state; the one
waits are a slow path's take during a commit and a collector thread's
birth, edge cases to be counted. P9: one judgement for W − U; U waits one
window, and a lane if refused again; the epoch stays as the lanes' clock,
not the judgement's.

**What the measurements before code are** (the Sage's order): the share of
the trace's expansions at F; corpses and read-live per batch against the
oldest root's age; windows a second and coincidence refusals; the owner's
time by posting kind; withdrawals at a mark; slow-path takes during a
commit; `tag_sets_weakly_held`; callgrind counts of `ll_retain` and
`ll_release`, all of the mutator's and the collection's share, against A;
the time from post to apply and R's length at each offer; bytes withheld a
batch and the offer-to-take latency; first and second refusals a batch
(`split_counts`); polls held back by standing drop slices; and why the
record scan reads 2.5 times the heap scan's read-live.

### Wave 2: the Critic on the draft, and Claude's answers (2026-10-06)

The Critic (opus) read the draft and the code, running nothing; nine
findings, each answered in the draft above or in `recycler-over-counts.md`.

1. "U waits one window" left out the second chance: refused again, U's
   roots read live and wait a lane (`worker.rs`, the split; §4.8). Written
   in. Edmond's Q3 («элементы с 1 ждут следующего окна») speaks of one
   refusal; whether a second refusal may send U to the lanes is put to the
   Sage as a possible fork with Q3.
2. §4.8 said a touched set is not judged and a weakly held S is proved;
   the code splits U and posts a weakly held S unmarked. §4.8 and the draft
   rewritten to the code.
3. Not every slot store tags its holder: §5f's table lists `write_through`,
   root slots, severs, keys before `set_used`. Written in; `write_through`
   stays open.
4. The P1/P7 table missed four cases: drop slices standing hold the offer
   back (`gc.rs`), a collector thread's birth joins the previous thread,
   the wake locks the collector's mutex, a reading hold leaves an offer
   standing. Added as rows.
5. §5f put the window's turn before the swap; the code stores F on the
   token, swaps, then sets the thread's window. §5f rewritten to the code.
6. U's and S's seeds were narrower than the code (unreadable members;
   members the collector does not free). Written in.
7. §1 said the epoch is as in the default build; Q6 re-bases it. Written
   in. Whether re-basing moves the maturation stamps or only the lanes is
   put to the Sage.
8. The handshake in §9 renamed to the offer's model.
9. Five measurements added to the list.

### The Critic on the compiler's retain before release (2026-10-06)

The Critic (opus) read the rfc and the runtime, running nothing; the
compiler is in neither repository, so what it does is unchecked; what the
rfc tells it to do is checked.

**Verdict.** Edmond's statement matches the chain rule
(`rfc/model/memory/static-lifetimes.md`) for `unset`, and is not enough as
the obligation: `unset` is one of about eight operations that break the
path a borrow is read through, and the common one is a slot overwrite.
Stated over the whole path, the order makes the untagged −1 that does not
reach zero sound, with the −1 kept a release store.

1. **An overwrite, no `unset`.** `function swap($new) { $old = $this->h;
   $this->h = $new; return $old; }` with `x ↔ y` held by `$this->h`: the
   store drops x 2 → 1 untagged, the collector reads `{x, y}` white and
   untouched and frees them, and `return $old` retains freed memory.
   Tagged −1: refused. The chain rule: converted before the store.
2. **"The last counted reference" is decided by counts**, which a ring
   holds up; the rfc rejects the same reasoning twice (Y11,
   `questions.md:772`; `static-lifetimes.md`). What is forbidden is sinking
   the retain below the −1 or hoisting the −1 above it; sinking a release
   is harmless.
3. **Four rfc texts, four policies:** Y11 (a local always counted,
   strictest), the chain rule (Edmond's), `lowering.md` and
   `arc-optimizations.md` (pair cancellation, silent on a −1 that does not
   reach zero), and §7.16 (no uncounted reference across a poll, which the
   untagged −1 takes back).
4. **Breakers in another frame:** a destructor run by an intermediate
   release, a borrowed receiver, by-reference writes, user hooks
   (`__set`, `offsetUnset`, …), `yield` and fibers; "a within-frame
   property" (`static-lifetimes.md:128`) is false for these.
5. **The order of the stores:** `lowering.md` shows plain inline `++`/`--`;
   on AArch64 the −1 can show before the +1's tag. §7 must state both
   stores' order.
6. **The runtime's own paths hold**, with two comments worded by counts
   (`element::get`, `box_element`).

**Proposed 16b** (the Critic's wording): a borrow is covered by a counted
path from a root; before any operation that can remove a reference on that
path (a store, unset, scope exit or last-use drop of the root; a store,
removal, pop, clear or separating copy of a slot on the path or of a
may-alias of a holder on it; a call, destructor or hook that may do either;
a `yield` or fiber suspension), the borrow is dead or converted: its +1, a
tagged count store, sequenced before that −1's release store, and no
transformation moves either across the other. Whether the −1 can reach zero
is not an argument. Checkable by a verifier after the ARC passes, and in
debug by a stress mode running trial deletion at every −1 that does not
reach zero.

Not written into §7: the untagged −1 is deferred (Q9). To the Sage, then
to Edmond with the verdict.

### Wave 2: the Sage (2026-10-06)

The Sage (Fable), reading the code, running nothing.

**Conclusion.** The polished draft is faithful to the code and the
accepted rules; Claude's nine answers hold. No fork for Edmond.

1. **A liveness defect in the build** (for the code wave):
   `queue/compaction.rs` sets `SECOND_CHANCE_MARK` on every `Unwalked`
   write-back, and `post_the_rest_unwalked` (`worker.rs`) posts `Unwalked`
   for a trace cut by a recall or stop. A root cut once and refused once
   reads live and goes to a lane, with no first Δ-test having cleared its
   set's stale tags. Read-live is always safe; the fix marks only the
   split's `Unwalked` (a verdict bit beside `VERDICT_DEFER_MARK`). Checked
   by Claude in the code; not fixed in this wave.
2. **The second refusal is a consequence of Q3, not a conflict.** Q3 speaks
   of one refusal; a second F after the first attempt cleared every stale
   tag is a real write, the root is live, and P8 sends a live root to the
   lanes. Settled by Claude on the Sage's advice, written into the draft;
   `split_counts` (second refusals, roots read live again) is a gate
   column so Edmond can overturn on data.
3. **Q6 is one clock.** The stamps' epoch and the lanes' are low bits of
   one cell (`epoch.rs`; `LANE_WAITS` in `queue.rs`): a root back from a
   lane must find its last reading's stamps retired. Taken batches become a
   second trigger beside `SPENT_PER_PROOF`, X the floor. Settled by Claude
   on the Sage's advice.
4. **16b.** The Critic is right and the wording complete enough; written
   into §7 word for word, marked in force only with the untagged −1, with
   the stores' order; `static-lifetimes.md`'s "within-frame property"
   corrected in the rfc. Later, with the arm: the verifier after the ARC
   passes, the stress mode, and the four rfc texts reconciled (one policy
   worded four ways).
5. A weakly held member of U alone keeps S unproved: conservative, noted
   in §4.8.

## Wave 3: the second chance spent on a cut trace (Claude's draft, 2026-10-06)

**The defect** (the Sage, wave 2; measured in `dev/BENCHMARKS.md`, "S68.14's
measurements before code"). The disposition writes every `Unwalked` entry of
P back into R with `SECOND_CHANCE_MARK` (`queue/compaction.rs`). `Unwalked`
carries two meanings: a root of a refused U (`verdict_for`, a completed
trace), and a root no trace reached (`post_the_rest_unwalked`, a recall, a
stop or a refused allocation). The second spends a chance no Δ-test gave,
so the root's next refusal reads it live and sends it to a lane, though no
attempt cleared its set's stale tags. On `web-heap` second refusals
outnumber first ones (4.3–5.1k against 3.5–3.8k), and batches offered 1–10
ms after the last post a third of their roots unwalked.

**The repair** (revised after the Critic, below). P's entry has no free
bit: two carry the verdict, and bit 2 is the mutator's deferral mark, which
it writes on unwalked roots of an `AllRoots` batch. So the fact goes beside
P, by position:
- The collector keeps, per batch, a bitset of up to `BATCH_BOUND` bits
  indexed by the order of its posts, and P's tail index at its first post.
  A post sets its bit when the root is written back marked: refused by this
  batch's U, or carrying `SECOND_CHANCE_MARK` on the collector's copy of its
  R entry (an incoming mark is kept). It stores the bitset and the base in
  the mutator's record in `ReleaseOnDrop::drop`, unconditionally, before the
  release of the token, which the mutator's take acquires.
- Every disposition of P, whatever the batch's form, takes the bitset from
  the record (copied and cleared before its first write-back) and writes an
  `Unwalked` entry back with the mark exactly where the entry's position
  from the base has its bit set; an entry outside the batch's range gets
  no mark. `reset_for_a_new_life` clears it too.
- A cut root that came in unmarked goes back unmarked, a refused root or a
  marked one goes back marked: the mark means "a Δ-test refused this root
  once", and only a Δ-test or an incoming mark sets it.

**Why it is sound.** The mark only chooses between another refusal and a
lane: a root read live is never freed, and a root without the mark is
refused again at worst, which §4.8 bounds for garbage by the next window's
clear. For a live set touched every window the mark is what bounds the
refusals; the repair keeps it across cut batches.

**Gate.** `cargo test` green in both builds, with cases: a cut batch's
unwalked roots come back unmarked, a refused U's marked, a marked root cut
comes back marked, an `AllRoots` close that ends early keeps the bits, a
stale bitset after an unwound disposition marks nothing of the next batch.
On `web-heap`, three repeats: second refusals below first refusals, roots
refused three times or more (a test-only count by address) near zero,
unwalked roots no worse, garbage within §9.

### Wave 3: the Critic on the repair (2026-10-06)

The Critic (opus) read the draft against the code, running nothing; the
draft above is revised to its findings.
1. **The first draft wiped incoming marks**: a live set refused, then cut,
   came back unmarked and was refused again without bound; the gate would
   have counted the loss as a gain. The mark is now the incoming mark or a
   refusal, and the gate counts roots refused three times or more.
2. **No mixed batch**: after a completed scan every root is posted through
   `verdict_for` with no recall reading (`worker.rs`, the posting loop), so
   a count stood for one bit. A bitset by position is as cheap and exact.
3. **`AllRoots`**: the disposition ignores the form today, and an
   `AllRoots` close that ends early writes unwalked roots back; one rule
   on every path now.
4. **Where to store and zero**: in `ReleaseOnDrop::drop`, unconditionally;
   copied and cleared before the first write-back; `reset_for_a_new_life`
   included, else an unwound disposition's stale state marks the next
   batch's cut roots.
5. Checked and holding: P never holds two batches but after an unwind; the
   take's acquire covers every disposition; `Unwalked` arises only the two
   ways named.

### Wave 3: the Sage on the repair (2026-10-06)

The Sage (Fable), reading the code, running nothing: sound and complete;
build it, no fork for Edmond, since it restores §4.8's premise and changes
no accepted rule. Three corrections, taken into the build:
1. **No base index.** Every disposition's prefix is P's whole count, and no
   collector posts between the release and the disposition, so the batch's
   posts are the prefix's last `posts` entries; entries nulled in place keep
   their slots. Leftovers of an unwound disposition stand before the batch
   and get no mark, a loss only after a panic, bounded by §4.8.
2. **One take-and-clear site**: every disposition ends in
   `compaction.rs::dispose_verdicts` with a prefix; the write-back has one
   site, where the bit replaces the build's unconditional mark.
3. **The bit is set in `FinishThePosts::post`**, by post order: refused
   where the posting loop after a completed scan posts `Unwalked`, or
   carrying the incoming mark; the bitset lives beside `proposed` on
   `ReleaseOnDrop`, whose drop stores it before the release.

Storage: two lines after `hold` in the record (384 bytes under the
feature, 170 records a block), the post count in `HoldLine`'s padding;
never on the token line or the mutator's writer line. The optional
simplification (mark any written-back entry whose bit is set, not only an
unwalked one) is not taken: the build keeps the mark on unwalked entries
alone, as before.

**Built** (2026-10-06): as the Sage corrected it; three unit cases
(`collect/tests/what_a_batch_without_a_proposal_owes.rs`) fail without the
repair and pass with it; both builds green. On `web-heap` nothing moves,
since no batch is cut there (`dev/BENCHMARKS.md`, "the second chance kept
for refusals alone"); the gate's count of roots refused three times or
more by address is dropped, addresses being reused.

## Wave 3: P6, the window read without a frame (Claude's draft, 2026-10-07)

**What the frame is** (read on the assembly, both before and after the
link). `cargo rustc --emit=asm` of the feature build shows `ll_retain`
reading the window through `callq __tls_get_addr@PLT`: rustc compiles a
library as position-independent code, and `thread_local!` then takes the
general-dynamic TLS model. The linker relaxes that call in an executable
to `mov %fs:0,%rax; lea -0x50(%rax),%rax`, so the linked binary has no
call (as the wave-2 audit found), but the register allocator ran before
the link: the call's clobbers left `push %r14; push %rbx; push %rax` and
their pops on the fast path. `ll_release` pays the same, plus a second
general-dynamic read (`MUTATOR_STATE`) because the feature build inlines
`register_candidate` into it; its frame is four registers. The A build
calls `register_candidate` out of line and reads no thread-local on the
fast path. Callgrind, self cost per call (2026-10-06): `ll_retain` A 13,
B 26; `ll_release` A 22, B 36.

The heap's comment ("ELF `__thread` is already a single `%fs`-relative
load") holds for the linked instructions only; `THREAD_HEAP` has the same
call in the compiled code. Its paths call out anyway, so the frame costs
them less; the comment is corrected here and its cost is not measured.

**The change.**
1. **x86_64 ELF**: the window is one byte in `.tbss`, defined by
   `global_asm!` (hidden), read and written by inline `asm!` through the
   initial-exec relocation `window@GOTTPOFF(%rip)`. The asm is opaque to
   the compiler, so it sees no call and keeps no frame. Linked into an
   executable the load relaxes to `mov $imm,%reg; movzbl %fs:(%reg)`;
   linked into a shared library it stays one GOT load before the `%fs`
   read, valid for a library loaded at start-up and, through glibc's
   static-TLS surplus, for one byte opened later. Checked on a scratch
   crate: an executable, a C program linked to the staticlib, and a
   shared library built from it all link; a thread starts at window 0 and
   sets its own; the linked retain is 7 instructions with no push.
2. **Every other target**, windows-msvc included, keeps `thread_local!`.
   The crate does not build for Windows today (`hash/process_key.rs`
   stops it with `compile_error!` for want of an OS randomness read), so a
   fast `TlsSlots` slot for the window waits until it does.
3. **`register_candidate` out of line** (`#[inline(never)]`, not `#[cold]`:
   a −1 that registers is common), so `ll_release`'s fast path holds no
   second thread-local read and the shape matches A's.

**What does not change.** The window's meaning, where it is opened
(`set_window` at the offer), the tag's store order (tag before the count
store), every accepted rule. `the_next_window`, `this_threads_window` and
the tagging slot stores read through the same two functions.

**Gate.** Both builds green, with a case that a new thread's window is 0
and a set on one thread is not seen on another (both the ELF and the
fallback form). The linked `ll_retain` of the feature build has no push;
callgrind self cost of `ll_retain` and `ll_release` in B falls from 26 and
36, the target being A's 13 and 22 plus the tag's 3. On `web-heap`, three
repeats: mutator CPU not above the 48.7–49.2 s of 2026-10-06.

### Wave 3: the Critic on P6, and what the build showed (2026-10-07)

The Critic (opus) read the draft against the code, running nothing. Seven
findings; the sixth decides the rest.
1. The gate measured self cost, so moving `register_candidate` out of line
   would lower `ll_release`'s figure by construction.
2. Miri runs the x86_64 Linux target and cannot execute inline `asm!`.
3. "x86_64 ELF" also reads as macOS or Android, where `@GOTTPOFF` fails.
4. False claim: one initial-exec relocation marks the whole module for
   static TLS, so a library opened later needs all of the crate's TLS in
   glibc's surplus, and musl refuses it outright.
5. The write's asm options were left open; `nomem` on it would let the
   compiler reuse a read across `set_window`.
6. **The frame may not exist where the code ships.** The hot paths reach
   compiled PHP code as the crate's bitcode merged into the program's IR
   (`Cargo.toml`, `[lib]`; `README.md`, "LLVM IR export"), and the TLS
   model is chosen when that is compiled, not when the rlib is.
7. The thread-local inventory test scans `thread_local!` alone and would
   not see a `.tbss` symbol.

**Checked, on 2026-10-07.** The IR declares the window
`thread_local global` with no model. Compiled with
`-C relocation-model=pie`, as code bound for an executable is, `ll_retain`
reads it with `movq …@GOTTPOFF(%rip); movzbl %fs:(…)` and pushes nothing,
and `ll_release` keeps one `push %rax`, as A's does. The test binary built
that way (`RUSTFLAGS="-C relocation-model=pie"` with an explicit
`--target`, so build scripts and proc macros keep their own) links the
read to `movzbl %fs:imm`. Callgrind, instructions per call, by the
difference between 1M and 2M pairs on one entity in R:

| | rlib as built today, A / B | built as for an executable, A / B |
|---|---|---|
| `ll_retain` | 13 / 26 | 13 / 18 |
| `ll_release` | 22 / 36 | 22 / 25 |

**Revised P6: no change to the runtime.** The TLS model is chosen by the
compile that produces the final code, not by the crate. Where that is the
merged bitcode compiled into an executable, the path `README.md` names,
the window is an initial-exec read and the tag costs, in the test binary
built that way, 5 instructions on a +1 and 3 on a −1 (an inference for the
merged build after `opt -O2`, not measured on it). The frame comes back
wherever the final compile is position-independent for a shared object:
the staticlib as rustc builds it by default, and merged bitcode compiled
`-fPIC` into a loadable module; if such a consumer appears, the merge step
can mark the window `thread_local(initialexec)` or the staticlib be built
for an executable. Windows stays open until the crate builds there
(`README.md`'s msvc check is of 2026-07, before `hash/process_key.rs`
stopped the build). The asm byte, its cfg, the Miri gap and the dlopen
question fall away (findings 2–5, 7).

What changes is the rig: its arms are built as code for an executable is
(`dev/tools/arms.sh`, its build line), so neither arm pays frames the
merged executable does not. A reads thread-locals on its hot paths too
(the heap's, the queue's state), so every A-against-B figure measured
before moves on both sides, in a direction not known until measured: the
`web-heap` row of A against B is to be measured again with the new build.
B-relative tables, such as the offer floor's, stand. The heap's
`THREAD_HEAP` comment is corrected to the same scope (the Sage, below).

### Wave 3: the Sage on the revised P6 (2026-10-07)

The Sage (Fable), reading the revision and the code: the conclusion holds
for the executable path only, and the text overstated it; the corrections
above are its: the TLS model as a property of the consumer's compile, the
staticlib and a `-fPIC` module named as carrying the frame, the shift of
A's figures as well as B's, the 5 and 3 instructions as an inference, and
the build line by `--config` rather than an environment variable (no
`.cargo/config.toml`, which would change Miri's build and the README's
bitcode path). Re-measure the A-against-B `web-heap` row with the new
build.

## Wave 3: P2, batches past 1,024 (Claude's draft, 2026-10-07)

**What holds a batch at 1,024 today** (read from the code at `743e509`).
- `BATCH_BOUND` (`cycle/worker.rs:345`) is asserted under a block of R
  (`BLOCK_ENTRIES`, 8,135), so that the peek spans two blocks, and its copy
  in the workspace at a quarter of the bump (1,780 roots at most), so that
  the trace's rows do not start by growing.
- P is one ring block that never grows: 8,135 verdicts
  (`queue/verdicts.rs`); every take is clamped to P's room.
- The copy of the take is one arena allocation, refused past a block's
  payload (65,280 bytes, 8,160 roots).
- The second-chance bits are a fixed array of `BATCH_BOUND / 64` words in
  every mutator record and on both stacks (`mutator_record.rs:172`,
  `worker.rs:2656`); its `Default` exists only up to 32 words (2,048 roots).
- The withholding marks count in three units with no common scale: 8,192
  deaths, 16 blocks, 256 chunks (`deferred_slot_reuse.rs:1672–1681`), each
  with a stop at twice the mark. A longer trace reaches them sooner; the
  first mark winds the trace down, the second cuts it.
- K doubles from 64 to the bound by the batch's outcome (`worker.rs:2813`).

**The change** (revised after the Critic, below).
1. **The bound made general once**, then measured, rather than 4k built
   and 16k left to a gate on it:
   - the trace reads its roots from R in place, with a side bitmap of a
     bit a root for what the copy's mark records today, so no copy grows
     with the batch and the bump keeps its three row arrays;
   - P can take a second block: the mutator draws it at its offer when the
     ceiling exceeds P's room, never the collector, and links it from the
     first; the disposition walks both;
   - the second-chance bits live in P's block, beside the verdicts they
     describe, not in the record (the collector's workspace is dropped
     before the disposition reads them);
   - `BATCH_BOUND` becomes a rig setting with the default 1,024 until the
     measurements choose, the asserts restated per block.
2. **The marks in bytes** (Q5, settled on the Sage's advice): one count of
   bytes withheld under a foreign holder, a death by its size class, a
   block and a run by the blocks they span, a chunk by its bytes, with an
   item cap beside it so that the drain's walk stays bounded; M starts at
   512 KiB, the deaths mark's bytes today, and is swept on its own
   (256 KiB, 512 KiB, 1 MiB, 2 MiB) at the default bound, not scaled with
   the batch.
3. **Measured**: bounds 1k, 4k, 10k (PHP's buffer) and 16k, each with the
   offer at 64 (the accepted rule) and at the bound (PHP's way, an arm),
   on `web-heap` and the six loads: mutator CPU, windows a second,
   collector CPU, refusals, held garbage, offer-to-take, marks reached,
   and the poll's pause disposing of P (`disposal_longest_us`). The offer
   threshold is an accepted rule; a change to it goes to Edmond with the
   figures.

**What does not change.** The window, the split, every accepted rule; the
offer stays at 64 in the build. K's adaptation keeps its rule; only its
ceiling moves.

**Gate.** Both builds green; the tests pinning the sizes changed with the
step that changes them, with cases that a take of the bound is posted
whole, that P's second block is drawn by the mutator and disposed of, and
that a death, a block and a chunk each reach the byte mark; the rig's
`DEFERRED_LARGE` scaled with the bound so that it stays past 64 batches,
and `what_a_grown_k_costs` read against the byte mark. At the default
bound and M = 512 KiB, `web-heap` and the six loads not worse than B.
Arms built as for an executable.

### Wave 3: the Critic on P2, and Claude's answers (2026-10-07)

The Critic (opus) read the draft against the code, running nothing.
1. **"With the offer at 64 the bound changes nothing."** Not on
   `web-heap`: R at an offer averages 1,613–1,730 and reaches 9,154, and
   38–41 % of offers find R at the bound (`dev/BENCHMARKS.md`, "S68.14's
   measurements before code"), since R grows while the token is not free.
   Taken in part: the offer at the bound is measured as an arm, as wave 2
   planned.
2. **1 MiB is not today's marks**: twice looser for 64-byte deaths, 256
   times looser for 16-byte chunks, and a combined count fires on mixed
   stacks where none does today. Taken: M starts at 512 KiB, an item cap
   keeps the drain's walk bounded, and M is swept on its own.
3. **Gating 16k on 4k's collector work per root** measures what P2 is not
   about and assumes a monotone curve. Taken: the bound is made general
   once and 1k, 4k, 10k and 16k measured on the mutator's side as well.
4. **The marks decide whether a long batch survives**, so a step fixing M
   before the bound measured M, not the batch. Taken: M swept at the
   default bound first, then the bounds at the M chosen.
5. **The poll's pause disposing of a 4k P** was not measured. Taken.
6. **A 4k copy leaves the bump one row array**, and under pool pressure
   the second's `grow` refuses and the whole batch is posted unwalked.
   Taken: the roots are read in place.
7. **Two readings in P2c**: the workspace is dropped before the
   disposition, and P "never grows". Taken: the bits in P's block, the
   second block drawn by the mutator at the offer.
8. **Tests the gate missed**: `DEFERRED_LARGE` and
   `what_a_grown_k_costs`. Taken.

### Wave 3: the Sage on P2 (2026-10-07)

The Sage (Fable), reading the revision and the code: not sound as written
in three places, and not worth the full build before a one-constant probe.
- **Reading R in place** is sound: the writer never touches the peeked
  span, and every mutator path that packs R first takes the token back,
  after the collector's posts. The copy carries contiguity and
  `HAS_A_VERDICT` (bit 1); a side bitmap replaces the bit, a segment table
  the contiguity. Missed: the peek spans at most two blocks of R
  (`ring.rs`), so a 10k or 16k take needs a peek over more. False: "the
  bump keeps its three row arrays": at 1k the copy already pushes the
  third past the bump.
- **P's second block** is drawn once and stays in the circle; a pool
  refusal clamps the offer's ceiling, never fails it. **The bits cannot go
  in P's block**: a verdict entry uses bits 0–2 and entities are 8-byte
  aligned, so no bit is free. They stay in the record, sized by the bound
  (`std::array::from_fn` in place of `Default`); the record is not dropped.
- **The marks' order is backwards for `web-heap`**: no offer is withdrawn
  at a mark there, so sweeping M at the default bound measures nothing.
  Bounds first at M = 512 KiB; M swept only where marks are reached. The
  item cap needs a value and a rule per stack.
- **The grid**: R at an offer peaks at 9,154, so 10k and 16k are one arm on
  `web-heap`; keep 1k, 2k, 4k, 10k with the offer at 64, the offer at the
  bound for 1k and 4k only.
- **Order**: roots a batch average 466–800 with 38–41 % of offers at the
  bound, so K rarely sits at 1,024. First a probe, `BATCH_BOUND = 2048`
  (the copy's assert at half the bump, the record at 512 bytes), three
  `web-heap` cells against B, reading roots a batch and the share at the
  bound. If they do not move, P2 waits; else the byte marks, then the
  in-place reading and the wider peek.

Taken whole (settled by Claude on the Sage's advice, open to Edmond's
overturning): the probe runs first, as a diagnostic arm not pushed.

## Wave 3: the offer at the bound (Claude's proposal, 2026-10-07)

**What the probes showed** (`dev/BENCHMARKS.md`, the three entries of
2026-10-07 on the bound and the offer). A bound of 2,048 with the offer at
64 moves the roots a batch and nothing else: the offers still come at 64.
Offering when R reaches the bound pays, as PHP's buffer does, on every load
but one, and a short standing interval bounds the wait it costs a slow
trickle.

**The proposal** (a change to §5f's accepted offer rule, Edmond's to
take): at its poll, with the gate open and the token `FREE`, the mutator
offers when R reaches `BATCH_BOUND` (1,024), or R has stood under it for
50 ms, or a lane merged into it. In place of: R reaches 64, or stands 4 s.
Unchanged: the window, the take, the split, K's adaptation (a first batch
then starts at the bound, not at 64), the withdrawal at a mark.

**What it would change in the code.** `SOFT_THRESHOLD` for offers becomes
the bound, separated from `INITIAL_BATCH` (the rounds' threshold and the
first K); `STANDING_INTERVAL` for the offer becomes 50 ms, measured on the
mutator's own clock if the round clock is too coarse at that scale; the
tests pinning 64 and 4 s at the offer move with it.

**Gate, if taken.** Both builds green; `web-heap` and the six loads by the
probes' cells, three repeats: collector CPU and mean garbage on `web-heap`
not above the probe's S50, time to free on `garbage-25` under 100 ms.

### The Critic on the offer at the bound (2026-10-07)

The Critic (opus) read the proposal and the code, running nothing. The
proposal as written is not to be taken; Edmond is told so.
1. **Below the bound nothing starts the collector.** The standing check
   waits on the round clock, which reads 0 until a round runs, and today
   the offer at 64 is what starts the elder thread; a process that builds
   500 garbage rings and keeps polling would hold them until pressure or
   exit. The probes' loads all reach 1,024 and could not show it.
2. **The 50 ms runs on the round clock**, which ticks only when a round
   starts, with gaps doubling to 1 s when idle: the measured wait is about
   63 ms after a busy stretch and up to a second after a quiet one. The
   83 ms on `garbage-25` fits that.
3. **A thread that blocks between requests holds up to 1,023 roots**
   instead of 63: offers happen only at a poll.
4. **Any non-empty R is offered every interval**: with many lightly busy
   threads, offers rise by about 80 times; every cell had two mutators.
5. **The text disagrees with the code**: K still starts at 64; the retire
   pass and the rig (`ROOTS = SOFT_THRESHOLD - 1` makes `garbage-25`)
   read `SOFT_THRESHOLD` too.
6. **The readings understate a regression**: `garbage-25` frees 3.7 times
   slower under S50, which the gate's "under 100 ms" accepts; "pays on
   every load but one" is false.
7. **1,024 is the workspace's limit, not a measured optimum** (T2k was
   cheaper still), and tying the offer to the bound ties it to P2.

**A repair to measure** (Claude): keep today's rule as the floor of the
behaviour and add the bound above it: offer at R ≥ the bound; or at
R ≥ 64 once R has stood a short interval on the mutator's own clock; or
any R after 4 s; the elder started at R ≥ 64 as today; and an offer at an
embedder's "about to block" point. Measured against B in the same rotation,
with many mutators and a load that blocks between requests, before it goes
back to Edmond.

### The offer at the bound, repaired (Claude's proposal, 2026-10-07)

Measured as R50 (`dev/BENCHMARKS.md`, "the offer at the bound over
today's rule"). At its poll, with the gate open and the token `FREE`, the
mutator offers when:
- R reaches the bound (1,024 today); or
- R holds at least 64 and has held it 50 ms on the thread's own clock;
  the elder is started at R ≥ 64, as today's offer starts it; or
- R holds anything and has stood the accepted 4 s; or a lane merged.

Today's rule stays as the floor of the behaviour: nothing it offers waits
more than 50 ms and one poll longer, an R under 64 keeps 4 s, so the
Critic's findings 1, 2 and 4 do not arise beyond what today's rule has (an
R under 64 with no elder born still waits until one is, as today). K is
unchanged: it starts at 64 and doubles to the bound. Finding 3 (a thread blocked for seconds with up to
1,023 roots) is bounded only by an offer at the embedder's "about to
block" point, which the runtime does not have yet; not measured. Finding 7
stands: 1,024 is the workspace's limit, and the bound's value is P2's to
measure. The cost measured: a trickle's time to free about doubles (45 ms
against 23 on `garbage-25`).

### The Sage on the repaired offer (2026-10-07)

The Sage (Fable), reading the proposal, the code and the figures: sound,
and worth putting to Edmond as the recommendation; three independent arms
agree on the saving. Corrections taken above: "50 ms and one poll"; the
hole for an R under 64 with no elder is today's; K unchanged; every rig
load reaches 64 at its second iteration, so the trickle loads measure the
interval, not the bound; eight blocking mutators pay the same doubling.
Advice:
- The clock read stays on `Instant`, after the token, ring and R ≥ 64
  tests: no syscall, and mutator CPU within B's spread everywhere; the
  round clock is too coarse and a poll count has no time.
- A thread blocked for seconds holds up to 1,023 roots: the repair, an
  offer at the embedder's "about to block" point, is embedder API and so
  Edmond's; the rule can land with the bound stated, measured first with a
  pace of seconds and the wait without a poll.
- The offer point is tied to the bound, whose value P2 measures.
- One arm more, 20 ms, may halve the trickle's cost at no loss; it changes
  the constant, not the rule (running).

### Taken (Edmond, 2026-10-07)

Edmond chose the repaired rule with 50 ms («Включить»), after A was measured
with the same higher offer (`dev/BENCHMARKS.md`, "A with the same higher
offer against B with R50"). Built: `worker::threshold_for_offers` is the
bound, `threshold_for_short_offers` the rounds' threshold,
`SHORT_STANDING_INTERVAL` 50 ms; `offer::is_due` reads `Instant` only after
the byte, the ring and the threshold, and the poll starts the elder while R
waits at the threshold. Open: the embedder's "about to block" offer; the
bound's value (P2).

## Wave 3: the tag before the load (2026-10-07)

**Asked.** Edmond, on the window's price ("это платится постоянно"):
tag only while a batch is in flight. The Critic (opus) on that idea:
sound only with the window's number kept apart from the byte that turns
the tag off (else every offer is F = 1 and §4.8's clear never runs), and
with tagging held until P's disposition (the owner's confirm by tags
needs the window not to turn while P stands); the in-flight share is
unknown and may be half the time, since POSTED lasts 7.5–7.9 ms a batch;
the idle path saves about one instruction on x86. Not built. Its cheaper
alternative is: store the tag before the count's load, not between the
load and the release store, so that the compiler can fold the increment.

**Built.** `refcount_load_to_write` stores the tag, then loads; every
count write goes through it (`ll_retain`, the release, the teardown
guards), `set_header_refcount` tags before its store, and
`refcount_store` stores the count alone. §5f's order holds: the tag is
still stored before the count's release store. `ll_retain` folds into
`incl (%rdi)`; callgrind, instructions a call (1M against 2M pairs, built
for an executable): `ll_retain` 18 → 16 (A 13), `ll_release` 25 → 25 (A
22; the release needs the new count, so it cannot fold).

## A third collector: the backup trace (Claude's draft, 2026-10-07)

**Asked.** Edmond, 2026-10-07: evaluate a hybrid of counting and tracing,
then build the experiment «отдельным модулем, чтобы можно выбрать тип GC»
(decision card: «Строить»). Background: the research note
`/mnt/project-files/research/rc-tracing-hybrids-2026-10-07.md` and
`dev/RESEARCH.md`, "The minimal experiment it proposes". The figures it rests
on are estimates: trial deletion 1.69 times the cost of a backup trace
(Frampton); 0.03–0.14 of a core on `web-heap-150k` against about 0.4; an owner
pause of 25–70 ms a trace.

**The proposal.** A cargo feature `trace-backup`, exclusive with
`recycler-over-counts` (a `compile_error!` on both), selecting a third cycle
collector, T, beside A (trial deletion on the mutator) and B (windows and an
off-thread collector). Its code is one module, `src/cycle/trace_backup/`;
outside it, only `cfg` seams at the registration and at the poll.

- **The mutator.** Counts stay immediate: +1 and −1 as A's, COW and
  `__destruct` timing untouched. The −1 that leaves a count above zero
  registers nothing: under `trace-backup` `release_word` skips the candidate
  bit and `register_candidate` (`src/refcount.rs`, the call after the
  `may_become_a_candidate` test). No window tags, no R.
- **The trigger.** At the owner's poll with the gate open (every reference
  the thread holds is counted, §4.7): the bytes of this thread's entity
  blocks have grown past twice what the last trace left live, with a 4 MB
  floor. The counter is per thread and kept at block grain (a block drawn or
  adopted adds its size, a block returned subtracts it), so the allocation
  fast path is not touched; today's `bytes_in_owned_blocks` is test-only and
  per slot.
- **The trace, at the poll, on the owner's thread.**
  1. Census: every live slot of the thread's owned entity blocks that cycle
     collection tracks (GC heap category, not acyclic by `ACYCLIC_GATE` or
     kind) gets a side count equal to its count (scratch from
     `TraceScratchArena`, keyed by slot).
  2. Subtract: for each such entity, each edge to another tracked entity of
     this thread's heap subtracts 1 from the target's side count
     (`cells::trace_cells`, the edge walker the trial deletion uses).
  3. Mark: entities whose side count stays above 0 are referenced from
     outside the heap (the stack, globals, another thread, the arena); mark
     everything reachable from them.
  4. The unmarked are garbage at this poll: the destructor phase as the
     existing path runs it (`finalization`: begin, the destructors,
     revalidate for resurrection), then `reclamation`, weak cells cleared
     there.
- **What is reused, what is new.** Reused: the edge walker, the scratch
  arena, finalization and reclamation, the heap's slot states. New: the
  per-thread walk over owned entity blocks (only a process-wide
  `for_each_entity_slot` exists, for a quiescent mutator), the side counts,
  the mark, the trigger.
- **A check in tests.** On the same heap, T's verdict equals A's trial
  deletion run over every entity as a candidate.

**What it is to answer**, measured against A and B (main) on `web-heap`, the
six deciding loads, the 400k-ring probe, and two loads where tracing should
lose (a large live heap with rare cycles; a long live list): mutator CPU,
trace CPU, the longest owner pause, mean and peak garbage held. The 5 ms
owner-pause gate of B is not expected to hold: a stop-the-owner trace grows
with the live heap. If T wins on CPU, the pause is the next design question
(a concurrent mark, which needs a mutation detector again); that is not part
of this experiment.

**Open for the Critic.** Whether the census is sound at the poll (no
ARC-elided borrow is an entity's sole reference there; references from other
threads or from adopted blocks); entities in owned blocks but of another
category; what else reads the candidate bit or R (weak tables, pressure
collections, teardown) and breaks with registration off; whether the 4 MB /
twice trigger can starve a thread that frees little.

### The Critic on the backup trace (2026-10-07)

The Critic (opus) read the draft and the code, running nothing. Checked by
Claude: `collect_before_exit` collects over R (`src/cycle/collect.rs:790`);
retained blocks are on no table (`src/memory/retained.rs:18`); the 202 ms a
whole walk of `web-heap-150k` is `dev/RESEARCH.md:1014`. The census rule holds
on its own terms (counts and edges consistent at the poll, as A relies on);
its coverage and its cost do not.
1. **R empty breaks four paths, not two seams.** Thread exit, allocation
   under pressure and `gc_collect_cycles()` all collect over R: a two-member
   ring with destructors never reaches the 4 MB floor, and at exit its
   destructors never run; under pressure the caller gets memory exhausted
   with garbage on the heap.
2. **The census misses retained former-arena blocks and large entities**
   (on no per-thread list): a ring through a promoted arena object pins its
   heap member for good, which A frees. Adopted blocks hold an exited
   thread's leftovers; freeing them on the adopter runs their destructors on
   the wrong thread and meets a thread-local weak table.
3. **The trigger is ambiguous, and both readings break**: by live slot bytes,
   fragmentation keeps it true and a whole-heap trace runs at every poll; by
   block bytes, array storage in the buffer arena is not counted, so hundreds
   of MB of it can pile up under the 4 MB floor.
4. **Not one module with two seams**: a block-grain counter at every block
   draw, adoption, return and abandonment; a walk over the private `owned`
   lists; a `Membership` for finalization whose driver state is private to
   `collect.rs`; the worker, token and lanes left running idle; much of the
   test suite assumes registration.
5. **The comparison cannot decide the question as framed.** The 1.69 times
   (Frampton) compares trial deletion with a trace from real roots. T's
   CPython rule subtracts every edge of every tracked object, then marks: in
   kind a trial deletion over the whole heap; only the roots and the
   frequency change. T reuses A's rows, about 202 ms a walk on
   `web-heap-150k`, so expect about 200–400 ms a trace, not 25–70 ms (that
   figure assumes an 8 ns mark on a header bit). At 2 times, held garbage is
   about the live heap by construction; a sweep of ratios is needed, and the
   price of registration alone, which the research named, was dropped.
6. **Resurrection holds the whole unmarked set**: finalization validates it
   as one union; one pooled object whose destructor stores `$this` keeps every
   trace from freeing anything until the next doubling.

### Claude's answer to the Critic (2026-10-07)

Finding 5 falsifies the draft's premise: the estimate it was built on
(0.03–0.14 of a core, 25–70 ms) assumed a mark from real roots on a header
bit, and the draft is neither. Brought to Edmond with the repairs; the choice
is his.

Repairs, each answering a finding:
- **Real roots, not the census** (5): a mark from roots the thread holds.
  Without stack maps, either a conservative scan of the owner's stack with an
  "is this an allocated entity start" lookup (entity blocks are aligned and
  carry slot bounds), or a trace only at a request boundary, where the stack
  is empty and the roots are globals and Rust-side holders. Both untested.
- **A mark byte, not rows** (5): under `trace-backup` byte 7, the window tag,
  is unused; the mark is a byte store per visit, the sweep a walk of slot
  states.
- **A per-thread index of everything it sweeps** (2): owned blocks, retained
  blocks and large entities of this thread; adopted leftovers swept only by a
  trace their own thread ran at its exit.
- **Seams at exit, at `gc_collect_cycles()` and under pressure** (1), the
  pressure path bounded by the critical reserve; the idle worker not started
  (4); a test suite of its own for T (4).
- **The trigger by block bytes plus buffer-arena and large bytes** (3), with a
  sweep of ratios (1.25, 1.5, 2, 4) compared at equal garbage held (5).
- **Re-mark from the resurrected, free the rest** (6).

Before any of it, two measurements that cost no new collector: the price of
registration alone (A with registration off and the collector off, mutator
CPU on `web-heap`), and the cost of one mark visit over the existing rows
against a header byte, on `web-heap-150k`. If registration is cheap and the
visit is not several times cheaper, T has nothing to win.

### The Sage on the backup trace (2026-10-07)

The Sage (Fable), reading the three sections and the code: the Critic is
right in kind. The census runs trial deletion's phases with every tracked
object as a candidate, so the 1.69 times and the 25–70 ms, both premised on
a header-bit visit from real roots, are not T's figures; 200–400 ms a trace
is sound as an order, nearer a floor (R1: 1.53M rows in 202 ms,
`dev/BENCHMARKS.md:1536`). Byte 7 is free without `recycler-over-counts`
(`flags_store` writes bits 0–15 only, `src/refcount.rs:925`), and under T
byte 6 too. A conservative scan has every ingredient of the address lookup
(test-only `describe_slot`) but no enumeration of Rust-side holders; a
request-boundary trace alone leaves pressure and long requests uncovered;
a per-thread index of retained blocks reintroduces a registry the project
removed on purpose (`src/memory/retained.rs:18`). Registration is already
bounded: 1.60M registrations a `web-heap` run (`dev/BENCHMARKS.md:1516`), under
1 % of the mutators' CPU at 200 ns each, so T's gain, if any, lies in the
collector.

Advice: measure first, no new collector: (a) registration's price with the
collector off; (b) a mark visit on rows against a header byte, with the pass
count (3 against 1); (c) entity bytes allocated a run over live bytes on
`web-heap` and `web-arena`, which fixes the trace count at each ratio. Build
the census variant as the selectable arm only if traces × passes × visit ×
rows comes out under the collector CPU it replaces, the pause recorded as a
known loss; the real-roots variant is a second stage, only if the census
wins CPU.

Claude's note: the CPU to beat is B's, not A's. T keeps A's mutator (about
70 s on `web-heap` against B's 46 s), so T's trace must cost under about
33 s for T to beat B's 102–105 s in total, against B's collector at 56–59 s.

### The Sage's arithmetic, measured (2026-10-07)

`dev/BENCHMARKS.md`, "the backup trace's inputs": on one mutator's `web-heap`
state a census costs 0.21–0.33 s, a mark 0.09–0.13 s; at a 2× trigger about 27
traces a mutator a run, about 19–29 s of CPU for two, against B's collector at
51–59 s, the frees counted in neither. The Sage's condition holds, so the
census variant is built as the selectable arm (settled by Claude on the
Sage's advice, Edmond having chosen to build; open to his overturning). Its
known loss: the owner stops 0.35–0.54 s a trace, about every 3.6 s.

For the build's design, to go to the Critic: the side count in header bytes
6–7 as a 16-bit count saturating at its top (a saturated entity is read as a
root, which only keeps it), in place of the 64 MB hash table, since T writes
neither the window tag nor the maturation stamp; the Critic's findings 1–4 and
6 as the build's scope.

## The backup trace: the build (Claude's draft, 2026-10-07)

**Selection.** Cargo feature `trace-backup`; with `recycler-over-counts` a
`compile_error!`. Its code in `src/cycle/trace_backup/` (`mod.rs`, `trigger.rs`,
`census.rs`, `mark.rs`, `sweep.rs`, `tests/`).

**The header under T.** Bytes 6–7, the maturation stamp and the window tag in
the other builds, are T's: during a trace, the side count, a `u16`
saturating at `u16::MAX` (an entity whose count or side count reaches it is a
root, which keeps it and anything it reaches: sound, and only conservative);
bit 15 of the pair is the mark once the census is done. Outside a trace both
bytes are 0. The mutator never writes them (`flags_store` writes bits 0–15),
and a trace runs on the owner's thread only, so no other writer exists.

**The mutator.** `release_word` under T sets no candidate bit and registers
nothing; nothing else in +1/−1 changes.

**What T walks: the thread's index.** Per thread: its owned entity blocks
(the private `owned` lists, read through a new `Heap` walk), its large
entities (a per-thread list kept at their allocation and free), and its
retained former-arena blocks (a per-thread list kept at promotion). Only
slots that are live, of the GC-heap category, and not acyclic by
`ACYCLIC_GATE` or kind take part; every other slot's references into the
walked set are counted references from outside, so they pin: conservative.

**The trigger.** A per-thread counter of bytes held, at block grain: entity
blocks drawn or adopted, large entities, retained blocks, and buffer-arena
bytes; a trace when it passes twice what the last trace left, 4 MB floor;
checked at the poll with the gate open.

**The trace**, all on the owner at that poll:
1. Fill: each walked entity's side count from its count (saturated).
2. Subtract: each edge between walked entities, one from the target's side
   count (`cells::trace_cells`).
3. Mark from every walked entity whose side count stays above 0.
4. The unmarked are garbage: their destructors by the existing finalization
   path, re-marking from anything a destructor resurrects and keeping only
   that; then reclamation, weak cells cleared there.
5. Clear bytes 6–7 of every walked survivor.

**Seams outside the module.** `release_word` (no registration); the poll
(the trigger); `gc_collect_cycles()` (a trace at once); thread exit (a trace
before the heap is abandoned, so adopted leftovers never reach another
thread's trace); allocation under pressure (a trace, its scratch nil since
the counts live in the headers). The off-thread worker, the token and the
lanes are not started under T.

**Tests.** T's own suite under `--features trace-backup`, the suites of the
other builds unchanged; a check that T's verdict equals A's trial deletion
over every entity as a candidate on the same heap.

**Measured against B (main) and A**: `web-heap`, the six deciding loads, the
400k-ring probe, a large live heap with rare cycles, a long live list;
trigger ratios 1.5, 2 and 4; mutator CPU, trace CPU, the longest owner pause,
mean and peak garbage.

### The Critic on the build (2026-10-07)

The Critic (opus), reading the draft and the code, running nothing; checked
by Claude: the gate reads only the collecting word, a reset and the teardown
depth (`src/cycle/collect.rs:93`); `stamp_component` writes byte 6
(`src/cycle/finalization.rs:916`).
1. **Traces nest.** A destructor in step 4 reaches a poll with the gate open
   and the trigger still true; the nested trace's step 5 clears the outer
   marks, and an `ll_thread_exit()` from the destructor abandons the heap
   under the outer trace.
2. **Bit 15 as the mark overlaps the count**: a side count of 0x8000 or more
   reads as marked, so a root is not walked and a child it alone holds is
   freed live.
3. **Saturation leaks for good**: a document held by 100,000 nodes saturates
   and is a root at every trace after the tree died.
4. **"The unmarked" is unspecified**: a sweep by header frees objects a
   destructor allocates (bytes 6–7 are 0 at birth); a list snapshot is a
   12 MB membership; a header membership needs a "walked" bit; step 5 over a
   snapshot of blocks can zero headers of a block reissued to another thread.
5. **Finalization judges one component**: one resurrection keeps all; T must
   split the garbage into components first. Its walks (confirm, revalidate,
   the sever) cost about the trace again on the owner, unmeasured.
6. **The trigger ratchets or thrashes**: blocks return to the pool only when
   empty, so held block bytes track the peak; by live bytes, held stays above
   twice live. Under a memory limit below twice live, only the pressure path
   collects, a full trace at every refusal.
7. **The index goes stale**: retained blocks and large entities are freed
   from any thread; adopted blocks join the adopter's `owned` lists, so an
   exited thread's leftovers are traced by the adopter; A runs up to 8 exit
   rounds, T one.
8. **Edges into another thread's entities** would write their headers if the
   target test is the block kind alone.
Held: bytes 6–7 have no mutator writer in the default build; a panic leaves
only stale side counts, overwritten by the next fill; destructors cannot
unwind.

### The build, revised (Claude, 2026-10-07)

- **The field** (2, 3, 8): bits 0–13 the side count, saturating at 0x3FFF
  into a small exact overflow map keyed by entity; bit 14 walked (set by the
  fill, only on this thread's indexed entities); bit 15 marked. Subtract and
  mark touch only walked entities, so no other thread's header is written.
  Roots are seeded by a pass of their own.
- **The gate** (1): the record's collecting word is raised from the fill to
  the clear, and the trigger's baseline reset before the destructors run.
- **Membership by the header** (4): a member is walked and unmarked; an
  object born in step 4 is not walked, so it is no member. Step 5 re-walks the
  live index, not a snapshot.
- **Components** (5): the unmarked are split into components (union over
  their edges) before finalization, each judged alone; `stamp_component` out
  under T. The finalization's cost is measured with the first build, and
  counted in the owner's pause.
- **The trigger** (6): live bytes from the census; held bytes counted as
  slots in use plus large and buffer-arena bytes; a trace at the smaller of
  twice live and the memory limit's headroom; the pressure path at most one
  trace per freed-nothing interval.
- **The index** (7): `owned` lists and the process-wide large-entity
  snapshot filtered by owner; retained former-arena blocks are not indexed,
  their objects pinning what they hold (a known leak for rings through a
  promoted arena object, to be measured on `web-arena`); at exit, traces until
  one frees nothing, up to A's 8 rounds, then the leftovers abandoned and
  adopted as today, traced by the adopter like its own.

### The Sage on the revised build (2026-10-07)

The Sage (Fable): findings 1, 4 and 5 answered (raising the collecting word
stops nesting, `src/cycle/collect.rs:93`; bytes 6–7 are 0 at birth,
`publish_header`; finalization runs per component, but `revalidate` itself
calls `stamp_component`, a seam inside finalization). Answered wrongly:
- **8, unsafe**: nothing serialises traces under T, so "walked" means walked
  by some thread; X subtracting an edge into Y's entity while Y traces
  removes a real external reference and frees Y's entity live. Repair: an
  ownership test before every subtract and mark (a non-test version of
  `block_is_owned_by_this_thread`, an owner field for large entities), or
  traces serialised process-wide.
- **7**: large entities carry no owner (`src/memory/large_entity.rs:30`), so
  "filtered by owner" has nothing to filter on.
- **6**: "slots in use" is the per-slot counter on the fast path the draft
  refused; one trace per freed-nothing interval reverses the recorded
  decision not to cache an empty pressure collection.

Scope: 2,500–4,000 lines touching 8–10 files, a subsystem, not an experiment.
Advice: build a reduced measuring arm first, a rig-only feature: the header
fill, subtract and mark; the component split; the real finalization and
reclamation (the pause needs them); the gate raised; an ownership test on
every target; the trigger from the existing test counter at a fixed ratio;
the check against A. Left out: exit rounds, the pressure and
`gc_collect_cycles()` seams, retained, large and adopted indexing, the
memory-limit headroom, the overflow map (a saturated count reads as a root).
Run on `web-heap`, the ring probe and the two loads where tracing should
lose, at ratios 1.5, 2 and 4. The full module only if that arm beats B's
collector with finalization counted.

Taken (settled by Claude on the Sage's advice, open to Edmond's
overturning): the reduced arm first, kept out of `main` as a diagnostic arm.

## Wave 3: P10, the proved set without the counts' sum (Claude's draft, 2026-10-08)

**What the owner does today** (read at `4da1230`). In the feature build a
set the collector proved by its tags reaches its owner marked
(`Posting::WholeProved`, or `Posting::S` with its edges; `worker.rs`,
`split_and_free`). The owner meets its members without reading a cell
(`trace.rs`, `trace_within_the_set`), takes the edges the collector
recorded between them and the references its free of C left held, and
commits. The commit's first step, `Finalization::confirm`
(`finalization.rs`), sums the members' counts against those edges
(`counts_sum_to`) and walks the set by the exact validation where the sum
differs; a debug build validates after a matching sum too. Destructors run
after it, and the revalidation after destructors reads the set again.

**The change.** Edmond's answer to Q4 («конечно убираем лишние проверки»;
P10's first clause, accepted 2026-10-06): for a set proved by its tags the
owner trusts the proof. `confirm` answers `Unreachable` without the sum,
and the debug build keeps the exact validation as an assertion, as it does
after a matching sum. The arena carries where its internal edges came
from: the collector's record (this case) or the owner's own drain, whose
sum stays as it is (the 2026-10-08 one-pass condition and A's commit read
it). The revalidation after destructors stays (Edmond: «это верно»). A
weakly held set is never marked proved (`delta_test.rs`), so it keeps the
exact way, as P10 says.

**Why it is sound.** The proof: every member of W − U carried no tag F at
the frame, after the fence; every count store, the −1 included, tags its
entity before the store (§5f), so no count of a member changed in the
window, and the members were unreachable from outside at the frame.
Unreachable stays unreachable: after the frame no code can name a member
but R's entries (read by the collector and the owner, never written
through), the collector's held drops (applied by the owner after the
commit) and the weak references, which keep a set unproved. So the sum
compares a count nothing could change with edges nothing could cut; it
catches only a wrong proof, which the debug assertion and the
revalidation after destructors keep catching in tests. The untagged
stores of §5f's table are slot stores, not counts, and the untagged −1 is
not built (Q9).

**What it saves.** One read of each member's header before destructors,
for sets with destructors only: B's collector frees C itself. On
`web-heap` and the six loads no member has a destructor or a weak
reference, so the rig shows nothing; the piece is the rule written into
the code, measured by a count, not by time.

**Gate.** Both builds green, with cases: a proved set is confirmed with no
sum read (a test count beside `CONFIRMED_BY_THE_SUM`), its destructors run
and its revalidation runs; a set the owner traced itself still goes through
the sum; a weakly held set stays unmarked and walks.

### The Critic on P10 (2026-10-08)

The Critic (opus) read the draft against the code, running nothing. No
failing scenario in the soundness argument: nothing changes a member's
count or cuts an internal edge between the tag test and the confirm (the
token stays `POSTED`, so no other collector takes this mutator's batch;
C's drops into S ride with the set and are applied only where it is
dropped unfreed; the untagged stores touch no member of S). The sum
matches today for every proved set, an S with held references included
(`split.rs`, `edges_into_the_marked`), so it walks only on a wrong proof.
1. **The sum is the release build's only check before destructors**, and
   it costs next to nothing: the guard loop right after it writes the same
   header word, so the sum's load only brings in a line the guard needs.
   Under P10 a wrong proof runs a live member's destructor and the
   revalidation after it can only notice. Proposed: add up the counts
   inside the guard loop, and on a mismatch release the guards and walk;
   the check stays with no pass of its own. The draft's saving is also
   misscoped: `WholeProved` posts W whole whenever the split is dropped,
   so sets with no destructors reach the owner too.
2. **The owner commits the set and P's roots** (`trace_within_the_set`
   meets the batch's roots too); "roots within S" is assumed, and today
   the sum would catch a stray root. Proposed: a release check that the
   roots added no row beyond the set's members.
3. **The plumbing**: a flag beside `internal_edges_read` that is not taken
   with it skips both checks for the next set; one typed reading
   (`None | Drained | Recorded`) taken once. The one-pass free and A's
   commit are scoped correctly as the code stands.
4. **Gate gaps**: an S with C non-empty (held > 0); a destructor that
   resurrects a member; a proved set with a root outside it; what a
   release build does on a wrong proof.

### The Sage on P10 (2026-10-08)

The Sage (Fable), reading the draft, the Critic and the code: the Critic
is right on all four findings.
- The sum is the release build's one check before destructors, and its
  real cost is a second walk of the membership's rows, not the header
  read: `mutator_guard_retain` loads the same word right after. Folded
  into the guard loop, the separate walk goes and the check stays. On a
  mismatch the prefix is released with `mutator_unguard_release` (not
  `release_guards`, which counts a release and may queue an entry), then
  the exact validation runs as today; the debug assertion moves after
  the loop, with one guard a member.
- What the sum guards besides the proof: a root of P outside the set.
  The proved arm of `trace_within_the_set` meets every batch root and
  colours every met row unreachable, so a stray root would become a
  member and its destructor would run before the revalidation could see
  it. Taken: after the roots, the rows met must number the set's members,
  one comparison, no header read.
- The arena's reading becomes one typed value taken once (`None`, the
  owner's drain, the collector's record), in place of three fields.
- The gate takes the Critic's four cases.

Recommended: fold the sum into the guard loop. Edmond's answer to Q4
removed the check as redundant; it is redundant against the proof, not
against a stray root, so the fold goes to him as a fork, built meanwhile
and not pushed until he answers.

### Taken (Edmond, 2026-10-08 19:44), and the Critic on it

Edmond: the sum stays in the debug build only ("I propose keeping it for
the debug mode only"; `dev/DECISIONS.md`). Built: the arena's reading is
one typed value (`EdgesRead::Drained` from the owner's drain,
`EdgesRead::Recorded` from the collector's record); `confirm` guards a
recorded set unread and only a debug build sums and walks it; a drained
set keeps the sum, folded into the guard loop and taken back by
`mutator_unguard_release` where it differs; a batch root outside a proved
set (`rows_met` grew under the roots' walk) refuses the proof.

The Critic (opus), on the built diff: no way for a release build without
the sum to free or destruct a live entity or read freed memory under the
accepted rules; the one real case the sum caught, a batch root C freed
and the mutator reused under `POSTED`, meets a fresh row and is walked by
the stray-root check. Residuals, liveness only: the walk after a stray
root validates without the held drops, so such a set with C non-empty
reads live once and is freed by the next collection; a drained set whose
sum is taken back carries the guards' tag F into the next window and is
refused once.

## Wave 3: P8, the lanes' clock (Claude's draft, 2026-10-08, revised after the Critic)

**What is owed.** Accepted with the algorithm (`dev/DECISIONS.md`,
2026-10-06, settled on the Sage's advice, open to Edmond): the lanes keep
the epoch as their clock, the epoch turning on the thread's taken batches
as a second trigger beside the proofs' price, X the floor; and the
caught-up rule (`queue.rs`, `collector_caught_up`) decoupled from the
offer's threshold. Neither is built.

**What the rig reads** (B at `6c78ad6`, `web-heap`, cap 1, three runs of
116 s; process-wide journal totals, two mutators): 28–29 turns by proofs
and 12–14 by X, 20–21 a mutator, one every 5.4–5.8 s. Proof turns alone
come every 8.0–8.3 s, no faster than X: the clock is bimodal, gaps of
about 4.5 s where the collector proves and 8 s where it does not. A
lane's first entry waits up to about 6, 18 and 42 s; later entries wait
less (the whole lane is spliced back), and an X turn with the collector
caught up releases every lane. The age table of the same day was taken on
runs that lock at each registration, so it is read beside these counts,
not with them.

**The change.**
1. **Taken batches as the second trigger**: the collector advances a
   mutator's epoch once K of its batches were taken since the last
   advance, beside the proofs' price and X. K is to be set so that the
   trigger fires where neither proofs nor X do soon enough, and measured:
   at 150 (about the batches a mutator makes between two turns on
   `web-heap` today) the clock on `web-heap` keeps its rate, and a load
   that proves nothing turns by its own work, not by X alone.
2. **The caught-up rule's own constant**: `collector_caught_up` reads
   `SOFT_THRESHOLD` (64), equal today to the short offers' threshold by
   value only; it gets a constant of its own, so that a later change of
   the offer threshold does not change when lanes are released.
3. **A test-only reading before K is chosen**: each verdict by the lane a
   root came back from, which `survived_readings` already counts (0, 1, 2,
   3 for none and lanes 1, 3, 7), read in `verdict_for` for dead roots
   too, Proposed apart from ZeroCount, with the time since the splice.

**Gate.** Both builds green; a case that K taken batches turn the epoch
with no proof and before X; the lanes' release unchanged where the offer
threshold changes. On `web-heap` and the six loads, three repeats: turns
by trigger, garbage held, collector CPU, re-reads, not worse than B.

### The Sage on P8 (2026-10-08)

The Sage (Fable): build it, measured first. Taken (settled by Claude on
the Sage's advice, open to Edmond's overturning):
- **The guard**: the second trigger turns no epoch before
  `SPENT_PER_PROOF` times the proving wall, as X (`dev/DECISIONS.md`,
  2026-10-03's amendment); an epoch that proved nothing meets it at once.
- **Counted in taken roots, not batches**: a batch is 64 or 1,024 roots
  under the offer rule; the start is the roots a mutator takes between
  two turns on `web-heap` today, which by construction changes that clock
  nothing; the loads that read it are those with no proof turns and lanes
  standing.
- **A turn by taken roots is not an X turn**: it releases no lane
  wholesale (the Sage of 2026-10-05: nothing hands every lane back
  oftener than X); its own journal code.
- **Order**: the test-only reading by lane of origin (`survived_readings`
  read in `verdict_for`, Proposed apart from ZeroCount, an instant a lane
  at its splice) with the turns by trigger per load; the caught-up rule's
  own constant (`CAUGHT_UP_ENTRIES`, 64) as a commit of its own; then the
  trigger with its case; then a sweep on the loads that show it.
- For Edmond, as a note: the second trigger reinstates a count the
  2026-10-03 ruling removed, kept honest by the wall guard.
