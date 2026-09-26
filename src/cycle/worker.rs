//! The collector thread, and what it does for one mutator: request the
//! mutator's token and wait for its consent, take a batch of the mutator's
//! candidates from behind its writer, trace them on a copy through
//! `cells::AtomicCells` in parts, one root's closure each under a block
//! budget, post one verdict per root into the mutator's verdict ring P in the
//! parts' order, advance R past them, and release — to `POSTED`, which tells
//! the mutator to collect over P, or to `NOTHING_PROPOSED` when no verdict
//! proposed a set, which owes it P's disposition alone (`rfc/dev/design/trace-token-handshake.md`;
//! `rfc/model/gc/rc-cycle.md`, "Worker-to-owner handoff";
//! `rfc/dev/DECISIONS.md`, "the candidate queue is read behind its writer,
//! and the collector's verdicts come back by a second ring", "The
//! collector's batch"). What the mutator does with the verdicts is
//! `crate::cycle::queue::verdicts`.
//!
//! # The batch
//!
//! Before any claim the collector reads whether the mutator has work — R at
//! the threshold, off the front block's words alone, and P's room: a
//! mutator with nothing to take pays no foreign-holder window, under which
//! every one of its deaths is withheld. Then it requests
//! the token by one swap from `FREE`, which fails on a mutator collecting in
//! line — the mutator holds `MUTATOR` through its close — on one that has
//! not disposed of the last batch (`POSTED`) and on every other holder, a
//! failure being a skip; and it waits for the mutator's consent, which the
//! mutator gives at its next slot free or poll ([`serve`] says how long,
//! and what a mutator that never answers costs). It peeks up to K entries
//! from R's front through the reader's pair without consuming them, K
//! clamped to P's room and to what R holds, and copies them into its
//! workspace. It posts first the roots no trace can place, a count read zero
//! *zero-count* and a root with no row *read live*, and traces the rest in
//! parts ([`trace_in_parts`]): in R's order, each root still without a
//! verdict opens one, the mark and the scan of its closure alone through
//! `cells::AtomicCells`, on the arena above a watermark over the copy and
//! under a budget of [`TRACE_BLOCK_BUDGET`] blocks of its own. A completed
//! part posts its root's verdict and the verdict of every other root its
//! rows met, *proposed* or *read live* ([`verdict_for`]), and the arena is
//! reset to the watermark for the next; a root an earlier part met opens
//! none. A part that meets its budget is retried at once under
//! [`RETRY_BLOCK_BUDGET`], `B_max`, once per grant. A retry that meets it too,
//! and a part that meets B with the retry spent, post every live root their
//! rows met *read live*, which defers each to the turnover, and the batch goes
//! on with the next root. A refused allocation or the mutator's recall of its
//! token ends the batch: no color of such a part is a verdict, so its root
//! and every root still without one are posted *unwalked*, which the
//! mutator's collection over P writes back into R untraced for the next
//! batch, while the verdicts of the parts before it stand (`rfc/model/gc/rc-cycle.md`, "Worker-to-owner handoff"). R's
//! front advances past the batch only after every verdict is posted, by
//! one guard that runs from the unwind as well and posts *unwalked* for
//! every root the unwind left without a verdict ([`FinishThePosts`]), so
//! that no entry is consumed without a verdict and none twice. The arena is opened under
//! the grant, one per batch, and is reset before the token goes — on the
//! unwind as on the return, since its rows stand over the mutator's blocks
//! (`rfc/dev/design/trace-token-handshake.md`, E2); a workspace the pool
//! refuses is a grant released with no batch.
//!
//! K starts at [`INITIAL_BATCH`] and doubles after a batch whose every part
//! completed over its whole clamp, up to [`BATCH_BOUND`]; nothing halves it,
//! since a part past the budget defers the roots it met rather than losing
//! the batch's rest to *unwalked*. The bound is under a
//! block's capacity, so a batch spans at most two blocks of R, and small
//! enough that the copy leaves the workspace to the rows. A take of a standing ring reads K
//! neither way ([`batch`]): it clamps one entry short of the threshold,
//! which such a ring cannot exceed, so its clamp is the threshold's rather
//! than K, and a take that filled it would size K from the threshold on a
//! thread that has produced no batch at all.
//!
//! # The recall of the token
//!
//! A mutator that needs its token while this thread traces for it recalls it:
//! its take marks the token before it waits
//! (`crate::cycle::token::TraceToken::take_unless`), and the trace reads the
//! mark every [`RECALL_STRIDE`](crate::cycle::arena::RECALL_STRIDE) positions
//! of storage it reads, whether or not a position holds a counted reference, in
//! the mark and in the scan alike
//! (`crate::cycle::arena::TraceScratchArena::inspect_position`), and at every
//! block the arena draws, before every part but the first and at every root
//! of the pass before the parts
//! (`crate::cycle::arena::TraceScratchArena::read_the_recall_now`). A trace
//! that finds the mark stops where it stands, and the release after it is the
//! release of any abandoned batch: [`FinishThePosts`] — at most K posts of
//! `Unwalked` and one advance of R — and one reset of the arena; a grant whose
//! mark stands before the batch is made is released with no batch. What the
//! mutator whose batch is traced waits through is therefore one stride of
//! positions, those posts and that reset, whatever the closure of its roots or
//! the width of an entity; the block budget bounds the arena and not the wait
//! (`rfc/model/gc/rc-cycle.md`, "The recall of the token"). A recalled batch
//! leaves K where it stands, the recall saying nothing of the batch's size.
//! A mutator freeing under the grant recalls it the same way without waiting,
//! once one of its withheld stacks holds its mark
//! (`crate::cycle::deferred_slot_reuse`, "The marks by stack length").
//!
//! A grant this thread holds while it traces another mutator's batch — one
//! the standing list holds after a consent that came during that batch — has
//! no batch of its own to abandon: the take that meets it also sets the slot's
//! word ([`recall_the_grants_of`]), and the same readings of the recall release
//! every such grant whose mutator recalls it, with no batch, while the batch traced
//! goes on ([`Standing::release_the_recalled`]). Its mutator waits at most one
//! stride of the other's batch and one pass over the list, or, where its take
//! lands after the batch's last reading, the rest of that batch, its posts and
//! reset, and the pass its consent admitted.
//!
//! # The epoch clock
//!
//! The collector keeps the epoch clock of every mutator named to it
//! (`crate::cycle::epoch`). At each visit of a round, before the serve, it
//! advances the record's cell once [`crate::cycle::epoch::BATCHES_PER_EPOCH`]
//! batches or [`EPOCH_INTERVAL`] of its own clock — X, the rfc's letter for
//! it (`rfc/model/gc/cycle/questions.md`, Y9) — have passed since the last
//! advance, whichever comes first ([`advance_the_epoch_if_due`]), and stores
//! the cell's low eight bits on the record's token line, where the mutator's
//! poll compares them with its deferred lane's mirror and re-offers the lane
//! (`crate::cycle::queue::reoffer_deferred_if_epoch_moved`). The mutator
//! stores nothing into its clock, so a thread whose batches all read live,
//! and one that registers nothing, turns over at X like any other, and a
//! component that became garbage behind one of its deferred roots waits
//! about one X and the next round's take of the merged lane rather than
//! until pressure or exit; the re-offer arms no collection of the mutator's
//! (`crate::cycle::queue`, "What the poll does for this module"). The first
//! visit of a life stamps the instant and advances nothing, X being counted
//! from a reading and never from the record's birth, unless the registry
//! noted a new life, which advances at once (`crate::cycle::epoch`, "A record's next life";
//! `dev/DECISIONS.md`, "the collector finds and the mutator judges, and a
//! recall of the token bounds the mutator's wait instead of the budget").
//!
//! # The thread, and the round over the records
//!
//! The elder collector thread is born by [`ensure_thread`] and never at
//! startup: at the first wake a mutator's poll would send it — a block of R
//! filled (`crate::cycle::queue::signal_the_collector_if_due`) — and at each
//! ending of a pressure collection
//! (`crate::cycle::collect::collect_under_pressure`), so that a process
//! that never fills a block and never runs short holds no thread. It starts
//! as any registered thread does, through `ll_thread_init`, whose base block
//! draw can be refused; a refused base block is a thread that never started,
//! and a call [`BIRTH_RETRY_INTERVAL`] or more after the refusal births
//! again. A round walks every record the registry has carved ([`round`])
//! and serves each mutator whose R holds [`SOFT_THRESHOLD`] entries or more,
//! by the collector's own reading off the front block, and each whose R has
//! stood non-empty below that threshold for the standing interval in force
//! — [`STANDING_INTERVAL`] unless the embedder replaced it
//! ([`standing_interval`]) — or holds a deferred lane its owner has merged
//! since the collector last accounted for its merges, by
//! [`decide_the_branch_and_stamp_the_instant`]; nothing but a test ends the
//! thread.
//!
//! **A wake starts a round and decides nothing else** (`rfc/dev/DECISIONS.md`,
//! "the collector traces on the count it reads itself"; `rfc/model/gc/
//! rc-cycle.md`, "Signals"). Between two rounds the thread waits on its
//! fallback timer, and four things end the wait: a mutator's poll, a
//! registration having filled a block of its R since its last signal
//! ([`wake`], through `crate::cycle::queue`); a mutator's consent to a
//! request this collector left standing; a pressure collection, at every
//! ending; and the timer. The timer is what serves a mutator whose signal
//! bought no batch — its token held or it collecting in line at the round,
//! P without room, the workspace refused — and a mutator at the threshold
//! that reaches no poll; a mutator below the threshold is served by the
//! round that reads its ring an interval overdue or merged into since the
//! last grant, and by the checkpoint that answers such a take's consent
//! when the mutator was asleep at it.
//! A walk spends at most [`EXPIRED_WAITS_PER_ROUND`] consent waits on
//! mutators that do not answer; past that every request it lands is left
//! standing at once, for the checkpoint that reads the consent, and that
//! checkpoint carries the batch's backlog and a refusal it read back to
//! the round ([`Standing::backlogged`], [`Standing::saw_work`]).
//! The interval adapts between [`FALLBACK_INTERVAL_MIN`] and
//! [`FALLBACK_INTERVAL_MAX`]: the minimum after a round that made a batch,
//! so that a backlog above the threshold drains at a batch per minimum,
//! and after one that read a mutator's note that the collection its poll
//! fired freed or retired something
//! ([`MutatorRecord::take_freeing_disposition_note`]); held after a round
//! that read a mutator at the threshold and could not serve it; doubled
//! after a round that read no mutator at the threshold or overdue — a mutator at
//! `POSTED` or one whose request stands unanswered is neither a batch nor
//! work, and its note is the way back — so that a process with nothing to
//! screen costs a wake a second. What a wake with no thread to receive it
//! costs is [`wake`]'s; one sent during a round ends the wait that follows
//! it, and a wait inside a round that consumed a wake is paid for by
//! skipping the sleep after that round once.
//!
//! # Siblings
//!
//! Several collectors divide the mutators, each mutator named to one collector
//! by a word in its record ([`MutatorRecord::collector`]); two collectors
//! never read one mutator's ring. The elder, slot [`ELDER`], is the one the
//! pressure path births and every fresh record is named to. A collector
//! that served a backlog — two or more mutators still at the threshold after
//! their batches, read off the front block under the token, since one
//! mutator is read by one collector at a time and a backlog of one is nothing
//! a sibling relieves — for [`BACKLOG_ROUNDS_TO_BIRTH`] rounds in a row
//! births a sibling into the first empty slot under the embedder's cap
//! ([`set_collector_cap`]), the elder's slot included, through the elder's
//! own birth path with its retry interval, and names every second of those
//! backlogged mutators to it; the sibling's first round runs at its birth. A
//! mutator's poll wakes the collector its word names. The elder ends a
//! sibling that made no batch and saw no work for [`IDLE_ROUNDS_TO_END`]
//! rounds in a row, and one above a lowered cap, by its state word, which
//! the sibling reads before its next round, and only in a round of its own
//! with no backlog, so that it does not end what it is about to birth back;
//! the mutators of a slot with no thread — ended, refused at its birth, or
//! unwound — are named back to the elder by its next round, and a signal
//! sent to that slot meanwhile is lost with its count standing. The word
//! says whose a mutator is between rounds; that
//! one collector reads a ring at any instant is the token's, and a reclaim
//! landing beside a slot's rebirth resolves at the token like any two
//! claims.
//!
//! # Cap zero
//!
//! A cap of zero removes the takes and keeps the thread: the elder is born
//! by the poll's signal as under any cap, and its rounds visit every record,
//! advance each epoch that is due and give back a stale live list, and
//! request no token; no sibling is born under it, and every sibling standing
//! ends at the elder's next round without a backlog. The collections the
//! takes would have made are the mutator's own, over R whole: the round that
//! reads a mutator's R where it would have taken it — at the threshold,
//! standing past its interval, or merged into — asks for one by writing
//! [`ASKED`](crate::cycle::token::ASKED) over an empty P
//! ([`ask_for_an_in_line_collection`]); a merged deferred lane is asked for as
//! a take would have taken it, at the round after the merge
//! (`rfc/model/gc/rc-cycle.md`, "Decision summary"). R is read by the elder
//! rather than by the poll, and the ask is a value of the byte the mutator
//! reads anyway, so that the mutator's side reads no cap at all: its reading
//! tells the ask from a batch's `POSTED` by the byte alone.
//!
//! **A cap set to zero while collectors work is met at the next checkpoint
//! and the next grant.** A checkpoint under the cap withdraws the standing
//! list instead of reading it: a standing request is taken back, a grant the
//! withdrawal reads back is released with no batch, and every record is
//! unlinked. A grant read on any path after the cap is stored is released
//! with no batch ([`serve_the_grant`]), so no trace starts after the store; a
//! trace already running finishes, and its release is answered by its owner
//! as any batch's is. A request the visit under way made after the
//! store stands until the next checkpoint withdraws it. A sibling's own
//! rounds withdraw its list in the same way, the elder ends it at its next
//! round, and its list's drop takes what is left. A cap set back above zero
//! resumes the takes at the next round, and an ask a round left standing is
//! collected over by its owner as an ask.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::cells::AtomicCells;
use crate::cycle::arena::{GrantsBehind, TraceScratchArena};
use crate::cycle::mark::{MarkResult, mark};
use crate::cycle::mutator_record::{self, MutatorRecord};
use crate::cycle::queue::verdicts::{Verdict, VerdictWriter};
use crate::cycle::row::{EdgeTarget, resolve_edge_target};
use crate::cycle::scan::{ScanResult, scan};
use crate::cycle::shadow::{self, Color};
use crate::cycle::token::{COLLECTOR, MUTATOR, POSTED, REQUESTED, Withdrawn, state, word};
use crate::refcount::RcHeader;
use crate::ring::{BLOCK_ENTRIES, Reader};

/// What one round over one mutator did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Served {
    /// The byte was not free: the mutator collects in line, another
    /// collector holds or asks for it, or the mutator refused the request.
    TokenHeld,
    /// The mutator has not disposed of the last batch's verdicts: the byte
    /// reads `POSTED`. Neither a batch nor work — nothing the collector can
    /// do serves it — and the mutator's note of its disposition is what
    /// brings the timer back.
    Posted,
    /// The request stands unanswered: the mutator reached no slot free and
    /// no poll inside the wait, or its request stood from an earlier round
    /// already, or a pass released it without a batch and this round's
    /// request was pushed with no wait, or this walk had spent its bound of
    /// expired waits ([`EXPIRED_WAITS_PER_ROUND`]) and the request was
    /// landed without one. Neither a batch nor work; the
    /// request is served at a checkpoint when the mutator answers.
    Unanswered,
    /// Nothing was taken: before any claim — R below the threshold, P
    /// without room, the record under another collector's reading — under
    /// the claim, when the workspace was refused, the mutator had recalled
    /// its token or the peek came up empty, or at the withdrawal, when the
    /// record had moved on.
    Idle,
    /// Under a collector cap of zero, R read where a take would have taken it
    /// and the mutator asked to collect it in line
    /// ([`ask_for_an_in_line_collection`]).
    Asked,
    /// A batch was made: this many roots taken from R, each with a verdict
    /// posted into P, whether their trace completed, and whether R still
    /// held the threshold behind it, by its count under the token.
    Batch {
        roots: usize,
        complete: bool,
        backlog: bool,
    },
}

/// Roots a batch takes from a mutator the collector has not served before.
/// Not a measured figure: the rfc names no start, and the size adapts from
/// here by the batch's outcome.
const INITIAL_BATCH: usize = 64;

/// The most roots a batch takes: under a block's capacity, so that the peek
/// spans at most two blocks of R; a copy and its order by address of at most
/// a quarter of the workspace's bump, so that the rows of the trace do not
/// start by growing; and an index of the copy fits the order's `u16`.
const BATCH_BOUND: usize = 1024;

const _: () = assert!(BATCH_BOUND < BLOCK_ENTRIES);
const _: () = assert!(
    BATCH_BOUND * (size_of::<usize>() + size_of::<u16>()) * 4
        <= crate::cycle::arena::WORKSPACE_BUMP_BYTES
);
const _: () = assert!(BATCH_BOUND <= u16::MAX as usize + 1);

/// Blocks one part of a batch's trace may draw above the collector's
/// workspace before the part is retried under [`RETRY_BLOCK_BUDGET`], or, with
/// the grant's retry spent, defers the roots it met to the turnover
/// ([`trace_in_parts`]). Not a measured figure: it
/// bounds a part's share of the collector's arena and the reset between two
/// parts, and `B_max` bounds the arena at any instant of a grant; the
/// mutator's wait for its token is the recall's
/// ([`RECALL_STRIDE`](crate::cycle::arena::RECALL_STRIDE)).
/// **What this bound decides is whether a part is worth anything to the
/// mutator at all.** Measured on takes traced as one part, before a part past
/// it was retried (`dev/BENCHMARKS.md`, "the live-roots arm"): 63 live roots
/// whose closure fit here left the mutator's collection over P nothing to
/// walk, 15,365 instructions against the 658,832 of collecting the same rings
/// in line; a closure past it was abandoned after 75 to 157 µs of the
/// collector's time, and the mutator met the same rows itself for 0.68 % more
/// than it would have spent without the take, whatever share of the ring was
/// live (`dev/BENCHMARKS.md`, "the mix"). Inside the bound that share is what the
/// mutator saves: it walks the roots the parts proposed and no others.
const TRACE_BLOCK_BUDGET: usize = 8;

/// `B_max`: the blocks a part that met [`TRACE_BLOCK_BUDGET`] is retried under,
/// at once and once per grant, before the roots its rows met are posted read
/// live (`dev/CYCLE-SPLIT-PACKAGE-3.md`, section 7). A borrowed number the stage's
/// rig reads (`PLAN.md`, S65.17); 128 blocks are 8 MiB drawn and given back
/// inside one grant.
const RETRY_BLOCK_BUDGET: usize = 128;

/// Entries a mutator's R holds at or above which a round takes a batch from
/// it. Not a measured figure: the rfc names the threshold as the runtime's
/// own and not its size, and this one is [`INITIAL_BATCH`], so that a
/// mutator at the threshold gets a full first batch. The poll's signal is
/// sent on a block filled, a coarser unit, and the timer's rounds read this
/// one.
pub(crate) const SOFT_THRESHOLD: usize = INITIAL_BATCH;

const _: () = assert!(SOFT_THRESHOLD <= BATCH_BOUND);

/// How long a collector waits for a mutator to consent to a request before
/// it leaves the request standing on the byte and moves on. Not a measured
/// figure: it lands above the tail of the interval between two polls or
/// slot frees of a running mutator on the corpus, which bench measures
/// (`rfc/dev/design/trace-token-handshake.md`, "Cost"); a mutator blocked
/// past it is asleep, its request stands until it answers, and the next
/// round's request fails on the standing one and waits nothing.
const REQUEST_WAIT: Duration = Duration::from_millis(2);

/// The fallback timer's minimum: the wait after a round that made a batch or
/// read a freeing disposition. Not a measured figure.
const FALLBACK_INTERVAL_MIN: Duration = Duration::from_millis(10);

/// The fallback timer's maximum, reached by doubling after empty rounds.
/// Not a measured figure: what it bounds is how long a mutator at the
/// threshold that reaches no poll waits for a round nobody signalled.
const FALLBACK_INTERVAL_MAX: Duration = Duration::from_secs(1);

/// The longest a mutator's epoch stands before its collector advances it,
/// on the collector's own clock: the bound on how long a component that
/// became garbage behind a deferred root waits on a thread whose batches do
/// not reach [`crate::cycle::epoch::BATCHES_PER_EPOCH`] first ("The epoch
/// clock"). 8 s, borrowed from V8's memory reducer, which collects a mutator
/// that went quiet after the same delay; not measured here, and the field
/// runs from that to Go's two minutes (`dev/RESEARCH.md`, "the idle-GC
/// timers of five runtimes"). The embedder's figure replaces it
/// ([`set_epoch_interval`]).
const EPOCH_INTERVAL: Duration = Duration::from_secs(8);

/// The embedder's epoch interval in nanoseconds, or zero for
/// [`EPOCH_INTERVAL`].
static EMBEDDERS_EPOCH_INTERVAL_NANOS: AtomicU64 = AtomicU64::new(0);

/// How many consent waits one walk of the records may spend on mutators
/// that do not answer: past it, every request the walk lands afterwards is
/// left standing at once and served at a checkpoint when its mutator wakes
/// (`dev/DECISIONS.md`, "the consent wait stays on both paths, and a
/// round's spending on expired waits is capped"). Two, chosen as a
/// placeholder and kept by the readings of `dev/BENCHMARKS.md`: it holds a
/// round's tail at 5.3 ms where 1,000 sleeping threads cost 2.106 s without
/// it ("what sleeping sub-threshold threads cost a round"), and costs a
/// second producing mutator behind them nothing measurable, its sibling
/// born in the same two or three rounds either way ("a second producer
/// behind the sleeping threads").
///
/// What it bounds is the round's spending on mutators that never answer: a
/// walk begins at most this many waits that expire, each spanning at most
/// [`REQUEST_WAIT`] plus the tail of one batch begun inside it. A working
/// mutator's consent or refusal costs its own latency instead, at most the
/// wait and once per mutator per round, which is the wait's price and not
/// the cap's. What the bound is paid with is the batches a walk gives up by
/// not waiting for a mutator that would have answered late.
const EXPIRED_WAITS_PER_ROUND: usize = 2;

/// How long a mutator's candidate ring may stand non-empty below the
/// round's threshold before the round takes it as an ordinary batch
/// (`dev/design/a-standing-r-is-taken-after-n-rounds.md`). 4 s, and not a
/// measured figure: what it bounds is how long the entities a ring of
/// fewer than [`SOFT_THRESHOLD`] candidates names wait on a thread that
/// never reaches the threshold, against one batch's foreign-holder window
/// per interval on a thread that registers a candidate now and then. The
/// collector's own time, read against the serve clock, so a mutator's rate
/// moves it neither way; the embedder's figure replaces it
/// ([`set_standing_interval`]). What one take at the interval costs the
/// mutator is measured (`dev/BENCHMARKS.md`, "the live-roots arm"): about 25
/// instructions a verdict, 1,495 to 1,562 for a full ring of 63, against the
/// hundreds of thousands the collection it rides on spends.
const STANDING_INTERVAL: Duration = Duration::from_secs(4);

/// The embedder's standing interval in nanoseconds, or zero for
/// [`STANDING_INTERVAL`].
static EMBEDDERS_STANDING_INTERVAL_NANOS: AtomicU64 = AtomicU64::new(0);

/// The instant every record's serve stamp counts from, fixed by the first
/// serve of the process; a stamp is nanoseconds past it, never zero, zero
/// being a record no serve has stamped.
static SERVE_CLOCK_BASE: OnceLock<Instant> = OnceLock::new();

/// How long after a refused birth the pressure path waits before it spawns
/// again: a process that stays short of memory collects at every refused
/// allocation, and without the wait it would spawn a thread per refusal.
/// A sibling's birth waits the same interval after a refused one.
const BIRTH_RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// When the last birth was refused — a spawn the operating system refused,
/// or a base block the pool refused — or `None`. Shared by every slot.
static REFUSED_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// Collector threads the process can hold at once; the embedder's cap is at
/// most this. The slot index is what a mutator's record names its collector
/// by ([`MutatorRecord::collector`]).
pub(crate) const MAX_COLLECTORS: usize = 8;

// The stamp a standing list puts on its records is the slot index plus one
// in a byte ([`Standing::stamp`]).
const _: () = assert!(MAX_COLLECTORS < u8::MAX as usize);

/// The elder's slot: the collector the pressure path births, that every
/// fresh record is named to, and that ends idle siblings.
pub(crate) const ELDER: usize = 0;

/// Collectors the process may hold until the embedder sets its own cap
/// ([`set_collector_cap`]). Not a measured figure.
const DEFAULT_COLLECTOR_CAP: usize = 4;

/// The embedder's cap on collector threads, zero to [`MAX_COLLECTORS`].
static COLLECTOR_CAP: AtomicUsize = AtomicUsize::new(DEFAULT_COLLECTOR_CAP);

/// Rounds in a row a collector serves a backlog — two or more mutators of its
/// own reading at the threshold after their batches, since one mutator is
/// read by one collector at a time and a backlog of one is no reason to
/// birth — before it births a sibling. Not a measured figure; two is the
/// rfc's.
const BACKLOG_ROUNDS_TO_BIRTH: usize = 2;

/// Mutators a round remembers as backlogged, for the handover: the first this
/// many, every second of which goes to the sibling.
const BACKLOGGED_REMEMBERED: usize = 16;

/// Rounds in a row a sibling makes no batch and reads no mutator at the
/// threshold before the elder ends it. Not a measured figure: the rfc says
/// several.
const IDLE_ROUNDS_TO_END: usize = 8;

/// One collector slot: where its thread stands, the word a wake reaches it
/// through, and the count the elder reads of a sibling.
struct Collector {
    /// [`UNBORN`], [`STARTING`] from the spawn until its `ll_thread_init`
    /// answered, [`ALIVE`] from a started init until the thread ends, and
    /// [`ENDING`] from the elder's word to end a sibling until it does.
    state: AtomicU8,
    /// The wake word: set under the mutex by [`wake`], whoever the sender,
    /// and taken by the thread's waits, so a wake sent before the wait ends
    /// it at once. Cleared when the thread announces itself alive, so a
    /// wake sent to an empty slot is lost rather than handed to the next
    /// birth as a round nobody asked for.
    wake_pending: Mutex<bool>,
    /// What the waits sleep on, notified with every set of the wake word.
    wake_signal: Condvar,
    /// Rounds in a row this collector made no batch and read no mutator at
    /// the threshold, its own count, read by the elder to end an idle
    /// sibling.
    idle_rounds: AtomicUsize,
    /// A sequence number of the byte events on this collector's requests:
    /// moved by every consent to and every refusal of a request naming this
    /// slot ([`wake_for_the_byte`]), and by nothing else. The standing
    /// list's checkpoint walks only when the number moved since its last
    /// pass, because no standing request's byte leaves `REQUESTED|slot`
    /// without one of the two or this collector's own withdrawal
    /// (`crate::cycle::token::TraceToken::withdraw`), which is its own to
    /// know (`dev/design/the-standing-request-lives-on-the-record.md`, "The
    /// collector").
    byte_wakes: AtomicUsize,
    /// Set by a mutator's recall of a grant this slot holds, after the recall
    /// marked its token ([`recall_the_grants_of`]), and taken by the
    /// trace's reading of the recall, which releases every grant its
    /// standing list holds unserved whose mutator recalls it
    /// ([`Standing::release_the_recalled`]). A hint as the token's mark is:
    /// a reading that missed it costs one stride more.
    grants_recalled: AtomicBool,
}

impl Collector {
    const fn unborn() -> Self {
        Self {
            state: AtomicU8::new(UNBORN),
            wake_pending: Mutex::new(false),
            wake_signal: Condvar::new(),
            idle_rounds: AtomicUsize::new(0),
            byte_wakes: AtomicUsize::new(0),
            grants_recalled: AtomicBool::new(false),
        }
    }
}

/// [`Standing::release_the_recalled`] behind a trace's reading of the recall.
///
/// # Safety
/// `list` is the collector thread's [`Standing`], borrowed by nobody else.
unsafe fn release_the_recalled_grants(list: *mut ()) {
    unsafe { &mut *list.cast::<Standing>() }.release_the_recalled();
}

/// Tell collector `slot` that a mutator recalls a grant it holds: the take
/// that met `COLLECTOR|slot` calls it after marking its token, and so does a
/// stack of returns withheld under the grant once it holds its mark, so that
/// the trace in progress releases the grant if it is one held behind another
/// mutator's batch (`rfc/model/gc/rc-cycle.md`, "The recall of the token").
/// A read-modify-write with Release, so that the trace which takes the word
/// reads the mark of every setter before it, a later setter's write
/// continuing the release sequence rather than starting one.
pub(crate) fn recall_the_grants_of(slot: usize) {
    COLLECTORS[slot]
        .grants_recalled
        .fetch_or(true, Ordering::Release);
}

/// Take collector `slot`'s recall word as a trace's reading does, and say
/// whether it was set: a case's reading of who told the slot.
#[cfg(test)]
pub(crate) fn take_the_recall_of(slot: usize) -> bool {
    COLLECTORS[slot]
        .grants_recalled
        .swap(false, Ordering::Acquire)
}

static COLLECTORS: [Collector; MAX_COLLECTORS] = [const { Collector::unborn() }; MAX_COLLECTORS];

const UNBORN: u8 = 0;
const STARTING: u8 = 1;
const ALIVE: u8 = 2;
const ENDING: u8 = 3;

/// Set the embedder's cap on collector threads: `cap` clamped to
/// [`MAX_COLLECTORS`]. Siblings above a lowered cap end as idle ones do, at
/// the elder's hand; a cap of one is a process with the elder alone, and a
/// cap of zero one where the elder keeps the epoch clock and takes nothing
/// ("Cap zero", the module doc).
pub(crate) fn set_collector_cap(cap: usize) {
    COLLECTOR_CAP.store(cap.min(MAX_COLLECTORS), Ordering::Relaxed);
}

#[inline]
fn collector_cap() -> usize {
    COLLECTOR_CAP.load(Ordering::Relaxed)
}

/// Whether the embedder capped the collectors at zero, so that a round asks
/// rather than takes ("Cap zero", the module doc). A relaxed load: a round
/// that misses a store of the cap reads it at the next one.
fn collectors_capped_at_zero() -> bool {
    collector_cap() == 0
}

/// Set the embedder's epoch interval: the longest a mutator's epoch stands
/// before its collector advances it. Zero restores the crate's
/// [`EPOCH_INTERVAL`].
pub(crate) fn set_epoch_interval(interval: Duration) {
    let nanos = u64::try_from(interval.as_nanos()).unwrap_or(u64::MAX);
    EMBEDDERS_EPOCH_INTERVAL_NANOS.store(nanos, Ordering::Relaxed);
}

/// The epoch interval in force: a case's, the embedder's, or the crate's.
fn epoch_interval() -> Duration {
    #[cfg(test)]
    if let Some(interval) = testing::epoch_interval() {
        return interval;
    }

    match EMBEDDERS_EPOCH_INTERVAL_NANOS.load(Ordering::Relaxed) {
        0 => EPOCH_INTERVAL,
        nanos => Duration::from_nanos(nanos),
    }
}

/// Set the embedder's standing interval: how long a sub-threshold ring may
/// stand non-empty before the round takes it. Zero restores the crate's
/// [`STANDING_INTERVAL`].
pub(crate) fn set_standing_interval(interval: Duration) {
    let nanos = u64::try_from(interval.as_nanos()).unwrap_or(u64::MAX);
    EMBEDDERS_STANDING_INTERVAL_NANOS.store(nanos, Ordering::Relaxed);
}

/// The standing interval in force: a case's, the embedder's, or the crate's.
fn standing_interval() -> Duration {
    #[cfg(test)]
    if let Some(interval) = testing::standing_interval() {
        return interval;
    }

    match EMBEDDERS_STANDING_INTERVAL_NANOS.load(Ordering::Relaxed) {
        0 => STANDING_INTERVAL,
        nanos => Duration::from_nanos(nanos),
    }
}

/// Nanoseconds since [`SERVE_CLOCK_BASE`], at least one.
pub(crate) fn serve_clock_now() -> u64 {
    let base = SERVE_CLOCK_BASE.get_or_init(Instant::now);
    (Instant::now().duration_since(*base).as_nanos() as u64).max(1)
}

/// The epoch clock (module doc): advance `record`'s cell when the registry
/// noted a new life since the last visit, or once
/// [`crate::cycle::epoch::BATCHES_PER_EPOCH`] batches or [`epoch_interval`]
/// have passed since the last advance; the first visit of a life stamps the
/// instant and advances nothing. `now` is the round's one reading of the
/// clock for this record, the one its serve reads too. The mutator reads the
/// advance at its next poll (`crate::gc`, the poll) and at its next
/// collection's open (`crate::cycle::arena`).
fn advance_the_epoch_if_due(record: &MutatorRecord, now: u64) {
    if record.take_new_life() {
        record.advance_the_epoch(now);
        return;
    }

    let last = record.advanced_at();
    if last == 0 {
        record.note_advanced_at(now);
        return;
    }

    if record.batches_since_the_advance() >= crate::cycle::epoch::BATCHES_PER_EPOCH
        || now.saturating_sub(last) >= epoch_interval().as_nanos() as u64
    {
        record.advance_the_epoch(now);
    }
}

/// Start the elder collector thread unless the process has one already, one
/// is starting, or a birth was refused less than [`BIRTH_RETRY_INTERVAL`]
/// ago. A spawn the operating system refuses, and a base block the pool
/// refuses, each leave the process without a thread until a call after the
/// interval. The pressure path calls it after its collection, so that the
/// thread's draws compete with no rows of the caller's own.
///
/// The birth is a thread the OS entry creates on a stack the slot keeps
/// ([`birth`]), so the path meets no allocation whose refusal is an abort.
/// **A call that births waits for the slot's last thread first**: the stack
/// is the slot's, so the birth joins what stood there, which returns once
/// that thread's teardown is done. A call that births nothing — the common
/// one, the thread standing — takes no lock and waits for nothing.
pub(crate) fn ensure_thread() {
    ensure_collector(ELDER);
}

/// Start the collector of slot `index` unless it stands or is starting, or a
/// birth was refused inside the interval; true when this call spawned it. A
/// call that spawns waits for the slot's last thread, as [`ensure_thread`]
/// says.
fn ensure_collector(index: usize) -> bool {
    #[cfg(test)]
    if !testing::births_permitted() {
        return false;
    }

    let collector = &COLLECTORS[index];
    // The load before the exchange keeps every pressure collection after the
    // birth off a read-modify-write of the word.
    if collector.state.load(Ordering::Relaxed) != UNBORN
        || birth_refused_recently()
        || collector
            .state
            .compare_exchange(UNBORN, STARTING, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
    {
        return false;
    }

    collector.idle_rounds.store(0, Ordering::Relaxed);
    if !birth::spawn(index) {
        note_refused_birth();
        collector.state.store(UNBORN, Ordering::Release);
        return false;
    }

    #[cfg(test)]
    testing::note_spawn();
    true
}

/// Whether a birth was refused less than [`BIRTH_RETRY_INTERVAL`] ago.
fn birth_refused_recently() -> bool {
    let refused_at = REFUSED_AT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    refused_at.is_some_and(|at| at.elapsed() < BIRTH_RETRY_INTERVAL)
}

/// How long ago the last birth was refused, or `None` for none since the
/// last case's retire.
#[cfg(test)]
fn refused_birth_age() -> Option<Duration> {
    REFUSED_AT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .map(|at| at.elapsed())
}

fn note_refused_birth() {
    let mut refused_at = REFUSED_AT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *refused_at = Some(Instant::now());
}

/// Wake the collector of slot `index` out of its wait, and answer whether
/// the process had one to wake: a wake is a soft signal that starts a round,
/// and a round reads every mutator's count itself (module doc). Callable from
/// any thread; a mutator's poll makes it at [`SOFT_THRESHOLD`] registrations,
/// to the collector its record names, and a pressure collection at each of
/// its endings, to the elder. False is a wake lost: before the thread's
/// birth, between its spawn and its init, while it ends, and after its
/// end. A lost wake
/// costs nothing but the round it did not start: the poll leaves its flag
/// standing and sends again at its next poll
/// (`crate::cycle::queue::signal_the_collector_if_due`), and a sibling's
/// first round runs at its birth. The one wake that answers true and starts
/// no round of its own is the one sent between [`forget_wakes`] and the
/// `ALIVE` store of [`begin_the_thread`]: it is cleared there, and the
/// thread's first round, which runs before its first wait, stands in for
/// it.
pub(crate) fn wake(index: usize) -> bool {
    let collector = &COLLECTORS[index];
    *collector
        .wake_pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
    collector.wake_signal.notify_all();
    is_alive(index)
}

/// Wake slot `index` for a byte event on one of its requests — the
/// mutator's consent or its refusal — moving the slot's sequence number
/// first, with a release the checkpoint's acquire load pairs with. Called
/// from the mutator's thread by `crate::cycle::token`; every other wake
/// goes through [`wake`] and moves the number not at all.
pub(crate) fn wake_for_the_byte(index: usize) -> bool {
    COLLECTORS[index].byte_wakes.fetch_add(1, Ordering::Release);
    wake(index)
}

/// Sleep on slot `index`'s wake word until a wake or `timeout`, and take
/// the word either way: a wake that lands as the timeout runs out is spent
/// on the round that follows rather than kept for the next wait. The thread
/// blocks on the condvar and never re-reads the word in a loop of its own,
/// which is what lets it make progress under Miri's weak-memory emulation
/// (`dev/WORKFLOW.md`, Miri, "A test thread waits, it does not spin").
fn wait_for_a_wake(index: usize, timeout: Duration) {
    let collector = &COLLECTORS[index];
    let pending = collector
        .wake_pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut pending, _) = collector
        .wake_signal
        .wait_timeout_while(pending, timeout, |pending| !*pending)
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *pending = false;
}

/// Clear slot `index`'s wake word, so that a wake sent while no thread
/// stood is not the next thread's first wait ended.
fn forget_wakes(index: usize) {
    *COLLECTORS[index]
        .wake_pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = false;
}

/// Whether slot `index` holds a thread that has started and not ended.
fn is_alive(index: usize) -> bool {
    COLLECTORS[index].state.load(Ordering::Acquire) == ALIVE
}

/// Whether slot `index` holds no thread of the crate's: none born, one
/// ended, or one whose birth was refused. A slot between its spawn and its
/// init is neither. An ended thread's OS thread may still stand — the word
/// goes unborn on the thread itself, before glibc's teardown and while the
/// slot still holds it to be joined ([`birth`]) — so this answers who reads
/// the mutators of the slot and never whether a stack is free.
fn has_no_thread(index: usize) -> bool {
    COLLECTORS[index].state.load(Ordering::Acquire) == UNBORN
}

/// The collector thread's life: its registration, its rounds, its exit.
/// Run by [`birth::run_the_life`], which stores the word back to unborn
/// however this returns — a refused base block, a test's retire, the
/// elder's end, or a panic in a round that unwinds out of here — after the
/// runtime exit, so that a later birth can happen rather than read a thread
/// that no longer exists.
fn thread_body(index: usize) {
    if !begin_the_thread(index) {
        return;
    }

    let collector = &COLLECTORS[index];
    let mut interval = FALLBACK_INTERVAL_MIN;
    let mut backlog_rounds = 0;
    // The requests this collector left standing on sleeping mutators, on this
    // frame for the thread's life; the drop withdraws them.
    let mut standing = Standing::new(index);
    while !retiring() && collector.state.load(Ordering::Relaxed) == ALIVE {
        #[cfg(test)]
        testing::note_round_start(index);
        let outcome = round(index, threshold_for_rounds(), &mut standing);
        interval = next_interval(interval, &outcome);

        backlog_rounds = if outcome.backlogged.len() >= 2 {
            backlog_rounds + 1
        } else {
            0
        };
        if backlog_rounds >= BACKLOG_ROUNDS_TO_BIRTH {
            backlog_rounds = 0;
            grow_the_siblings(index, &outcome.backlogged);
        }

        note_idleness(index, &outcome);

        #[cfg(test)]
        testing::note_round(index, interval);
        #[cfg(test)]
        let interval = testing::interval_for_this_wait().unwrap_or(interval);
        // A wait inside a round that ended early with nothing served may
        // have consumed a round-start wake, so the sleep after that round is
        // skipped once (`rfc/dev/design/trace-token-handshake.md`, "The two
        // sides", the collector).
        if !standing.take_consumed_a_wake() {
            wait_for_a_wake(index, interval);
        }
    }

    crate::memory::heap::ll_thread_exit();
}

/// Draw this collector thread's base block and announce it alive, or answer
/// false for a thread that never started.
///
/// A refused base block is a birth that did not happen
/// (`rfc/dev/DECISIONS.md`, "the baseline overflow segment is
/// allocator-issued"), and a call after the interval births again.
fn begin_the_thread(index: usize) -> bool {
    #[cfg(test)]
    {
        testing::pin_this_collector(index);
        testing::note_collector_born(index);
    }
    let started = {
        #[cfg(test)]
        let _budget = testing::base_block_budget_for_this_birth();
        crate::memory::heap::ll_thread_init()
    };
    if !started {
        note_refused_birth();
        return false;
    }

    // Before the state, so that the first wake this thread's wait can take
    // is one sent after the state that wake answers on.
    forget_wakes(index);
    COLLECTORS[index].state.store(ALIVE, Ordering::Release);
    true
}

/// The wait before the next round, from what this one did: a batch or a
/// freeing disposition is the shortest interval, work seen without a batch
/// holds it where it stands, and an idle round doubles it up to
/// [`FALLBACK_INTERVAL_MAX`].
fn next_interval(interval: Duration, outcome: &Round) -> Duration {
    if outcome.made_a_batch || outcome.read_a_freeing_disposition {
        FALLBACK_INTERVAL_MIN
    } else if outcome.saw_work {
        interval
    } else {
        (interval * 2).min(FALLBACK_INTERVAL_MAX)
    }
}

/// Birth one sibling for the backlog this round left and hand it half of
/// the backlogged mutators; a cap that refuses the birth leaves the backlog
/// where it is.
fn grow_the_siblings(index: usize, backlogged: &Backlogged) {
    match birth_a_sibling(index) {
        Some(sibling) => hand_over_half(backlogged, sibling),
        None => {
            #[cfg(test)]
            testing::note_backlog_round_without_a_birth();
        }
    }
}

/// Count this round against collector `index`'s idle rounds, and let the
/// elder end the siblings that have run out of work.
///
/// A mutator this round meant to serve and could not, its own token being
/// held, is work and not idleness, so a sibling whose mutator collects in
/// line for a while is not ended for it. The reading is the walk's where it
/// waited out the refusal and the checkpoint's where it did not, and it
/// covers a mutator below the threshold as well: a take's request meets the
/// same claim ([`Round::saw_work`]).
fn note_idleness(index: usize, outcome: &Round) {
    let collector = &COLLECTORS[index];
    let idle = if outcome.made_a_batch || outcome.saw_work {
        0
    } else {
        collector.idle_rounds.load(Ordering::Relaxed) + 1
    };
    collector.idle_rounds.store(idle, Ordering::Relaxed);
    if index == ELDER && outcome.backlogged.len() < 2 {
        end_idle_siblings();
    }
}

/// Birth a sibling in the first empty slot under the cap other than `from`,
/// the elder's slot included — an elder that unwound is reborn by the
/// first backlogged sibling rather than by the next memory shortage —
/// through the same path as the elder's birth, retry interval included;
/// the slot it took, or `None` for no slot or a refused spawn. The
/// sibling's first round runs at its birth, so no wake is owed.
fn birth_a_sibling(from: usize) -> Option<usize> {
    (0..collector_cap())
        .filter(|&slot| slot != from)
        .find(|&slot| ensure_collector(slot))
}

/// Name every second of `backlogged` to `to`: the handover the sibling's
/// first rounds read, made over the mutators the round read at the threshold
/// after their batches and no other, so that what moves is work.
///
/// A record this collector's request stands on is left where it is, whatever
/// its place in the list. The rounds may read a mutator's backlog at a
/// checkpoint, whose batch unlinked it, and leave a request standing on it
/// at the walk that follows, so a record can be backlogged and linked at
/// once ([`Standing::backlogged`]); renaming it would put the elder's
/// standing request on a record a sibling reclaims, and the sibling's
/// [`Standing::forget`] would splice a record out of a list that is not
/// its own (`rfc/dev/design/trace-token-handshake.md`, "The two sides": a
/// record is renamed to another collector or freed only while unlinked).
/// What it costs to leave it is one round of that mutator's work with this
/// collector.
fn hand_over_half(backlogged: &Backlogged, to: usize) {
    for record in backlogged.iter().skip(1).step_by(2) {
        let record = unsafe { &**record };
        if record.is_standing() {
            continue;
        }

        record.name_to_collector(to);
    }
}

/// End every sibling that made no batch and saw no work for
/// [`IDLE_ROUNDS_TO_END`] rounds, and every one above the cap: the elder's,
/// once per round in which it read no backlog of its own — a sibling is not
/// ended while the elder would birth one back. The mutators of an ended
/// sibling are the elder's again at its next round ([`reclaims`]).
fn end_idle_siblings() {
    let cap = collector_cap();
    for (slot, collector) in COLLECTORS.iter().enumerate().skip(1) {
        if !is_alive(slot) {
            continue;
        }

        if slot >= cap || collector.idle_rounds.load(Ordering::Relaxed) >= IDLE_ROUNDS_TO_END {
            // From alive alone: a slot that ended or unwound meanwhile is
            // left as it is, for a birth to take.
            let _ = collector.state.compare_exchange(
                ALIVE,
                ENDING,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
            let _ = wake(slot);
        }
    }
}

/// Whether the round of `index` serves `record`: a mutator named to it, and
/// for the elder also a mutator named to a slot with no living thread — a
/// sibling ended, refused at its birth, or unwound — which it takes back by
/// rewriting the word.
fn reclaims(index: usize, record: &MutatorRecord) -> bool {
    let named = record.collector();
    if named == index {
        return true;
    }

    if index == ELDER && has_no_thread(named) {
        record.name_to_collector(ELDER);
        return true;
    }

    false
}

/// The threshold the rounds serve at: the module's own, or a case's.
fn threshold_for_rounds() -> usize {
    #[cfg(test)]
    if let Some(threshold) = testing::threshold_for_rounds() {
        return threshold;
    }

    SOFT_THRESHOLD
}

/// The block budget the next batch traces under: the module's own, or a
/// case's.
fn budget_for_this_batch() -> usize {
    #[cfg(test)]
    if let Some(budget) = testing::budget_for_this_batch() {
        return budget;
    }

    TRACE_BLOCK_BUDGET
}

/// Clear the last refusal, so that a case's birth is not held by the
/// refusal of the case before it.
#[cfg(test)]
fn forget_refused_birth() {
    let mut refused_at = REFUSED_AT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *refused_at = None;
}

/// Whether a test asked the thread to end; false in every other build.
#[cfg(not(test))]
fn retiring() -> bool {
    false
}

#[cfg(test)]
use testing::retiring;

/// The mutators a round read at the threshold after their batches, the first
/// [`BACKLOGGED_REMEMBERED`] of them, in a fixed array on the round's frame.
#[derive(Debug)]
struct Backlogged {
    records: [*mut MutatorRecord; BACKLOGGED_REMEMBERED],
    len: usize,
}

impl Default for Backlogged {
    fn default() -> Self {
        Self {
            records: [std::ptr::null_mut(); BACKLOGGED_REMEMBERED],
            len: 0,
        }
    }
}

impl Backlogged {
    /// Remember `record` once, or drop it when it is here already or the
    /// array is full. Once, because a round reads a mutator's backlog at a
    /// checkpoint and again at the walk's own batch — the mutator having
    /// disposed of the first between them — and a birth asks for two
    /// mutators at the threshold rather than one counted twice.
    fn push(&mut self, record: *mut MutatorRecord) {
        if self.records[..self.len].contains(&record) {
            return;
        }

        if self.len < BACKLOGGED_REMEMBERED {
            self.records[self.len] = record;
            self.len += 1;
        }
    }

    /// Move what this one holds into `into`, emptying it.
    fn drain_into(&mut self, into: &mut Backlogged) {
        if self.len == 0 {
            return;
        }

        for record in self.records[..self.len].iter() {
            into.push(*record);
        }

        self.len = 0;
    }

    fn len(&self) -> usize {
        self.len
    }

    fn iter(&self) -> impl Iterator<Item = &*mut MutatorRecord> {
        self.records[..self.len].iter()
    }
}

/// What a round read across the records, for the timer and the siblings.
#[derive(Debug, Default)]
struct Round {
    /// Some mutator was served a batch.
    made_a_batch: bool,
    /// Some mutator this round meant to serve was not served because it
    /// holds its own token: collecting in line, or exiting. Read by the
    /// walk where it waits out the refusal — of a threshold request or of a
    /// take's — and by the checkpoint's pass where the walk did not wait
    /// ([`Standing::saw_work`]).
    saw_work: bool,
    /// Some mutator's poll noted a disposition that freed something since the
    /// last round.
    read_a_freeing_disposition: bool,
    /// The mutators still at the threshold after their batches, from the
    /// walk's own batches and from the checkpoints' ([`Standing::backlogged`]),
    /// each standing once.
    backlogged: Backlogged,
}

/// One round of the collector of slot `index` over the records: visit every
/// record named to it but the thread's own, which polls nothing, read each
/// mutator's note for the timer, and serve each whose R holds `threshold`
/// entries or more. What a serve does is [`serve`]'s.
fn round(index: usize, threshold: usize, standing: &mut Standing) -> Round {
    let own = mutator_record::this_thread_record();
    let mut outcome = Round::default();
    // The round's first checkpoint: a consent that came between rounds is
    // served before any record is read, and a record that exited under a
    // standing request is unlinked for the registry whether or not this
    // round serves anything.
    standing.start_a_round();
    standing.checkpoint(threshold);
    mutator_record::for_each_record(|record| {
        if record == own || !reclaims(index, unsafe { &*record }) {
            return;
        }

        #[cfg(test)]
        if !testing::in_round(record) {
            return;
        }

        unsafe { read_one_record(record, index, threshold, standing, &mut outcome) };
    });
    outcome
}

/// One record of a round: read its note for the timer, advance its epoch
/// where it is due ([`advance_the_epoch_if_due`]), serve it once — under a
/// cap of zero, ask it to collect in line instead — and fold what the serve
/// answered into `outcome`.
///
/// **The advance comes before the serve.** A batch granted at the visit that
/// advances opens its arena on the turned epoch and descends where the old
/// stamps would have pruned, and the grant's release to `POSTED` orders the
/// cell's store before the mutator's reading of P, so the collection over P
/// reads the same epoch the batch did. After the serve, a root the batch read
/// live against the old epoch would be deferred against the new one and wait
/// one advance more.
///
/// The note is read whatever the serve answers — a free-list record has a
/// count equal to the copy, and a record between threads answers one
/// spurious shortening at most. A batch served at a checkpoint inside the
/// serve counts as this round's too, which is what the standing list's
/// count carries out.
///
/// # Safety
/// `record` is a record of the registry's that this collector reclaims, and
/// the calling thread is not its mutator.
unsafe fn read_one_record(
    record: *mut MutatorRecord,
    index: usize,
    threshold: usize,
    standing: &mut Standing,
    outcome: &mut Round,
) {
    if unsafe { &*record }.take_freeing_disposition_note() {
        outcome.read_a_freeing_disposition = true;
    }

    let now = serve_clock_now();
    advance_the_epoch_if_due(unsafe { &*record }, now);
    crate::cycle::live_list::give_back_a_stale_list(unsafe { &*record });
    // Under a cap of zero the visit keeps the clock and requests nothing.
    let served = if collectors_capped_at_zero() {
        unsafe { ask_for_an_in_line_collection(record, threshold, now) }
    } else {
        unsafe { serve(record, index, threshold, standing, now) }
    };
    outcome.made_a_batch |= standing.take_batches_served() > 0;
    outcome.saw_work |= standing.take_saw_work();
    standing.take_backlogged(&mut outcome.backlogged);
    match served {
        Served::Batch { backlog: true, .. } => {
            outcome.made_a_batch = true;
            outcome.backlogged.push(record);
        }
        Served::Batch { .. } => outcome.made_a_batch = true,
        Served::TokenHeld | Served::Asked => outcome.saw_work = true,
        Served::Idle | Served::Posted | Served::Unanswered => {}
    }

    #[cfg(test)]
    testing::note_served(served);
}

/// Serve `record`'s mutator once: request its token, wait for the mutator's
/// consent, make one batch (module doc) under the grant and release —
/// to `POSTED` when the batch proposed a set, which is what tells the
/// mutator to collect, to `NOTHING_PROPOSED` when it posted verdicts and
/// none proposed a set, which owes the mutator P's disposition and no trace
/// window, and to `FREE` when it posted nothing. `threshold`
/// is the count of R, read before any request, at which the mutator is
/// served outright; below it the serve goes on only for a ring that has
/// stood an interval ([`decide_the_branch_and_stamp_the_instant`]), and the round
/// passes [`SOFT_THRESHOLD`]. `now` is the round's one reading of the serve
/// clock for this record, the instant a standing ring is measured against
/// and the one the epoch's advance read before the serve. `slot` is the
/// calling collector's, the name its request writes into the byte.
///
/// **The request is one swap `FREE → REQUESTED|slot`**, and every other
/// value is a skip: `MUTATOR` a mutator collecting in line, `POSTED` one
/// that has not disposed of the last batch, `REQUESTED` or `COLLECTOR`
/// another collector's — its own `REQUESTED|slot` a request still standing
/// from an earlier round. The mutator is waited for up to [`REQUEST_WAIT`],
/// in a loop on the byte alone — the wait's return is not an answer — and
/// a refusal or a life ended inside it is the withdrawal's read-back
/// ([`crate::cycle::token::Withdrawn`]). Past the deadline the request is
/// not withdrawn: it stays on the byte, the record in the collector's
/// standing list ([`Standing`]) since before the request was made, to be
/// served at a checkpoint
/// ([`Standing::checkpoint`]) once the mutator answers — at its first slot
/// free or poll after waking, at most one stranger's batch away — or
/// withdrawn when the thread ends. A mutator a checkpoint released without
/// a batch ([`MutatorRecord::was_released_unserved`]) is pushed at once,
/// with no wait, and so is every request this walk lands after
/// [`EXPIRED_WAITS_PER_ROUND`] of its waits have run out unanswered
/// ([`Standing::spent_its_waits`]): a walk pays the bound once, whatever
/// the registry's count of sleeping mutators. The checkpoints are where the collector commits time:
/// at the round's start, before every request, here, and after every
/// return of the wait (`rfc/dev/design/trace-token-handshake.md`, "The two
/// sides", the collector, and the second and third rounds;
/// `dev/design/the-standing-request-lives-on-the-record.md`, "The
/// collector").
///
/// Runs on a collector thread, which holds a base block of its own for the
/// workspace the batch's trace opens (`crate::memory::heap::ll_thread_init`).
///
/// # Safety
/// `record` is a record of the registry's, and the calling thread is not its
/// mutator.
pub(crate) unsafe fn serve(
    record: *mut MutatorRecord,
    slot: usize,
    threshold: usize,
    standing: &mut Standing,
    now: u64,
) -> Served {
    let mutator = unsafe { &*record };
    // The checkpoint before the request: a sleeping mutator that consented
    // since the last one is served ahead of any stranger.
    standing.checkpoint(threshold);

    // Work first, and the collector's own memory, before any request — by
    // loads alone, since nothing of the mutator's may be written under no
    // claim, and off the front block alone (`Reader::front_block_reading`
    // says why): the ring's one reading, P's room off its index words,
    // and the workspace this thread's. The figures are an idle test and not
    // the clamp: the clamp is re-read under the token. The blocks read are
    // held for the reading, since a mutator exiting meanwhile returns them
    // (`crate::cycle::mutator_record`, "The blocks a collector reads before
    // its claim are held"); a record another reading holds is idle to this
    // round.
    if !unsafe { mutator_record::take_for_reading(record) } {
        return Served::Idle;
    }

    let hold = HandBackOnDrop(record);
    // The merges before the ring: a merge between the two loads is read in
    // the ring and missed by the count, which takes the ring a round later;
    // the other order would record a merge its reading never saw.
    let merges = mutator.merges();
    #[cfg(test)]
    testing::between_the_take_and_the_reading();
    let reading = unsafe { Reader::new(mutator.candidate_ring()) }.front_block_reading();
    let room = unsafe { VerdictWriter::open(mutator) }.room_by_loads();
    let branch = decide_the_branch_and_stamp_the_instant(
        mutator,
        reading,
        merges,
        threshold,
        now,
        standing_interval().as_nanos() as u64,
    );
    if branch == RingRound::Leaves || room == 0 {
        return Served::Idle;
    }

    if branch == RingRound::Takes && mutator.is_standing() {
        // A take's request from an earlier round stands on the byte, and
        // the swap would only read it back: the record is in the list for
        // the checkpoints, and the instant stands as it is until the grant
        // ends (`dev/design/a-standing-r-is-taken-after-n-rounds.md`, "The
        // round"). Skipped for the take alone — a mutator at the threshold
        // is worth the swap, which serves a grant that landed between the
        // checkpoint and the request one pass earlier.
        return Served::Unanswered;
    }

    #[cfg(test)]
    testing::between_the_reading_and_the_request();
    // Linked before the request lands, so that an exit which takes the
    // request finds the record already in the list and the registry's gate
    // holds it; every outcome that leaves no request standing unlinks it.
    // Linking after the wait would let the exit's refusal, the free list
    // and a new life's take all run between the byte read and the link.
    // The reading's hold spans the link and the request: a request that
    // succeeds publishes the link to the exit's take by its release, and a
    // request that fails publishes nothing, so there the hold is handed
    // back only after the unlink — an exit that met the hold leaves its
    // blocks to the hand-back, and the registry's gate, which reads the
    // hold word before the link word, is ordered after it.
    standing.push(record);
    if let Err(seen) = mutator.token.request(slot) {
        #[cfg(test)]
        testing::at_a_refused_request();
        return unsafe { answer_a_refused_request(mutator, seen, slot, threshold, standing, hold) };
    }

    drop(hold);

    if mutator.was_released_unserved() {
        // Asleep again by the time the walk reached it, as a mutator a
        // pass released without a batch is: no wait, the request stands.
        mutator.note_released_unserved(false);
        return Served::Unanswered;
    }

    if standing.spent_its_waits() {
        // This walk has waited out its bound on mutators that did not
        // answer ([`EXPIRED_WAITS_PER_ROUND`]): the request stays on the
        // byte and the record in the list, for the checkpoint that reads
        // the consent, and the walk goes on to the next record.
        return Served::Unanswered;
    }

    unsafe { wait_for_consent(mutator, slot, threshold, standing) }
}

/// Under a collector cap of zero, ask `record`'s mutator to collect R whole in
/// line where the round would have taken R: the same three branches over the
/// same reading as [`serve`]'s — R at `threshold`, a ring standing below it
/// past the standing interval, a lane merged since the merges last accounted
/// for ([`decide_the_branch_and_stamp_the_instant`]) — made under the reading
/// hold, and the byte's swap `FREE → ASKED` over an empty P
/// ([`crate::cycle::token::TraceToken::ask_to_collect_in_line`]) made under the
/// same hold, so that it cannot land on the record's next life. The ask comes
/// at the elder's round, as a batch would, so the in-line collection frees
/// what the collector would have proposed within one fallback interval; the
/// mutator pays nothing for the mode on any reading of its byte but the ask.
/// An ask that lands restamps the standing instant and accounts for the
/// merges, as a grant's release does, so that a ring the collection leaves
/// standing waits its interval again.
///
/// A refused swap answers by the byte: `POSTED`, an ask or a batch's verdicts
/// undisposed of, and every holder, which is a mutator collecting in line or
/// the leftover of a positive cap, the skip.
///
/// # Safety
/// `record` is a record of the registry's, and the calling thread is not its
/// mutator.
unsafe fn ask_for_an_in_line_collection(
    record: *mut MutatorRecord,
    threshold: usize,
    now: u64,
) -> Served {
    if !unsafe { mutator_record::take_for_reading(record) } {
        return Served::Idle;
    }

    let _hold = HandBackOnDrop(record);
    let mutator = unsafe { &*record };
    // The merges before the ring, as `serve` reads them.
    let merges = mutator.merges();
    let reading = unsafe { Reader::new(mutator.candidate_ring()) }.front_block_reading();
    // No term for the chain's death check here: under a cap of zero no grant
    // follows to check, and only a ready part or an epoch past a block asks.
    if decide_the_branch_and_stamp_the_instant(mutator, reading, merges, threshold, now, u64::MAX)
        == RingRound::Leaves
    {
        return Served::Idle;
    }

    match mutator.token.ask_to_collect_in_line() {
        Ok(()) => {
            mutator.note_standing_since(serve_clock_now());
            mutator.note_merges_seen(merges);
            Served::Asked
        }
        Err(seen) if state(seen) == POSTED => Served::Posted,
        Err(_) => Served::TokenHeld,
    }
}

/// What a round does with the mutator's R, and the three branches are one
/// reading apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RingRound {
    /// R holds the round's threshold: the serve of today.
    Serves,
    /// R stands below the threshold and has stood an interval: the take.
    Takes,
    /// Nothing this round: R empty, or standing inside its interval.
    Leaves,
}

/// The round's three branches over R, off the one reading `ring` the serve
/// made under its hold and the mutator's merge count `merges` it loaded
/// before that reading, with the record's standing instant stamped or
/// cleared on the way (`dev/design/a-standing-r-is-taken-after-n-rounds.md`,
/// "The round"; module doc).
///
/// A ring at the threshold is the serve of today, and a ring read empty is
/// an idle round; both leave no interval to count, so the instant is
/// cleared, and the empty one accounts for every merge `merges` counts,
/// since whatever those merges brought has left R. A ring standing below
/// the threshold is taken at once when the mutator has merged its deferred
/// lane since the merges last accounted for: the merged roots are the
/// collector's to trace, and a ring the owner packed into one block reads
/// below the threshold off its front block
/// (`dev/CYCLE-SPLIT-PACKAGE-3-LANE-CRITIC.md`, F2). Otherwise it is taken
/// as an ordinary batch at the first visit [`standing_interval`] or more
/// after the visit that first read it standing, and that visit is where the
/// instant is stamped — of the collector's clock, not the mutator's, so that
/// a mutator's rate moves the take neither way. `now` is the round's one
/// reading of that clock for this record.
///
/// The instant and the merges seen are the collector's words on the
/// record's hold line, read and written here under the reading hold
/// ([`MutatorRecord::standing_since`], [`MutatorRecord::merges_seen`]).
fn decide_the_branch_and_stamp_the_instant(
    mutator: &MutatorRecord,
    reading: Option<crate::ring::FrontBlockReading>,
    merges: u32,
    threshold: usize,
    now: u64,
    chain_term: u64,
) -> RingRound {
    // The collector's chain owes a grant of its own, whatever R reads: a
    // ready part to read, or a waiting part the epoch or the check's term,
    // `chain_term`, has come for (`crate::cycle::chain`).
    #[cfg(feature = "collector-chain")]
    if crate::cycle::chain::is_due(mutator, now, chain_term) {
        return RingRound::Serves;
    }
    #[cfg(not(feature = "collector-chain"))]
    let _ = chain_term;

    let stands = reading.filter(|reading| reading.holds_at_least(1));
    let Some(ring) = stands else {
        mutator.note_standing_since(0);
        mutator.note_merges_seen(merges);
        return RingRound::Leaves;
    };

    if ring.holds_at_least(threshold) {
        mutator.note_standing_since(0);
        return RingRound::Serves;
    }

    if merges != mutator.merges_seen() {
        return RingRound::Takes;
    }

    match mutator.standing_since() {
        0 => {
            mutator.note_standing_since(now);
            RingRound::Leaves
        }
        since if now.saturating_sub(since) >= standing_interval().as_nanos() as u64 => {
            RingRound::Takes
        }
        _ => RingRound::Leaves,
    }
}

/// The four-way reading of a request the token refused, as [`serve`]'s
/// answer.
///
/// The refusal's value names who holds the byte: a batch of this mutator's
/// own that nothing has disposed of, this collector's request still standing
/// on a sleeping mutator, this collector's grant — consented to between the
/// checkpoint and the request, so the grant is served here and the record
/// taken out of the list — or any other holder, which is a skip. `reading`
/// is the pre-claim reading's hold, handed back here after the unlink where
/// there is one ([`serve`] says why), and before the grant's batch.
///
/// # Safety
/// As [`serve`], and `seen` is the value that refusal read back.
unsafe fn answer_a_refused_request(
    mutator: &MutatorRecord,
    seen: u8,
    slot: usize,
    threshold: usize,
    standing: &mut Standing,
    hold: HandBackOnDrop,
) -> Served {
    if state(seen) == POSTED {
        standing.forget(mutator);
        drop(hold);
        return Served::Posted;
    }

    if seen == word(REQUESTED, slot) {
        // This collector's own request, still standing on a sleeping
        // mutator: neither a batch nor work, round after round; the record
        // stays in the list.
        drop(hold);
        return Served::Unanswered;
    }

    if seen == word(COLLECTOR, slot) {
        // This collector's own grant: a standing request consented to
        // between [`serve`]'s checkpoint and its request, served here and
        // taken out of the list.
        drop(hold);
        return unsafe { serve_the_grant(mutator, slot, threshold, standing) };
    }

    standing.forget(mutator);
    drop(hold);
    Served::TokenHeld
}

/// The pre-claim reading's hold on a record, handed back when dropped — on
/// the unwind too: a hold left standing keeps the record off the registry's
/// free list and its blocks out of the pool for good.
struct HandBackOnDrop(*mut MutatorRecord);

impl Drop for HandBackOnDrop {
    fn drop(&mut self) {
        unsafe { mutator_record::hand_back_reading(self.0) };
    }
}

/// Wait out [`REQUEST_WAIT`] for the mutator's consent to the request
/// [`serve`] just made, and answer: the grant is served; a refusal or a
/// life ended is the withdrawal's read-back ([`answer_the_withdrawal`]);
/// the deadline leaves the request standing on the byte, the record in the
/// list since before the request, and counts against this walk's bound
/// ([`EXPIRED_WAITS_PER_ROUND`]). Only a deadline counts: a consent, a
/// refusal or a grant read back is a mutator that answered, whatever it
/// answered, and counts nothing. The deadline is absolute from the
/// request, so a wait whose loop served a stranger's grant charges this
/// mutator all the same: a byte still reading `REQUESTED|slot` a whole
/// wait after the request is a mutator that reached no poll and no slot
/// free in it, whatever the collector did meanwhile, and a consent that
/// lands during that batch is read back as the grant here.
///
/// The wait is on the byte alone, its own return being no answer; a return
/// before the deadline that is not the grant runs the second checkpoint and
/// remembers a wake it may have consumed. That checkpoint can serve this
/// very mutator, whose record is linked and whose consent landed between
/// the loop's read and the pass: the loop's next read then finds `POSTED`
/// or `FREE`, the withdrawal reads a record moved on, and the batch is
/// already counted among the checkpoints'.
///
/// # Safety
/// As [`serve`], and this collector's request stands on `mutator`.
unsafe fn wait_for_consent(
    mutator: &MutatorRecord,
    slot: usize,
    threshold: usize,
    standing: &mut Standing,
) -> Served {
    // Withdrawn on the unwind between the request and the grant: a request
    // left standing by a collector that is gone would be consented to by a
    // mutator that then withholds forever.
    let mut request = WithdrawOnDrop {
        token: &mutator.token,
        slot,
        standing: true,
    };
    let granted = word(COLLECTOR, slot);
    let requested = word(REQUESTED, slot);
    #[cfg(not(test))]
    let wait = REQUEST_WAIT;
    #[cfg(test)]
    let wait = testing::request_wait();
    let deadline = Instant::now() + wait;
    loop {
        let seen = mutator.token.read();
        if seen == granted {
            request.standing = false;
            break;
        }

        // Anything but the standing request is an answer: the grant above,
        // or a refusal or a life ended, which the withdrawal's read-back
        // names without waiting out the bound.
        if seen != requested {
            request.standing = false;
            return unsafe { answer_the_withdrawal(mutator, slot, threshold, standing) };
        }

        // The deadline: the mutator is asleep, and the request stays on the
        // byte for its waking, the record in the list — linked before the
        // request — for the checkpoints.
        let now = Instant::now();
        if now >= deadline {
            request.standing = false;
            standing.note_an_expired_wait();
            return Served::Unanswered;
        }

        wait_for_a_wake(slot, deadline - now);
        // A return before the deadline that was not the grant: the
        // checkpoint, and if it served nothing the wake this wait may have
        // consumed is remembered.
        if Instant::now() < deadline
            && mutator.token.read() != granted
            && standing.checkpoint(threshold) == 0
        {
            standing.consumed_a_wake = true;
        }
    }

    unsafe { serve_the_grant(mutator, slot, threshold, standing) }
}

/// Withdraw collector `slot`'s request from `mutator` and answer by the
/// read-back: a grant is served, a withdrawal that landed is a mutator
/// unanswered, a take by the mutator is its refusal, and a record moved on
/// — `FREE`, or another slot's value — is idle to this serve, the
/// collector holding nothing of it.
///
/// # Safety
/// The calling collector made the request `REQUESTED|slot` on `mutator`.
unsafe fn answer_the_withdrawal(
    mutator: &MutatorRecord,
    slot: usize,
    threshold: usize,
    standing: &mut Standing,
) -> Served {
    let outcome = match mutator.token.withdraw(slot) {
        Withdrawn::Granted => {
            return unsafe { serve_the_grant(mutator, slot, threshold, standing) };
        }
        Withdrawn::Withdrawn => Served::Unanswered,
        Withdrawn::TakenByTheMutator => {
            #[cfg(test)]
            testing::note_refusal();
            Served::TokenHeld
        }
        Withdrawn::MovedOn => Served::Idle,
    };
    // No request stands after a withdrawal that landed or a refusal: the
    // record leaves the list.
    standing.forget(mutator);
    outcome
}

/// Serve a grant this collector holds: the record taken out of the standing
/// list first, so that an unwind inside the batch leaves the list whole;
/// then the arena opened, the batch under `COLLECTOR|slot`, the arena's
/// reset, the release — to `POSTED` or `NOTHING_PROPOSED` when the batch
/// posted, and to `FREE` at once when the pool refuses the workspace, which
/// is `Idle`.
///
/// # Safety
/// The calling collector holds `mutator`'s token as `COLLECTOR|slot`.
unsafe fn serve_the_grant(
    mutator: &MutatorRecord,
    slot: usize,
    threshold: usize,
    standing: &mut Standing,
) -> Served {
    standing.forget(mutator);
    // A grant read after the cap was stored as zero traces nothing: every
    // path to a batch passes here, so the trace that runs past this reading
    // is the one already under way when the cap was stored.
    if collectors_capped_at_zero() {
        mutator.token.release_claim(slot, false);
        return Served::Idle;
    }

    // Released on the unwind too: a collector that panicked under the claim
    // would otherwise leave the mutator's wait forever; the posted fact is
    // set before the first post, so the unwind's release says what the
    // return's would.
    struct ReleaseOnDrop<'a> {
        mutator: &'a MutatorRecord,
        slot: usize,
        posted: std::cell::Cell<bool>,
        /// Whether the batch posted a [`Verdict::Proposed`]: without one the
        /// release is to `NOTHING_PROPOSED`, which owes the mutator P's
        /// disposition and no trace window.
        proposed: std::cell::Cell<bool>,
        /// The mutator's merge count as the grant read it, before the
        /// batch's peek: every merge it counts has its entries in the ring
        /// the batch reads.
        merges: u32,
    }
    impl Drop for ReleaseOnDrop<'_> {
        fn drop(&mut self) {
            crate::cycle::token::note_traced_mutator(std::ptr::null_mut());
            // The interval a standing ring is taken after is counted from
            // the end of the grant rather than from a batch that posted: a
            // grant the workspace refused, one whose peek found R drained
            // under it, and one an unwind ended each opened the window a
            // take costs, and an instant left standing across any of them
            // would have the next round take again at its own cadence
            // ([`decide_the_branch_and_stamp_the_instant`]). Under the grant, which is
            // where the word may be written, and before the release, which
            // is what ends it. The merges the grant read are accounted for
            // on the same terms: what the batch left of them stands the
            // interval.
            self.mutator.note_standing_since(serve_clock_now());
            self.mutator.note_merges_seen(self.merges);
            // Before the store: the mutator the release wakes may read the
            // instant as soon as the store lands.
            #[cfg(test)]
            testing::note_release();
            let released = match (self.posted.get(), self.proposed.get()) {
                (false, _) => crate::cycle::token::FREE,
                (true, true) => crate::cycle::token::POSTED,
                (true, false) => crate::cycle::token::NOTHING_PROPOSED,
            };
            self.mutator.token.release_claim_to(self.slot, released);
        }
    }
    let held = ReleaseOnDrop {
        mutator,
        slot,
        posted: std::cell::Cell::new(false),
        proposed: std::cell::Cell::new(false),
        merges: mutator.merges(),
    };
    crate::cycle::token::note_traced_mutator(std::ptr::from_ref(mutator).cast_mut());
    #[cfg(test)]
    testing::note_grant();

    // A recall that stands before the batch is made goes back with no batch,
    // as a refused workspace does, rather than after the peek and a stride.
    if mutator.token.is_recalled() {
        return Served::Idle;
    }

    // Declared after the release guard, so that its drop — the reset of
    // the rows, which stand over the mutator's blocks — runs before the
    // release on the unwind as on the return. The epoch is the served
    // mutator's cell, not this thread's: the stamps this trace reads were
    // written against that mutator's clock (`crate::cycle::epoch`).
    let Some(mut arena) = (unsafe { TraceScratchArena::open_for_owner(mutator) }) else {
        return Served::Idle;
    };
    // The list outlives the arena, and nothing else touches it until the
    // batch returns.
    arena.hold_the_grants_behind(unsafe {
        GrantsBehind::new(
            &COLLECTORS[slot].grants_recalled,
            release_the_recalled_grants,
            std::ptr::from_mut(standing).cast(),
        )
    });
    #[cfg(test)]
    testing::note_serving_slot(slot);
    unsafe { batch(mutator, &mut arena, threshold, &held.posted, &held.proposed) }
}

/// A request between its swap and its grant, withdrawn on the unwind.
struct WithdrawOnDrop<'a> {
    token: &'a crate::cycle::token::TraceToken,
    slot: usize,
    standing: bool,
}

impl Drop for WithdrawOnDrop<'_> {
    fn drop(&mut self) {
        if !self.standing {
            return;
        }

        if self.token.withdraw(self.slot) == Withdrawn::Granted {
            // A grant read back on the unwind is released with no batch.
            self.token.release_claim(self.slot, false);
        }
    }
}

/// The requests a collector left standing on the bytes of mutators that did
/// not answer inside the wait: a doubly linked list threaded through the
/// records ([`MutatorRecord::standing_next`], [`MutatorRecord::standing_prev`]), its two ends on the
/// collector thread's frame, with no capacity — the number of mutators is
/// nobody's to know in advance (`dev/DECISIONS.md`, "the standing request
/// lives on the record, the checkpoint serves one grant, and no count is
/// capped"). Read at the checkpoints, withdrawn when the thread ends. A
/// standing request costs no wait; the consent wake cannot be lost, since
/// the slot's wake word makes a wake sent mid-round end the next wait at
/// once, and the slot's byte-event number ([`Collector::byte_wakes`]) says
/// whether a checkpoint has anything to read.
pub(crate) struct Standing {
    slot: usize,
    /// The first and the last record of the list, null when it is empty;
    /// the ends are self-terminated, the first's `prev` and the last's
    /// `next` naming themselves.
    first: *mut MutatorRecord,
    last: *mut MutatorRecord,
    /// The slot's byte-event number as the last pass read it: a checkpoint
    /// that reads the same number has no byte to re-read.
    byte_wakes_seen: usize,
    /// Batches the checkpoints made since the round last asked.
    batches_served: usize,
    /// Whether a wait inside a round returned early and served nothing, so
    /// that the sleep after the round is skipped once.
    consumed_a_wake: bool,
    /// Consent waits of this walk that ran out with the request unanswered,
    /// counted from the round's start ([`Standing::start_a_round`]): past
    /// [`EXPIRED_WAITS_PER_ROUND`] the walk stops waiting and leaves every
    /// later request standing at once.
    expired_waits: usize,
    /// Whether a pass read a listed mutator collecting in line: work this
    /// round rather than idleness, which the walk reads for itself only
    /// where it waits out a refusal ([`note_idleness`]).
    saw_work: bool,
    /// The mutators a checkpoint's batch left at the threshold, carried to
    /// the round that drains them ([`read_one_record`]). A batch the walk
    /// did not make reads its own backlog, and every batch of a mutator
    /// past the walk's bound of waits is one, so without this the round
    /// reads no backlog at all on the records behind a cycling sleeper and
    /// no sibling is ever born (`dev/DECISIONS.md`, "a checkpoint carries
    /// its batch's backlog and a refusal it read out to the round").
    backlogged: Backlogged,
}

impl Standing {
    pub(crate) fn new(slot: usize) -> Self {
        Self {
            slot,
            first: std::ptr::null_mut(),
            last: std::ptr::null_mut(),
            byte_wakes_seen: COLLECTORS[slot].byte_wakes.load(Ordering::Acquire),
            batches_served: 0,
            consumed_a_wake: false,
            expired_waits: 0,
            saw_work: false,
            backlogged: Backlogged::default(),
        }
    }

    /// This list's stamp for a record it holds: the slot index plus one, so
    /// that zero is a record in no list ([`MutatorRecord::standing_slot`]).
    fn stamp(&self) -> u8 {
        self.slot as u8 + 1
    }

    /// Keep `record`'s request standing: appended at the tail, or left where
    /// it stands when already linked — a stale entry whose byte moved on
    /// between a pass's read and the walk's request. The stamp goes down
    /// before the link, and of the link words `next` is first, the word the
    /// registry reads.
    fn push(&mut self, record: *mut MutatorRecord) {
        let mutator = unsafe { &*record };
        if !mutator.standing_next().load(Ordering::Relaxed).is_null() {
            debug_assert_eq!(
                mutator.standing_slot(),
                self.stamp(),
                "a record of another collector's list was pushed onto this one"
            );
            return;
        }

        mutator.note_standing_slot(self.stamp());
        mutator.standing_next().store(record, Ordering::Release);
        if self.last.is_null() {
            mutator.standing_prev().store(record, Ordering::Relaxed);
            self.first = record;
        } else {
            mutator.standing_prev().store(self.last, Ordering::Relaxed);
            unsafe { &*self.last }
                .standing_next()
                .store(record, Ordering::Relaxed);
        }
        self.last = record;
    }

    /// Take `record` out of the list, if it stands: the neighbours' words
    /// first, `prev` second, `next` last, with a release, so that the
    /// registry's acquire load of `next` sees the record unlinked only once
    /// it is.
    fn forget(&mut self, mutator: &MutatorRecord) {
        let record = std::ptr::from_ref(mutator).cast_mut();
        let next_record = mutator.standing_next().load(Ordering::Relaxed);
        if next_record.is_null() {
            return;
        }

        debug_assert_eq!(
            mutator.standing_slot(),
            self.stamp(),
            "a record of another collector's list was spliced out of this one"
        );

        let prev_record = mutator.standing_prev().load(Ordering::Relaxed);
        let is_first = prev_record == record;
        let is_last = next_record == record;
        if is_first {
            self.first = if is_last {
                std::ptr::null_mut()
            } else {
                next_record
            };
        } else {
            unsafe { &*prev_record }.standing_next().store(
                if is_last { prev_record } else { next_record },
                Ordering::Relaxed,
            );
        }
        if is_last {
            self.last = if is_first {
                std::ptr::null_mut()
            } else {
                prev_record
            };
        } else {
            unsafe { &*next_record }.standing_prev().store(
                if is_first { next_record } else { prev_record },
                Ordering::Relaxed,
            );
        }
        mutator
            .standing_prev()
            .store(std::ptr::null_mut(), Ordering::Relaxed);
        // The stamp before the release, as `push` stamps before the link: the
        // registry's gate is the `next` word, so a record whose stamp is
        // cleared after it would already be another life's, and the clear
        // would land in a list that is not this one's.
        mutator.note_standing_slot(0);
        mutator
            .standing_next()
            .store(std::ptr::null_mut(), Ordering::Release);
    }

    /// The record after `record` in the list, or null past the last.
    fn after(record: *mut MutatorRecord) -> *mut MutatorRecord {
        let next_record = unsafe { &*record }.standing_next().load(Ordering::Relaxed);
        if next_record == record {
            std::ptr::null_mut()
        } else {
            next_record
        }
    }

    /// Read every standing request once, when a byte event has happened
    /// since the last pass, and serve one: `REQUESTED|slot` is left
    /// standing; anything else — `MUTATOR`, `FREE`, another slot's value —
    /// is a record moved on, taken out of the list; of the grants
    /// `COLLECTOR|slot` the first read is served after the pass, and every
    /// other is released with no batch and marked
    /// ([`MutatorRecord::note_released_unserved`]), so that a burst of
    /// wakers withholds one stranger's batch each and not a queue of them.
    /// Answers how many batches it made. Under a cap of zero it withdraws
    /// the list instead and makes none ("Cap zero", the module doc).
    fn checkpoint(&mut self, threshold: usize) -> usize {
        if self.first.is_null() {
            return 0;
        }

        if collectors_capped_at_zero() {
            self.withdraw_every_request();
            return 0;
        }

        let wakes = COLLECTORS[self.slot].byte_wakes.load(Ordering::Acquire);
        if wakes == self.byte_wakes_seen {
            return 0;
        }

        self.byte_wakes_seen = wakes;
        #[cfg(test)]
        testing::note_pass();
        let requested = word(REQUESTED, self.slot);
        let granted = word(COLLECTOR, self.slot);
        let mut kept: *mut MutatorRecord = std::ptr::null_mut();
        let mut cursor = self.first;
        while !cursor.is_null() {
            let following = Self::after(cursor);
            let mutator = unsafe { &*cursor };
            let seen = mutator.token.read();
            if seen != requested {
                if state(seen) == MUTATOR {
                    // The mutator took its token over this request and is
                    // collecting in line: work this round, and the reading
                    // the walk makes for itself only where it waits out the
                    // refusal ([`note_idleness`]).
                    self.saw_work = true;
                }

                self.forget(mutator);
                if seen == granted {
                    if kept.is_null() {
                        kept = cursor;
                    } else {
                        mutator.note_released_unserved(true);
                        mutator.token.release_claim(self.slot, false);
                        #[cfg(test)]
                        testing::note_release_unserved();
                    }
                }
            }
            cursor = following;
        }

        if kept.is_null() {
            return 0;
        }

        let outcome = unsafe { serve_the_grant(&*kept, self.slot, threshold, self) };
        #[cfg(test)]
        testing::note_served(outcome);
        if matches!(outcome, Served::Batch { backlog: true, .. }) {
            // The batch left the mutator at the threshold. The walk that
            // would have read that for itself is elsewhere — past its bound
            // of waits, or between rounds — so the reading is carried to
            // the round that drains it, which is what a sibling's birth
            // counts ([`Standing::backlogged`]).
            self.backlogged.push(kept);
        }

        let served = usize::from(matches!(outcome, Served::Batch { .. }));
        self.batches_served += served;
        served
    }

    /// Release every grant this list holds unserved whose mutator recalls
    /// it, from its take's wait or at a withheld stack's mark, with no batch:
    /// the collector traces another
    /// mutator's batch meanwhile, and the grant has none of its own to
    /// abandon. Not marked released unserved, since its mutator is awake
    /// (`MutatorRecord::note_released_unserved`).
    fn release_the_recalled(&mut self) {
        let granted = word(COLLECTOR, self.slot);
        let mut cursor = self.first;
        while !cursor.is_null() {
            let following = Self::after(cursor);
            let mutator = unsafe { &*cursor };
            if mutator.token.read() == granted && mutator.token.is_recalled() {
                self.forget(mutator);
                mutator.token.release_claim(self.slot, false);
            }
            cursor = following;
        }
    }

    /// Start a walk of the records: the waits this walk may spend are its
    /// own, so that a round pays the bound at most once however many
    /// sleeping mutators the registry holds.
    fn start_a_round(&mut self) {
        self.expired_waits = 0;
    }

    /// A consent wait that ran out with the request left standing.
    fn note_an_expired_wait(&mut self) {
        self.expired_waits += 1;
    }

    /// Whether this walk has spent its bound of expired waits, so that a
    /// request landing now is left standing without one.
    fn spent_its_waits(&self) -> bool {
        #[cfg(test)]
        if let Some(cap) = testing::expired_waits_cap() {
            return self.expired_waits >= cap;
        }

        self.expired_waits >= EXPIRED_WAITS_PER_ROUND
    }

    fn take_batches_served(&mut self) -> usize {
        std::mem::replace(&mut self.batches_served, 0)
    }

    /// Whether a pass read a mutator of the list collecting in line since
    /// the round last asked, and forget it.
    ///
    /// "Since the round last asked" and not "this round": the fold is
    /// [`read_one_record`]'s, so a round whose walk reads no record — every
    /// record of the registry named elsewhere — carries what its opening
    /// checkpoint read to the next round, as `batches_served` and the
    /// backlog do. The cost is that round's timer, which lengthens where the
    /// checkpoint's batch would have shortened it (`PLAN.md`, "A round
    /// whose walk reads no record carries its checkpoint's readings").
    fn take_saw_work(&mut self) -> bool {
        std::mem::replace(&mut self.saw_work, false)
    }

    /// Move the backlog the passes read into the round's, emptying the
    /// carried one.
    fn take_backlogged(&mut self, into: &mut Backlogged) {
        self.backlogged.drain_into(into);
    }

    /// Batches the checkpoints made since the round last asked, left as
    /// they are.
    #[cfg(test)]
    pub(crate) fn batches_served_for_test(&self) -> usize {
        self.batches_served
    }

    /// The records standing in the list, first to last.
    #[cfg(test)]
    pub(crate) fn standing_for_test(&self) -> Vec<*mut MutatorRecord> {
        let mut records = Vec::new();
        let mut cursor = self.first;
        while !cursor.is_null() {
            records.push(cursor);
            cursor = Self::after(cursor);
        }
        records
    }

    fn take_consumed_a_wake(&mut self) -> bool {
        std::mem::replace(&mut self.consumed_a_wake, false)
    }

    /// Withdraw every standing request and empty the list: a grant the
    /// withdrawal reads back is released without a batch, and every record is
    /// taken out of the list. At the thread's end and on the unwind, and at
    /// every round under a cap of zero, where no request may stand and no
    /// grant be served ("Cap zero", the module doc).
    fn withdraw_every_request(&mut self) {
        let mut cursor = self.first;
        while !cursor.is_null() {
            let following = Self::after(cursor);
            let mutator = unsafe { &*cursor };
            if mutator.token.withdraw(self.slot) == Withdrawn::Granted {
                mutator.token.release_claim(self.slot, false);
            }
            self.forget(mutator);
            cursor = following;
        }
    }
}

impl Drop for Standing {
    /// The withdrawal of every standing request, at the thread's end and on
    /// the unwind, so that no record reaches the registry linked to a frame
    /// that is gone ([`Standing::withdraw_every_request`]).
    fn drop(&mut self) {
        self.withdraw_every_request();
    }
}

/// One batch over `mutator`, under its token, on `arena` — the collector's own
/// memory, reset before the token goes (module doc); `threshold` is what
/// the batch's form and its backlog reading are read against. The form is
/// read off the front block and the backlog by R's count: a ring the batch
/// leaves in two blocks reads at the threshold off its front block whatever
/// the two hold, a merged lane of two behind the batch's last block being
/// one such (`dev/CYCLE-SPLIT-PACKAGE-3-LANE-CRITIC.md`, F7), and a backlog
/// read that way would vote a sibling's birth for nothing.
///
/// **The ring under the token decides the form, not the request's origin**
/// (`dev/design/a-standing-r-is-taken-after-n-rounds.md`, "The round"). R
/// read at the threshold is the batch of today: K's clamp and the sizing of
/// the next batch. R read below it is the take of a standing ring, clamped
/// one entry short of the threshold — an upper bound on such a ring's
/// count, the front block being the tail block and holding all of it — with
/// K neither read nor sized, since K is the collector's estimate of what a
/// producing mutator offers per batch and the take's clamp is the
/// threshold's: a take that filled it would size K from the threshold on a
/// thread that never produced a batch at all. The take is the ring whole
/// where P has the room for it, and what P's room leaves behind stands a
/// further interval.
///
/// Each part's budget bounds the part whatever the clamp is, and it is spent
/// by the closure of the part's root rather than by the number of roots: a
/// root inside that closure is posted with the part and opens none
/// ([`trace_in_parts`]). A part that meets it is retried under `B_max` once
/// per grant, and past that defers the roots it met to the turnover while the
/// batch goes on; sixty-three roots over closures that do not
/// overlap, each inside the workspace, are sixty-three parts and a verdict
/// per root (`dev/BENCHMARKS.md`, "S65.5 what a mutator waits for under a
/// take in parts").
///
/// The reading holds for the batch under the grant: nothing leaves R while
/// `COLLECTOR|slot` stands, the mutator withholding its frees and its
/// collections for the window's length, and every path that drains or packs
/// R — the in-line collection, the pressure path, the exit — takes the
/// mutator's own claim over a byte this grant holds. Under the grant, then,
/// a ring can only grow; before it the mutator is free to collect in line
/// between the round's pre-claim reading and the request, so a request made
/// at the threshold can meet a ring below it here and is served as the take
/// the ring now asks for, K neither read nor sized for that batch. Neither
/// the checkpoint nor this function needs to know which kind of request it
/// serves, because nothing asks the origin and the ring to agree: a take
/// whose ring crossed the threshold while its owner slept is the threshold
/// batch it now is, and a threshold request over a drained ring is the take
/// its remainder is.
///
/// # Safety
/// The calling thread holds `mutator`'s token and `mutator` is not collecting
/// in line.
unsafe fn batch(
    mutator: &MutatorRecord,
    arena: &mut TraceScratchArena,
    threshold: usize,
    posted: &std::cell::Cell<bool>,
    proposed: &std::cell::Cell<bool>,
) -> Served {
    // Dropped last, so that the trace's segment holds the posts.
    #[cfg(test)]
    let mut segments = testing::BatchSegments::open(if cfg!(feature = "collector-chain") {
        testing::SEGMENT_EXPIRY
    } else {
        testing::SEGMENT_TRACE
    });
    let verdicts = unsafe { VerdictWriter::open(mutator) };
    let reader = unsafe { Reader::new(mutator.candidate_ring()) };
    // The chain's work before the roots: the blocks the epoch passed become
    // ready, and the deaths the check finds are posted, each a verdict the
    // release answers as any other.
    #[cfg(feature = "collector-chain")]
    unsafe {
        crate::cycle::chain::expire(mutator, || arena.read_the_recall_now().is_break());
        #[cfg(test)]
        segments.enter(testing::SEGMENT_CHECK);
        let deaths = crate::cycle::chain::check_the_deaths(
            mutator,
            serve_clock_now(),
            || arena.read_the_recall_now().is_break(),
            |entity| verdicts.room() > 0 && verdicts.post(entity, Verdict::ZeroCount).is_ok(),
        );
        if deaths > 0 {
            posted.set(true);
        }
    }
    #[cfg(test)]
    segments.enter(testing::SEGMENT_TRACE);
    let (at_the_threshold, clamp) = the_form_and_the_clamp(mutator, &reader, threshold);
    // The clamp's shares: R alone takes the clamp its form reads. Beside a
    // ready part R takes what it holds up to that clamp, and the ready part
    // up to half the batch's bound whatever K reads — K grows only on R's
    // batches at the threshold, and a ready part read at a small K would
    // drain behind its own refills; where P's room holds less than both
    // want, the two share it in halves, what one leaves the other taking.
    #[cfg(feature = "collector-chain")]
    let (take, chain_share) = match mutator.chain_ready().len() {
        0 => (verdicts.room().min(clamp), 0),
        ready => {
            let wants_r = reader.unread_at_most(clamp);
            let wants_chain = ready.min(BATCH_BOUND / 2);
            let take = verdicts.room().min(BATCH_BOUND).min(wants_r + wants_chain);
            let r_share = if wants_r + wants_chain > take {
                wants_r.min(take.div_ceil(2).max(take.saturating_sub(wants_chain)))
            } else {
                wants_r
            };
            (take, take - r_share)
        }
    };
    #[cfg(not(feature = "collector-chain"))]
    let take = verdicts.room().min(clamp);
    if take == 0 {
        return Served::Idle;
    }
    let (copy, order) = the_copy_in_the_workspace(arena, threshold, take);

    // The entries copied out of the ready part and out of R, which stay in
    // both until the advance.
    let out = unsafe { std::slice::from_raw_parts_mut(copy, take) };
    #[cfg(feature = "collector-chain")]
    let chain_peek =
        unsafe { crate::cycle::chain::peek_the_ready_part(mutator, &mut out[..chain_share]) };
    #[cfg(feature = "collector-chain")]
    let from_the_chain = chain_peek.copied;
    #[cfg(not(feature = "collector-chain"))]
    let from_the_chain = 0;
    let peeked = reader.peek(&mut out[from_the_chain..]);
    let taken = from_the_chain + peeked.len();
    #[cfg(all(test, feature = "collector-chain"))]
    testing::note_chain_batch(
        peeked.len(),
        from_the_chain,
        peeked.len() == 0 && reader.has_at_least_by_count(1),
    );
    if taken == 0 {
        return Served::Idle;
    }

    let by_address = unsafe { std::slice::from_raw_parts_mut(order, taken) };
    for (position, index) in by_address.iter_mut().enumerate() {
        *index = position as u16;
    }
    by_address.sort_unstable_by_key(|&index| {
        crate::cycle::queue::entry_root(out[usize::from(index)]) as usize
    });
    arena.set_watermark();

    // The live list the parts write, published below for the mutator's take;
    // dropped on the unwind, which gives its blocks back here.
    let mut live = crate::cycle::live_list::Writer::new(arena.turnovers());
    // From the guard on, every root is owed a verdict and R its advance, on
    // the unwind too, and the release that follows is to `POSTED` or, with no
    // set proposed, to `NOTHING_PROPOSED`. With the chain a root read live
    // or unwalked goes into it, and a batch that posted nothing into P
    // releases `FREE`.
    #[cfg(not(feature = "collector-chain"))]
    posted.set(true);
    let mut posts = FinishThePosts {
        verdicts: &verdicts,
        roots: &mut out[..taken],
        reader: &reader,
        peeked,
        proposed,
        #[cfg(feature = "collector-chain")]
        chain: ChainPosts {
            mutator,
            posted,
            peek: chain_peek,
            now: serve_clock_now(),
        },
    };
    #[cfg(test)]
    testing::at_the_start_of_the_trace();
    #[cfg(test)]
    let traced_from = std::time::Instant::now();
    #[cfg(test)]
    let _ = (
        testing::take_lookup_visits(),
        crate::cycle::mark::take_edges_pruned(),
        testing::take_rows_met(),
    );
    let outcome = unsafe { trace_in_parts(arena, &mut posts, by_address, &mut live) };
    let (parts, complete) = (outcome.parts, outcome.complete);
    #[cfg(test)]
    testing::note_traced_batch(|| testing::TracedBatch {
        roots: taken,
        parts,
        complete,
        blocks: arena.blocks_held(),
        wall: traced_from.elapsed(),
        positions_after_the_hook: testing::take_positions_after_the_hook(),
        lookup_visits: testing::take_lookup_visits(),
        edges_pruned: crate::cycle::mark::take_edges_pruned(),
        rows_met: testing::take_rows_met(),
        parts_met_budget: outcome.parts_met_budget,
        retried: outcome.retried,
        deferred_parts: outcome.deferred_parts,
    });
    #[cfg(not(test))]
    let _ = parts;

    posts.post_the_rest_unwalked();
    #[cfg(test)]
    testing::between_the_post_and_the_advance();
    drop(posts);
    let backlog = reader.has_at_least_by_count(threshold);

    arena.reset();
    // A batch that posted nothing into P releases `FREE`, and the live list
    // goes back here, the mutator taking no list from `FREE`.
    if posted.get() {
        live.publish(mutator, serve_clock_now());
    } else {
        drop(live);
    }
    mutator.note_batch();
    if at_the_threshold {
        // K against what R gave: the chain's roots size no K, and R's part
        // must fill K itself, as without the chain.
        size_the_next_batch(mutator, clamp, taken - from_the_chain, complete);
    }

    Served::Batch {
        roots: taken,
        complete,
        backlog,
    }
}

/// The batch's form and clamp, read under the token, where P's room cannot
/// move and R's count can only grow ([`batch`]): whether R reads at
/// `threshold` off its front block, and the clamp that form takes — K, or
/// one entry short of the threshold, which a ring read below it cannot
/// exceed as of that reading.
fn the_form_and_the_clamp(
    mutator: &MutatorRecord,
    reader: &Reader,
    threshold: usize,
) -> (bool, usize) {
    if reader.has_at_least(threshold) {
        let k = match mutator.batch_size() {
            0 => INITIAL_BATCH,
            size => size,
        };
        return (true, k);
    }

    // Saturating because a threshold of zero reaches this arm for a ring
    // with no front block, where the peek takes nothing anyway, and `0 - 1`
    // would clamp at `usize::MAX` instead.
    (false, threshold.saturating_sub(1))
}

/// Room for `take` entries in the batch's workspace, `arena`, and for their
/// indices in the order of their roots' addresses, with the budget of each
/// part set on it first; `threshold` is the batch's, which bounds the copy of
/// a take as K bounds a threshold batch's, and a take beside the collector's
/// chain is bounded by [`BATCH_BOUND`] itself. Neither is null, by the bounds
/// ([`BATCH_BOUND`]).
fn the_copy_in_the_workspace(
    arena: &mut TraceScratchArena,
    threshold: usize,
    take: usize,
) -> (*mut usize, *mut u16) {
    debug_assert!(
        threshold <= BATCH_BOUND,
        "the copy is bounded by the threshold as well as by K"
    );
    arena.budget_blocks(budget_for_this_batch());
    let copy = arena.alloc(take * size_of::<usize>()) as *mut usize;
    let order = arena.alloc(take * size_of::<u16>()) as *mut u16;
    assert!(
        !copy.is_null() && !order.is_null(),
        "the copy fits the workspace by the bound on K and on the threshold"
    );
    (copy, order)
}

/// Bit 1 of an entry of the batch's copy, set once its root's verdict is
/// posted. The copy is the collector's own memory and never goes back into
/// R, and every registered population is at least eight-aligned, so the bit
/// is clear in every entry R holds (`crate::cycle::queue`, "The shape").
const HAS_A_VERDICT: usize = 2;

/// The batch's posts, one per root of its copy, and the advance they owe R.
/// Every root left without a verdict — all of them, when the trace was
/// abandoned or unwound — is posted [`Verdict::Unwalked`] at the drop, on
/// the return and on the unwind alike, and R advances past the batch once,
/// after the last post: no entry is consumed without a verdict and none is
/// posted twice. Past the last verdict the entries are the collector's
/// answer, and leaving them unread would hand the same roots to the next
/// batch.
struct FinishThePosts<'a> {
    verdicts: &'a VerdictWriter<'a>,
    /// The copy's entries, [`HAS_A_VERDICT`] set on each root posted.
    roots: &'a mut [usize],
    reader: &'a Reader<'a>,
    peeked: crate::ring::Peeked,
    /// Set at the first [`Verdict::Proposed`] posted, before the release
    /// reads it.
    proposed: &'a std::cell::Cell<bool>,
    #[cfg(feature = "collector-chain")]
    chain: ChainPosts<'a>,
}

/// What the batch owes the collector's chain: the roots it read out of the
/// ready part, whose advance the drop makes beside R's, and the record the
/// roots read live or unwalked go back into (`crate::cycle::chain`).
#[cfg(feature = "collector-chain")]
struct ChainPosts<'a> {
    mutator: &'a MutatorRecord,
    /// Set at the first post into P, which is what the release reads.
    posted: &'a std::cell::Cell<bool>,
    peek: crate::ring::ChainPeek,
    now: u64,
}

impl FinishThePosts<'_> {
    fn root(&self, index: usize) -> *mut RcHeader {
        crate::cycle::queue::entry_root(self.roots[index] & !HAS_A_VERDICT)
    }

    fn has_a_verdict(&self, index: usize) -> bool {
        self.roots[index] & HAS_A_VERDICT != 0
    }

    /// Post `verdict` for the root at `index`, which has none yet.
    fn post(&mut self, index: usize, verdict: Verdict) {
        debug_assert!(!self.has_a_verdict(index), "one verdict per root");
        // A root read live, or one the trace did not reach, goes into the
        // chain rather than into P — but not on the unwind, where a drawn
        // block is an allocation inside a drop, and not where the pool
        // refused the block: P takes it then, as without the chain.
        #[cfg(feature = "collector-chain")]
        if !std::thread::panicking() {
            let kept = match verdict {
                Verdict::ReadLive => unsafe {
                    crate::cycle::chain::keep_read_live(
                        self.chain.mutator,
                        self.root(index),
                        self.chain.now,
                    )
                },
                Verdict::Unwalked => unsafe {
                    crate::cycle::chain::keep_unwalked(self.chain.mutator, self.root(index))
                },
                Verdict::Proposed | Verdict::ZeroCount => false,
            };
            if kept {
                self.roots[index] |= HAS_A_VERDICT;
                return;
            }
        }
        #[cfg(feature = "collector-chain")]
        self.chain.posted.set(true);
        self.verdicts
            .post(self.root(index), verdict)
            .expect("the batch was clamped to P's room");
        self.roots[index] |= HAS_A_VERDICT;
        if verdict == Verdict::Proposed {
            self.proposed.set(true);
        }
    }

    /// Post [`Verdict::Unwalked`] for every root still without a verdict: no
    /// colour of an abandoned trace is a verdict (`rfc/model/gc/rc-cycle.md`,
    /// "Speculative tracing and exact validation").
    fn post_the_rest_unwalked(&mut self) {
        for index in 0..self.roots.len() {
            if !self.has_a_verdict(index) {
                self.post(index, Verdict::Unwalked);
            }
        }
    }
}

impl Drop for FinishThePosts<'_> {
    fn drop(&mut self) {
        self.post_the_rest_unwalked();
        self.reader.commit(self.peeked);
        #[cfg(feature = "collector-chain")]
        unsafe {
            crate::cycle::chain::commit_the_ready_part(self.chain.mutator, self.chain.peek)
        };
    }
}

/// Size the mutator's next batch from what this one, clamped to `size` roots
/// and taking `taken` of them, did: a batch whose every part finished over the
/// whole clamp doubles it up to [`BATCH_BOUND`], and any other leaves it. A
/// batch that deferred the roots of a part past its budget finished no such
/// part, and lost no root to *unwalked* either, which is what halving would
/// have answered. A trace that finished short of its clamp leaves the
/// size where it stands, the ring or P's room having held no more, which
/// says nothing of what the mutator offers per batch: a merged lane of three
/// roots read at the threshold off its blocks would double K at every
/// turnover of a thread that produces nothing
/// (`dev/CYCLE-SPLIT-PACKAGE-3-LANE-CRITIC.md`, F3). So does a trace
/// abandoned for a refused allocation or the mutator's recall, neither saying
/// how much of the heap the batch would have reached.
fn size_the_next_batch(mutator: &MutatorRecord, size: usize, taken: usize, complete: bool) {
    if complete && taken == size {
        mutator.set_batch_size((size * 2).min(BATCH_BOUND));
    }
}

/// Trace the batch's roots in parts, posting each verdict as its part
/// completes, and answer how many parts were opened and whether every one
/// ran to its end.
///
/// A root no part can place is posted first, before any part: a count read
/// zero, and a root with no row ([`read_the_root`]). Then each root still
/// without a verdict, in R's order, opens a part — the mark and the scan of
/// its closure alone, on the arena above the watermark — and when the part
/// completes, the root and every other root the part met are posted from
/// its rows ([`for_each_met_root`]) and the arena is reset to the
/// watermark for the next part, with a block budget of its own. A root met
/// by an earlier part opens none: its closure is inside that part's, and a
/// root read within a larger closure can read unreachable where its own part
/// would read it live and never the reverse, the proposals over a subset of
/// roots being a subset of the proposals over all of them; either verdict is
/// the owner's exact validation to decide (`rfc/model/gc/rc-cycle.md`,
/// "Worker-to-owner handoff").
///
/// A part whose root it reads live appends the live rows it met to `live`
/// before the reset, which the mutator stamps from at its take
/// (`crate::cycle::live_list`); a part that read its root unreachable lists
/// nothing.
///
/// A part that meets its budget is retried at once under `B_max`
/// ([`retry_under_the_ceiling`]), once per grant. A retry that meets `B_max`,
/// and a part that meets its budget with the retry spent, post every live
/// root their rows met read live, and the batch goes on with the next root
/// on an arena reset to the watermark: the deferred lane hands such a root
/// back at the turnover, and a root of the same closure the rows did not meet
/// opens a part of its own. A retry the pool refuses or the mutator recalls
/// ends the batch as a part would; the budget is read before the pool at
/// every growth, so a part past B meets the budget on the collector's reserve
/// before it sees a refusal. A refused allocation or the mutator's recall
/// ends the batch where it stands, and every root without a verdict is left
/// to [`FinishThePosts`]; the verdicts the earlier parts posted stand, each
/// resting on the owner's exact validation as every verdict does. Besides its
/// stride, the recall is read at each root of the first pass, whose every
/// root costs a header read the stride does not count, a miss where the
/// roots stand in blocks of their own, and before every part but the first,
/// so that neither the pass nor the resets between parts add a walk the
/// recall cannot stop; the lookup of met roots counts a position per root or
/// row it visits, and the list's walk one per live row. No reading follows
/// the last part: a batch whose every part completed, none deferred, is
/// complete, and sizes K as one.
///
/// # Safety
/// As [`mark`] through `AtomicCells`: the calling thread holds the mutator's
/// token, every root of `posts` is an entry of the mutator's R, and
/// `by_address` holds each index of `posts`' roots once, in the order of
/// their roots' addresses.
unsafe fn trace_in_parts(
    arena: &mut TraceScratchArena,
    posts: &mut FinishThePosts<'_>,
    by_address: &[u16],
    live: &mut crate::cycle::live_list::Writer,
) -> PartsOutcome {
    let mut outcome = PartsOutcome::default();
    for index in 0..posts.roots.len() {
        if arena.read_the_recall_now().is_break() {
            return outcome;
        }

        if let RootReading::Verdict(verdict) = unsafe { read_the_root(posts.root(index)) } {
            posts.post(index, verdict);
        }
    }

    let budget = arena.block_budget();
    for index in 0..posts.roots.len() {
        if posts.has_a_verdict(index) {
            continue;
        }

        // Between two parts, and not after the last: a batch whose every
        // part completed is a completed batch, whatever the recall says.
        if outcome.parts > 0 && arena.read_the_recall_now().is_break() {
            return outcome;
        }

        let root = posts.root(index);
        outcome.parts += 1;
        // The flag a deferred part left is not this part's answer.
        arena.forget_the_budget_met();
        #[cfg(test)]
        testing::before_the_trace_of_part(outcome.parts);
        if !unsafe { trace(arena, root) } {
            if !arena.met_its_budget() {
                return outcome;
            }

            // A part that met B is retried at once under `B_max`, once per
            // grant; a second part that meets B finds the attempt spent.
            outcome.parts_met_budget += 1;
            let retried = !outcome.retried && {
                outcome.retried = true;
                unsafe { retry_under_the_ceiling(arena, root, budget) }
            };
            if !retried {
                if !arena.met_its_budget() {
                    return outcome;
                }

                // Past B with the retry spent, or past `B_max`: every live
                // root the rows met is deferred to the turnover, and the
                // batch goes on.
                outcome.deferred_parts += 1;
                #[cfg(test)]
                testing::note_part_deferred();
                let posted = unsafe {
                    for_each_met_root(arena, posts, by_address, |posts, index| {
                        posts.post(index, Verdict::ReadLive);
                    })
                };
                if posted.is_break() {
                    return outcome;
                }

                arena.reset_to_the_watermark();
                continue;
            }
        }

        #[cfg(test)]
        testing::after_the_trace_of_part(outcome.parts);
        #[cfg(test)]
        unsafe {
            testing::note_rows_met(arena)
        };
        let verdict = unsafe { verdict_for(root) };
        posts.post(index, verdict);
        let posted = unsafe {
            for_each_met_root(arena, posts, by_address, |posts, index| {
                posts.post(index, verdict_for(posts.root(index)));
            })
        };
        if posted.is_break() {
            return outcome;
        }

        if verdict == Verdict::ReadLive && unsafe { live.append_the_part(arena) }.is_break() {
            return outcome;
        }

        arena.reset_to_the_watermark();
    }

    outcome.complete = outcome.deferred_parts == 0;
    outcome
}

/// What [`trace_in_parts`] answers of a batch's parts.
#[derive(Clone, Copy, Default)]
struct PartsOutcome {
    /// Parts opened, a retry under `B_max` not counted as one.
    parts: usize,
    /// Whether every root without a verdict of its own had its part finish.
    complete: bool,
    /// Parts that met B.
    parts_met_budget: usize,
    /// Whether a part was retried under `B_max`.
    retried: bool,
    /// Parts whose met roots were deferred read live: past B with the retry
    /// spent, or past `B_max`.
    deferred_parts: usize,
}

/// Retry the part of `root`, which met B, under `B_max` on an arena reset to
/// its watermark, and put B back after it: true when the retry finished,
/// false when it met `B_max` too, a refused allocation or the recall — the
/// rows of the attempt standing either way.
///
/// # Safety
/// As [`trace`].
unsafe fn retry_under_the_ceiling(
    arena: &mut TraceScratchArena,
    root: *mut RcHeader,
    budget: usize,
) -> bool {
    arena.reset_to_the_watermark();
    arena.forget_the_budget_met();
    arena.budget_blocks(retry_budget());
    #[cfg(test)]
    testing::at_the_start_of_the_retry();
    let finished = unsafe { trace(arena, root) };
    arena.budget_blocks(budget);
    finished
}

/// `B_max`, or the budget a case set.
fn retry_budget() -> usize {
    #[cfg(test)]
    if let Some(blocks) = testing::retry_budget() {
        return blocks;
    }

    RETRY_BLOCK_BUDGET
}

/// Act on every root without a verdict whose row the part just traced met,
/// `act` posting one for it. For each block on the part's touched list, the roots whose
/// addresses fall in that block are found in `by_address` by a binary search,
/// and one of two walks is taken: the block's roots, each read for a met row,
/// where they are no more than eight times the groups the part met there, or
/// else the rows the part met in the block, each looked up among those roots.
/// Either walk is bounded by eight times the part's met groups, plus the one
/// root of a large entity's block, so a block holding many roots, each a part
/// of its own, costs each part the rows it met rather than every root of the
/// block (`dev/DECISIONS.md`, "A part's met roots are found by a walk its own
/// rows bound"). Every
/// root or row visited counts a position; answers `Break` where the mutator's
/// recall stood at a reading.
///
/// # Safety
/// As [`verdict_for`] for every root it posts: the part completed on this
/// thread and its rows still stand; `by_address` as [`trace_in_parts`] has it.
unsafe fn for_each_met_root(
    arena: &mut TraceScratchArena,
    posts: &mut FinishThePosts<'_>,
    by_address: &[u16],
    mut act: impl FnMut(&mut FinishThePosts<'_>, usize),
) -> std::ops::ControlFlow<()> {
    let mut array = arena.touched_head();
    while !array.is_null() {
        // A position per block as well as per root or row: a block that holds
        // no root costs two searches and a count, and a retry under `B_max`
        // leaves thousands of such blocks before the part's root's own.
        arena.inspect_position()?;
        let (block, population) = unsafe { ((*array).block, (*array).population) };
        let address =
            |posts: &FinishThePosts<'_>, index: u16| posts.root(usize::from(index)) as usize;
        let first = by_address.partition_point(|&index| address(posts, index) < block as usize);
        let in_the_block = &by_address[first..];
        let in_the_block = &in_the_block[..in_the_block.partition_point(|&index| {
            address(posts, index) < block as usize + crate::memory::block_pool::BLOCK_SIZE
        })];
        let rows_at_most = unsafe { shadow::groups_met(array) } * shadow::GROUP;
        if population == crate::cycle::row::Population::SingleEntity
            || in_the_block.len() <= rows_at_most as usize
        {
            for &index in in_the_block {
                arena.inspect_position()?;
                #[cfg(test)]
                testing::note_a_lookup_visit();
                unsafe { act_if_met(posts, usize::from(index), &mut act) };
            }
        } else {
            unsafe {
                shadow::for_each_met_row(array, |row| {
                    arena.inspect_position()?;
                    #[cfg(test)]
                    testing::note_a_lookup_visit();
                    let Some(entity) = crate::cycle::row::entity_at(block, population, row) else {
                        return std::ops::ControlFlow::Continue(());
                    };
                    if let Ok(at) = in_the_block
                        .binary_search_by_key(&(entity as usize), |&index| address(posts, index))
                    {
                        act_if_met(posts, usize::from(in_the_block[at]), &mut act);
                    }

                    std::ops::ControlFlow::Continue(())
                })?;
            }
        }

        array = unsafe { (*array).next };
    }

    std::ops::ControlFlow::Continue(())
}

/// Act on the root at `index` if it has no verdict and is live with a row the
/// part met; a root read at count zero here opens a part of its own, which its
/// count lets finish at once.
///
/// # Safety
/// As [`for_each_met_root`].
unsafe fn act_if_met(
    posts: &mut FinishThePosts<'_>,
    index: usize,
    act: &mut impl FnMut(&mut FinishThePosts<'_>, usize),
) {
    if posts.has_a_verdict(index) {
        return;
    }

    let root = posts.root(index);
    if let RootReading::Tracked(key) = unsafe { read_the_root(root) }
        && unsafe { crate::cycle::arena::find_initialized_row(key) }.is_some()
    {
        act(posts, index);
    }
}

/// Mark the closure of `root`, then scan it: true when both phases
/// completed, false when either met the budget, a refused allocation or the
/// mutator's recall — at which point no color of the part is a verdict.
///
/// # Safety
/// As [`mark`] through `AtomicCells`: the calling thread holds the mutator's
/// token, and `root` is an entry of the mutator's R.
unsafe fn trace(arena: &mut TraceScratchArena, root: *mut RcHeader) -> bool {
    if unsafe { mark::<AtomicCells>(arena, root) } != MarkResult::Complete {
        return false;
    }

    #[cfg(test)]
    let hooked_at = testing::between_the_phases().then(|| arena.positions_inspected());
    let scanned = (unsafe { scan::<AtomicCells>(arena, root) }) == ScanResult::Complete;
    #[cfg(test)]
    if let Some(from) = hooked_at {
        testing::note_positions_after_the_hook(arena.positions_inspected() - from);
    }

    scanned
}

/// What a root's header says before any row is read: the verdict of a root
/// no trace can place, or the key of the row a trace would give it.
enum RootReading {
    Verdict(Verdict),
    Tracked(crate::cycle::row::RowKey),
}

/// Read `root` for [`RootReading`]: a count read zero is
/// [`Verdict::ZeroCount`], and a live root with no row to place it —
/// untracked, which the trace's own rule reads as an external live reference
/// and which the mutator's trace would place no better — is
/// [`Verdict::ReadLive`]; an *unwalked* verdict would send it round P and R at
/// every batch. Any other root names its row.
///
/// # Safety
/// The calling thread holds the mutator's token, and `root` is an entry of
/// the mutator's R.
unsafe fn read_the_root(root: *mut RcHeader) -> RootReading {
    if unsafe { crate::refcount::slot_state(root) } != crate::refcount::SlotState::Live {
        return RootReading::Verdict(Verdict::ZeroCount);
    }

    match unsafe { resolve_edge_target(root) } {
        EdgeTarget::Tracked(key) => RootReading::Tracked(key),
        _ => RootReading::Verdict(Verdict::ReadLive),
    }
}

/// The verdict a completed part supports for `root`: the verdict of
/// [`read_the_root`] where it gives one, and otherwise the color of its row,
/// potentially unreachable being [`Verdict::Proposed`] and live being
/// [`Verdict::ReadLive`]. A root with no met row is read live too: the part
/// could not place it, which the trace's own rule reads as an external live
/// reference.
///
/// # Safety
/// The part over `root`'s closure completed on this thread and its rows still
/// stand.
unsafe fn verdict_for(root: *mut RcHeader) -> Verdict {
    let key = match unsafe { read_the_root(root) } {
        RootReading::Verdict(verdict) => return verdict,
        RootReading::Tracked(key) => key,
    };

    match unsafe { crate::cycle::arena::find_initialized_row(key) } {
        Some(row) => match shadow::color(unsafe { *row }) {
            Color::PotentiallyUnreachable => Verdict::Proposed,
            _ => Verdict::ReadLive,
        },
        None => Verdict::ReadLive,
    }
}

mod birth;

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests;
