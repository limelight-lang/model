# The standing request lives on the record

Accepted 2026-09-22 (`dev/DECISIONS.md`, "the standing request lives on the
record, the checkpoint serves one grant, and no count is capped"): the Sage's
ruling, attacked by a Critic, amended by the Sage, accepted by Edmond. Built
the same day, as `worker::Standing` and the link pair on the reader line of
`MutatorRecord`; the `rfc`'s handshake
(`rfc/dev/design/trace-token-handshake.md`) is rewritten to this and is
normative. This document keeps the argument and the refused forms.

## What changes and what does not

The collector's request for a mutator's token and the wait for its consent
stay as built: one swap `FREE → REQUESTED|slot` on the byte, up to
`REQUEST_WAIT` on the byte in a loop, a mutator that answers inside it — a
working mutator answers at its first free or poll — batched at once. The
mutator's side changes in nothing: `token.rs`'s state machine,
`read_and_act_on_this_thread`, the poll and the free path are untouched; the
one edit in `token.rs` is which wake entry the consent and the refusal call.

What goes is the array: `Standing` as sixteen entries on the collector's
frame, `STANDING_CAPACITY`, the "past the capacity, withdrawn at once" arm,
and the silent mark in the meaning "missed a wait". A request the mutator
did not answer inside the wait is not withdrawn; it stays on the byte, and
the record joins a list threaded through the records themselves, with no
capacity. The reason for the cap's removal is Edmond's: the number of
mutator threads is not known in advance, and a fixed count of them cannot
bound anything — and as built the seventeenth is stranded, requested and
withdrawn within nanoseconds every round until an entry frees.

## The words

On the reader line of `MutatorRecord`, the collector's line, in the sixteen
bytes at 48–64 and the one at 28 that `silent`'s removal leaves (the layout
asserts stand):

- `standing_next`, `standing_prev: AtomicPtr<MutatorRecord>` — the link
  pair of a doubly linked intrusive list whose two end pointers stand on
  the collector's frame. The ends are self-terminated: the last record's
  `next` and the first's `prev` name the record itself, so both words are
  null exactly when the record is in no list, `next != null` is "linked" in
  O(1), and no pointer names anything but a record (a head sentinel on the
  frame would be a `*mut MutatorRecord` to two stack words, dereferenceable
  by nothing). Written by the collector the record names, read by no
  mutator, in one order: `next` is the first word a link writes and the
  last an unlink clears, both with Release, `prev` and the neighbours' words
  strictly between — so the registry's one Acquire load of `next` sees
  nothing of the link or all of it, and a record can be renamed only while
  unlinked. Without that order the registry could hand out a record between
  an unlink's two stores (the reset's assert fires against a correct
  registry) or a push's (the record renamed under a link in flight, two
  lists in one chain).
- `released_unserved: AtomicU8` — set by the collector when a pass released
  this mutator's grant without a batch; read and cleared by the collector's
  next request to it. The one meaning the byte cannot carry ("this collector
  wrote `FREE` here without serving"), set and read by the collector alone.

`ReaderLine::reset` at a re-take clears the byte, does not write the links,
and debug-asserts them null. The registry hands out no record whose `next`
is non-null: `first_free_record` skips it as it skips a record whose hold
word is not clear, takes the next free record or carves one, so no thread
start waits; the skipped record stays on the free list until its
collector's next pass drops it — one round at most, since the exit's take
from `REQUESTED|c` is a refusal that wakes slot c. The invariant, stated at
the field: a record's link words are written by the collector its record
names, and a record is renamed only while unlinked — `hand_over_half` moves
only batched records, which are unlinked before their batch; `reclaims`
takes only a slot reading `UNBORN`, stored after that thread's `Standing`
dropped; the registry renames only what the gate saw unlinked.

On each collector's slot (`Collector`, the static array): `byte_wakes:
AtomicUsize`, a sequence number. `wake_for_the_byte(slot)` increments it
(Release) and wakes; `TraceToken::consent` and the refusal in `take_unless`
call it. The block-filled signal, the pressure ending's wake and
`end_idle_siblings` keep `wake` and move nothing.

## The collector

`Standing` is the end pair (first, last), `byte_wakes_seen`,
`batches_served` and `consumed_a_wake`, a `thread_body` local passed by
`&mut` as today. `push` appends at the tail and is idempotent — a linked record is left where it is
(by the invariant it can only be in this list; the case is a stale entry
whose byte moved `REQUESTED|c → MUTATOR → FREE` between the pass's read and
the walk's request). `forget` unlinks in O(1) and is a no-op on an unlinked
record.

**The checkpoint.** If the list is empty or `byte_wakes` (Acquire) equals
`byte_wakes_seen`, answer 0 without walking: nothing moves a standing
entry's byte out of `REQUESTED|c` without a wake to slot c — a consent, a
refusal (the exit's and the retirement pass's takes are that refusal), a
foreign request failing on anything but `FREE`; this collector's own
withdrawals are its own. Else record the loaded value and pass: read the
whole list once, one Acquire load per byte. `REQUESTED|c` stays.
`COLLECTOR|c`: the first is remembered; every further one is unlinked,
marked `released_unserved` and released with `release_claim(slot, false)` —
`FREE`, never `POSTED`, which over an empty P would arm a collection over R
for nothing. Anything else is unlinked. Then, if one was remembered, it is
unlinked and served; a batch counts in `batches_served`. A wake during a
pass is caught by the next checkpoint. One load per checkpoint, one pass
per byte event; a burst of n consents is one pass.

Checkpoints run at the round's start, before every request (the top of
`serve`), and after every wait return inside `wait_for_consent` that was
not the grant.

**The serve.** Checkpoint; the pre-claim reading as today; then the push,
and only then the request — linked before the request lands, so that an
exit which takes the request finds the record in the list and the
registry's gate holds it (linking after the wait would let the exit's
refusal, the free list and a new life's take all run between the byte read
and the link; the Critic's finding of 2026-09-22 over the list). `TraceToken::request`'s success
is `AcqRel` for the same reason: the exit's take synchronizes with it, and
the registry's acquire load of `next` then sees the link. A request that
fails publishes nothing, so the reading's hold spans the push and the
request and is handed back only after the refusal's unlink: an exit that
met the hold leaves its blocks to the hand-back, and the gate, which reads
the hold word before the link word, is ordered after the hand-back and so
after the unlink (the Code Reviewer's finding of 2026-09-22; the case
`a_refused_request_unlinks_before_the_readings_hold_goes`). Every outcome
that leaves no request standing unlinks: the refusal's `POSTED` and
`TokenHeld` arms, the withdrawal's `Withdrawn`, `TakenByTheMutator` and
`MovedOn` arms, and `serve_the_grant`'s top; the refusal's own-request arm
leaves the record linked, as it is. On the request's success: if the
record is marked `released_unserved`, clear the mark and answer
`Unanswered` with no wait — a released mutator is asleep again by the time
the walk reaches it, and a wait per released worker would cost a
broadcast-woken pool n²·W/2 of collector time. Else `wait_for_consent`.

**The wait.** The loop as today; the grant arm serves through
`serve_the_grant`, which unlinks first; a read neither grant nor request
goes to `answer_the_withdrawal`. At the deadline: clear
`WithdrawOnDrop.standing` and answer `Unanswered`; the byte stays
`REQUESTED|c` and the record stays linked. The in-wait checkpoint can serve
the waited mutator itself when its consent lands between the loop's read
and the pass; the loop's next read then finds it moved on and the withdrawal
answers `Idle`, the batch already counted. Every entry to `serve_the_grant`
is on an unlinked record, so an unwind inside the batch leaves a consistent
list.

**The drop**, at the thread's end and on the unwind: for each entry,
withdraw; a `Granted` read-back is released with no batch; then unlink.
The take of a standing R (`dev/design/a-standing-r-is-taken-after-n-rounds.md`) and the
unwind guard are unchanged: the take withdraws at its deadline and leaves
nothing standing, by Edmond's rule.

No checkpoint inside a batch: one arena per collector; the batch is the
floor of any window a consent lands inside.

## The bounds, as they are

A mutator that answers inside the wait: its own batch. A sleeper consenting
from a standing request: the stranger's batch in progress at its consent,
plus its own if it is the head at the next pass; otherwise it is released at
that pass and re-requested without a wait within one round, and served at
the pass where it is the head — in the walk's order, which is fixed across
rounds since a released record is pushed again at the walk's next request to
it, so in a lockstep burst of n the last is served at its n-th event, each
window at most one stranger's batch. A blocked take or an exit waits at most
one stranger's batch plus, if served, its own; the release is real for it
(both CAS on `FREE`; a re-request the take meets is a refusal that wins).
Returns withheld inside a window are made at the mutator's first reading
after the release that finds `FREE` or `POSTED`; a mutator whose one reading
per event meets the re-request consents instead and carries what it
withheld to its service. No term grows with the number of threads; what
grows with a burst is memory held under microsecond windows, not time under
the byte.

Collector time: O(1) per checkpoint without a byte wake, one pass per byte
event, no wait for a released mutator, one `REQUEST_WAIT` once per new
sleeper.

Inside a neighbour's batch a consent is to a standing request, or to the
walk's own outstanding request on the mutator whose wait loop that batch was
served from (the handshake's third round, two requests outstanding); the
loop's next read after the batch is the grant and serves it at once, its
window the rest of the standing batch plus its own.

## Refused

- **No wait; the consent pushes the record onto a lock-free stack.** A
  round over n active mutators leaves n requests standing within one walk,
  all consent within one poll interval, the i-th withholds i batches; and a
  second RMW on the free path, which the owner's budget of one acquire load
  per free forbids. Edmond's reason: a mutator that consented gives no memory
  back until the collector reaches it.
- **No wait; request, serve the neighbour, come back.** The active
  mutator's window doubles whenever the neighbour has a batch; with idle
  neighbours the collector returns in microseconds and withdraws before a
  mutator polling at 100 µs–1 ms answered — service once in tens of rounds.
  The "walk until 2 ms of useful work" variant degrades into the wait when
  there is no work and into the doubled window when there is.
- **Serving every grant found at a checkpoint.** The burst-wake queue once
  the cap is gone: the last waker withholds for the whole burst.
- **Deferring a released mutator's re-request to the next round.** Costs at
  least `FALLBACK_INTERVAL_MIN` of unserved R per record to save carrying
  one window's withheld returns through a window that is its own batch.
- **A fixed capacity in any form.** Strands the entry past it.
- **A checkpoint inside the batch.** A second grant could be served only
  with a second arena or by abandoning the batch on the wrong thread.
- **A singly linked list with `forget` deleted.** A grant served through
  `answer_a_refused_request` leaves the entry linked; a handover then gives
  the record to a sibling whose push writes the same word — two lists in
  one chain, orphans standing `REQUESTED|c` forever.
- **A wait-return flag as the pass's gate.** Misses a consent that lands
  mid-walk with no wait between two checkpoints; the sequence number does
  not.

## Tests the build owes

More than sixteen sleepers all standing and served on waking. A burst of k
consents inside one held batch: one batch at the pass, k−1 releases to
`FREE` within `A_WAKES_DELAY` of its end, then served FIFO with no wait paid
for a released one. A record with a standing entry whose thread exited,
refused by the registry until the pass dropped it (`take_this_record_for_test`
naming it, the take falling through to a carve). A consent between
`between_the_take_and_the_reading` and the request, served with the record
unlinked afterwards. A retired collector leaving every record unlinked. A
test-only pass counter: k idle serves make no pass, one consent makes one.

Of the existing cases, `what_the_byte_arms.rs`'s assertions of `is_silent`
and of `FREE, "withdrawn"` after the first expired wait are the contract
moving — the byte reads `REQUESTED|ELDER` there and no mark exists; the
consent's part stays byte for byte. `under_stress.rs`'s assertions and both
ignored probes stay as they are: they are the window bound's instrument.
`a_batch_held_open_beside_a_silent_sleeper`'s wait is met at the first round
and stays.

## Not established from the files

(The reader line's layout, 64 bytes with the pair and the byte, is a
`const` assert in `mutator_record`.) No writer of the byte outside `token.rs` and its three test-only writers
was read; another would need the byte wake. "No listed record is renamed"
rests on `hand_over_half` reading only `backlogged`, on `reclaims` taking
only `UNBORN` slots, and on `catch_unwind` in `run_the_life` under the test
profile's unwinding (release is `panic = "abort"`, where the question is
moot).
