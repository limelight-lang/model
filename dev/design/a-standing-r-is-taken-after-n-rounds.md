# A standing R is taken after an interval

Accepted 2026-09-22 as the final algorithm; sixth form, the
algorithm as Edmond ruled it after the Critic's two rounds and repaired to
a third, with the sleeper's arm as the Sage ruled it the same evening
(`dev/DECISIONS.md`, "a take's unanswered request stands on the record, and
the ring under the token decides the batch's form")
(`dev/DECISIONS.md`, "a standing R is taken after an interval of the
collector's own, and no request count is capped"). Built by `PLAN.md`'s
S64, opened 2026-09-22, whose steps are where a part of it stands or does
not; not normative until the `rfc` states the take at the stage's close.

## The rule

The collector visits every mutator's record on its own timer. A mutator
whose candidate ring R holds `SOFT_THRESHOLD` (64) entries or more is served
as today. One whose R holds fewer is not served at that visit — the
threshold spares a mutator with three candidates the foreign-holder window a
batch costs — but the collector notes when it first saw the ring non-empty,
and at the first visit `STANDING_INTERVAL` or more after that at which the
ring reads non-empty, grown meanwhile or not, it takes the ring as an
ordinary batch. The word measures the collector's visits and not the ring's
history: a ring emptied and refilled between two visits inherits the
instant, and is taken up to one interval early, once. A mutator that
does not consent to the take's request inside the consent wait is asleep: the
request stays on its byte and its record stays in the collector's standing
list, so nothing touches the thread while it sleeps and its ring is taken at
its own first poll or slot free after waking, at the checkpoint that reads
the consent. The mutator's side does not change:
it registers into R, signals a filled block, reads its byte at a poll or a
slot free, consents, and disposes of P after `POSTED`, as for any batch.

`STANDING_INTERVAL` is the collector's own time, 4 s by default, and the
embedder sets it through `ll_gc_set_standing_interval(millis)` beside
`ll_gc_set_quiet_interval`, zero restoring the crate's. It is not a count of
rounds: rounds start on every wake — a block of R filled anywhere, a consent,
a pressure collection's ending — and the timer's wait after any batch is
10 ms, so a count of rounds would hand the standing mutator's rate to the
busiest thread, the form the S60 ruling refused for the deferred lane.

What the rule accepts: a sleeping thread's garbage is not taken while it
sleeps; a working thread
pays one batch per interval while its sub-threshold ring is non-empty —
a mutator registering one candidate a second is taken every four seconds
with a handful of entries — and nothing else new.

The rule is the second half of `PLAN.md`, "A quiet thread's garbage is taken
after X"; the first, the deferred lane's turnover on the collector's request,
is built (`dev/DECISIONS.md`, "a quiet thread's turnover is the collector's
to ask for"). The two are one mechanism read twice: the lane is turned over
after X of the mutator's clock standing, R is taken after
`STANDING_INTERVAL` of R standing non-empty.

## The word on the record

One word on the hold line of `MutatorRecord`, which has 62 of its 64 bytes
spare and is already the collector's per-visit write target; the reader line
is full since the standing list's link pair took its last 16 bytes:

- `standing_since: AtomicU64` — the serve clock's reading (nanoseconds
  since `SERVE_CLOCK_BASE`, the clock `served_at` uses, never zero) at the
  round that first read R non-empty and below the threshold; zero for no
  standing ring.

The collector's own word, written by the collector the record names and read
by no mutator, so relaxed; cleared where the registry clears the hold line at
a re-take. Two collectors never read one record — a record moves
between collectors only through the handover — and the instant is the
process's clock, so a record handed over keeps its standing.

## The round

`serve` reads R before any request, off the front block, under the reading
hold, by `Reader::has_at_least(threshold)`: true for a span at or above the
threshold and for a front block that is not the tail block. The idle test
gains one reading, whether the ring is empty at all, which the same loads
answer (`Reader::front_block_reading`, one pass of the loads `has_at_least`
makes, returning the span and whether the front block is the tail block;
`has_at_least` is that reading compared with a count). The test becomes:

1. R holds the threshold — the serve continues as today; `standing_since`
   is zeroed.
2. R is empty — idle, as today; `standing_since` is zeroed.
3. R holds fewer than the threshold. If `standing_since` is zero it takes
   the serve clock's reading now and the serve returns `Served::Idle`. If
   `now − standing_since ≥ STANDING_INTERVAL` the serve continues to the
   take; otherwise it returns `Served::Idle`. The clock is read once per
   record and shared with the turnover ask's reading after the serve. A
   record whose take's request already stands answers `Served::Unanswered`
   here without a swap, the instant left as it is: the record is in the
   standing list for the checkpoints, and a failed compare-and-swap per
   round per sleeping mutator is what the standing form exists to spare.

The reading is the front block's alone, so "R holds the threshold" is true
of any ring with an entry past a front block read out, whatever the count
(`Reader::has_at_least`; `rfc/model/gc/rc-cycle.md`, "Signals"). A mutator
whose last batch consumed its front block to the entry and that has
registered one candidate since is therefore served at the next visit rather
than spared the window for an interval — the exception to the rule's first
paragraph, unchanged from the threshold serve of today, and not repairable
without following a link the pre-claim reading may not follow.

The take is the serve of today from the P-room test on — the room read, the
record pushed onto the standing list, the request by one swap from `FREE`,
the wait for consent, the batch under the grant, the release — and no flag is
threaded through it. What a take does differently lives at two places only,
branch 3 above and the batch's form inside `batch`, so `wait_for_consent`,
`answer_the_withdrawal`, `serve_the_grant`, `answer_a_refused_request`'s
grant arm and `Standing::checkpoint` are the built ones and need no kind of
request to reason about:

- **The instant restarts where the grant ends, not at the request's
  landing.** The release of every grant — a take's and a threshold batch's
  alike, branch 1 zeroing the word at the next visit anyway — stamps
  `standing_since` with the clock's reading, so the write-back and whatever
  the window registered are an interval away. The release rather than the
  batch, because three grants open the window and post nothing: one whose
  workspace the pool refused, one whose peek found R drained between the
  round's reading and the request, and one an unwind ended. An instant left
  standing across any of them has the next round take again at its own
  cadence, which is the form refused below (S64.2, the Critic of
  2026-09-22). A request that stood while its owner slept must not
  restart the instant at its landing: the round after its service would then
  read the write-back as a ring an interval overdue. A request refused at
  the swap — `POSTED`, `MUTATOR`, another collector's — leaves the instant,
  and the take is tried again next round; a record already linked answers
  `Served::Unanswered` at branch 3 without a swap, the instant left as it
  is.

- **The ring under the token decides the batch's form, not the request's
  origin.** `batch` reads `has_at_least(threshold)` under the grant, before
  the peek: at or above the threshold it is today's batch — K's clamp,
  `size_the_next_batch`, the backlog reading — and below it the ring is
  taken whole, the clamp one entry short of the threshold, which such a ring
  cannot exceed, with `size_the_next_batch` not called. Under the grant R
  only grows, every path that drains it taking `MUTATOR` over the claim
  first; before the grant it may shrink, the mutator being free to collect
  in line between the round's pre-claim reading and its request. So the two
  readings are made apart and neither is asked to agree with the other: a
  take whose ring crossed the threshold while its owner slept is served as
  the threshold batch it now is, a threshold request over a ring drained
  under it is served as the take its remainder asks for, and the checkpoint,
  which cannot know which kind of request it serves, needs no kind of its
  own. A take must feed K
  neither way: four takes of three live roots each would double K toward
  `BATCH_BOUND` and hand the thread's first real batch to the budget whole,
  every root `Unwalked`; a K halved to one would take a ring of three one
  root per round.
- **An unanswered request stands, as the threshold path's does.** A mutator
  that consents inside `REQUEST_WAIT` (2 ms) is batched; one that does not
  is asleep: the request stays on the byte, the record stays in the standing
  list it was pushed into before the swap, and the serve answers
  `Served::Unanswered`. The sleeper is served at the checkpoint that reads
  its consent — its own first poll or slot free after waking, one stranger's
  batch away at most — and the round pays one wait per standing request and
  none thereafter, where a withdrawal at the deadline would have cost 2 ms
  per sleeping sub-threshold mutator per interval inside the round. The
  wait itself is today's: its early return on a stranger's wake runs
  `Standing::checkpoint` and notes the consumed wake as any wait does.
- **The round's spending on expired waits is capped, on both paths.** A
  counter on `Standing`, reset at the round's start, counts this walk's
  waits that expired unanswered; past `EXPIRED_WAITS_PER_ROUND`
  (placeholder 2, not a measured figure) every later request that lands is
  left standing at once, through the same arm a released-unserved record
  takes, and served at a checkpoint. A pool of P threads that parks and
  wakes would otherwise cost up to P × W per interval, one wait per park,
  which the "one two-second round once" of a permanently parked population
  does not bound. The Sage's ruling of 2026-09-22 (`dev/DECISIONS.md`, "the
  consent wait stays on both paths, and a round's spending on expired waits
  is capped") carries what the wait buys, what the cap costs, the arm that
  sizes the constant and the case the build owes. What the bound bounds is
  the round's spending on mutators that never answer — at most
  `EXPIRED_WAITS_PER_ROUND` waits that expire, each spanning a wait plus the
  tail of one batch begun inside it — and not the round's length: a working
  mutator's consent or refusal costs its own latency and counts nothing,
  which is the wait's price rather than the cap's.
- **The backlog reading is unchanged.** `batch` answers
  `Served::Batch { backlog }` by `has_at_least(threshold)` after the batch,
  against the round's threshold; a take of a ring below sixty-four leaves
  nothing that reads as a backlog, so a take births no sibling and moves no
  record in a handover. The exception is a mutator that filled R during the
  trace: the reading after the advance is of the ring as it stands then, so
  a thread that registered sixty-four entries inside one take's window
  answers a backlog and votes for a sibling as any batch does. That thread
  is producing at the rate the siblings exist for; what it does not get is
  K sized by that batch, the form having been read before the peek (S64.3,
  the Critic of 2026-09-22). A batch made at a checkpoint reads its own
  backlog too, and the checkpoint carries it to the round that visits the
  record next, since past the cap every batch of a mutator behind a
  sleeping one is a checkpoint's and the round would otherwise read no
  backlog and birth no sibling (`dev/DECISIONS.md`, "a checkpoint carries
  its batch's backlog and a refusal it read out to the round").

After a take the verdicts stand in P and R's front has advanced past the
batch; the mutator's next poll or slot free arms the collection over P, whose
close retires the completed deaths, defers the roots read live and writes
back what it could not dispose of. The next round reads R empty, or holding
the write-back and what the mutator registered since, and branch 2 or 3
takes it from there: a write-back stands as any non-empty ring does, and is
taken an interval later if it still stands.

After a take whose consent has not come, the ring stands as it was and the
request stands on the byte; the next round reads branch 3, finds the record
linked, and answers `Served::Unanswered` without a swap.

## Interactions

**The turnover request (S60).** `ask_for_a_turnover_if_quiet` runs after
every serve, and since `2183b6a` the restamp is decided by
`clock_stood_since_the_stamp` alone — what the serve reached decides nothing
(`dev/DECISIONS.md`, "a quiet thread's turnover is the collector's to ask
for", amended 2026-09-22). The take needs that rule and nothing more: a take
whose roots all read live leaves the mutator a collection over P that closes
`EmptyLane` with no commit, so the clock stands and the ask keeps counting,
where a restamp per take would have kept a drip of one live candidate a
second from ever reaching X and left the lane the takes fill to pressure or
exit. A take whose roots were proposed moves the clock at the mutator's poll
and is restamped at the next visit. A take waiting for a consent answers
`Served::Unanswered` and feeds the ask as any unanswered serve does.

A root the take read live is deferred at the close, so the take gives a
thread that had no deferred lane one, and from then on the S60 ask turns
that lane over every X of the thread's clock standing — a collection of the
mutator's own, in line, over the lane and over R whole, drip included. For such a thread the
take and the X collection both reach the standing ring; a take empties it,
so after one the X collection finds the lane and what dripped in since. The
collector cannot see the lane — it is `MutatorCycleState`'s, not the
record's — so it cannot skip such a thread, and the two are the price.

**The pressure path and the exit.** Both read R whole under the mutator's own
claim, below any threshold; neither reads the word. After either the next
round reads R empty and clears the standing, or non-empty and lets it stand.

**The siblings and the timer.** A take reads no backlog, so it neither births
a sibling nor moves a record; `note_idleness` reads `made_a_batch`, and a
take is a batch, so a sibling that made one is not idle that round. A round
that made a take sets the timer's interval to its minimum, as any batch
does; the standing interval is read against the serve clock and is
unaffected.

**The test override.** `testing::take_standing_after(Option<Duration>)`
replaces the interval for a case, as `ask_turnovers_after` does for X, reset
in `retire`; the word gets a `_for_test` reader and no setter, a case
reaching the take by running rounds against an interval of a millisecond.

## Cost

One word on a line already loaded; one compare and one clock read per round
per sub-threshold record, the clock read shared with the ask's. On the
mutator, nothing until the take; the take is one batch's foreign-holder
window over the standing roots' closure under `TRACE_BLOCK_BUDGET` — the
closure of a median candidate on the corpus of 2026-08-25 is the heap, 381
objects — and one collection over P at its next poll. The budget is spent
by the union of the roots' closures rather than by their number, a root
inside another's closure meeting rows that already say met, so a take of
sixty-three roots over one shared closure costs the rows one root costs.
Where the closures do not overlap and their sum meets the budget, every
root of the take comes back `Unwalked` and the mutator traces them exactly
at that poll — the trace the X collection over R whole would have run four
seconds later (`dev/DECISIONS.md`, "a take's trace is budgeted as one
batch's, and an unwalked take shifts the mutator's trace rather than adding
one", the Sage's `Final` of 2026-09-22).

On the collector, one `REQUEST_WAIT` per standing request and none after it:
a sleeping mutator pays its collector 2 ms once, when the take's request
first goes onto its byte, and then stands until it wakes. A thousand parked
threads cost one two-second round once, where a withdrawal at each deadline
would have cost that round every interval for as long as they sleep. The
term the standing form adds instead is the checkpoint's pass — one load per
standing entry per byte event on the slot, and the list holds every parked
sub-threshold thread, so a thousand of them make a pass a thousand cold-line
loads, of the order of 100 µs, an estimate and not a measurement. The
building stage measures it with a parked-thread arm. Then, once the take has
deferred a live root, the S60 term: one in-line collection per X while the roots live, which
a thread that never reached the threshold did not pay before this rule. For
live candidates the rule is that recurring term and nothing gained; for dead
ones it is the memory of the standing ring returned an interval after it
appeared instead of at the threshold, at pressure, or at exit.

What the building stage measures first, per take on the corpus: what the
mutator pays — the frees withheld during the take's window and the in-line
collection over P after `POSTED` (an exact trace of the proposed and
unwalked roots, `EmptyLane` when all read live) — the recurring in-line
collection per X once the thread has a lane, and the round's length against
the number of sleeping sub-threshold mutators, each against the memory a
standing ring holds, at most 63 entries and the entities they name. The
null arm is the interval at its off value. The take's trace runs on the
collector's thread and is not the mutator's time.

## Refused

- **A count of rounds** as the interval: rounds are wake-driven and
  unbounded below; the S60 ruling rejected the form by name.
- **An unchanged ring rather than a non-empty one** (the second and third
  forms of this document, with a tail-index word and a clock test): Edmond,
  2026-09-22 — once the first condition held, the length no longer matters.
- **The mutator's poll tracing its own sub-threshold R.** Edmond's five
  lines of 2026-09-17: the mutator does not collect its roots itself except
  under memory shortage.
- **A withdrawal at the take's deadline** (the fifth form, and Edmond's
  refusal of the morning of 2026-09-22, made when standing requests lived in
  a sixteen-entry array on the collector's frame). The array is gone, the
  record is where a standing request lives, and the reason the refusal
  rested on with it; the Sage ruled the standing form the same evening on
  Edmond's delegation. What he refused and what stands refused is the
  no-wait form, in which a working mutator consents and withholds until the
  collector comes round: the wait stays, and only a sleeper's request
  stands.
- **A take under K.** K is the collector's estimate for a producing
  mutator's batches; a take of three would double it toward the budget, and
  a K of one would take a ring of three one root per round.
- **Dropping the threshold parameter from `batch`.** The take passes the
  round's threshold and carries its own flag; the parameter's removal would
  re-aim `the_batch`'s clamp cases for no runtime change.
- **A take restamping the turnover ask by fiat**, as a threshold batch
  does: an all-live take moves no clock, and the restamp would keep the ask
  from ever reaching X on a dripping thread.
- **An instant left standing across a take**: the round after it would
  re-take a write-back at the round's cadence.

## Open

None. The three questions of the third form were ruled on 2026-09-22 —
time rather than rounds, non-empty rather than unchanged, the sleeping
thread left alone — and the rulings are the entry in `dev/DECISIONS.md`.
