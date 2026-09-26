# The collector keeps the live roots

Proposed 2026-09-26, not built, not ruled: the target of three design rounds
(a Fable Critic and the Sage each round) over Edmond's two requirements —
**the mutator does no unnecessary work, and the collector takes on the most
it can** — with only the mutator freeing memory. The chain amends earlier
rulings and waits for Edmond's word; the rest is the Sage's to decide and is
marked so.

## After the owner's review

Edmond's review (`dev/THE-COLLECTOR-KEEPS-THE-LIVE-ROOTS-REVIEW.md`, F1–F6)
holds on the code in every finding, and the Sage ruled its open choices on
2026-09-26; `PLAN.md` S65.24–S65.26 carry the rulings, and they take
precedence over the text below where the two differ:

- The exit and the pressure path read the whole chain before each round;
  `cap 0`'s asked collection reads its ready part (F1, S8).
- The chain has a waiting part (roots read live, stamped per block) and a
  ready part (expired blocks relinked whole, then unwalked retries, then
  deaths found); with R and the ready part both standing, K is split in
  halves and the first source alternates; chain roots do not grow K (F2).
- The death check reads at most 1,024 headers a grant, the recall read before
  each, its cursor in the chain block's own header; the record holds two
  words, the waiting head and the ready head (F3).
- A standing chain is served after 4 s whatever its stamps, the term the
  round already has for a standing R; deaths found are posted with any
  batch that posts, alone at 32 or after 4 s (F4).
- The 15,365 figure predates the list's stamping (F5).
- A, B (form D alone) and C (form D and the chain) are built and measured
  (F6); the stamp arms wait inside C for C's win.
- A mutator's push into the chain signals the collector, and the exit's
  count of registered entries counts the chain (the Critic's cycle-1 finds).

## What changes

**A root the collector read live costs the mutator nothing.** Today such a
root costs the mutator a whole collection over P each epoch: the token, a
trace window, the arena, three walks of P, a cold header read and a store
into the deferred lane (`queue::compaction::dispose_verdicts`,
`queue::defer_entry`), then a splice back into R once the epoch turns (the
poll reads the record's epoch byte only while the lane is occupied, behind a
thread-local load, `gc.rs:288-291`). A batch of 63 live roots left the
mutator 15,365 instructions against 658,832 in line (`dev/BENCHMARKS.md`,
the live-roots arm, measured on 2026-09-22, before the live list's stamping
existed).

**The collector's chain.** Under a grant the collector puts a root it read
live into the chain: pool blocks the collector draws, each stamped with the
record's epoch, the bytes charged to the owner's GC ledger
(`gc_metadata::hand_over`). The deferred parts' met roots, posted `ReadLive`
today (`worker.rs:2504-2520`), count as read live. A root its trace did not
reach (today's `Unwalked`, written back into R at the close) goes into the
chain stamped as already expired, so the next grant reads it first and it
waits no longer than today.

- **The chain's words.** Its head, its root count and its oldest stamp stand
  in `ReaderLine`'s 16 free bytes (`mutator_record.rs:160-209`), written only
  by the token's holder with release and read under the reading hold. The
  line's comment changes from "the collector's line" to that rule; the
  standing-list words there keep their own discipline. `HoldLine` has one
  free word, not three.
- **Coming back.** The round reads R's front block only and leaves an empty R
  (`worker.rs:1450-1485`), so without the chain's words a mutator with an
  empty R and an expired chain is never served and a parked ring that became
  garbage is never found. The round and the cap-0 ask
  (`worker.rs:1383-1410`) read the count and the oldest stamp under the hold
  and take when the record's epoch has passed that stamp.
- **Reading a block.** At a grant the collector reads the expired blocks
  ahead of R through a per-block front cursor, as `ring::Chain` blocks carry
  one (`ring.rs:1244-1280`): a block holds up to 8,135 roots against K ≤ 1,024.
  Chain roots and R roots taken together stay within P's room
  (`worker.rs:2177-2181`); an over-full P panics at `FinishThePosts::post`,
  and on an unwind that is an abort.
- **One entry.** A root pushed into the chain gets `HAS_A_VERDICT` in its
  copy, or `FinishThePosts::drop` posts it `Unwalked` as well. On an unwind
  and on a pool refusal the drop posts into P and allocates nothing.
- **Nothing posted.** A batch that posted nothing releases the token to
  `FREE`, and the mutator does nothing at all.

**The live list of a batch that posted nothing (the Sage's, built with the
chain).** Today every live root posts `ReadLive`, so a list always crosses
with `POSTED` and the mutator stamps byte 6 from it at its take
(`token.rs:815-826`). A take from `FREE` asserts a null list word, so a list
published before a release to `FREE` would never be stamped and would hold
its blocks until `give_back_a_stale_list` after the advance. The collector
publishes the list only when the batch posted something
(`worker.rs:2261`); otherwise it releases the list's blocks on its own
thread, with no `hand_over` and no record write. The cost: an R root traced
later in the same epoch re-descends that unstamped core, and the collector
pays it. A parked root re-traced after its block expired loses nothing,
since a stamp of the old epoch prunes nothing (`live_list.rs:416-417`).

**Completed deaths in the chain (the Sage's).** The candidate bit stays set
on a parked root, so a parked root that dies keeps its slot. At every grant
the collector reads one header per chain root, with no trace, and posts
`ZeroCount` for each completed death when the batch posts anything else
anyway, when 32 such deaths (`DEATHS_TO_RETIRE`) stand, or when the block
expired and is re-read. Fewer than 32 alone wait for the next grant or the
expiry, which keeps a pass over P from being armed for one death every
10 ms. P's room counts these posts. Form D's pass does not sweep the chain:
that is mutator work for deaths the collector already finds.

**A P without `Proposed` opens no trace window (form D; the Sage's,
final).** `ReadLive`, `Unwalked` and `ZeroCount` are all disposed of by the
pass over P alone (`compaction.rs:243-306`), so a batch that posted no
`Proposed` releases to a third value of the byte, `word(POSTED, 2)`, beside
`ASKED`, `NOTHING_PROPOSED`. Built in S65.25 as `collect::dispose_of_p`:
the collection over P's take and close with nothing between. The take of
the token from that value stamps the live list (`live_list::stamp_from`);
the guard's drop retires completed deaths, defers roots read live into the
deferred lane (under the chain, where `ReadLive` is only a pool refusal's,
writes them back into R), writes unwalked roots back, frees R's front run,
advances P, spends the arming, re-arms the retirement pass where its count
stands and writes `FREE`. `Arming::Disposal` ranks between `Retire` and
`Verdicts`, and `read_and_act_on_this_thread` has a third arm.

**The mutator's deferred lane goes,** with its mirror, the poll's epoch
compare and the splice. A pool refusal of a chain block posts the root into
P as `ReadLive`; the mutator writes it back into R, R's one writer, in a
form-D pass. The mutator's own collections (the poll's over R whole, the
explicit one, the exit, cap 0) read the chain's expired blocks as a batch
source under `MUTATOR`; the pressure path reads the whole chain, since
garbage may stand there. Their close pushes the roots they read live into
the chain, drawing pool blocks under `MUTATOR`; on a refusal the root stays
in R, as today (`compaction.rs:143-150`). No spare-cell lane is kept.
Collector-drawn blocks arrive on the mutator's ledger by `hand_over`,
mutator-drawn ones are its own. The exit dismantles the chain as it does R
(`queue.rs:1120-1140`), and the exit under a hold gets a `C_LEFT` flag
beside `R_LEFT` and `P_LEFT` (`mutator_record.rs:365-370`).

## What stays

- Registration, the overflow buffer, the mutator's frees.
- The collection over P traces the proposed roots itself (rows form, exact
  validation, guards, destructors, revalidation, sever, frees).
- The close's run of completed deaths at R's front (S65.23) and the count's
  pass below the threshold.
- The live list of a batch that posts anything: the mutator stamps it at its
  take (Edmond's 2026-09-23 ruling stands for every batch that posts).
- Nothing new is withheld under `POSTED`. P names roots alone, each held by
  its own candidate bit, so today's argument for the mutator's freeing
  under `POSTED` stands.
- One registered entity, one entry: in R, in transit under a grant with its
  advance owed, in P, in the chain, or in the overflow buffer.
- No new cost on the mutator's hot paths: the poll loses a thread-local
  load, the free path is unchanged, the third byte value is one compare on
  a byte already loaded.

## How long a completed death holds its slot

- **In R below `SOFT_THRESHOLD` (64).** The free path counts it
  (`stdapi.rs:503-506`), and the retirement pass at 32 counted deaths
  retires it wherever it stands in R (`queue.rs:215, 1333-1377`); the figure
  doubles, up to 4,096, when a pass retires under half.
- **In R at the threshold.** The pass only signals the collector
  (`queue.rs:1357-1361`); the death waits ceil(position / K) batches, one per
  round per mutator (10 ms to 1 s), then the mutator's next poll.
- **Behind a chain entry.** Until the next grant to its mutator that posts
  anything, or until 32 such deaths stand, or until its block expires (64
  batches or 8 s); then one form-D pass at the next poll frees it. Today it
  holds until the epoch turns, the splice, and a pass or a batch after it.
- **The count.** The free path counts deaths in the lane today, and in the
  chain tomorrow, that no retirement pass reaches (`queue.rs:1466`), so they
  push the pass's figure up. Counting them apart would put a header read on
  the free path, so it is refused. With the collector's header reads the
  inflation lasts one grant instead of one epoch.

The rig's `withheld_by_an_entry_peak` and `withheld_by_an_entry_time`
(`the_rig.rs:711-716`) on the live-heavy loads, the chain against
`f59503c`, are the figure at the step's done-line: if either is more than
10 % above today's, the floor of 32 drops to 1. The tally is `cfg(test)` and
counts deaths withheld by any entry, not only those behind a live one.

## What waits for a measurement

**The collector's live-core stamps.** The collector could write header
byte 6 itself under its grant instead of handing the mutator a list.
Correct by the token's release and acquire, once it is shown that no header
a list names is one the arena's reset rewrites word-wide during the grant.
Each store lands on a line the mutator retains and releases. Three arms on
`what_a_take_costs`, `overlapping-live`: (1) the mutator stamps from the
list at its take, (2) the collector stamps under the grant, (3) nothing
stamps. The figure is the mutator's instructions and cycles from the
grant's open to the end of its next collection or pass, two pinned runs A/B
as `dev/BENCHMARKS.md` 2026-09-22 runs them; the collector's rows met on the
second take is reported, not deciding. The arm with the fewest mutator
cycles is taken; a tie within 2 % of cycles goes to (2), then (3), and (2)
is taken only if it also stays within 2 % of (3). Until then the collector
writes no mutator header. Taking (2) amends Edmond's 2026-09-23 ruling and
`classes.md`'s one-writer rule for byte 6.

## What was dropped

**Validation by the collector's member list** instead of the mutator's
trace. Its gain is unmeasured and bounded by the mark and scan share, since
the validation still walks every member's fields and searches a sorted list
per edge where a row is address-computed. The list holds 64 KiB blocks from
the post to the collection over P, where a dense 381-member ring's rows
draw no ledger block (2,112 bytes in the thread's workspace at class 256,
8,216 at class 32). A member never registered carries no candidate bit, so
its slot can be reissued under `POSTED`; making the list safe needs either
withholding at `POSTED` (four gates, every death paying 0.3 to 0.6 ns now,
S65.7, and 4.2 ns at the return) or stale hooks with a fallback to the
trace. Edmond doubted it; the Sage dropped it. It returns only if a phase
split shows the mark and scan at 50 % or more of an overlapping garbage
collection **and** a hand-fed listed validation beats the rows form by 25 %
of the whole collection.

**Stamping a fully live batch by the mutator** (a `POSTED`-like release with
an empty P): 381 read-modify-writes on lines the collector's core just
took, the coherence miss moved onto the mutator. **Sweeping the chain in
form D's pass**: mutator work for deaths the collector finds.

## Edmond decides

1. The collector's chain. Taking it brings the list rule above at once and
   leaves the stamps to the measurement; refusing it keeps today's list and
   leaves the stamps open all the same. It amends `rfc/dev/DECISIONS.md`
   2026-08-27 ("the deferred-candidate buffer is the owner's") and the
   ruling at lines 236-240 ("the collector keeps no cursor across batches
   … its per-owner batch size K excepted": the chain's words are a second
   exception), Y12 clause 8, and `rfc/model/gc/rc-cycle.md` "The live list
   of a batch", "The mutator's disposition" and "P does not grow". The code
   docs that name the lane's mirror (`epoch.rs`, `HoldLine::turnover`, the
   `queue.rs` module doc) move with it.
2. Form D stands on the Sage's ruling; recorded here for the plan.
3. The live-core stamps, after the three arms.
