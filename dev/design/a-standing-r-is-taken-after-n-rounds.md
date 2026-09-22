# A standing R is taken after an interval

Accepted 2026-09-22 as the final algorithm; fifth form, the
algorithm as Edmond ruled it after the Critic's two rounds and repaired to
a third
(`dev/DECISIONS.md`, "a standing R is taken after an interval of the
collector's own, and no request count is capped"). Not built, not normative,
and no stage opened for the build — `PLAN.md`, "A quiet thread's garbage is
taken after X", is the owner; the `rfc` moves on adoption.

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
does not consent to the take's request inside the consent wait is asleep and
is left alone until the next interval. The mutator's side does not change:
it registers into R, signals a filled block, reads its byte at a poll or a
slot free, consents, and disposes of P after `POSTED`, as for any batch.

`STANDING_INTERVAL` is the collector's own time, 4 s by default, and the
embedder sets it through `ll_gc_set_standing_interval(millis)` beside
`ll_gc_set_quiet_interval`, zero restoring the crate's. It is not a count of
rounds: rounds start on every wake — a block of R filled anywhere, a consent,
a pressure collection's ending — and the timer's wait after any batch is
10 ms, so a count of rounds would hand the standing mutator's rate to the
busiest thread, the form the S60 ruling refused for the deferred lane.

What the rule accepts: a sleeping thread's garbage is not taken until it
reaches the threshold, collects under pressure, or exits; a working thread
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

One word on the reader line of `MutatorRecord`, the collector's line, in the
16 bytes the line has spare (48 of 64 used; the layout asserts stand):

- `standing_since: AtomicU64` — the serve clock's reading (nanoseconds
  since `SERVE_CLOCK_BASE`, the clock `served_at` uses, never zero) at the
  round that first read R non-empty and below the threshold; zero for no
  standing ring.

The collector's own word, written by the collector the record names and read
by no mutator, so relaxed; reset with the line at a re-take
(`ReaderLine::reset`). Two collectors never read one record — a record moves
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
   record and shared with the turnover ask's reading after the serve.

The take is the serve of today from the P-room test on — the room read, the
request by one swap from `FREE`, the wait for consent, the batch under the
grant, the release — with four differences, carried by a `take_anyway` flag
that `serve` sets from branch 3 and passes through `wait_for_consent`,
`answer_the_withdrawal`, `serve_the_grant` and `batch`; the threshold stays
the round's, and the grant arm of `answer_a_refused_request` and
`Standing::checkpoint` keep their threshold form, since neither is a take:

- **The instant restarts when the request lands.** At `token.request`'s
  `Ok`, hit or miss to come, `standing_since` takes the clock's reading
  now, so the next take of this ring is an interval away whatever this one
  does. A request refused at the swap — `POSTED`, `MUTATOR`, another
  collector's — leaves the instant, and the take is tried again next round.
  Without the restart the round ten milliseconds after a take would read a
  write-back, or a registration inside the window, as a ring past its
  interval and take it again at the round's cadence.

- **The batch is the ring whole, and K is left alone.** `batch` clamps K to
  P's room and to what R holds and sizes the next K from this batch's
  outcome; under `take_anyway` the clamp is R's count as the peek reads it,
  at most 63, and `size_the_next_batch` is not called. A take must feed K
  neither way: four takes of three live roots each would double K toward
  `BATCH_BOUND` and hand the thread's first real batch to the budget whole,
  every root `Unwalked`; a K halved to one would take a ring of three one
  root per round.
- **An unanswered request is the end of the take.** A mutator that
  consents inside `REQUEST_WAIT` (2 ms) is batched; one that does not is
  asleep: the request is withdrawn, the withdrawal's read-back answered as
  today's is for every value but the silent mark — under `take_anyway`
  `answer_the_withdrawal` stores no mark — and the serve returns
  `Served::Idle`. The take reads no silent mark and leaves no request
  standing; the grant clears the mark as every grant does, which is right,
  a consenting thread being not silent. The wait itself is today's: its
  early return on a stranger's wake runs `Standing::checkpoint` and notes
  the consumed wake as any wait does, so a silent mutator of the threshold
  path that consented during a take's wait is served at that checkpoint and
  not left withholding until the next. The standing requests are the
  threshold path's, and their bounded array is filed for replacement
  (`dev/DECISIONS.md`, the entry above; `PLAN.md`, "The standing request
  moves to the record"); the take leaves a request on the record with no
  wait only when that stage has given a request that home.
- **The backlog reading is unchanged.** `batch` answers
  `Served::Batch { backlog }` by `has_at_least(threshold)` after the batch,
  against the round's threshold; a take of a ring below sixty-four leaves
  nothing that reads as a backlog, so a take births no sibling and moves no
  record in a handover.

After a take the verdicts stand in P and R's front has advanced past the
batch; the mutator's next poll or slot free arms the collection over P, whose
close retires the completed deaths, defers the roots read live and writes
back what it could not dispose of. The next round reads R empty, or holding
the write-back and what the mutator registered since, and branch 2 or 3
takes it from there: a write-back stands as any non-empty ring does, and is
taken an interval later if it still stands.

After a take that missed the consent, the ring stands as it was; the next
round reads branch 3 with the instant just reset.

## Interactions

**The turnover request (S60).** `ask_for_a_turnover_if_quiet` runs after
every serve and restamps `served_at` and `commits_seen` for a serve that
made a batch, by fiat. A take must not be that serve: a take whose roots all
read live leaves the mutator a collection over P that closes `EmptyLane`
with no commit, so the clock stands, and a take every interval would
restamp every interval — for a drip of one live candidate a second the ask
would never reach X and the lane the takes fill would be turned over by
pressure or exit alone, the two halves of the mechanism cancelling on the
thread the rule is for. So a take's outcome reaches the ask as a serve that
reached the ring and not the clock: the restamp is decided by
`clock_stood_since_the_stamp` alone, and a take whose roots were proposed
moves the clock at the mutator's poll and is restamped at the next visit. A
take that missed the consent returns `Served::Idle` and feeds the ask as an
idle serve does: with the clock standing and X elapsed a turnover request is
left for the sleeper's first poll, as today's idle path leaves one. The
same fiat restamp stands in the built S60 rule for a threshold batch whose
verdicts all read live — a thread batched more often than X with all-live
batches is never asked — and is filed outside this stage (`PLAN.md`, "A
batch that moved no clock restamps the turnover ask").

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
objects — and one collection over P at its next poll.

On the collector, a term the rule's "left alone" does not make free: a
sleeping mutator with a non-empty sub-threshold ring costs its collector a
full `REQUEST_WAIT` of waiting, inside the round, once per interval — the
threshold path spares the second wait by the silent mark, which the take
refuses. Sixty-four pool threads parked with a handful of candidates each
lengthen one round in four seconds by 128 ms; a thousand make that round
two seconds, and every producing mutator visited later in it waits that
long for its batch. What removes the term is the filed stage that gives a
request a home on the record: a take could then leave its request without
a wait, served at the checkpoints as the threshold path's are today. Until
then the embedder's off switch is the interval at `u64::MAX` milliseconds. Then, once the take has deferred a live
root, the S60 term: one in-line collection per X while the roots live, which
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
- **A standing request for a take, or a silent mark set by a take's miss.**
  A sleeping thread is left alone until the next interval; the standing
  requests and the mark are the threshold path's.
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
