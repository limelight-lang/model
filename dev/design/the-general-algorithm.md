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

**Revised P6: no change to the runtime.** The frame came from building
the rlib position-independent for a shared library it never goes into; the
tag's price where the code ships is 5 instructions on a +1 and 3 on a −1.
The asm byte, its cfg, the Miri gap and the dlopen question fall away
with it (findings 2–5, 7). What changes is the rig: its arms are built as
code for an executable is, so B is not charged a frame the shipped code
does not have (`dev/tools/arms.sh`, its build line). Every A-against-B
figure measured before carries that frame on B's side: B's mutator CPU on
`web-heap` (48.7–49.2 s against A's 71–72 s) is a bound from above. The
heap's `THREAD_HEAP` comment holds for the shipped build and now for the
rig's too. Windows stays open until the crate builds there.
