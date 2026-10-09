//! The rig: mutators running one load for a set wall time, each pinned to the
//! logical CPU the driver names, beside collectors pinned the same way. Its
//! placements are those of
//! `docs/history/s64-gc-improvement-analysis-2026-09-27.md`, "Какие опыты
//! нужны": C−1 mutators and the collector on a core of its own, C mutators and
//! the collector competing with them, C+1 mutators under a cap of zero. One
//! process runs one cell, a placement and a load, and prints one line;
//! `dev/tools/rig.sh` reads the topology, names the CPUs and runs every cell
//! into one CSV.
//!
//! A mutator's operation is one iteration of its loop: build the load's
//! garbage graphs and register their roots, register again the roots of the
//! live graphs it built at its start, and poll. The live graphs are held by
//! keepers until the loop ends, so every iteration hands the collector the
//! same mix of garbage and live roots, and what memory stands is the live set
//! plus the garbage no collection has freed yet. Some loads vary this, each
//! described at its field of [`Load`]: a live set registered once and let go
//! at half the run, live rings let go a window of iterations after their
//! build, a live ring whose registrations interleave the garbage ring's.
//!
//! A cell reads, over the mutators' loops: operations a second, each
//! mutator's iterations over its own loop's wall, summed; CPU time an
//! operation (`CLOCK_THREAD_CPUTIME_ID`); the latency, which is an
//! iteration's wall, at p50, p99 and p99.9 of every mutator's iterations
//! within an eighth; context switches (`getrusage(RUSAGE_THREAD)`); the
//! garbage built and freed, the mean time a garbage member waited for its
//! free (the garbage outstanding integrated over the loop, over the garbage
//! built), and the bytes left standing at the loop's end; the most deaths a
//! mutator withheld under a foreign holder; the GC ledger's high-water
//! marks; the turnovers, the roots written back from P into R untraced (one
//! round P → R → P each), and the grants recalled by the mark and by a take; the
//! takes' waits for the token; per grant segment (`testing::SEGMENT_AROUND`
//! and its neighbours), the takes' waits by the segment they met and the
//! returns withheld in it with their time withheld; and the collectors'
//! lives, CPU, wall from birth to end and context switches. Each is read once
//! on an input whose answer is known in
//! [`the_rigs_figures_read_their_known_answers`], the split by segment in
//! [`the_split_by_segment_reads_a_hold_the_case_sets`].
//!
//! Beside those, with no case of known answer, a cell reads: the iterations
//! longer than [`A_LONG_ITERATION`]; the collector's time in each grant
//! segment; the mutators' dispositions of P with no trace window; the
//! completed deaths withheld by a queue entry, at the peak, on the
//! mean and at the loop's end; how long the requests a mutator answered
//! inside its loop stood, by consent and by its own take, how long the
//! collectors' `POSTED` and `ASKED` stood until its take, and how long the
//! requests withdrawn before the stop had stood; and, over the drain, what it
//! freed and how long it took to bring those deaths to zero and to free every
//! garbage member. With a drain, a mutator's CPU, context switches and minor faults
//! are read over the loop and the drain together, the CPU time an operation
//! included; the figures named `_in_the_loop` are the loop's alone.
//!
//! The cell is read from the environment, as `dev/tools/rig.sh` sets it; with
//! nothing set the probe runs every load for [`SMOKE_RUN`] on
//! [`SMOKE_MUTATORS`] unpinned mutators:
//!
//! - `LL_RIG_PLACEMENT` — the placement's name, echoed into the line;
//! - `LL_RIG_LOAD` — the one load to run, by [`Load::name`];
//! - `LL_RIG_MUTATOR_CPUS` — one logical CPU per mutator, comma-separated,
//!   which is also the mutator count;
//! - `LL_RIG_COLLECTOR_CPUS` — the CPUs the collectors pin to, slot by slot
//!   (`worker::testing::pin_collectors_to`);
//! - `LL_RIG_CAP` — the collector cap, the crate's own when unset;
//! - `LL_RIG_SECONDS` — how long the mutators run their loops;
//! - `LL_RIG_PACE_MS` — the period, in milliseconds, at which an iteration
//!   starts, the wait between two iterations polling every millisecond unless
//!   `LL_RIG_WAIT_WITHOUT_POLL` is set; unpaced when unset;
//! - `LL_RIG_WAIT_WITHOUT_POLL` — set to 1, the paced wait is one
//!   [`sleep_without_poll`] to the next iteration's start
//!   (`dev/BENCHMARKS.md`, "readings the S65 and S67 stage notes held, carried
//!   at the stages' close", a mutator asleep to its requests); the drain polls
//!   every millisecond either way;
//! - `LL_RIG_FIRE_EVERY` — the web loads' explicit fire
//!   (`ll_gc_collect_cycles`) after every that many requests a mutator
//!   serves, a take that can land on a collector's commit; none when unset;
//! - `LL_RIG_STANDINGS` — set to 1, the tokens' byte states are timed
//!   (`worker::testing::record_standings`), whose table's lock the handshake
//!   then takes on both sides; the standing figures read zero when unset;
//! - `LL_RIG_ROOT_AGES` — set to 1, the feature build times each
//!   registration (`worker::testing::wave_three::record_root_ages`), a lock
//!   on the mutator's own map a registration, and buckets the batches by
//!   their oldest root's age and the roots by their own; the columns read
//!   empty when unset;
//! - `LL_RIG_DRAIN_MS` — how long, in milliseconds, each mutator polls with
//!   no registration after its loop, the drain; no drain when unset;
//! - `LL_RIG_CHURN_GRAPHS`, `LL_RIG_CHURN_WINDOW` — a churn load's rings an
//!   iteration and the iterations each is held, 16 and 1,024 unless set: the
//!   live set it keeps is their product;
//! - `LL_RIG_REPEAT` — the repeat a web load's draws are seeded with beside
//!   the mutator's index, 1 when unset, so that paired arms draw alike;
//! - `LL_RIG_WARM_UP_SECONDS` — the part of a web load's loop its own
//!   figures leave out, zero when unset;
//! - `LL_RIG_TRACED_BATCHES` — set to 1, a web cell records every batch
//!   traced over its loop and counts those that walked through the whole
//!   long-lived state;
//! - `LL_RIG_STUB_TRACE_MS` — every batch after the setup spins this many
//!   milliseconds in place of its parts and posts its roots read live
//!   (`worker::testing::stub_the_trace`), so that with
//!   `LL_RIG_TRACED_BATCHES` the batches a second a mutator read the rounds'
//!   cadence alone (`dev/BENCHMARKS.md`, "readings the S65 and S67 stage notes
//!   held, carried at the stages' close", run R0); the real trace when unset;
//! - `LL_RIG_BATCH_ROOTS` — every batch at the threshold after the setup
//!   takes this many roots, K fixed rather than grown (`dev/BENCHMARKS.md`,
//!   "readings the S65 and S67 stage notes held, carried at the stages' close",
//!   run R2); the mutators' own K when unset;
//! - `LL_RIG_FIRST_REGIONS` — set to 1, every collector's mark subtracts an
//!   edge into a registered candidate past its root and expands it no further
//!   (`mark::stop_at_candidates`), each part the walk of its root's first
//!   region (`dev/BENCHMARKS.md`, "readings the S65 and S67 stage notes held,
//!   carried at the stages' close", run R2).
//! - `LL_RIG_EPOCH_MS` — X, the epoch's longest stand, in milliseconds, in
//!   place of the crate's 8 s (`ll_gc_set_epoch_interval`; the Critic of
//!   2026-09-30 on the ceiling, finding 6);
//! - `LL_RIG_YOUNG_CUT_MS` — the young cut, in milliseconds, in place of the
//!   crate's 100 ms;
//! - `LL_RIG_KEEP_EVERY_PAGE` — set to 1, the arena's resets give back no
//!   page to the operating system, the control of the discard past the warm
//!   blocks (`arena::keep_every_page`);
//! - `LL_RIG_SPENT_PER_PROOF` — the ratio of the epoch's turn in place of
//!   `epoch::SPENT_PER_PROOF`, set through `ll_gc_set_epoch_ratio`
//!   (`dev/BENCHMARKS.md`, "S67.15: the epoch's ratio at 2, 4 and 8");
//! - `LL_RIG_HOLD_NOTHING` — set to 1, every mark expands its registered
//!   targets as it meets them, the plain depth-first descent the held stack
//!   reorders, as the control of `collector_passes`, `collector_held` and
//!   `collector_widest_pass` (`mark::hold_nothing`);
//! - `LL_RIG_BATCH_DUMP` — a path: every batch of the loop is written there,
//!   one CSV line each, with the mutator's epoch clock at its end, and every
//!   batch of the drain to the same path with `.drain` appended.
//!
//! **The web loads** (`dev/design/the-web-loads.md`) run [`a_web_mutator`] in
//! place of the ring loop: an iteration is one request of
//! `the_web_loads`, its latency the request's wall, and the line's columns
//! from `web_values` on are theirs (`WebCell`), zero for a ring load.
//!
//! Where the kernel grants them, each mutator's user-mode cycles and
//! instructions over its loop are read beside its CPU
//! ([`testing::ThreadCycles`]); they sum to `perf stat`'s count of the whole
//! process within 0.01 % on a cell with no collector (`dev/BENCHMARKS.md`,
//! "S65.24 A, B and C on a box with a PMU").
//!
//! Run in a release build:
//! `cargo test --release --lib -- --ignored --exact
//! cycle::worker::tests::the_rig::a_cell_of_the_rig --test-threads=1 --nocapture`.

use super::the_web_loads::{
    Advanced, Arrivals, CORE_OBJECTS, CacheCounts, Garbage, LongLived, LongLivedShape, Plan,
    Request, RequestBuild, Streams, VALUE_OBJECTS, Variant, WebClasses, build_to, held_by_size,
    longest_build_step, specified_interarrival,
};
use super::what_a_take_costs::{MEMBER_CLASS_BYTES, member_class};
use super::*;
use crate::class::Class;
use crate::cycle::loads::slots_per_block;
use crate::cycle::queue::POLL_STRIDE;
use crate::cycle::testing::{Sent, move_prop};
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, SlotState, ll_release, ll_retain, slot_state};
use crate::test_support::{prop_offset, store_prop};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

/// The property a ring's members are linked through.
const NEXT: u32 = 0;

/// The property through which the first member of a ring holds its graph's
/// shared ring.
const SHARED: u32 = 1;

/// One graph a load builds: `rings` rings of `members` each, the first
/// `roots` members of every ring registered as candidates. When `shared` is
/// not zero the graph has one ring more, of that many members and with no
/// root, which the first member of every other ring holds an edge into, so
/// that the roots' closures overlap there and nowhere else. `fillers` objects
/// of the members' class follow every member, which puts each member in a
/// block of its own.
#[derive(Clone, Copy)]
struct Graph {
    rings: usize,
    members: usize,
    roots: usize,
    shared: usize,
    fillers: usize,
}

impl Graph {
    const NONE: Graph = Graph {
        rings: 0,
        members: 0,
        roots: 0,
        shared: 0,
        fillers: 0,
    };

    const fn roots(&self) -> usize {
        self.rings * self.roots
    }

    const fn members(&self) -> usize {
        self.rings * self.members + self.shared
    }
}

/// What one iteration of a mutator's loop hands the collector:
/// `garbage_graphs` of `garbage` built, registered and let go, and the roots
/// of `live_graphs` of `live` registered again. The live graphs are built at
/// the mutator's start and held by a keeper per ring until its loop ends.
#[derive(Clone, Copy)]
struct Load {
    name: &'static str,
    garbage_graphs: usize,
    garbage: Graph,
    live_graphs: usize,
    live: Graph,
    /// A ring built at every iteration and held by a keeper until the loop
    /// ends, its members' registrations interleaved with the garbage ring's,
    /// so that entries of a ring still live stand between the garbage
    /// members' entries in R however far the collector lags
    /// (`registered-ring-interleaved`). Only with one garbage graph.
    held: Graph,
    /// Whether the live graphs' roots are registered once, at the mutator's
    /// start, and not again at every iteration: a live set past the poll's
    /// stride, built with a poll after every [`LIVE_REGISTRATIONS_A_POLL`]
    /// registrations, whose roots stand deferred once read live.
    live_once: bool,
    /// Whether the keepers let the live graphs go at half the cell's run,
    /// which makes garbage of rings whose roots stand deferred, read live: no
    /// decrement registers them again, and only the deferred roots' own
    /// re-reading finds them. Their members count as garbage built from
    /// then on. Only with `live_once`.
    live_dies_at_half: bool,
    /// Live rings of `churn` built and registered at every iteration, each
    /// held by a keeper for [`churn_window`] iterations and then let go:
    /// every iteration registers live roots, each dying after a round has
    /// read it live.
    churn_graphs: usize,
    churn: Graph,
    /// Whether a churn ring's closing edge, its last member's into its
    /// first, is nulled before its keeper lets it go: the ring then dies by
    /// its counts, no member is garbage a collection must find, and each
    /// root read live before it dies is a completed death whose slot the
    /// entry naming it withholds until a retirement.
    churn_dies_by_count: bool,
    /// A web load of `dev/design/the-web-loads.md`, "The loads", run by
    /// [`a_web_mutator`] in place of the ring loop; every field above is
    /// empty beside it.
    web: Option<Web>,
}

/// A web load: where its requests' objects live, and the values its cache
/// holds, N.
#[derive(Clone, Copy)]
struct Web {
    variant: Variant,
    values: usize,
}

/// The web load `name`: no ring, and `web`.
const fn web(name: &'static str, variant: Variant, values: usize) -> Load {
    Load {
        name,
        garbage_graphs: 0,
        garbage: Graph::NONE,
        held: Graph::NONE,
        live_once: false,
        live_dies_at_half: false,
        churn_graphs: 0,
        churn: Graph::NONE,
        churn_dies_by_count: false,
        web: Some(Web { variant, values }),
        live_graphs: 0,
        live: Graph::NONE,
    }
}

impl Load {
    const fn roots_per_iteration(&self) -> usize {
        self.garbage_graphs * self.garbage.roots()
            + self.live_graphs * self.live.roots()
            + self.churn_graphs * self.churn.roots()
    }

    const fn live_members(&self) -> usize {
        self.live_graphs * self.live.members()
    }
}

/// Roots an iteration of the ring loads registers: one short of the soft
/// threshold, the count `what_a_take_costs` reads its shapes at.
const ROOTS: usize = SOFT_THRESHOLD - 1;

/// A ring of six with one root: the corpus's median closure cut into 63
/// pieces, the component of `what_a_take_costs`'s mixed shapes.
const SMALL_RING: Graph = Graph {
    rings: 1,
    members: 6,
    roots: 1,
    shared: 0,
    fillers: 0,
};

/// Rings of [`SMALL_RING`] whose `garbage_rings` of 63 are garbage and the
/// rest live.
const fn mixed(name: &'static str, garbage_rings: usize) -> Load {
    Load {
        name,
        garbage_graphs: garbage_rings,
        garbage: SMALL_RING,
        held: Graph::NONE,
        live_once: false,
        live_dies_at_half: false,
        churn_graphs: 0,
        churn: Graph::NONE,
        churn_dies_by_count: false,
        web: None,
        live_graphs: ROOTS - garbage_rings,
        live: SMALL_RING,
    }
}

/// The loads of `docs/history/s64-gc-improvement-analysis-2026-09-27.md`'s
/// list, then those of `dev/BENCHMARKS.md`, "S65.21 the front run against
/// leaving the deaths in R" and "S65.24 A, B and C on the rig", then the web
/// loads of `dev/design/the-web-loads.md`, "The protocol (fixed before any run,
/// 2026-09-28)", its deciding cells. Change a name or add a ring load, and
/// change `LOADS` in `dev/tools/rig.sh` with it; the web loads are not in its
/// sweep.
const LOADS: [Load; 23] = [
    // Garbage at 0, 25, 50, 75 and 100 % of the roots, rounded to whole
    // rings of 63.
    mixed("garbage-0", 0),
    mixed("garbage-25", 16),
    mixed("garbage-50", 32),
    mixed("garbage-75", 47),
    mixed("garbage-100", 63),
    // Every root inside one live component of 381 members, the corpus's
    // median closure.
    Load {
        name: "overlapping-live",
        garbage_graphs: 0,
        garbage: Graph::NONE,
        held: Graph::NONE,
        live_once: false,
        live_dies_at_half: false,
        churn_graphs: 0,
        churn: Graph::NONE,
        churn_dies_by_count: false,
        web: None,
        live_graphs: 1,
        live: Graph {
            rings: 1,
            members: 381,
            roots: ROOTS,
            shared: 0,
            fillers: 0,
        },
    },
    // Every root the root of a live ring of five, one member per block, so
    // that the rings' row arrays pass a batch's block budget together
    // (`what_a_take_costs`, the disjoint shape).
    Load {
        name: "disjoint-live",
        garbage_graphs: 0,
        garbage: Graph::NONE,
        held: Graph::NONE,
        live_once: false,
        live_dies_at_half: false,
        churn_graphs: 0,
        churn: Graph::NONE,
        churn_dies_by_count: false,
        web: None,
        live_graphs: ROOTS,
        live: Graph {
            rings: 1,
            members: 5,
            roots: 1,
            shared: 0,
            fillers: slots_per_block(MEMBER_CLASS_BYTES) - 1,
        },
    },
    // One garbage root whose closure is a ring of 4,096.
    Load {
        name: "one-large-root",
        garbage_graphs: 1,
        garbage: Graph {
            rings: 1,
            members: 4096,
            roots: 1,
            shared: 0,
            fillers: 0,
        },
        held: Graph::NONE,
        live_once: false,
        live_dies_at_half: false,
        churn_graphs: 0,
        churn: Graph::NONE,
        churn_dies_by_count: false,
        web: None,
        live_graphs: 0,
        live: Graph::NONE,
    },
    // Seven garbage graphs of nine rings of four around a shared ring of 24:
    // a root's closure is its own ring and the shared one.
    Load {
        name: "partly-overlapping",
        garbage_graphs: 7,
        garbage: Graph {
            rings: 9,
            members: 4,
            roots: 1,
            shared: 24,
            fillers: 0,
        },
        held: Graph::NONE,
        live_once: false,
        live_dies_at_half: false,
        churn_graphs: 0,
        churn: Graph::NONE,
        churn_dies_by_count: false,
        web: None,
        live_graphs: 0,
        live: Graph::NONE,
    },
    // One live component of 2,048 members, every one of them registered
    // again at every iteration.
    Load {
        name: "large-live-core",
        garbage_graphs: 0,
        garbage: Graph::NONE,
        held: Graph::NONE,
        live_once: false,
        live_dies_at_half: false,
        churn_graphs: 0,
        churn: Graph::NONE,
        churn_dies_by_count: false,
        web: None,
        live_graphs: 1,
        live: Graph {
            rings: 1,
            members: 2048,
            roots: 2048,
            shared: 0,
            fillers: 0,
        },
    },
    // One garbage ring of 2,048 members, every one of them registered: a
    // batch proposes a part of the ring's entries, and the teardown kills
    // members whose entries stand in R behind it (`dev/BENCHMARKS.md`,
    // "S65.21 the front run against leaving the deaths in R").
    Load {
        name: "registered-ring",
        garbage_graphs: 1,
        garbage: REGISTERED_RING,
        held: Graph::NONE,
        live_once: false,
        live_dies_at_half: false,
        churn_graphs: 0,
        churn: Graph::NONE,
        churn_dies_by_count: false,
        web: None,
        live_graphs: 0,
        live: Graph::NONE,
    },
    // The same ring beside the live roots of `garbage-0`, re-registered at
    // every iteration behind the ring's members.
    Load {
        name: "registered-ring-live",
        garbage_graphs: 1,
        garbage: REGISTERED_RING,
        held: Graph::NONE,
        live_once: false,
        live_dies_at_half: false,
        churn_graphs: 0,
        churn: Graph::NONE,
        churn_dies_by_count: false,
        web: None,
        live_graphs: ROOTS,
        live: SMALL_RING,
    },
    // Rings under and over a batch's bound of 1,024 roots.
    Load {
        name: "registered-ring-1000",
        garbage_graphs: 1,
        garbage: registered_ring(1000),
        held: Graph::NONE,
        live_once: false,
        live_dies_at_half: false,
        churn_graphs: 0,
        churn: Graph::NONE,
        churn_dies_by_count: false,
        web: None,
        live_graphs: 0,
        live: Graph::NONE,
    },
    Load {
        name: "registered-ring-4000",
        garbage_graphs: 1,
        garbage: registered_ring(4000),
        held: Graph::NONE,
        live_once: false,
        live_dies_at_half: false,
        churn_graphs: 0,
        churn: Graph::NONE,
        churn_dies_by_count: false,
        web: None,
        live_graphs: 0,
        live: Graph::NONE,
    },
    // 70,000 live rings of one member, their roots registered once and
    // deferred when read live: more than 64 batches of K = 1,024 hold, so
    // the deferred set outlasts a reading of 64 batches. Each iteration
    // builds one garbage ring of six.
    Load {
        name: "deferred-live-large",
        garbage_graphs: 1,
        garbage: SMALL_RING,
        held: Graph::NONE,
        live_once: true,
        live_dies_at_half: false,
        churn_graphs: 0,
        churn: Graph::NONE,
        churn_dies_by_count: false,
        web: None,
        live_graphs: DEFERRED_LARGE,
        live: registered_ring(1),
    },
    // 8,192 live rings of six, registered once — the build fills a block of
    // R and births the collector — and let go at half the run: garbage only
    // the deferred roots' re-reading can find. Each iteration builds one
    // garbage ring of six.
    Load {
        name: "deferred-then-dead",
        garbage_graphs: 1,
        garbage: SMALL_RING,
        held: Graph::NONE,
        live_once: true,
        live_dies_at_half: true,
        churn_graphs: 0,
        churn: Graph::NONE,
        churn_dies_by_count: false,
        web: None,
        live_graphs: 8192,
        live: SMALL_RING,
    },
    // Sixteen live rings of six built and registered each iteration, let go
    // [`churn_window`] iterations later: roots read live before they die, the
    // path the arms of `dev/BENCHMARKS.md`, "S65.24 A, B and C on the rig",
    // differ on.
    Load {
        name: "live-churn",
        garbage_graphs: 0,
        garbage: Graph::NONE,
        held: Graph::NONE,
        live_once: false,
        live_dies_at_half: false,
        churn_graphs: 16,
        churn: SMALL_RING,
        churn_dies_by_count: false,
        web: None,
        live_graphs: 0,
        live: Graph::NONE,
    },
    // `live-churn` whose rings die by their counts: each root read live
    // dies a completed death behind its entry, which the deferred lane's
    // turnover retires.
    Load {
        name: "live-churn-dies-by-count",
        garbage_graphs: 0,
        garbage: Graph::NONE,
        held: Graph::NONE,
        live_once: false,
        live_dies_at_half: false,
        churn_graphs: 16,
        churn: SMALL_RING,
        churn_dies_by_count: true,
        web: None,
        live_graphs: 0,
        live: Graph::NONE,
    },
    // The ring of `registered-ring` with a held ring of 64 registered
    // between its members, one entry in 33.
    Load {
        name: "registered-ring-interleaved",
        garbage_graphs: 1,
        garbage: REGISTERED_RING,
        held: registered_ring(64),
        live_once: false,
        live_dies_at_half: false,
        churn_graphs: 0,
        churn: Graph::NONE,
        churn_dies_by_count: false,
        web: None,
        live_graphs: 0,
        live: Graph::NONE,
    },
    // `web-heap` at the larger cache, whose scan the specification names
    // (`dev/design/the-web-loads.md`, "The protocol (fixed before any run,
    // 2026-09-28)", its deciding cells), and `web-arena` at both; `web-heap` at
    // the smaller one for the footprint of a complete walk
    // (`dev/BENCHMARKS.md`, "readings the S65 and S67 stage notes held, carried
    // at the stages' close", run R1).
    web("web-heap", Variant::Heap, 150_000),
    web("web-heap-40k", Variant::Heap, 40_000),
    web("web-arena-40k", Variant::Arena, 40_000),
    web("web-arena-150k", Variant::Arena, 150_000),
];

/// Iterations a ring of `live-churn` is held for: at a 1 ms pace about a
/// second, long enough for a round to read its root live first; 1,024 unless
/// `LL_RIG_CHURN_WINDOW` says otherwise.
fn churn_window() -> usize {
    static WINDOW: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *WINDOW.get_or_init(|| {
        std::env::var("LL_RIG_CHURN_WINDOW").map_or(1024, |window| {
            window.parse().expect("LL_RIG_CHURN_WINDOW is a count")
        })
    })
}

/// Live rings of `deferred-live-large`: past 64 batches of `BATCH_BOUND`, YRC's
/// collections an epoch.
const DEFERRED_LARGE: usize = 70_000;

/// Registrations the build of a registered-once live set makes between two
/// polls, inside the poll's stride.
const LIVE_REGISTRATIONS_A_POLL: usize = POLL_STRIDE / 2;

/// A ring of `members` whose every member is a registered candidate.
const fn registered_ring(members: usize) -> Graph {
    Graph {
        rings: 1,
        members,
        roots: members,
        shared: 0,
        fillers: 0,
    }
}

/// A ring of 2,048 whose every member is a registered candidate.
const REGISTERED_RING: Graph = Graph {
    rings: 1,
    members: 2048,
    roots: 2048,
    shared: 0,
    fillers: 0,
};

// An iteration registers its roots with no poll between them, and the
// teardown registers a member at every edge it nulls: both stay inside the
// poll's stride, past which a registration aborts the process
// (`crate::cycle::queue`, `POLL_STRIDE`).
const _: () = {
    let mut index = 0;
    while index < LOADS.len() {
        let load = LOADS[index];
        let registered_each_iteration = if load.live_once {
            load.garbage_graphs * load.garbage.roots()
        } else {
            load.roots_per_iteration()
        };
        assert!(registered_each_iteration + load.held.roots() < POLL_STRIDE);
        assert!(load.live_once || load.live_members() < POLL_STRIDE);
        assert!(!load.live_dies_at_half || load.live_once);
        index += 1;
    }
};

/// Garbage members a mutator may have built and not yet seen freed before
/// its loop ends early: 512 MiB of members of [`MEMBER_CLASS_BYTES`]. A load
/// whose garbage outruns its collector would stand until the loop ends; the
/// ceiling keeps such a cell off the box's swap, and the line counts the
/// mutators that reached it.
const OUTSTANDING_CEILING: usize = 1 << 22;

/// The wall past which an iteration counts as long: the tail reading of the
/// protocol in `dev/BENCHMARKS.md`, "The Critic's reading, 2026-09-26", in
/// place of a quantile a few collections decide.
const A_LONG_ITERATION: Duration = Duration::from_micros(200);

/// Iterations between two readings of R's length.
const R_SAMPLE_STRIDE: usize = 64;

/// The mutators and the wall time of the run with nothing set.
const SMOKE_MUTATORS: usize = 2;
const SMOKE_RUN: Duration = Duration::from_millis(200);

/// One graph's objects, kept for the teardown of a live graph and reused
/// from iteration to iteration for a garbage one, so that the loop allocates
/// nothing of its own once the vectors have grown.
#[derive(Default)]
struct Built {
    /// Every member, the shared ring's included.
    members: Vec<*mut Object>,
    /// The members registered as candidates.
    roots: Vec<*mut Object>,
    /// The first member of every ring but the shared one: what a live
    /// graph's keepers hold.
    heads: Vec<*mut Object>,
    fillers: Vec<*mut Object>,
}

impl Built {
    fn clear(&mut self) {
        self.members.clear();
        self.roots.clear();
        self.heads.clear();
        self.fillers.clear();
    }
}

/// Register `root` as a candidate by a retain and a non-final release, the
/// state a candidate of a running program is in; a root standing in R
/// already stays there once.
///
/// # Safety
/// `root` is a live object of this thread's heap that something else holds.
pub(super) unsafe fn register(root: *mut Object) {
    unsafe {
        ll_retain(root as *mut RcHeader);
        assert!(
            !ll_release(root as *mut RcHeader),
            "the ring's edge holds the root, so the release is not the last"
        );
    }
}

/// A ring of `members` appended to `built`, every member's creation reference
/// moved into the slot of the member before it, so that the ring stands at
/// one reference per member and no member is registered by its construction.
/// Returns the first member, or null for an empty ring.
///
/// # Safety
/// As [`build`].
unsafe fn ring(
    context: &mut LLContext,
    class: *const Class,
    members: usize,
    fillers: usize,
    built: &mut Built,
) -> *mut Object {
    if members == 0 {
        return std::ptr::null_mut();
    }

    let first = built.members.len();
    for _ in 0..members {
        unsafe {
            built
                .members
                .push(new_constructed(context, class, MemoryCategory::GcHeap));
            for _ in 0..fillers {
                built
                    .fillers
                    .push(new_constructed(context, class, MemoryCategory::GcHeap));
            }
        }
    }

    let ring = &built.members[first..];
    for position in 0..members {
        unsafe {
            move_prop(
                ring[position],
                prop_offset(NEXT),
                ring[(position + 1) % members],
            )
        };
    }

    ring[0]
}

/// Build `graph` on this thread's heap into `built`, which it clears first,
/// and register its roots.
///
/// # Safety
/// `context` and `arena` are this thread's, and `class` carries the
/// properties [`NEXT`] and [`SHARED`] as Box properties.
unsafe fn build(
    context: &mut LLContext,
    arena: *mut Arena,
    class: *const Class,
    graph: Graph,
    built: &mut Built,
) {
    built.clear();
    let shared = unsafe { ring(context, class, graph.shared, graph.fillers, built) };
    for _ in 0..graph.rings {
        let head = unsafe { ring(context, class, graph.members, graph.fillers, built) };
        built.heads.push(head);
        let first = built.members.len() - graph.members;
        built
            .roots
            .extend_from_slice(&built.members[first..first + graph.roots]);
        if !shared.is_null() {
            unsafe { store_prop(arena, head, prop_offset(SHARED), shared) };
        }
    }

    for &root in &built.roots {
        unsafe { register(root) };
    }
}

/// Build `graph` as [`build`] does and register nothing.
///
/// # Safety
/// As [`build`].
unsafe fn build_unregistered(
    context: &mut LLContext,
    arena: *mut Arena,
    class: *const Class,
    graph: Graph,
    built: &mut Built,
) {
    unsafe { build(context, arena, class, Graph { roots: 0, ..graph }, built) };
    let first = built.members.len() - graph.members;
    built
        .roots
        .extend_from_slice(&built.members[first..first + graph.roots]);
}

/// Register the roots of `garbage` and of `held` interleaved, one of `held`'s
/// after every run of `garbage`'s that keeps both spread over the whole.
///
/// # Safety
/// Every root is a live object of this thread's heap that something else
/// holds.
unsafe fn register_interleaved(garbage: &[*mut Object], held: &[*mut Object]) {
    let run = garbage.len().div_ceil(held.len().max(1));
    let mut held = held.iter();
    for chunk in garbage.chunks(run.max(1)) {
        for &root in chunk {
            unsafe { register(root) };
        }
        if let Some(&root) = held.next() {
            unsafe { register(root) };
        }
    }
    for &root in held {
        unsafe { register(root) };
    }
}

/// Kill what held the members apart: a filler carries its creation reference
/// still, so its release is the last one.
///
/// # Safety
/// `fillers` are live objects of this thread's heap nothing else holds.
unsafe fn kill(fillers: &[*mut Object]) {
    for &filler in fillers {
        unsafe {
            assert!(ll_release(filler as *mut RcHeader), "the filler's last");
            ll_object_die(filler);
        }
    }
}

/// The load's live graphs, built on this thread's heap with their roots
/// registered, and the keepers that hold them: one per ring, holding its
/// first member through [`NEXT`] with its own creation reference, so that
/// nothing registers a keeper and the trial deletion cannot subtract its
/// edge.
///
/// # Safety
/// As [`build`].
unsafe fn hold_the_live_graphs(
    context: &mut LLContext,
    arena: *mut Arena,
    class: *const Class,
    load: Load,
) -> (Vec<Built>, Vec<*mut Object>) {
    let mut graphs = Vec::with_capacity(load.live_graphs);
    let mut keepers = Vec::new();
    let mut since_a_poll = 0;
    for _ in 0..load.live_graphs {
        let mut built = Built::default();
        unsafe {
            build(context, arena, class, load.live, &mut built);
            for &head in &built.heads {
                let keeper = new_constructed(context, class, MemoryCategory::GcHeap);
                store_prop(arena, keeper, prop_offset(NEXT), head);
                keepers.push(keeper);
            }
        }

        graphs.push(built);
        since_a_poll += load.live.roots();
        if load.live_once && since_a_poll >= LIVE_REGISTRATIONS_A_POLL {
            let _ = unsafe { crate::gc::ll_gc_maybe_collect() };
            since_a_poll = 0;
        }
    }

    (graphs, keepers)
}

/// Let the keepers go, which makes garbage of the live graphs whose roots
/// stand registered: each keeper's edge nulled — a decrement a registered
/// head does not register again — and the keeper released and died.
/// Returns the members the polls between the keepers freed, which the
/// cell's tally of frees counts beside the loop's.
///
/// # Safety
/// As [`let_the_live_graphs_go`], and the graphs are not touched again.
unsafe fn let_the_keepers_go(arena: *mut Arena, keepers: &[*mut Object]) -> usize {
    let mut freed = 0;
    for (index, &keeper) in keepers.iter().enumerate() {
        unsafe {
            store_prop(arena, keeper, prop_offset(NEXT), std::ptr::null_mut());
            assert!(
                ll_release(keeper as *mut RcHeader),
                "the keeper's edge is null, so its creation reference is its last"
            );
            ll_object_die(keeper);
        }
        if (index + 1) % LIVE_REGISTRATIONS_A_POLL == 0 {
            freed += unsafe { crate::gc::ll_gc_maybe_collect() };
        }
    }

    freed
}

/// Take the live graphs apart by hand: every member retained, so that no
/// edge's null store frees a member the loop is still walking; every edge
/// nulled; every member released and died, then every keeper. By hand and
/// not by a collection, because a commit ages what it reads live and a later
/// trace prunes at a mature member (`what_a_take_costs`, `let_the_rings_go`).
///
/// # Safety
/// The graphs and keepers came from [`hold_the_live_graphs`] on this thread
/// and `arena` is this thread's.
unsafe fn let_the_live_graphs_go(arena: *mut Arena, graphs: &[Built], keepers: &[*mut Object]) {
    let members = || {
        graphs
            .iter()
            .flat_map(|graph| graph.members.iter().copied())
    };
    unsafe {
        for member in members() {
            assert_eq!(
                slot_state(member as *mut RcHeader),
                SlotState::Live,
                "no collection freed a member of a live graph"
            );
            ll_retain(member as *mut RcHeader);
        }

        for &keeper in keepers {
            store_prop(arena, keeper, prop_offset(NEXT), std::ptr::null_mut());
        }

        for member in members() {
            store_prop(arena, member, prop_offset(NEXT), std::ptr::null_mut());
            store_prop(arena, member, prop_offset(SHARED), std::ptr::null_mut());
        }

        for member in members() {
            assert!(ll_release(member as *mut RcHeader), "the member's last");
            ll_object_die(member);
        }

        for &keeper in keepers {
            assert!(
                ll_release(keeper as *mut RcHeader),
                "the keeper's edge is null, so its creation reference is its last"
            );
            ll_object_die(keeper);
        }

        for graph in graphs {
            kill(&graph.fillers);
        }
    }
}

/// The live rings of `live-churn` a mutator holds: one slot an iteration,
/// [`churn_window`] of them in turn, each the rings built at that iteration
/// and their keepers. The vectors are reused from lap to lap.
#[derive(Default)]
struct Churn {
    slots: Vec<(Vec<Built>, Vec<*mut Object>)>,
    next: usize,
}

impl Churn {
    /// Let go the rings of the slot the iteration takes, and build the
    /// load's churn rings into it, registered and held; answers the members
    /// let go as garbage, none when the rings die by their counts.
    ///
    /// # Safety
    /// As [`build`].
    unsafe fn turn(
        &mut self,
        context: &mut LLContext,
        arena: *mut Arena,
        class: *const Class,
        load: Load,
    ) -> usize {
        let window = churn_window();
        if self.slots.len() < window {
            self.slots.push((
                (0..load.churn_graphs).map(|_| Built::default()).collect(),
                Vec::with_capacity(load.churn_graphs * load.churn.rings),
            ));
        }

        let (graphs, keepers) = &mut self.slots[self.next];
        self.next = (self.next + 1) % window;
        let let_go = if load.churn_dies_by_count {
            unsafe { open_the_rings(arena, graphs, load.churn) };
            0
        } else {
            keepers.len() * load.churn.members
        };
        unsafe { let_the_keepers_go(arena, keepers) };
        keepers.clear();
        for built in graphs.iter_mut() {
            unsafe {
                build(context, arena, class, load.churn, built);
                for &head in &built.heads {
                    let keeper = new_constructed(context, class, MemoryCategory::GcHeap);
                    store_prop(arena, keeper, prop_offset(NEXT), head);
                    keepers.push(keeper);
                }
            }
        }

        let_go
    }

    /// Let every ring still held go, at the loop's end.
    ///
    /// # Safety
    /// As [`let_the_keepers_go`].
    unsafe fn let_all_go(&mut self, arena: *mut Arena, load: Load) {
        for (graphs, keepers) in &mut self.slots {
            if load.churn_dies_by_count && !keepers.is_empty() {
                unsafe { open_the_rings(arena, graphs, load.churn) };
            }
            unsafe { let_the_keepers_go(arena, keepers) };
            keepers.clear();
        }
    }
}

/// Null the closing edge of every ring of `graphs`, built of `graph`, so
/// that each ring hangs from its keeper alone and dies by its counts when
/// the keeper lets it go.
///
/// # Safety
/// The graphs were built by [`build`] on this thread and are still held,
/// and `arena` is this thread's.
unsafe fn open_the_rings(arena: *mut Arena, graphs: &[Built], graph: Graph) {
    for built in graphs {
        for ring in built.members[graph.shared..].chunks(graph.members) {
            let last = ring[graph.members - 1];
            unsafe { store_prop(arena, last, prop_offset(NEXT), std::ptr::null_mut()) };
        }
    }
}

/// Latencies in a log-linear histogram of nanoseconds: eight buckets to a
/// power of two, so a quantile is read within an eighth of its samples, and a
/// sample is recorded with no allocation.
#[derive(Clone)]
struct Latencies {
    counts: [u64; 64 * 8],
}

impl Default for Latencies {
    fn default() -> Self {
        Self {
            counts: [0; 64 * 8],
        }
    }
}

impl Latencies {
    fn bucket(nanos: u64) -> usize {
        if nanos < 8 {
            return nanos as usize;
        }

        let power = 63 - nanos.leading_zeros() as usize;
        let eighth = (nanos >> (power - 3)) as usize & 7;
        (power - 2) * 8 + eighth
    }

    /// The largest value bucket `index` holds.
    fn ceiling_of(index: usize) -> u64 {
        if index < 8 {
            return index as u64;
        }

        let (power, eighth) = (index / 8 + 2, (index % 8) as u64);
        ((8 + eighth + 1) << (power - 3)) - 1
    }

    fn record(&mut self, sample: Duration) {
        self.counts[Self::bucket(sample.as_nanos() as u64)] += 1;
    }

    fn add(&mut self, other: &Latencies) {
        for (count, more) in self.counts.iter_mut().zip(other.counts) {
            *count += more;
        }
    }

    /// The sample below which `share` of the samples fall, read as its
    /// bucket's ceiling; zero with no samples.
    fn quantile(&self, share: f64) -> u64 {
        let samples: u64 = self.counts.iter().sum();
        let rank = (samples as f64 * share).ceil().max(1.0) as u64;
        let mut seen = 0;
        for (index, &count) in self.counts.iter().enumerate() {
            seen += count;
            if seen >= rank {
                return Self::ceiling_of(index);
            }
        }

        0
    }
}

/// What one mutator's run read.
#[derive(Clone, Default)]
struct MutatorReading {
    iterations: usize,
    /// The loop's wall, from the start every mutator waits at to the end.
    wall: Duration,
    /// The thread's CPU time over the loop and the drain.
    cpu: Duration,
    /// Context switches over the loop and the drain, voluntary and
    /// involuntary.
    switches: (u64, u64),
    /// Minor page faults over the loop and the drain.
    minor_faults: u64,
    /// Queue records the compactions read over the loop and the drain
    /// (`crate::cycle::queue::take_queue_work`).
    records_read: usize,
    /// The longest R stood at one iteration in [`R_SAMPLE_STRIDE`].
    ring_peak: usize,
    /// The wall of every iteration: the mutator's latency.
    latencies: Latencies,
    /// Members of the garbage graphs the loop built, and of the live and
    /// churn rings it let go.
    garbage_members: usize,
    /// What the loop's polls freed.
    freed_by_polls: usize,
    /// The garbage built and not yet freed, integrated over the loop's time,
    /// in members times nanoseconds: over what was built, the mean time a
    /// member waited for its free (Little's law).
    outstanding_time: u128,
    /// The most deaths the thread withheld under a foreign holder of its
    /// token at the end of an iteration.
    withheld_peak: usize,
    /// The turnovers of the thread's epoch clock over the loop and the
    /// drain: a record another thread's life left keeps its count.
    turnovers: u64,
    /// What the collection after the loop freed, the live graphs gone.
    freed_at_the_end: usize,
    /// The most completed deaths the thread withheld for a queue entry at
    /// the end of an iteration (`crate::cycle::queue::withheld_by_an_entry`).
    withheld_by_an_entry_peak: u64,
    /// Those deaths integrated over the loop's time, in deaths times
    /// nanoseconds: over the loop's wall, the mean withheld.
    withheld_by_an_entry_time: u128,
    /// Those deaths at the loop's end.
    withheld_by_an_entry_at_the_end: u64,
    /// How long, after the loop's end, polls with no registration took to
    /// bring those deaths to zero, or the whole drain where they stayed
    /// (`LL_RIG_DRAIN_MS`).
    remnant_wait: Duration,
    /// Whether the drain brought them to zero.
    remnant_cleared: bool,
    /// What the drain's polls freed.
    freed_in_the_drain: usize,
    /// The thread's CPU over the loop alone.
    cpu_in_the_loop: Duration,
    /// The thread's user-mode cycles and instructions over the loop alone,
    /// and the lower of the shares the counters ran over the loop and over
    /// the loop with the drain; zeros where the kernel refused them
    /// ([`testing::ThreadCycles`]).
    cycles_in_the_loop: u64,
    instructions_in_the_loop: u64,
    counter_share: f64,
    /// The thread's user-mode instructions over the loop and the drain
    /// together: a build that frees in the loop what another leaves to the
    /// drain is charged for the same teardown in both
    /// (`dev/BENCHMARKS.md`, "S65.43: a crossing of the heap's growth releases the lanes").
    instructions_with_the_drain: u64,
    /// Garbage built and not yet freed at the loop's end.
    backlog_at_the_stop: usize,
    /// Iterations whose wall passed [`A_LONG_ITERATION`].
    long_iterations: usize,
    /// The returns the thread withheld under a foreign holder over the loop,
    /// by the grant segment the holder was in, with their time withheld up
    /// to the drain of the withheld stacks that gave them back; a return
    /// still withheld at the loop's end is counted and not timed.
    withheld_by_segment: testing::WithheldBySegment,
    /// How long the requests this thread answered and the `POSTED` releases
    /// and `ASKED` asks it consumed stood, for those answered and consumed
    /// inside the loop; a request standing at the loop's end counts nowhere
    /// (`dev/BENCHMARKS.md`, "readings the S65 and S67 stage notes held,
    /// carried at the stages' close", a mutator asleep to its requests).
    standings: testing::MutatorStandings,
    /// How long after the loop's end the drain's polls took to free every
    /// garbage member built and bring the withheld deaths to zero, or the
    /// whole drain where they did not.
    last_free: Duration,
    /// Whether the loop ended at [`OUTSTANDING_CEILING`] rather than at the
    /// cell's stop.
    at_the_ceiling: bool,
}

/// One mutator: pinned to `cpu` when one is named, its live graphs built,
/// then the loop from `start` until `stop` or [`OUTSTANDING_CEILING`].
fn a_mutator(
    load: Load,
    cpu: Option<usize>,
    class: *const Class,
    start: &Barrier,
    stop: &AtomicBool,
    run_for: Duration,
) -> MutatorReading {
    if let Some(cpu) = cpu {
        testing::pin_this_thread_to(cpu)
            .unwrap_or_else(|error| panic!("the kernel pinned the mutator to CPU {cpu}: {error}"));
    }

    assert!(
        crate::memory::heap::ll_thread_init(),
        "the pool served the mutator thread"
    );
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let (live, keepers) = unsafe { hold_the_live_graphs(&mut context, arena_ptr, class, load) };
    let mut garbage = Built::default();
    let mut held = Built::default();
    let mut held_keepers: Vec<*mut Object> = Vec::new();
    let mut reading = MutatorReading::default();
    start.wait();
    let counters = ThreadCounters::begin();
    let from = counters.from;
    let mut last = from;
    let pace = millis_from_env("LL_RIG_PACE_MS");
    let wait_without_poll = switch_from_env("LL_RIG_WAIT_WITHOUT_POLL");
    let mut keepers_let_go = false;
    let mut churn = Churn::default();
    while !stop.load(Ordering::Relaxed) {
        if load.live_dies_at_half && !keepers_let_go && from.elapsed() >= run_for / 2 {
            reading.freed_by_polls += unsafe { let_the_keepers_go(arena_ptr, &keepers) };
            reading.garbage_members += load.live_members();
            keepers_let_go = true;
        }

        if load.held.members() == 0 {
            for _ in 0..load.garbage_graphs {
                unsafe {
                    build(&mut context, arena_ptr, class, load.garbage, &mut garbage);
                    kill(&garbage.fillers);
                }
                reading.garbage_members += load.garbage.members();
            }
        } else {
            unsafe {
                build_unregistered(&mut context, arena_ptr, class, load.garbage, &mut garbage);
                build_unregistered(&mut context, arena_ptr, class, load.held, &mut held);
                let keeper = new_constructed(&mut context, class, MemoryCategory::GcHeap);
                store_prop(arena_ptr, keeper, prop_offset(NEXT), held.heads[0]);
                held_keepers.push(keeper);
                register_interleaved(&garbage.roots, &held.roots);
            }
            reading.garbage_members += load.garbage.members();
        }

        if load.churn_graphs > 0 {
            reading.garbage_members += unsafe { churn.turn(&mut context, arena_ptr, class, load) };
        }

        if !load.live_once {
            for graph in &live {
                for &root in &graph.roots {
                    unsafe { register(root) };
                }
            }
        }

        reading.freed_by_polls += unsafe { crate::gc::ll_gc_maybe_collect() };
        reading.iterations += 1;
        let now = Instant::now();
        let outstanding = reading.garbage_members - reading.freed_by_polls;
        reading.latencies.record(now - last);
        reading.long_iterations += usize::from(now - last > A_LONG_ITERATION);
        reading.outstanding_time += outstanding as u128 * (now - last).as_nanos();
        reading.withheld_peak = reading
            .withheld_peak
            .max(crate::cycle::deferred_slot_reuse::foreign_withheld_counts().0);
        let by_an_entry = crate::cycle::queue::withheld_by_an_entry();
        reading.withheld_by_an_entry_peak = reading.withheld_by_an_entry_peak.max(by_an_entry);
        reading.withheld_by_an_entry_time += u128::from(by_an_entry) * (now - last).as_nanos();
        last = now;
        if reading.iterations % R_SAMPLE_STRIDE == 0 {
            reading.ring_peak = reading
                .ring_peak
                .max(crate::cycle::queue::candidate_count());
        }

        // A paced loop starts its iterations at a fixed period, so that the
        // offered load is the same in both arms of a comparison.
        // The wait polls every millisecond, as a running program between
        // two bursts would, so that the collections a ring needs are not
        // bounded by the pace; under `LL_RIG_WAIT_WITHOUT_POLL` it is
        // [`sleep_without_poll`].
        if !pace.is_zero() {
            let due = from + pace * u32::try_from(reading.iterations).unwrap_or(u32::MAX);
            wait_until(due, wait_without_poll, || {
                reading.freed_by_polls += unsafe { crate::gc::ll_gc_maybe_collect() };
            });

            // Garbage and withheld deaths stand through the wait, so it
            // counts in their integrals; only the iteration's latency leaves
            // it out.
            let woke = Instant::now();
            let waited = (woke - last).as_nanos();
            let outstanding = reading.garbage_members - reading.freed_by_polls;
            reading.outstanding_time += outstanding as u128 * waited;
            reading.withheld_by_an_entry_time +=
                u128::from(crate::cycle::queue::withheld_by_an_entry()) * waited;
            last = woke;
        }

        if outstanding > OUTSTANDING_CEILING {
            reading.at_the_ceiling = true;
            break;
        }
    }

    counters.read_the_loop(&mut reading);
    reading.backlog_at_the_stop = reading
        .garbage_members
        .saturating_sub(reading.freed_by_polls);
    // The drain: polls with no registration for the same wall in both arms,
    // so that the CPU and what stands after it compare over one window.
    let drain = millis_from_env("LL_RIG_DRAIN_MS");
    let drained_from = Instant::now();
    let mut remnant_wait = None;
    let mut last_free = None;
    while drained_from.elapsed() < drain {
        if remnant_wait.is_none() && crate::cycle::queue::withheld_by_an_entry() == 0 {
            remnant_wait = Some(drained_from.elapsed());
        }
        if last_free.is_none()
            && crate::cycle::queue::withheld_by_an_entry() == 0
            && reading.freed_by_polls + reading.freed_in_the_drain >= reading.garbage_members
        {
            last_free = Some(drained_from.elapsed());
        }
        reading.freed_in_the_drain += unsafe { crate::gc::ll_gc_maybe_collect() };
        std::thread::sleep(Duration::from_millis(1));
    }
    // What the drain's last poll freed, and the whole state for a drain of
    // zero, is read once more at its end.
    if crate::cycle::queue::withheld_by_an_entry() == 0 {
        remnant_wait.get_or_insert(drain);
        if reading.freed_by_polls + reading.freed_in_the_drain >= reading.garbage_members {
            last_free.get_or_insert(drain);
        }
    }
    reading.last_free = last_free.unwrap_or(drain);
    counters.read_the_loop_and_the_drain(&mut reading);
    reading.remnant_cleared = remnant_wait.is_some();
    reading.remnant_wait = remnant_wait.unwrap_or(drain);
    unsafe { churn.let_all_go(arena_ptr, load) };
    // A registered-once set is let go by its keepers alone, garbage the
    // collection after the loop or the thread's exit finds: taken apart by
    // hand, its null stores would register past the poll's stride.
    if !load.live_once {
        unsafe { let_the_live_graphs_go(arena_ptr, &live, &keepers) };
    } else if !keepers_let_go {
        unsafe { let_the_keepers_go(arena_ptr, &keepers) };
    }
    // The held rings go with their keepers, garbage the collection after
    // the loop finds.
    for keeper in held_keepers {
        unsafe {
            assert!(ll_release(keeper as *mut RcHeader), "the keeper's last");
            ll_object_die(keeper);
        }
    }
    reading.freed_at_the_end = unsafe { crate::gc::ll_gc_collect_cycles() };
    drop(arena);
    crate::cycle::queue::release_queue_segments();
    reading
}

/// The steps a paced web request's timeline is advanced in, a poll after
/// each [A]; a cell with `LL_RIG_ARRIVALS` spins the drawn CPU instead, in
/// slices of [`SLICE`].
const REQUEST_STEPS: usize = 128;

/// The synthetic CPU between two polls of a request's phase
/// (`dev/design/the-web-loads.md`, "The loads").
const SLICE: Duration = Duration::from_micros(50);

/// The garbage a web mutator may hold before its loop ends early: 1.5 GiB
/// [A], six mutators' with their long-lived state inside the box's memory.
const WEB_GARBAGE_CEILING: usize = 3 << 29;

/// The requests whose draws the checksum folds, with the setup's: fewer
/// than any arm completes in a cell, so that two paired arms print the same
/// sum.
const CHECKSUM_REQUESTS: usize = 500;

/// The most passes of the teardown, a turnover by hand and a collection
/// each: one pass does not always free the whole state, and the smoke cells
/// took one to three.
const TEARDOWN_PASSES: usize = 20;

/// What one web mutator read beside its [`MutatorReading`]. The figures
/// over the window run from the warm-up's end (`LL_RIG_WARM_UP_SECONDS`) to
/// the stop.
#[derive(Clone, Default)]
struct WebReading {
    /// The setup's wall: the long-lived state built and registered.
    setup: Duration,
    /// The draws of the setup and of the first [`CHECKSUM_REQUESTS`]
    /// requests folded, zero where fewer ran.
    checksum: u64,
    /// Requests over the window.
    requests: usize,
    /// The garbage's mean, in bytes by size index, and its peaks, by size
    /// index and of the sum, over the window.
    garbage_mean: [f64; 3],
    garbage_peak: ([usize; 3], usize),
    /// The garbage at the drain's end, in bytes.
    garbage_at_the_drain_end: usize,
    /// The roots standing in the deferred lane and in R at the drain's end:
    /// garbage behind a root read live waits in the first for a turn.
    deferred_at_the_drain_end: usize,
    candidates_at_the_drain_end: usize,
    /// The process's resident set at this mutator's drain's end, in bytes,
    /// zero where `/proc` does not answer.
    resident_at_the_drain_end: usize,
    /// The cache and the sessions over the window.
    counts: CacheCounts,
    /// Requests whose end landed on a registered root, over the window.
    silent_ends: usize,
    /// The thread's registrations over the window, and those of the arena
    /// resets among them.
    registrations: usize,
    reset_registrations: usize,
    /// Of the resets' registrations, the cache values (`Ended`).
    reset_values_registered: usize,
    /// The value heads and the session heads standing candidate at the
    /// warm-up's end and at the stop.
    standing_at_the_warm_up: (usize, usize),
    standing_at_the_stop: (usize, usize),
    /// The blocks the live session writes hold at the stop.
    blocks_the_writes_hold: usize,
    /// Bytes the heap held past the setup's baseline once the state was let
    /// go and collected after the drain: what the teardown left, after
    /// `teardown_passes` passes.
    held_after_the_teardown: usize,
    teardown_passes: usize,
    /// What the arrivals' loop read (`LL_RIG_ARRIVALS`), empty for a paced
    /// one.
    arrivals: ArrivalFigures,
}

/// One served request of the window, for the pairing across arms
/// (`LL_RIG_REQUESTS_TO`): its index among the mutator's arrivals, its
/// arrival, its service's start and end as offsets from the start barrier,
/// and its drawn CPU and waits.
#[derive(Clone, Copy)]
struct RequestRecord {
    index: usize,
    arrival: Duration,
    start: Duration,
    end: Duration,
    cpu: Duration,
    waits: Duration,
}

/// What a web mutator's arrivals' loop read over its window, the arrivals
/// in `[warm-up, LL_RIG_SECONDS)`.
#[derive(Clone, Default)]
struct ArrivalFigures {
    /// Each request's wall from its arrival and from its service's start.
    from_arrival: Latencies,
    from_service: Latencies,
    /// The arrivals waiting, this one among them, at each service's start:
    /// their sum and their most.
    queued_sum: usize,
    queued_peak: usize,
    /// The services' walls, waits inside, summed: over the window's wall,
    /// the busy share.
    busy: Duration,
    /// Requests whose build and polls outran their drawn CPU.
    over_their_cpu: usize,
    /// The timed spin's calls and turns, and the instructions the plans'
    /// draws took.
    spin_calls: u64,
    spin_turns: u64,
    plan_instructions: u64,
    /// The plans' draws' wall: the rig's work between two services, which
    /// holds the mutator as a service does.
    plan_wall: Duration,
    /// The thread's user-mode instructions from the first window request's
    /// service to the last's end, all of the above inside, and its counter's
    /// reading at that start, which the loop and the drain's reading is taken
    /// from.
    instructions: u64,
    instructions_at_the_window_start: u64,
    /// Bytes the window's requests and state made garbage, and the garbage
    /// when the window started and at its end.
    garbage_made: usize,
    garbage_at_the_start: usize,
    garbage_at_the_end: usize,
    records: Vec<RequestRecord>,
}

/// A web mutator's state from its setup to its teardown: the arena its
/// requests use, its long-lived state, its streams, its garbage and what it
/// reads over the window.
struct WebLoop {
    arena: *mut Arena,
    long_lived: LongLived,
    streams: Streams,
    /// What the heap held for the thread before the setup, taken as the
    /// thread's own.
    baseline: [usize; 3],
    garbage: Garbage,
    reading: WebReading,
    counts_from: CacheCounts,
    admissions_from: usize,
    /// Requests since the start barrier, the window's and before it.
    requests: usize,
    /// The mutator's index and the repeat, which seed its arrivals.
    index: usize,
    repeat: u64,
    /// Members the loop's polls freed, as `ll_gc_maybe_collect` counts
    /// them.
    freed_by_polls: usize,
}

impl WebLoop {
    /// Build and register `web`'s long-lived state for mutator `index` in
    /// repeat `repeat`, polling within the poll's stride; the setup's wall
    /// is read.
    ///
    /// # Safety
    /// `arena` is the calling mutator's, empty, and outlives the loop.
    unsafe fn set_up(
        web: Web,
        index: usize,
        classes: WebClasses,
        repeat: u64,
        arena: *mut Arena,
    ) -> Self {
        let setup_from = Instant::now();
        let baseline = held_by_size();
        let mut garbage = Garbage::new(setup_from, baseline);
        let shape = LongLivedShape::specified(web.values);
        let mut long_lived =
            unsafe { LongLived::new(classes, shape, web.variant, index as u64, repeat, arena) };
        garbage.born(long_lived.held());
        unsafe {
            long_lived.register_the_steady_state(&mut || {
                let _ = crate::gc::ll_gc_maybe_collect();
            })
        };
        Self {
            arena,
            long_lived,
            streams: Streams::new(index as u64, repeat),
            baseline,
            garbage,
            reading: WebReading {
                setup: setup_from.elapsed(),
                ..WebReading::default()
            },
            counts_from: CacheCounts::default(),
            admissions_from: 0,
            requests: 0,
            index,
            repeat,
            freed_by_polls: 0,
        }
    }

    /// Start the window at `now`.
    fn restart(&mut self, now: Instant) {
        self.garbage.restart(now);
        self.counts_from = self.long_lived.counts;
        self.admissions_from = crate::refcount::admissions();
        self.reading = WebReading {
            setup: self.reading.setup,
            checksum: self.reading.checksum,
            standing_at_the_warm_up: self.long_lived.standing_candidate(),
            ..WebReading::default()
        };
    }

    /// Draw one request and run it: started, advanced at [`REQUEST_STEPS`]
    /// even steps of its timeline and on to its end, built by [`build_to`]
    /// with a poll after each of its steps, ended, and a poll.
    fn run_a_request(&mut self) {
        let plan = self.draw_a_plan(None);
        let variant = plan.variant;
        let garbage = &mut self.garbage;
        let mut build = RequestBuild {
            context: LLContext { arena: self.arena },
            arena: self.arena,
            long_lived: &mut self.long_lived,
        };
        let (mut request, advanced) = unsafe { Request::start(&mut build, plan) };
        account(garbage, advanced);
        let freed_by_polls = &mut self.freed_by_polls;
        let mut step = |advanced| {
            account(garbage, advanced);
            *freed_by_polls += poll_and_read(garbage);
            Duration::ZERO
        };
        for step_at in 1..=REQUEST_STEPS {
            let to = step_at as f64 / REQUEST_STEPS as f64;
            let _ = unsafe { build_to(&mut request, &mut build, to, &mut step) };
        }

        while !request.is_complete() {
            let _ = unsafe { build_to(&mut request, &mut build, 1.0, &mut step) };
        }

        testing::enter_the_rig_section(testing::RigSection::End);
        let ended = unsafe { request.finish(&mut build) };
        garbage.ended(ended.bytes);
        self.freed_by_polls += poll_and_read(garbage);
        self.reading.silent_ends += usize::from(ended.silent);
        if variant == Variant::Arena {
            self.reading.reset_registrations += ended.registered;
        }

        self.reading.requests += 1;
        self.requests += 1;
        if self.requests == CHECKSUM_REQUESTS {
            self.reading.checksum =
                self.streams.checksum().rotate_left(17) ^ self.long_lived.draws_checksum();
        }
    }

    /// Run requests from `from`, the start barrier, until `stop` or
    /// [`WEB_GARBAGE_CEILING`]: the window restarted at `from` and again at
    /// the warm-up's end, each request's wall its latency, and with
    /// `LL_RIG_PACE_MS` a request started at each period.
    fn run_until(&mut self, stop: &AtomicBool, from: Instant, reading: &mut MutatorReading) {
        self.restart(from);
        let warm_up = seconds_from_env("LL_RIG_WARM_UP_SECONDS");
        let mut warmed = warm_up.is_zero();
        let pace = millis_from_env("LL_RIG_PACE_MS");
        let wait_without_poll = switch_from_env("LL_RIG_WAIT_WITHOUT_POLL");
        while !stop.load(Ordering::Relaxed) {
            let began = Instant::now();
            if !warmed && began - from >= warm_up {
                self.restart(began);
                warmed = true;
            }

            self.run_a_request();
            reading.iterations += 1;
            let wall = began.elapsed();
            reading.latencies.record(wall);
            reading.long_iterations += usize::from(wall > A_LONG_ITERATION);
            if !pace.is_zero() {
                let due = from + pace * u32::try_from(reading.iterations).unwrap_or(u32::MAX);
                wait_until(due, wait_without_poll, || {
                    self.freed_by_polls += poll_and_read(&mut self.garbage);
                });
            }

            if self.garbage.current().iter().sum::<usize>() > WEB_GARBAGE_CEILING {
                reading.at_the_ceiling = true;
                break;
            }
        }
    }

    /// Serve the arrivals of [`Arrivals`] (`LL_RIG_ARRIVALS=1`) from `from`,
    /// the start barrier: the oldest arrival first, the mutator sleeping
    /// without a poll while none waits, until the arrivals pass `run_for`,
    /// the window's arrivals all served, or [`WEB_GARBAGE_CEILING`]. The
    /// window is the arrivals from the warm-up to `run_for`, the same set in
    /// every arm; its figures and the thread's instructions run from the
    /// first one's service to the last one's end, the backlog at `run_for`
    /// served after it.
    fn run_the_arrivals(
        &mut self,
        from: Instant,
        run_for: Duration,
        counters: &ThreadCounters,
        reading: &mut MutatorReading,
    ) {
        let warm_up = seconds_from_env("LL_RIG_WARM_UP_SECONDS");
        let interarrival = std::env::var("LL_RIG_INTERARRIVAL_MS").map_or_else(
            |_| specified_interarrival(),
            |millis| {
                Duration::from_secs_f64(
                    millis
                        .parse::<f64>()
                        .expect("LL_RIG_INTERARRIVAL_MS is a number")
                        / 1e3,
                )
            },
        );
        let arrivals = Arrivals::new(self.index as u64, self.repeat, interarrival);
        let expected = ((run_for - warm_up).as_secs_f64() / interarrival.as_secs_f64()) as usize;
        let mut figures = ArrivalFigures {
            records: Vec::with_capacity(expected * 2),
            ..ArrivalFigures::default()
        };
        let mut queue = ArrivalQueue::new(
            std::iter::from_fn({
                let mut arrivals = arrivals;
                move || Some(arrivals.next())
            }),
            run_for,
        );
        let mut windowed = false;
        let mut instructions_from = 0;
        let mut plan = self.draw_a_plan(None);
        for index in 0.. {
            let Some((arrival, queued)) = queue.take(from) else {
                break;
            };

            let in_the_window = arrival >= warm_up;
            if in_the_window && !windowed {
                windowed = true;
                self.restart(Instant::now());
                instructions_from = counters.instructions();
                figures.instructions_at_the_window_start = instructions_from;
                figures.garbage_at_the_start = self.garbage.current().iter().sum();
                (
                    figures.spin_calls,
                    figures.spin_turns,
                    figures.plan_instructions,
                ) = (0, 0, 0);
            }

            let start = Instant::now();
            if in_the_window {
                figures.queued_sum += queued;
                figures.queued_peak = figures.queued_peak.max(queued);
            }
            let (cpu, waits) = (plan.cpu, plan.waits[0] + plan.waits[1]);
            let overran = self.serve(plan, &mut figures);
            let end = Instant::now();
            reading.iterations += 1;
            if in_the_window {
                figures.from_arrival.record(end - (from + arrival));
                figures.from_service.record(end - start);
                figures.busy += end - start;
                figures.over_their_cpu += usize::from(overran);
                figures.records.push(RequestRecord {
                    index,
                    arrival,
                    start: start - from,
                    end: end - from,
                    cpu,
                    waits,
                });
                figures.instructions = counters.instructions() - instructions_from;
            }

            if self.garbage.current().iter().sum::<usize>() > WEB_GARBAGE_CEILING {
                reading.at_the_ceiling = true;
                break;
            }

            plan = self.draw_a_plan(in_the_window.then_some((counters, &mut figures)));
        }

        figures.garbage_made = self.garbage.made();
        figures.garbage_at_the_end = self.garbage.current().iter().sum();
        self.reading.arrivals = figures;
    }

    /// The next request's plan, its draw's instructions counted into the
    /// figures `counted` names: the rig's work and not the runtime's,
    /// subtracted from the window's. The draw runs in a section of its own
    /// ([`Stretch::Draw`]), entered and left outside the count: it touches no
    /// entity of the runtime's heap, and is the harness taken out of the
    /// program.
    fn draw_a_plan(&mut self, counted: Option<(&ThreadCounters, &mut ArrivalFigures)>) -> Plan {
        in_a_stretch(Stretch::Draw, || {
            let Some((counters, figures)) = counted else {
                return Plan::draw(&mut self.streams, self.long_lived.targets());
            };
            let (before, drawn_from) = (counters.instructions(), Instant::now());
            let plan = Plan::draw(&mut self.streams, self.long_lived.targets());
            figures.plan_instructions += counters.instructions() - before;
            figures.plan_wall += drawn_from.elapsed();
            plan
        })
    }

    /// Serve one request of `plan`: its three phases of drawn CPU split at
    /// its waits' places, each run in slices of [`SLICE`] of the drawn CPU —
    /// the timeline advanced to the slice's end by [`build_to`], a poll after
    /// each of its steps, and the timed spin to the slice's deadline — and
    /// each wait slept without a poll. The
    /// build's work is inside the drawn CPU; a poll's wall moves the
    /// deadline, so that the runtime's pauses add to the request's wall. Answers whether the
    /// build and the polls outran the drawn CPU.
    fn serve(&mut self, plan: Plan, figures: &mut ArrivalFigures) -> bool {
        let variant = plan.variant;
        let (cpu, waits, bounds) = (
            plan.cpu,
            plan.waits,
            [0.0, plan.waits_at[0], plan.waits_at[1], 1.0],
        );
        let garbage = &mut self.garbage;
        let mut build = RequestBuild {
            context: LLContext { arena: self.arena },
            arena: self.arena,
            long_lived: &mut self.long_lived,
        };
        let (mut request, advanced) = unsafe { Request::start(&mut build, plan) };
        account(garbage, advanced);
        let mut overran = false;
        for phase in 0..3 {
            let (low, high) = (bounds[phase], bounds[phase + 1]);
            let spun = run_a_phase(cpu.mul_f64(high - low), |share| {
                let to = low + (high - low) * share;
                unsafe {
                    build_to(&mut request, &mut build, to, |advanced| {
                        account(garbage, advanced);
                        let polled = Instant::now();
                        self.freed_by_polls += poll_and_read(garbage);
                        polled.elapsed()
                    })
                }
                .0
            });
            overran |= phase == 2 && spun.overran;
            figures.spin_turns += spun.turns;
            figures.spin_calls += spun.calls;
            if phase < 2 {
                sleep_without_poll(waits[phase]);
            }
        }

        while !request.is_complete() {
            let _ = unsafe {
                build_to(&mut request, &mut build, 1.0, |advanced| {
                    account(garbage, advanced);
                    self.freed_by_polls += poll_and_read(garbage);
                    Duration::ZERO
                })
            };
        }

        testing::enter_the_rig_section(testing::RigSection::End);
        let ended = unsafe { request.finish(&mut build) };
        garbage.ended(ended.bytes);
        self.freed_by_polls += poll_and_read(garbage);
        self.reading.silent_ends += usize::from(ended.silent);
        if variant == Variant::Arena {
            self.reading.reset_registrations += ended.registered;
            self.reading.reset_values_registered += ended.values_registered;
        }

        self.reading.requests += 1;
        self.requests += 1;
        if self.requests == CHECKSUM_REQUESTS {
            self.reading.checksum =
                self.streams.checksum().rotate_left(17) ^ self.long_lived.draws_checksum();
        }
        let fire_every = count_from_env("LL_RIG_FIRE_EVERY");
        if fire_every > 0 && self.requests.is_multiple_of(fire_every) {
            self.freed_by_polls += unsafe { crate::gc::ll_gc_collect_cycles() };
            garbage.read(Instant::now(), held_by_size());
        }
        overran
    }

    /// The window's figures at the loop's end.
    fn read_at_the_stop(&mut self) {
        self.garbage.read(Instant::now(), held_by_size());
        self.reading.garbage_mean = self.garbage.mean();
        self.reading.garbage_peak = self.garbage.peak();
        self.reading.counts = counts_since(self.long_lived.counts, self.counts_from);
        self.reading.registrations = crate::refcount::admissions() - self.admissions_from;
        self.reading.standing_at_the_stop = self.long_lived.standing_candidate();
        self.reading.blocks_the_writes_hold = self.long_lived.blocks_the_writes_hold();
    }

    /// The drain: polls every millisecond with no request, as the ring
    /// loads' drain, for `LL_RIG_DRAIN_MS`. Reads what its polls freed, how
    /// long the withheld deaths took to reach zero and the garbage with them,
    /// each the whole drain where they did not, and the garbage at its end.
    fn drain(&mut self, reading: &mut MutatorReading) {
        reading.freed_by_polls = self.freed_by_polls;
        let drain = millis_from_env("LL_RIG_DRAIN_MS");
        let drained_from = Instant::now();
        let (mut remnant_wait, mut last_free) = (None, None);
        loop {
            let at = drained_from.elapsed();
            if crate::cycle::queue::withheld_by_an_entry() == 0 {
                remnant_wait.get_or_insert(at);
                if self.garbage.current().iter().sum::<usize>() == 0 {
                    last_free.get_or_insert(at);
                }
            }

            if at >= drain {
                break;
            }

            reading.freed_in_the_drain += poll_and_read(&mut self.garbage);
            std::thread::sleep(Duration::from_millis(1));
        }

        reading.remnant_cleared = remnant_wait.is_some();
        reading.remnant_wait = remnant_wait.unwrap_or(drain);
        reading.last_free = last_free.unwrap_or(drain);
        self.reading.garbage_at_the_drain_end = self.garbage.current().iter().sum();
        self.reading.deferred_at_the_drain_end = crate::cycle::queue::deferred_count();
        self.reading.candidates_at_the_drain_end = crate::cycle::queue::candidate_count();
        self.reading.resident_at_the_drain_end = resident_bytes();
    }

    /// Let the state go and free it: a state the collections read live is
    /// stamped and freed only after a turnover, and its roots read live
    /// stand in the deferred lane, so each pass turns the thread's epoch by
    /// hand, re-offers the lane and collects, until the heap holds the
    /// setup's baseline again or [`TEARDOWN_PASSES`] ran. Answers the reading
    /// with what the teardown left.
    fn tear_down(self, reading: &mut MutatorReading) -> WebReading {
        let mut web_reading = self.reading;
        let _ = unsafe { self.long_lived.let_go() };
        let baseline = self.baseline;
        let held_past_the_baseline = || -> usize {
            held_by_size()
                .iter()
                .zip(baseline)
                .map(|(held, baseline)| held.saturating_sub(baseline))
                .sum()
        };
        while web_reading.teardown_passes < TEARDOWN_PASSES {
            crate::cycle::epoch::turn_this_threads_cell();
            crate::cycle::queue::reoffer_deferred_candidates();
            reading.freed_at_the_end += unsafe { crate::gc::ll_gc_collect_cycles() };
            web_reading.teardown_passes += 1;
            if held_past_the_baseline() == 0 {
                break;
            }

            std::thread::sleep(Duration::from_millis(10));
        }

        web_reading.held_after_the_teardown = held_past_the_baseline();
        web_reading
    }
}

/// What one phase of a request spun: the timed spin's calls and turns, and
/// whether its last slice's work outran the phase's drawn CPU.
struct PhaseSpun {
    calls: u64,
    turns: u64,
    overran: bool,
}

/// Run one phase of `cpu` drawn CPU in slices of [`SLICE`]: in each, `step`
/// with the share of the phase done at the slice's end — the request's work
/// to there and its polls, answering their walls summed — and the timed spin
/// to the slice's deadline. The deadlines run from the phase's start by the
/// slices' CPU, and each poll's wall pushes them back, so the work is inside
/// the drawn CPU and the polls add to it.
fn run_a_phase(cpu: Duration, mut step: impl FnMut(f64) -> Duration) -> PhaseSpun {
    let slices = cpu.as_nanos().div_ceil(SLICE.as_nanos()).max(1) as u32;
    let mut spun = PhaseSpun {
        calls: 0,
        turns: 0,
        overran: false,
    };
    let mut deadline = Instant::now();
    for slice in 1..=slices {
        deadline += cpu / slices;
        deadline += step(f64::from(slice) / f64::from(slices));
        spun.overran = Instant::now() > deadline;
        testing::enter_the_rig_section(testing::RigSection::Spin);
        spun.turns += spin_until(deadline);
        spun.calls += 1;
    }

    spun
}

/// A mutator's arrivals as its loop serves them: `offsets` from the start
/// barrier, those before `run_for` taken, the oldest waiting first.
struct ArrivalQueue<I: Iterator<Item = Duration>> {
    offsets: I,
    next: Option<Duration>,
    waiting: std::collections::VecDeque<Duration>,
    run_for: Duration,
}

impl<I: Iterator<Item = Duration>> ArrivalQueue<I> {
    fn new(mut offsets: I, run_for: Duration) -> Self {
        let next = offsets.next().filter(|&next| next < run_for);
        Self {
            offsets,
            next,
            waiting: std::collections::VecDeque::new(),
            run_for,
        }
    }

    /// The next arrival to serve, counted from `from`, and the arrivals
    /// waiting at its service's start, itself among them: the oldest waiting,
    /// or, with none, the next, slept for without a poll. `None` once every
    /// arrival before `run_for` was taken.
    fn take(&mut self, from: Instant) -> Option<(Duration, usize)> {
        self.admit(from);
        if self.waiting.is_empty() {
            let next = self.next?;
            sleep_without_poll((from + next).saturating_duration_since(Instant::now()));
            self.admit(from);
        }

        let queued = self.waiting.len();
        self.waiting.pop_front().map(|arrival| (arrival, queued))
    }

    /// Move every arrival due by now into the waiting ones.
    fn admit(&mut self, from: Instant) {
        while let Some(next) = self.next.filter(|&next| from + next <= Instant::now()) {
            self.waiting.push_back(next);
            self.next = self.offsets.next().filter(|&next| next < self.run_for);
        }
    }
}

/// A step's bytes into `garbage`'s count.
fn account(garbage: &mut Garbage, advanced: Advanced) {
    garbage.born(advanced.born);
    garbage.ended(advanced.ended);
}

/// Poll, then read the garbage the poll left. Answers the members the poll
/// freed.
fn poll_and_read(garbage: &mut Garbage) -> usize {
    testing::enter_the_rig_section(testing::RigSection::Poll);
    let freed = unsafe { crate::gc::ll_gc_maybe_collect() };
    testing::enter_the_rig_section(testing::RigSection::Other);
    garbage.read(Instant::now(), held_by_size());
    freed
}

/// Wait until `due`: asleep with no poll, [`sleep_without_poll`], with
/// `without_poll`, else calling `poll` every millisecond, as a running
/// program between two bursts.
fn wait_until(due: Instant, without_poll: bool, mut poll: impl FnMut()) {
    while let Some(ahead) = due.checked_duration_since(Instant::now()) {
        if without_poll {
            sleep_without_poll(ahead);
            continue;
        }

        poll();
        std::thread::sleep(ahead.min(Duration::from_millis(1)));
    }
}

/// One web mutator: pinned to `cpu` when one is named, its long-lived state
/// built and registered to its steady state, then requests from `start`
/// until `stop` or [`WEB_GARBAGE_CEILING`], the drain, and the teardown,
/// waiting at `stages` for the driver's readings between them.
#[expect(
    clippy::too_many_arguments,
    reason = "the cell's parts, each read once"
)]
fn a_web_mutator(
    web: Web,
    index: usize,
    cpu: Option<usize>,
    classes: WebClasses,
    start: &Barrier,
    stages: &Barrier,
    stop: &AtomicBool,
    repeat: u64,
    run_for: Duration,
) -> (MutatorReading, WebReading) {
    if let Some(cpu) = cpu {
        testing::pin_this_thread_to(cpu)
            .unwrap_or_else(|error| panic!("the kernel pinned the mutator to CPU {cpu}: {error}"));
    }

    assert!(
        crate::memory::heap::ll_thread_init(),
        "the pool served the mutator thread"
    );
    let mut arena = Arena::new();
    let mut web_loop = unsafe { WebLoop::set_up(web, index, classes, repeat, &mut arena) };
    let mut reading = MutatorReading::default();
    start.wait();
    let counters = ThreadCounters::begin();
    if switch_from_env("LL_RIG_ARRIVALS") {
        assert!(
            millis_from_env("LL_RIG_PACE_MS").is_zero(),
            "LL_RIG_ARRIVALS replaces LL_RIG_PACE_MS"
        );
        web_loop.run_the_arrivals(counters.from, run_for, &counters, &mut reading);
    } else {
        web_loop.run_until(stop, counters.from, &mut reading);
    }
    counters.read_the_loop(&mut reading);
    web_loop.read_at_the_stop();
    // The three stages of `run`, each the driver's reading.
    stages.wait();
    web_loop.drain(&mut reading);
    counters.read_the_loop_and_the_drain(&mut reading);
    stages.wait();
    stages.wait();
    let web_reading = web_loop.tear_down(&mut reading);
    drop(arena);
    crate::cycle::queue::release_queue_segments();
    (reading, web_reading)
}

/// The counts `now` holds over those of `from`.
fn counts_since(now: CacheCounts, from: CacheCounts) -> CacheCounts {
    CacheCounts {
        hits: now.hits - from.hits,
        hits_registering: now.hits_registering - from.hits_registering,
        misses: now.misses - from.misses,
        evictions_silent: now.evictions_silent - from.evictions_silent,
        evictions_registering: now.evictions_registering - from.evictions_registering,
        sessions_replaced: now.sessions_replaced - from.sessions_replaced,
        session_writes: now.session_writes - from.session_writes,
    }
}

/// The counters of a mutator's thread from the start barrier on: its CPU, its
/// context switches, its minor faults, its user-mode cycles and instructions,
/// its queue work and its epoch's turnovers, read at the loop's end and at the
/// drain's into a [`MutatorReading`].
struct ThreadCounters {
    from: Instant,
    cpu_from: Duration,
    switches_from: (u64, u64),
    faults_from: u64,
    cycles: Option<testing::ThreadCycles>,
    record: &'static mutator_record::MutatorRecord,
    turnovers_from: u64,
}

impl ThreadCounters {
    /// Begin counting on the calling mutator, whose record is registered;
    /// the per-thread figures a cell reads are zeroed.
    fn begin() -> Self {
        let (cpu_from, switches_from) = (
            testing::thread_cpu_time(),
            testing::thread_context_switches(),
        );
        let faults_from = testing::thread_minor_faults();
        let cycles = testing::ThreadCycles::open();
        let _ = crate::cycle::queue::take_queue_work();
        let _ = testing::take_withheld_by_segment();
        let _ = testing::take_this_threads_standings();
        let record = unsafe { &*mutator_record::this_thread_record() };
        Self {
            turnovers_from: record.turnovers(),
            record,
            cpu_from,
            switches_from,
            faults_from,
            cycles,
            from: Instant::now(),
        }
    }

    /// The loop's figures: its wall, CPU, cycles and instructions, and what
    /// the thread withheld and answered in it.
    fn read_the_loop(&self, reading: &mut MutatorReading) {
        reading.wall = self.from.elapsed();
        reading.withheld_by_segment = testing::take_withheld_by_segment();
        reading.standings = testing::take_this_threads_standings();
        reading.withheld_by_an_entry_at_the_end = crate::cycle::queue::withheld_by_an_entry();
        reading.cpu_in_the_loop = testing::thread_cpu_time() - self.cpu_from;
        if let Some(cycles) = &self.cycles {
            (
                reading.cycles_in_the_loop,
                reading.instructions_in_the_loop,
                reading.counter_share,
            ) = cycles.read();
        }
    }

    /// The thread's user-mode instructions since the begin, zero where the
    /// kernel refused the counters.
    fn instructions(&self) -> u64 {
        self.cycles.as_ref().map_or(0, |cycles| cycles.read().1)
    }

    /// The figures over the loop and the drain together.
    fn read_the_loop_and_the_drain(&self, reading: &mut MutatorReading) {
        if let Some(cycles) = &self.cycles {
            let share;
            (_, reading.instructions_with_the_drain, share) = cycles.read();
            reading.counter_share = reading.counter_share.min(share);
        }

        reading.cpu = testing::thread_cpu_time() - self.cpu_from;
        let switches = testing::thread_context_switches();
        reading.switches = (
            switches.0 - self.switches_from.0,
            switches.1 - self.switches_from.1,
        );
        reading.minor_faults = testing::thread_minor_faults() - self.faults_from;
        reading.records_read = crate::cycle::queue::take_queue_work().records_read;
        reading.turnovers = self.record.turnovers() - self.turnovers_from;
    }
}

/// One cell as the environment names it.
struct Cell {
    placement: String,
    load: Option<String>,
    /// The CPU of each mutator, `None` for an unpinned one.
    mutators: Vec<Option<usize>>,
    collector_cpus: Vec<usize>,
    cap: usize,
    run_for: Duration,
}

impl Cell {
    fn from_env() -> Self {
        let mutator_cpus = cpus("LL_RIG_MUTATOR_CPUS");
        Self {
            placement: std::env::var("LL_RIG_PLACEMENT").unwrap_or_else(|_| "unpinned".into()),
            load: std::env::var("LL_RIG_LOAD").ok(),
            mutators: match mutator_cpus.as_slice() {
                [] => vec![None; SMOKE_MUTATORS],
                cpus => cpus.iter().copied().map(Some).collect(),
            },
            collector_cpus: cpus("LL_RIG_COLLECTOR_CPUS"),
            cap: std::env::var("LL_RIG_CAP").map_or(DEFAULT_COLLECTOR_CAP, |cap| {
                cap.parse().expect("LL_RIG_CAP is a count")
            }),
            run_for: std::env::var("LL_RIG_SECONDS").map_or(SMOKE_RUN, |seconds| {
                Duration::from_secs_f64(seconds.parse().expect("LL_RIG_SECONDS is a number"))
            }),
        }
    }
}

/// Block the calling mutator for `wait` and make no poll, as a server worker
/// blocked in `accept` or on a database read makes none: its byte is not
/// read, so a request stands and a `POSTED` release waits until the thread
/// polls or frees again, and the returns it withholds under a foreign trace
/// stay withheld through the sleep. The web loads' wait is its second
/// caller.
fn sleep_without_poll(wait: Duration) {
    in_a_stretch(Stretch::Wait, || std::thread::sleep(wait));
}

/// What a stretch of the rig's own stands for, named as its section.
#[derive(Clone, Copy)]
enum Stretch {
    /// The draw of the next plan: the harness's own work.
    Draw,
    /// A wait slept without a poll: a worker's blocking call.
    Wait,
}

/// Names the rig's section `Other` when dropped: what follows a stretch is
/// no part of it.
struct LeaveTheSection;

impl Drop for LeaveTheSection {
    fn drop(&mut self) {
        testing::enter_the_rig_section(testing::RigSection::Other);
    }
}

/// Run `work`, which writes no count, slot or tag of the runtime's heap, in
/// the section `stretch` names.
fn in_a_stretch<R>(stretch: Stretch, work: impl FnOnce() -> R) -> R {
    testing::enter_the_rig_section(match stretch {
        Stretch::Draw => testing::RigSection::Draw,
        Stretch::Wait => testing::RigSection::Wait,
    });
    let _section = LeaveTheSection;
    work()
}

/// Turns the timed spin makes between two readings of its deadline.
const SPIN_CHUNK: u64 = 1_000;

/// Spin until `deadline`, in chunks of [`SPIN_CHUNK`] turns of a
/// `black_box` addition with the clock read before each: the synthetic CPU
/// of a web request's phase. Answers the
/// turns spun, zero where the deadline had passed. Not inlined, so that each
/// call costs what [`SpinCost`] fitted.
#[inline(never)]
fn spin_until(deadline: Instant) -> u64 {
    let (mut turns, mut sum) = (0u64, 0u64);
    while Instant::now() < deadline {
        for turn in 0..SPIN_CHUNK {
            sum = std::hint::black_box(sum.wrapping_add(turn));
        }
        turns += SPIN_CHUNK;
    }

    std::hint::black_box(sum);
    turns
}

/// What [`spin_until`] costs in user-mode instructions: `per_call` for a call
/// and `per_turn` for a turn, the clock's reading between chunks inside the
/// latter. Fitted on the calling thread from two runs and checked on a third.
#[derive(Clone, Copy, Default)]
struct SpinCost {
    per_call: f64,
    per_turn: f64,
    /// The third run's error against the fit, as a share of its reading.
    error: f64,
}

impl SpinCost {
    /// The share of a reading the fit may miss by on its third run.
    const TOLERANCE: f64 = 0.001;

    /// Fit on the calling thread: a thousand calls whose deadline has passed
    /// give the call, one call of 20 ms the turn, and two hundred calls of
    /// 50 µs, a request's slices, are read against the two. `None` where the
    /// kernel refused the counters.
    fn fit() -> Option<Self> {
        let counters = testing::ThreadCycles::open()?;
        let instructions = || counters.read().1 as f64;
        let calls = 1_000;
        let before = instructions();
        for _ in 0..calls {
            spin_until(Instant::now() - Duration::from_millis(1));
        }
        let per_call = (instructions() - before) / calls as f64;

        let before = instructions();
        let turns = spin_until(Instant::now() + Duration::from_millis(20));
        let per_turn = (instructions() - before - per_call) / turns as f64;

        let (slices, mut turns) = (200, 0);
        let before = instructions();
        for _ in 0..slices {
            turns += spin_until(Instant::now() + Duration::from_micros(50));
        }
        let read = instructions() - before;
        let fitted = per_call * slices as f64 + per_turn * turns as f64;
        let cost = Self {
            per_call,
            per_turn,
            error: (read - fitted).abs() / read,
        };
        assert!(
            cost.error < Self::TOLERANCE,
            "the spin's fit missed its third run by {:.4} %: {read} read, {fitted} fitted",
            cost.error * 100.0
        );
        Some(cost)
    }

    /// The instructions `calls` calls spinning `turns` turns cost.
    fn of(&self, calls: u64, turns: u64) -> u64 {
        (self.per_call * calls as f64 + self.per_turn * turns as f64) as u64
    }
}

/// Arrivals at 0, 1 and 2 ms and one at 100 ms past a 50 ms run, each
/// served for 5 ms: the first finds itself alone, the second finds the third
/// waiting behind it, the third alone, and the fourth is never taken; the
/// later two's walls from arrival exceed their services by the time they
/// waited.
#[test]
fn an_arrival_waits_behind_the_services_before_it() {
    let millis = Duration::from_millis;
    let offsets = [millis(0), millis(1), millis(2), millis(100)];
    let mut queue = ArrivalQueue::new(offsets.into_iter(), millis(50));
    let from = Instant::now();
    let mut served = Vec::new();
    while let Some((arrival, queued)) = queue.take(from) {
        let start = from.elapsed();
        std::thread::sleep(millis(5));
        served.push((arrival, queued, start - arrival));
    }

    assert_eq!(
        served
            .iter()
            .map(|&(arrival, queued, _)| (arrival, queued))
            .collect::<Vec<_>>(),
        [(millis(0), 1), (millis(1), 2), (millis(2), 1)]
    );
    assert!(served[1].2 >= millis(4), "{:?} waited", served[1].2);
    assert!(served[2].2 >= millis(8), "{:?} waited", served[2].2);
}

/// The draw of a plan stands in a section of its own, left when the draw
/// returns, from both of the loop's draws: a request run whole and the
/// arrivals' draw ahead of its service. Red with either draw out of its
/// section, or the section left standing.
#[test]
fn a_drawn_plan_stands_in_a_section_of_its_own() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let web = Web {
        variant: Variant::Heap,
        values: 200,
    };
    let mut web_loop =
        unsafe { WebLoop::set_up(web, 0, WebClasses::new("DrawnPlan"), 0, &mut arena) };
    let drawn = in_a_stretch(Stretch::Draw, testing::this_threads_rig_section);
    assert_eq!(
        drawn,
        testing::RigSection::Draw as usize,
        "the draw's section"
    );
    let _ = web_loop.draw_a_plan(None);
    let other = testing::RigSection::Other as usize;
    assert_eq!(testing::this_threads_rig_section(), other, "and left");
    web_loop.run_a_request();
    assert_eq!(testing::this_threads_rig_section(), other);
    let _ = web_loop.tear_down(&mut MutatorReading::default());
}

/// A phase spins its drawn CPU with the work inside it, and the polls' walls
/// add to it: 2 ms of CPU with no work runs at least 2 ms and short of twice
/// it, a bound a loaded box's preemption stays under; with 20 µs of work a
/// slice, the same; with a poll
/// of 100 µs a slice, at least the CPU and the polls' walls together; and
/// work of twice a slice outruns its deadline.
#[test]
fn a_phase_spins_its_cpu_and_its_polls_add_to_it() {
    let cpu = Duration::from_millis(2);
    let busy = |length: Duration| {
        let until = Instant::now() + length;
        while Instant::now() < until {
            std::hint::spin_loop();
        }
    };
    let wall_of = |step: &mut dyn FnMut(f64) -> Duration| {
        let started = Instant::now();
        let spun = run_a_phase(cpu, step);
        (started.elapsed(), spun)
    };

    let (idle, spun) = wall_of(&mut |_| Duration::ZERO);
    assert!(idle >= cpu && idle < cpu * 2, "{idle:?}");
    assert_eq!(spun.calls, 40);
    assert!(!spun.overran);

    let (working, _) = wall_of(&mut |_| {
        busy(Duration::from_micros(20));
        Duration::ZERO
    });
    assert!(
        working >= cpu && working < cpu * 2,
        "the work stays inside the CPU: {working:?}"
    );

    let mut polled = Duration::ZERO;
    let (polling, _) = wall_of(&mut |_| {
        let at = Instant::now();
        std::thread::sleep(Duration::from_micros(100));
        let wall = at.elapsed();
        polled += wall;
        wall
    });
    assert!(
        polling >= cpu + polled,
        "the polls add to the CPU: {polling:?} against {:?}",
        cpu + polled
    );

    let (_, spun) = wall_of(&mut |_| {
        busy(SLICE * 2);
        Duration::ZERO
    });
    assert!(spun.overran, "work past the deadline outran it");
}

/// The collector threads' instructions are read live from another thread
/// and folded in at a life's end: a life on slot 7, which no case's
/// collector is born into, spins a known stretch, and the driver's live
/// readings around it grow by what the life's own counter read, within 2 %,
/// and its end adds no more than its last few instructions. Ignored with the
/// rig: it reads the PMU.
#[test]
#[ignore = "reads the PMU, as the rig's cells do"]
fn the_collectors_instructions_are_read_live_and_folded_at_the_end() {
    const SLOT: usize = 7;
    let (to_driver, from_life) = std::sync::mpsc::channel::<u64>();
    let (to_life, from_driver) = std::sync::mpsc::channel::<()>();
    let life = std::thread::spawn(move || {
        testing::note_collector_born(SLOT);
        let own = testing::ThreadCycles::open().expect("the kernel opened the counters");
        to_driver.send(0).unwrap();
        from_driver.recv().unwrap();
        let before = own.read().1;
        spin_until(Instant::now() + Duration::from_millis(20));
        to_driver.send(own.read().1 - before).unwrap();
        from_driver.recv().unwrap();
        testing::note_collector_life_end(SLOT);
    });

    from_life.recv().unwrap();
    let born = testing::collector_instructions_to_now();
    to_life.send(()).unwrap();
    let spun = from_life.recv().unwrap();
    let live = testing::collector_instructions_to_now();
    to_life.send(()).unwrap();
    life.join().unwrap();
    let ended = testing::collector_instructions_to_now();

    let read = (live - born) as f64;
    assert!(
        (read / spun as f64 - 1.0).abs() < 0.02,
        "the live reading grew by {read} over a spin of {spun}"
    );
    assert!(
        ended >= live && ended - live < 100_000,
        "the end folded {} more",
        ended - live
    );
}

/// Another process spinning a core for a second reads as about one core of
/// other CPU more than the second before it: the reading's increment is
/// known, and its level is whatever else the guest runs — under WSL2 the
/// other distributions' processes too, which no `ps` here lists. Ignored with
/// the rig: it reads the box.
#[test]
#[cfg(not(miri))]
#[ignore = "reads the box's load, as the rig's cells do"]
fn another_process_spinning_reads_as_one_core_of_other_cpu() {
    let quiet = WindowEdge::now();
    std::thread::sleep(Duration::from_secs(1));
    let spinning_from = WindowEdge::now();
    let mut child = std::process::Command::new("timeout")
        .args(["1", "sh", "-c", "while :; do :; done"])
        .spawn()
        .expect("the shell started");
    child.wait().expect("the shell ended");
    let spinning_to = WindowEdge::now();
    let (idle, busy) = (
        spinning_from.other_cores_since(&quiet),
        spinning_to.other_cores_since(&spinning_from),
    );
    println!("other CPU: {idle:.3} cores before, {busy:.3} while a process spun");
    assert!(
        (busy - idle - 1.0).abs() < 0.3,
        "a process spinning a core added {} cores",
        busy - idle
    );
}

/// The timed spin's fit holds on a run it was not fitted on, within
/// [`SpinCost::TOLERANCE`], at a cost a turn of a few instructions. Ignored
/// with the rig: it reads the PMU.
#[test]
#[ignore = "reads the PMU, as the rig's cells do"]
fn the_timed_spin_is_fitted_by_its_calls_and_turns() {
    let cost = SpinCost::fit().expect("the kernel opened the counters");
    println!(
        "spin: {:.1} instructions a call, {:.4} a turn, the third run within {:.4} %",
        cost.per_call,
        cost.per_turn,
        cost.error * 100.0
    );
    assert!(
        (1.0..20.0).contains(&cost.per_turn),
        "{} instructions a turn",
        cost.per_turn
    );
}

/// Write every batch of the loop, one line each, to the file
/// `LL_RIG_BATCH_DUMP` names, and nothing when it is unset: the epoch's
/// saw-tooth is read per batch, not from a cell's sums.
fn dump_the_batches(batches: &[testing::TracedBatch], start: Instant) {
    dump_the_batches_to(batches, start, "");
}

/// As [`dump_the_batches`], to the named path with `suffix` appended: the
/// drain's batches go beside the loop's, so that a batch the loop left
/// running is read when it ends.
fn dump_the_batches_to(batches: &[testing::TracedBatch], start: Instant, suffix: &str) {
    use std::fmt::Write as _;

    let Ok(path) = std::env::var("LL_RIG_BATCH_DUMP") else {
        return;
    };
    let path = format!("{path}{suffix}");
    let mut mutators: Vec<usize> = Vec::new();
    let mut text = String::from(
        "mutator,ended_ms,turnovers,roots,traced,complete,ending,positions,edges_pruned,rows_met,wall_us\n",
    );
    for batch in batches {
        let mutator = mutators
            .iter()
            .position(|&seen| seen == batch.mutator)
            .unwrap_or_else(|| {
                mutators.push(batch.mutator);
                mutators.len() - 1
            });
        let ended = batch.ended.saturating_duration_since(start).as_secs_f64() * 1e3;
        let _ = writeln!(
            text,
            "{mutator},{ended:.3},{},{},{},{},{},{},{},{},{}",
            batch.turnovers,
            batch.roots,
            u8::from(batch.traced),
            u8::from(batch.complete),
            batch.ending,
            batch.positions,
            batch.edges_pruned,
            batch.rows_met,
            batch.wall.as_micros(),
        );
    }
    std::fs::write(path, text).expect("the batch dump is written");
}

/// The process's resident set in bytes, read from `/proc/self/statm` at a
/// 4-KiB page, or zero where it cannot be read.
fn resident_bytes() -> usize {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|statm| statm.split_whitespace().nth(1)?.parse::<usize>().ok())
        .map_or(0, |pages| pages * 4096)
}

/// Whether the switch `variable` is on: unset is off and `1` is on; any other
/// value is refused, so that a misspelt switch does not run the other arm.
fn switch_from_env(variable: &str) -> bool {
    match std::env::var(variable) {
        Err(_) => false,
        Ok(value) if value == "1" => true,
        Ok(value) => panic!("{variable} is unset or 1, not {value:?}"),
    }
}

/// Seconds `variable` names, zero when it is unset.
fn seconds_from_env(variable: &str) -> Duration {
    std::env::var(variable).map_or(Duration::ZERO, |seconds| {
        Duration::from_secs_f64(seconds.parse().expect("a count of seconds"))
    })
}

/// Milliseconds `variable` names, zero when it is unset.
/// The count `variable` names, read once a process; zero when unset.
fn count_from_env(variable: &'static str) -> usize {
    static COUNTS: std::sync::Mutex<Vec<(&str, usize)>> = std::sync::Mutex::new(Vec::new());
    let mut counts = COUNTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(&(_, count)) = counts.iter().find(|(name, _)| *name == variable) {
        return count;
    }
    let count = std::env::var(variable).map_or(0, |count| {
        count
            .parse()
            .unwrap_or_else(|_| panic!("{variable} is a count"))
    });
    counts.push((variable, count));
    count
}

fn millis_from_env(variable: &str) -> Duration {
    std::env::var(variable).map_or(Duration::ZERO, |millis| {
        Duration::from_secs_f64(millis.parse::<f64>().expect("a count of milliseconds") / 1000.0)
    })
}

/// The comma-separated CPU numbers of `variable`, empty when it is unset or
/// empty.
fn cpus(variable: &str) -> Vec<usize> {
    match std::env::var(variable) {
        Ok(list) if !list.is_empty() => list
            .split(',')
            .map(|cpu| {
                cpu.trim()
                    .parse()
                    .unwrap_or_else(|_| panic!("{variable} lists CPU numbers: {list:?}"))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// What one cell read: every mutator's reading, and what the collectors and
/// the tokens did while the mutators ran.
struct CellReading {
    mutators: Vec<MutatorReading>,
    collectors_born: usize,
    collectors_pinned: usize,
    collectors: testing::CollectorLives,
    /// The collectors' CPU at the instant the mutators passed the start
    /// barrier and at the instant they were stopped: its difference is the
    /// loop's, apart from a web load's setup.
    collector_cpu_at_the_start: Duration,
    collector_cpu_at_the_stop: Duration,
    /// What the rounds did while the cell ran: batches and the roots they
    /// carried, grants, and the rounds themselves.
    outcomes: testing::Outcomes,
    rounds: usize,
    /// The mutators' collections over P.
    verdict_collections: testing::VerdictCollections,
    /// The mutators' dispositions of P with no trace window.
    disposals: testing::VerdictCollections,
    /// What the collector's stamps and the lane cost, in every build.
    scheme: testing::SchemeFigures,
    token_waits: testing::TokenWaits,
    /// The requests collectors withdrew from the mutators' start to the
    /// stop, and how long they had stood.
    withdrawn: testing::StandingTimes,
    /// The collector's time in each segment of its batches.
    segment_times: testing::SegmentTimes,
    /// Grants recalled by a stack's mark, and by a take.
    recalls: (usize, usize),
    /// Recalls raised to the stop level by a stack's second mark.
    second_mark_recalls: usize,
    written_back: usize,
    /// The GC ledger's high-water marks over the process so far: blocks
    /// reserved and bytes taken into use, both in bytes.
    ledger_peak: (usize, usize),
    /// A web load's figures; empty for a ring load.
    web: WebCell,
    /// What the journal counted over the loop and over the drain, zero in a
    /// build without `debug-journal` (`JOURNAL_COLUMNS`).
    journal: (crate::journal::Counts, crate::journal::Counts),
    /// The timed spin's cost, fitted on the driver's thread before the
    /// mutators start; `None` where the kernel refused the counters.
    spin_cost: Option<SpinCost>,
    /// The driver's readings at the window's start, at its end and after the
    /// drains (`WindowEdge`).
    edges: [WindowEdge; 3],
}

/// What the driver reads at an edge of the window: the collector threads'
/// instructions so far, the box's busy time and the process's own from
/// `/proc`, the members commits reclaimed and the turnovers by cause.
#[derive(Clone, Copy)]
struct WindowEdge {
    at: Instant,
    collector_instructions: u64,
    /// Jiffies: the box's busy ones over every CPU, and the process's own.
    box_busy: u64,
    own: u64,
    members_reclaimed: usize,
    turnovers: [usize; 4],
}

impl WindowEdge {
    fn now() -> Self {
        let stat = std::fs::read_to_string("/proc/stat").unwrap_or_default();
        // user, nice, system, idle, iowait, irq, softirq, steal: busy is all
        // but idle and iowait.
        let fields: Vec<u64> = stat
            .lines()
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .skip(1)
            .filter_map(|field| field.parse().ok())
            .collect();
        let box_busy = [0, 1, 2, 5, 6, 7]
            .iter()
            .filter_map(|&index| fields.get(index))
            .sum();
        // `/proc/self/stat`'s fields 14 and 15, utime and stime, counted after
        // the command's closing parenthesis, which may itself hold spaces.
        let own_stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
        let own = own_stat.rsplit_once(')').map_or(0, |(_, rest)| {
            rest.split_whitespace()
                .skip(11)
                .take(2)
                .filter_map(|field| field.parse::<u64>().ok())
                .sum()
        });
        Self {
            at: Instant::now(),
            collector_instructions: testing::collector_instructions_to_now(),
            box_busy,
            own,
            members_reclaimed: testing::members_reclaimed(),
            turnovers: testing::turnovers_by_cause(),
        }
    }

    /// The other processes' CPU in the guest between `earlier` and this
    /// edge, in cores: the box's busy jiffies less the process's own, at 100
    /// a second, over the wall. Under WSL2 the host's load is charged to the
    /// guest's tasks and not read here.
    fn other_cores_since(&self, earlier: &WindowEdge) -> f64 {
        const JIFFIES_A_SECOND: f64 = 100.0;
        let other = (self.box_busy - earlier.box_busy) as f64 - (self.own - earlier.own) as f64;
        other / JIFFIES_A_SECOND / (self.at - earlier.at).as_secs_f64()
    }
}

/// A cell whose guest held more than this share of a core busy with other
/// processes over the window is void (`dev/design/the-web-loads.md`, the protocol).
const VOID_CORES: f64 = 0.5;

/// The collector's stamping's columns, in the order
/// `collector_stamps::testing::Stamping` reads them.
const STAMPING_COLUMNS: [&str; 5] = [
    "web_stamping_walks",
    "web_stamping_stamps_mean",
    "web_stamping_stamps_most",
    "web_stamping_us_mean",
    "web_stamping_longest_us",
];

/// The withheld returns' columns by stack, deaths, chunks and blocks, in the
/// order `testing::WithheldReadings` reads each.
const WITHHELD_COLUMNS: [[&str; 6]; 3] = [
    [
        "web_withheld_crossings_deaths",
        "web_withheld_at_a_crossing_mean_deaths",
        "web_withheld_at_a_crossing_most_deaths",
        "web_withheld_releases_deaths",
        "web_withheld_at_a_release_mean_deaths",
        "web_withheld_at_a_release_most_deaths",
    ],
    [
        "web_withheld_crossings_chunks",
        "web_withheld_at_a_crossing_mean_chunks",
        "web_withheld_at_a_crossing_most_chunks",
        "web_withheld_releases_chunks",
        "web_withheld_at_a_release_mean_chunks",
        "web_withheld_at_a_release_most_chunks",
    ],
    [
        "web_withheld_crossings_blocks",
        "web_withheld_at_a_crossing_mean_blocks",
        "web_withheld_at_a_crossing_most_blocks",
        "web_withheld_releases_blocks",
        "web_withheld_at_a_release_mean_blocks",
        "web_withheld_at_a_release_most_blocks",
    ],
];

/// One column of the journal's counts: `kind` under `code`, or under every
/// code where `code` is `None`, read as its records or as the sum of their
/// `b`; named once per window.
struct JournalColumn {
    names: [&'static str; 2],
    kind: u32,
    code: Option<u64>,
    sum_of_b: bool,
}

macro_rules! journal_column {
    ($name:literal, $kind:ident, every) => {
        journal_column!(@ $name, $kind, None, false)
    };
    ($name:literal, $kind:ident, every, sum) => {
        journal_column!(@ $name, $kind, None, true)
    };
    ($name:literal, $kind:ident, $code:ident) => {
        journal_column!(@ $name, $kind, Some(crate::journal::kinds::$code), false)
    };
    ($name:literal, $kind:ident, $code:ident, sum) => {
        journal_column!(@ $name, $kind, Some(crate::journal::kinds::$code), true)
    };
    (@ $name:literal, $kind:ident, $code:expr, $sum:expr) => {
        JournalColumn {
            names: [concat!("j_loop_", $name), concat!("j_drain_", $name)],
            kind: crate::journal::kinds::$kind,
            code: $code,
            sum_of_b: $sum,
        }
    };
}

/// The journal's columns of the rig's line, in
/// the kinds' order: what the collector and the mutators' dispositions did,
/// counted where a cell's records outrun a ring.
const JOURNAL_COLUMNS: &[JournalColumn] = &[
    journal_column!("births", KIND_ENTITY_BIRTH, every),
    journal_column!("deaths", KIND_ENTITY_DEATH, every),
    journal_column!("registered_now", KIND_CANDIDATE_REGISTERED, REGISTERED_NOW),
    journal_column!(
        "registered_already",
        KIND_CANDIDATE_REGISTERED,
        REGISTERED_ALREADY
    ),
    journal_column!(
        "batches_at_the_threshold",
        KIND_BATCH_START,
        BATCH_AT_THE_THRESHOLD
    ),
    journal_column!(
        "batches_of_a_standing_ring",
        KIND_BATCH_START,
        BATCH_OF_A_STANDING_RING
    ),
    journal_column!("batch_roots", KIND_BATCH_START, every, sum),
    journal_column!("batch_end_complete", KIND_BATCH_END, BATCH_END_COMPLETE),
    journal_column!(
        "batch_end_recalled_in_the_pass",
        KIND_BATCH_END,
        BATCH_END_RECALLED_IN_THE_PASS
    ),
    journal_column!(
        "batch_end_recalled_in_the_trace",
        KIND_BATCH_END,
        BATCH_END_RECALLED_IN_THE_TRACE
    ),
    journal_column!(
        "batch_end_recalled_after_the_trace",
        KIND_BATCH_END,
        BATCH_END_RECALLED_AFTER_THE_TRACE
    ),
    journal_column!(
        "batch_end_refused_in_the_trace",
        KIND_BATCH_END,
        BATCH_END_REFUSED_IN_THE_TRACE
    ),
    journal_column!("batch_end_wound_down", KIND_BATCH_END, BATCH_END_WOUND_DOWN),
    journal_column!(
        "batch_end_wound_down_then_cut",
        KIND_BATCH_END,
        BATCH_END_WOUND_DOWN_THEN_CUT
    ),
    journal_column!("batch_regions_ended", KIND_BATCH_END, every, sum),
    journal_column!("verdict_proposed", KIND_ROOT_VERDICT, VERDICT_PROPOSED),
    journal_column!("verdict_read_live", KIND_ROOT_VERDICT, VERDICT_READ_LIVE),
    journal_column!("verdict_zero_count", KIND_ROOT_VERDICT, VERDICT_ZERO_COUNT),
    journal_column!("verdict_unwalked", KIND_ROOT_VERDICT, VERDICT_UNWALKED),
    journal_column!("deferred_from_r", KIND_ROOT_DEFERRED, DEFERRED_FROM_R),
    journal_column!("deferred_from_p", KIND_ROOT_DEFERRED, DEFERRED_FROM_P),
    journal_column!(
        "written_back_proposed",
        KIND_ROOT_WRITTEN_BACK,
        VERDICT_PROPOSED
    ),
    journal_column!(
        "written_back_read_live",
        KIND_ROOT_WRITTEN_BACK,
        VERDICT_READ_LIVE
    ),
    journal_column!(
        "written_back_zero_count",
        KIND_ROOT_WRITTEN_BACK,
        VERDICT_ZERO_COUNT
    ),
    journal_column!(
        "written_back_unwalked",
        KIND_ROOT_WRITTEN_BACK,
        VERDICT_UNWALKED
    ),
    journal_column!(
        "reoffered_lane_due",
        KIND_REOFFERED,
        REOFFERED_LANE_DUE,
        sum
    ),
    journal_column!(
        "reoffered_every_lane",
        KIND_REOFFERED,
        REOFFERED_EVERY_LANE,
        sum
    ),
    journal_column!("turnover_by_proofs", KIND_TURNOVER, TURNOVER_BY_PROOFS),
    journal_column!("turnover_by_x", KIND_TURNOVER, TURNOVER_BY_X),
    journal_column!("turnover_new_life", KIND_TURNOVER, TURNOVER_NEW_LIFE),
    journal_column!("turnover_by_hand", KIND_TURNOVER, TURNOVER_BY_HAND),
    journal_column!("components_reclaimed", KIND_COMPONENT_RECLAIMED, every),
    journal_column!("members_reclaimed", KIND_COMPONENT_RECLAIMED, every, sum),
    journal_column!("slots_from_p", KIND_WITHHELD_SLOT_RETURNED, SLOT_FROM_P),
    journal_column!("slots_from_r", KIND_WITHHELD_SLOT_RETURNED, SLOT_FROM_R),
    journal_column!(
        "slots_from_r_front_run",
        KIND_WITHHELD_SLOT_RETURNED,
        SLOT_FROM_R_FRONT_RUN
    ),
    journal_column!(
        "slots_from_overflow",
        KIND_WITHHELD_SLOT_RETURNED,
        SLOT_FROM_OVERFLOW
    ),
    journal_column!(
        "slots_from_a_deferred_lane",
        KIND_WITHHELD_SLOT_RETURNED,
        SLOT_FROM_A_DEFERRED_LANE
    ),
    journal_column!(
        "grant_recalled_before_the_batch",
        KIND_GRANT_WITHOUT_BATCH,
        GRANT_RECALLED_BEFORE_THE_BATCH
    ),
    journal_column!(
        "grant_workspace_refused",
        KIND_GRANT_WITHOUT_BATCH,
        GRANT_WORKSPACE_REFUSED
    ),
    journal_column!(
        "grant_nothing_taken",
        KIND_GRANT_WITHOUT_BATCH,
        GRANT_NOTHING_TAKEN
    ),
];

/// What a web load's cell read beside the ring loads' figures: each
/// mutator's [`WebReading`]; the collections while the mutators set up,
/// which the cell's figures of its rounds leave out, as they leave out the
/// teardown's; the retained blocks at the loop's end and after the drains;
/// the frees from another thread from the start to the drains' end, asserted
/// zero; and with `LL_RIG_TRACED_BATCHES` the batches traced over the loop
/// and those whose rows met reach the core's and the values' objects, a walk
/// through the whole state.
#[derive(Default)]
struct WebCell {
    mutators: Vec<WebReading>,
    setup: RoundFigures,
    /// The warm-up's collections, apart from the window's, where the
    /// arrivals run.
    warm_up: RoundFigures,
    retained_at_the_stop: usize,
    retained_after_the_drains: usize,
    frees_from_another_thread: usize,
    batches_traced: usize,
    batches_through_the_state: usize,
    /// The stubbed trace's wall (`LL_RIG_STUB_TRACE_MS`), zero for the real
    /// trace.
    stub_trace: Duration,
    /// Each mutator's batches that ended inside the window, over the
    /// window's wall, a mutator with none reading zero.
    batches_a_second: Vec<f64>,
    /// The roots those batches took, over the window's wall.
    roots_a_second: f64,
    /// The window's wall, from its start to the loops' end.
    window: Duration,
    /// The widest completed part of the batches traced over the loop and the
    /// drain.
    widest_part: testing::PartReading,
    /// The window's batches recalled, and their positions in all and at the
    /// most; the same of its completed batches.
    recalled: (usize, usize, usize),
    completed: (usize, usize, usize),
    /// The returns the mutators withheld over the window, at the crossings
    /// and at the releases.
    withheld: testing::WithheldReadings,
    /// The stamps the collectors wrote over the window, under their grants.
    stamping: crate::cycle::collector_stamps::testing::Stamping,
    /// The sets the collectors posted over the window — their count, members
    /// in all and at the most — and the sets a return dropped.
    sets: (usize, usize, usize),
    sets_dropped: usize,
    /// The collectors' resets over the window, in their two parts.
    resets: crate::cycle::arena::ResetTiming,
}

impl WebCell {
    /// Read each of `mutators` mutators' batches a second, and the roots a
    /// second of them all, from the `batches` that ended inside `window`.
    fn read_the_cadence(
        &mut self,
        batches: &[testing::TracedBatch],
        window: std::ops::Range<Instant>,
        mutators: usize,
    ) {
        let seconds = (window.end - window.start)
            .as_secs_f64()
            .max(f64::MIN_POSITIVE);
        let inside: Vec<_> = batches
            .iter()
            .filter(|batch| window.contains(&batch.ended))
            .collect();
        let tally = |ended: &dyn Fn(u64) -> bool| {
            inside.iter().filter(|batch| ended(batch.ending)).fold(
                (0, 0, 0),
                |(count, all, most), batch| {
                    (count + 1, all + batch.positions, most.max(batch.positions))
                },
            )
        };
        self.recalled = tally(&|ending| {
            (crate::journal::kinds::BATCH_END_RECALLED_IN_THE_PASS
                ..=crate::journal::kinds::BATCH_END_RECALLED_AFTER_THE_TRACE)
                .contains(&ending)
                || ending == crate::journal::kinds::BATCH_END_WOUND_DOWN
                || ending == crate::journal::kinds::BATCH_END_WOUND_DOWN_THEN_CUT
        });
        self.completed = tally(&|ending| ending == crate::journal::kinds::BATCH_END_COMPLETE);
        let mut by_mutator = std::collections::BTreeMap::<usize, usize>::new();
        for batch in &inside {
            *by_mutator.entry(batch.mutator).or_default() += 1;
        }
        self.batches_a_second = by_mutator
            .values()
            .map(|&count| count as f64 / seconds)
            .collect();
        self.batches_a_second
            .resize(mutators.max(by_mutator.len()), 0.0);
        self.roots_a_second =
            inside.iter().map(|batch| batch.roots).sum::<usize>() as f64 / seconds;
    }
}

impl CellReading {
    fn sum(&self, field: impl Fn(&MutatorReading) -> usize) -> usize {
        self.mutators.iter().map(field).sum()
    }

    /// Operations a second: every mutator's iterations over its own loop's
    /// wall, summed.
    fn operations_a_second(&self) -> f64 {
        self.mutators
            .iter()
            .map(|reading| reading.iterations as f64 / reading.wall.as_secs_f64())
            .sum()
    }

    /// The mutators' CPU over their loops, over their iterations: the drain's
    /// polls are not an operation.
    fn cpu_an_operation(&self) -> Duration {
        let cpu: Duration = self
            .mutators
            .iter()
            .map(|reading| reading.cpu_in_the_loop)
            .sum();
        cpu / self.sum(|reading| reading.iterations).max(1) as u32
    }

    fn latencies(&self) -> Latencies {
        let mut all = Latencies::default();
        for reading in &self.mutators {
            all.add(&reading.latencies);
        }

        all
    }

    /// The mean time a garbage member waited for its free while the loops
    /// ran, over every mutator's garbage.
    fn time_to_free(&self) -> Duration {
        let outstanding: u128 = self
            .mutators
            .iter()
            .map(|reading| reading.outstanding_time)
            .sum();
        let built = self.sum(|reading| reading.garbage_members).max(1) as u128;
        Duration::from_nanos((outstanding / built) as u64)
    }

    /// The garbage the loops left standing at their end, in bytes: built and
    /// not freed by a poll. Zero on a web load, which counts no garbage
    /// members and reads its garbage in bytes instead.
    fn standing_bytes(&self) -> usize {
        self.sum(|reading| {
            reading
                .garbage_members
                .saturating_sub(reading.freed_by_polls)
        }) * MEMBER_CLASS_BYTES
    }

    /// The line's fields by name, in the line's order: the header is their
    /// names, the line their values.
    fn fields(&self, cell: &Cell, load: Load) -> Vec<(&'static str, String)> {
        let listed = |cpus: Vec<String>| cpus.join(" ");
        let latencies = self.latencies();
        let standings = self
            .mutators
            .iter()
            .fold(testing::MutatorStandings::default(), |sum, reading| {
                sum.merged(reading.standings)
            });
        let switches = |pick: fn(&(u64, u64)) -> u64| {
            self.mutators
                .iter()
                .map(|reading| pick(&reading.switches))
                .sum::<u64>()
        };
        vec![
            ("placement", cell.placement.clone()),
            ("load", load.name.into()),
            ("mutators", self.mutators.len().to_string()),
            (
                "mutator_cpus",
                listed(
                    cell.mutators
                        .iter()
                        .map(|cpu| cpu.map_or("-".into(), |cpu| cpu.to_string()))
                        .collect(),
                ),
            ),
            (
                "collector_cpus",
                listed(cell.collector_cpus.iter().map(usize::to_string).collect()),
            ),
            ("cap", cell.cap.to_string()),
            ("seconds", cell.run_for.as_secs_f64().to_string()),
            (
                "iterations",
                self.sum(|reading| reading.iterations).to_string(),
            ),
            (
                "least_iterations",
                self.mutators
                    .iter()
                    .map(|reading| reading.iterations)
                    .min()
                    .unwrap_or(0)
                    .to_string(),
            ),
            (
                "operations_a_second",
                format!("{:.0}", self.operations_a_second()),
            ),
            (
                "cpu_ns_an_operation",
                self.cpu_an_operation().as_nanos().to_string(),
            ),
            ("latency_p50_ns", latencies.quantile(0.5).to_string()),
            ("latency_p99_ns", latencies.quantile(0.99).to_string()),
            ("latency_p999_ns", latencies.quantile(0.999).to_string()),
            (
                "minor_faults",
                self.mutators
                    .iter()
                    .map(|reading| reading.minor_faults)
                    .sum::<u64>()
                    .to_string(),
            ),
            (
                "queue_records_read",
                self.sum(|reading| reading.records_read).to_string(),
            ),
            (
                "ring_peak",
                self.mutators
                    .iter()
                    .map(|reading| reading.ring_peak)
                    .max()
                    .unwrap_or(0)
                    .to_string(),
            ),
            ("voluntary_switches", switches(|pair| pair.0).to_string()),
            ("involuntary_switches", switches(|pair| pair.1).to_string()),
            (
                "garbage_members",
                self.sum(|reading| reading.garbage_members).to_string(),
            ),
            (
                "freed_by_polls",
                self.sum(|reading| reading.freed_by_polls).to_string(),
            ),
            (
                "time_to_free_us",
                self.time_to_free().as_micros().to_string(),
            ),
            ("standing_bytes", self.standing_bytes().to_string()),
            (
                "freed_at_the_end",
                self.sum(|reading| reading.freed_at_the_end).to_string(),
            ),
            (
                "withheld_peak",
                self.mutators
                    .iter()
                    .map(|reading| reading.withheld_peak)
                    .max()
                    .unwrap_or(0)
                    .to_string(),
            ),
            ("ledger_peak_bytes", self.ledger_peak.0.to_string()),
            ("ledger_peak_bytes_in_use", self.ledger_peak.1.to_string()),
            (
                "turnovers",
                self.mutators
                    .iter()
                    .map(|reading| reading.turnovers)
                    .sum::<u64>()
                    .to_string(),
            ),
            ("written_back", self.written_back.to_string()),
            ("recalls_by_the_mark", self.recalls.0.to_string()),
            ("recalls_by_a_take", self.recalls.1.to_string()),
            (
                "recalls_at_the_second_mark",
                self.second_mark_recalls.to_string(),
            ),
            ("token_waits", self.token_waits.waits.to_string()),
            (
                "token_wait_us",
                self.token_waits.total.as_micros().to_string(),
            ),
            (
                "token_wait_longest_us",
                self.token_waits.longest.as_micros().to_string(),
            ),
            ("rounds", self.rounds.to_string()),
            ("batches", self.outcomes.batches.to_string()),
            ("roots_batched", self.outcomes.roots_served.to_string()),
            ("grants", self.outcomes.grants.to_string()),
            // Serves that found the mutator `POSTED`: P holding the last
            // batch's verdicts, its disposition not yet run.
            ("serves_posted", self.outcomes.posted.to_string()),
            ("idle_serves", self.outcomes.idle.to_string()),
            ("unanswered_requests", self.outcomes.unanswered.to_string()),
            (
                "verdict_collections",
                self.verdict_collections.collections.to_string(),
            ),
            (
                "verdict_collection_us",
                self.verdict_collections.total.as_micros().to_string(),
            ),
            (
                "verdict_collection_longest_us",
                self.verdict_collections.longest.as_micros().to_string(),
            ),
            // By the kind of set read (`crate::cycle::posted_set::kind`):
            // not tested, proved S, proved whole, a retired kind, past the
            // cap, touched, retired, weakly held, cut, unmarked whole,
            // mark lost, unreadable, none.
            (
                "verdict_collection_longest_by_kind_us",
                self.verdict_collections
                    .longest_by_kind
                    .iter()
                    .map(|wall| wall.as_micros().to_string())
                    .collect::<Vec<_>>()
                    .join(";"),
            ),
            (
                "freed_by_verdict_collections",
                self.verdict_collections.freed.to_string(),
            ),
            (
                "verdict_collection_positions",
                self.verdict_collections.positions.to_string(),
            ),
            (
                "verdict_collection_positions_longest",
                self.verdict_collections.positions_longest.to_string(),
            ),
            (
                "verdict_mark_us",
                self.verdict_collections.phases[0].as_micros().to_string(),
            ),
            (
                "verdict_scan_us",
                self.verdict_collections.phases[1].as_micros().to_string(),
            ),
            (
                "verdict_membership_us",
                self.verdict_collections.phases[2].as_micros().to_string(),
            ),
            (
                "verdict_confirm_us",
                self.verdict_collections.phases[3].as_micros().to_string(),
            ),
            (
                "verdict_destructors_us",
                self.verdict_collections.phases[4].as_micros().to_string(),
            ),
            (
                "verdict_reclaim_us",
                self.verdict_collections.phases[5].as_micros().to_string(),
            ),
            (
                "verdict_drops_us",
                self.verdict_collections.phases[6].as_micros().to_string(),
            ),
            (
                "verdict_longest_mark_us",
                self.verdict_collections.phases_of_the_longest[0]
                    .as_micros()
                    .to_string(),
            ),
            (
                "verdict_longest_scan_us",
                self.verdict_collections.phases_of_the_longest[1]
                    .as_micros()
                    .to_string(),
            ),
            (
                "verdict_longest_membership_us",
                self.verdict_collections.phases_of_the_longest[2]
                    .as_micros()
                    .to_string(),
            ),
            (
                "verdict_longest_confirm_us",
                self.verdict_collections.phases_of_the_longest[3]
                    .as_micros()
                    .to_string(),
            ),
            (
                "verdict_longest_destructors_us",
                self.verdict_collections.phases_of_the_longest[4]
                    .as_micros()
                    .to_string(),
            ),
            (
                "verdict_longest_reclaim_us",
                self.verdict_collections.phases_of_the_longest[5]
                    .as_micros()
                    .to_string(),
            ),
            (
                "verdict_longest_drops_us",
                self.verdict_collections.phases_of_the_longest[6]
                    .as_micros()
                    .to_string(),
            ),
            ("disposals", self.disposals.collections.to_string()),
            ("disposal_us", self.disposals.total.as_micros().to_string()),
            (
                "disposal_longest_us",
                self.disposals.longest.as_micros().to_string(),
            ),
            (
                "stamped_registered",
                self.scheme.stamped_registered.to_string(),
            ),
            ("stamped_other", self.scheme.stamped_other.to_string()),
            (
                "collector_edges_pruned",
                self.scheme.collector_edges_pruned.to_string(),
            ),
            ("roots_reoffered", self.scheme.roots_reoffered.to_string()),
            ("collector_passes", self.scheme.collector_passes.to_string()),
            ("collector_held", self.scheme.collector_held.to_string()),
            (
                "collector_widest_pass",
                self.scheme.collector_widest_pass.to_string(),
            ),
            (
                "request_standing_consented",
                standings.consented.count.to_string(),
            ),
            (
                "request_standing_consented_us",
                standings.consented.total.as_micros().to_string(),
            ),
            (
                "request_standing_consented_longest_us",
                standings.consented.longest.as_micros().to_string(),
            ),
            (
                "request_standing_taken_over",
                standings.taken_over.count.to_string(),
            ),
            (
                "request_standing_taken_over_us",
                standings.taken_over.total.as_micros().to_string(),
            ),
            (
                "request_standing_taken_over_longest_us",
                standings.taken_over.longest.as_micros().to_string(),
            ),
            (
                "request_standing_withdrawn",
                self.withdrawn.count.to_string(),
            ),
            (
                "request_standing_withdrawn_us",
                self.withdrawn.total.as_micros().to_string(),
            ),
            ("posted_standing", standings.posted.count.to_string()),
            (
                "posted_standing_us",
                standings.posted.total.as_micros().to_string(),
            ),
            (
                "posted_standing_longest_us",
                standings.posted.longest.as_micros().to_string(),
            ),
            ("collectors_born", self.collectors_born.to_string()),
            ("collectors_pinned", self.collectors_pinned.to_string()),
            (
                "collector_cpu_us",
                self.collectors.cpu.as_micros().to_string(),
            ),
            (
                "collector_cpu_at_the_stop_us",
                self.collector_cpu_at_the_stop.as_micros().to_string(),
            ),
            (
                "collector_cpu_from_the_start_us",
                (self.collector_cpu_at_the_stop - self.collector_cpu_at_the_start)
                    .as_micros()
                    .to_string(),
            ),
            (
                "collector_wall_us",
                self.collectors.wall.as_micros().to_string(),
            ),
            (
                "collector_voluntary_switches",
                self.collectors.voluntary_switches.to_string(),
            ),
            (
                "collector_involuntary_switches",
                self.collectors.involuntary_switches.to_string(),
            ),
            (
                "withheld_by_an_entry_peak_bytes",
                (self
                    .mutators
                    .iter()
                    .map(|reading| reading.withheld_by_an_entry_peak)
                    .sum::<u64>()
                    * MEMBER_CLASS_BYTES as u64)
                    .to_string(),
            ),
            (
                "withheld_by_an_entry_mean_bytes",
                (self
                    .mutators
                    .iter()
                    .map(|reading| {
                        reading.withheld_by_an_entry_time / reading.wall.as_nanos().max(1)
                    })
                    .sum::<u128>()
                    * MEMBER_CLASS_BYTES as u128)
                    .to_string(),
            ),
            (
                "withheld_by_an_entry_at_the_end_bytes",
                (self
                    .mutators
                    .iter()
                    .map(|reading| reading.withheld_by_an_entry_at_the_end)
                    .sum::<u64>()
                    * MEMBER_CLASS_BYTES as u64)
                    .to_string(),
            ),
            (
                "pace_ms",
                (millis_from_env("LL_RIG_PACE_MS").as_secs_f64() * 1000.0).to_string(),
            ),
            (
                "wait_without_poll",
                u8::from(switch_from_env("LL_RIG_WAIT_WITHOUT_POLL")).to_string(),
            ),
            (
                "standings_recorded",
                u8::from(switch_from_env("LL_RIG_STANDINGS")).to_string(),
            ),
            (
                "mutator_cpu_us",
                self.mutators
                    .iter()
                    .map(|reading| reading.cpu.as_micros())
                    .sum::<u128>()
                    .to_string(),
            ),
            (
                "mutator_cpu_in_the_loop_us",
                self.mutators
                    .iter()
                    .map(|reading| reading.cpu_in_the_loop.as_micros())
                    .sum::<u128>()
                    .to_string(),
            ),
            (
                "mutator_cycles_in_the_loop",
                self.mutators
                    .iter()
                    .map(|reading| reading.cycles_in_the_loop)
                    .sum::<u64>()
                    .to_string(),
            ),
            (
                "mutator_instructions_in_the_loop",
                self.mutators
                    .iter()
                    .map(|reading| reading.instructions_in_the_loop)
                    .sum::<u64>()
                    .to_string(),
            ),
            (
                "mutator_instructions_with_the_drain",
                self.mutators
                    .iter()
                    .map(|reading| reading.instructions_with_the_drain)
                    .sum::<u64>()
                    .to_string(),
            ),
            (
                "mutator_counter_share_min",
                self.mutators
                    .iter()
                    .map(|reading| reading.counter_share)
                    .fold(1.0f64, f64::min)
                    .to_string(),
            ),
            (
                "backlog_at_the_stop",
                self.sum(|reading| reading.backlog_at_the_stop).to_string(),
            ),
            (
                "last_free_us_max",
                self.mutators
                    .iter()
                    .map(|reading| reading.last_free.as_micros())
                    .max()
                    .unwrap_or(0)
                    .to_string(),
            ),
            (
                "remnant_wait_us_max",
                self.mutators
                    .iter()
                    .map(|reading| reading.remnant_wait.as_micros())
                    .max()
                    .unwrap_or(0)
                    .to_string(),
            ),
            (
                "remnants_cleared",
                self.sum(|reading| usize::from(reading.remnant_cleared))
                    .to_string(),
            ),
            (
                "freed_in_the_drain",
                self.sum(|reading| reading.freed_in_the_drain).to_string(),
            ),
            (
                "live_bytes",
                (self.mutators.len() * load.live_members() * MEMBER_CLASS_BYTES).to_string(),
            ),
            (
                "mutators_at_the_ceiling",
                self.sum(|reading| usize::from(reading.at_the_ceiling))
                    .to_string(),
            ),
            (
                "long_iterations",
                self.sum(|reading| reading.long_iterations).to_string(),
            ),
            ("batches_timed", self.segment_times.batches.to_string()),
        ]
        .into_iter()
        .chain(self.segment_fields())
        .chain(self.web_fields(load))
        .chain(self.journal_fields())
        .chain(self.arrival_fields())
        .collect()
    }

    /// What the arrivals' window read (`LL_RIG_ARRIVALS`), zero for a paced
    /// cell: the requests and their latencies from arrival and from service,
    /// the queue, the busy share, the backlog at the stop, the spin's cost and
    /// share, and the mutators' instructions with the spin and the plans'
    /// draws taken out.
    fn arrival_fields(&self) -> Vec<(&'static str, String)> {
        let figures: Vec<&ArrivalFigures> =
            self.web.mutators.iter().map(|web| &web.arrivals).collect();
        let (mut from_arrival, mut from_service) = (Latencies::default(), Latencies::default());
        for one in &figures {
            from_arrival.add(&one.from_arrival);
            from_service.add(&one.from_service);
        }
        let requests: usize = figures.iter().map(|one| one.records.len()).sum();
        let queued: usize = figures.iter().map(|one| one.queued_sum).sum();
        let offers = offer_counts();
        let (wave, by_span) = wave_three();
        let (by_oldest_age, by_own_age) = root_ages();
        let (live_sites, live_survived, stale_ages) = testing::read_live::take();
        #[cfg(feature = "gc-window")]
        let written_back =
            joined(&testing::wave_three::take_unwalked_write_backs().map(|n| n as usize));
        #[cfg(feature = "gc-checkpoint")]
        let written_back = String::new();
        let run_for = seconds_from_env("LL_RIG_SECONDS");
        let window = run_for.saturating_sub(seconds_from_env("LL_RIG_WARM_UP_SECONDS"));
        let busy: Duration = figures.iter().map(|one| one.busy).sum();
        let backlog = figures
            .iter()
            .flat_map(|one| &one.records)
            .filter(|record| record.start >= run_for)
            .count();
        let cost = self.spin_cost.unwrap_or_default();
        let spin: u64 = figures
            .iter()
            .map(|one| cost.of(one.spin_calls, one.spin_turns))
            .sum();
        let instructions: u64 = figures.iter().map(|one| one.instructions).sum();
        let plans: u64 = figures.iter().map(|one| one.plan_instructions).sum();
        let runtime = instructions.saturating_sub(spin + plans);
        let [start, stop, drained] = &self.edges;
        let collectors = stop.collector_instructions - start.collector_instructions;
        let collectors_with_the_drain =
            drained.collector_instructions - start.collector_instructions;
        let with_the_drain: u64 = self
            .mutators
            .iter()
            .zip(&figures)
            .map(|(reading, one)| {
                reading
                    .instructions_with_the_drain
                    .saturating_sub(one.instructions_at_the_window_start)
            })
            .sum();
        let other_cores = stop.other_cores_since(start);
        let share = |part: u64| {
            if instructions == 0 {
                0.0
            } else {
                part as f64 / instructions as f64
            }
        };
        vec![
            (
                "arrival_interarrival_us",
                std::env::var("LL_RIG_INTERARRIVAL_MS").map_or_else(
                    |_| specified_interarrival().as_micros().to_string(),
                    |millis| {
                        ((millis.parse::<f64>().unwrap_or(0.0)) * 1e3)
                            .round()
                            .to_string()
                    },
                ),
            ),
            ("arrival_requests", requests.to_string()),
            (
                "arrival_latency_p50_ns",
                from_arrival.quantile(0.5).to_string(),
            ),
            (
                "arrival_latency_p99_ns",
                from_arrival.quantile(0.99).to_string(),
            ),
            (
                "arrival_latency_p999_ns",
                from_arrival.quantile(0.999).to_string(),
            ),
            (
                "service_latency_p50_ns",
                from_service.quantile(0.5).to_string(),
            ),
            (
                "service_latency_p99_ns",
                from_service.quantile(0.99).to_string(),
            ),
            (
                "service_latency_p999_ns",
                from_service.quantile(0.999).to_string(),
            ),
            (
                "arrival_queue_mean",
                format!("{:.3}", queued as f64 / requests.max(1) as f64),
            ),
            (
                "arrival_queue_peak",
                figures
                    .iter()
                    .map(|one| one.queued_peak)
                    .max()
                    .unwrap_or(0)
                    .to_string(),
            ),
            (
                "arrival_busy_share",
                format!(
                    "{:.4}",
                    busy.as_secs_f64() / (window.as_secs_f64() * figures.len().max(1) as f64)
                ),
            ),
            ("arrival_backlog_at_the_stop", backlog.to_string()),
            (
                "arrival_service_mean_us",
                (busy.as_micros() / requests.max(1) as u128).to_string(),
            ),
            (
                "arrival_draw_mean_us",
                (figures
                    .iter()
                    .map(|one| one.plan_wall)
                    .sum::<Duration>()
                    .as_micros()
                    / requests.max(1) as u128)
                    .to_string(),
            ),
            (
                "arrival_over_their_cpu",
                figures
                    .iter()
                    .map(|one| one.over_their_cpu)
                    .sum::<usize>()
                    .to_string(),
            ),
            ("spin_instructions_a_call", format!("{:.1}", cost.per_call)),
            ("spin_instructions_a_turn", format!("{:.4}", cost.per_turn)),
            ("spin_fit_error_ppm", format!("{:.1}", cost.error * 1e6)),
            ("spin_instructions", spin.to_string()),
            ("spin_share", format!("{:.4}", share(spin))),
            ("plan_instructions", plans.to_string()),
            ("window_mutator_instructions", instructions.to_string()),
            ("window_runtime_instructions", runtime.to_string()),
            ("window_collector_instructions", collectors.to_string()),
            (
                "collector_instructions_with_the_drain",
                collectors_with_the_drain.to_string(),
            ),
            (
                "instructions_a_request",
                ((runtime + collectors) / requests.max(1) as u64).to_string(),
            ),
            (
                "instructions_a_request_with_the_drain",
                ((with_the_drain.saturating_sub(spin + plans) + collectors_with_the_drain)
                    / requests.max(1) as u64)
                    .to_string(),
            ),
            ("other_cpu_cores", format!("{other_cores:.3}")),
            ("void", u8::from(other_cores > VOID_CORES).to_string()),
            (
                "window_members_reclaimed",
                (stop.members_reclaimed - start.members_reclaimed).to_string(),
            ),
            (
                "window_turnovers_by_proofs",
                (stop.turnovers[0] - start.turnovers[0]).to_string(),
            ),
            (
                "window_turnovers_by_x",
                (stop.turnovers[1] - start.turnovers[1]).to_string(),
            ),
            (
                "window_turnovers_new_life",
                (stop.turnovers[2] - start.turnovers[2]).to_string(),
            ),
            (
                "web_garbage_made_bytes",
                figures
                    .iter()
                    .map(|one| one.garbage_made)
                    .sum::<usize>()
                    .to_string(),
            ),
            (
                "web_garbage_freed_bytes",
                figures
                    .iter()
                    .map(|one| {
                        (one.garbage_at_the_start + one.garbage_made)
                            .saturating_sub(one.garbage_at_the_end)
                    })
                    .sum::<usize>()
                    .to_string(),
            ),
            // The collector's Δ-test over the whole process — warm-up and
            // drain included, one cell a process — and zeros without
            // `gc-window` (`crate::cycle::delta_test`).
            ("tag_sets_proved", tag_counts().0.to_string()),
            ("tag_sets_touched", tag_counts().1.to_string()),
            ("tag_sets_weakly_held", tag_counts().2.to_string()),
            // The mutators' offers the same way: taken, the standing of a
            // taken offer from the offer to the take at the median, the 99th
            // centile and the longest in microseconds, and offers withdrawn
            // (`crate::cycle::offer`).
            ("offers_taken", offers[0].to_string()),
            ("offer_to_take_p50_us", offers[1].to_string()),
            ("offer_to_take_p99_us", offers[2].to_string()),
            ("offer_to_take_longest_us", offers[3].to_string()),
            ("offers_withdrawn", offers[4].to_string()),
            // The collector's own frees and the split, the same way
            // (`crate::cycle::collector_frees`, `crate::cycle::split`).
            ("frees_sets", frees_counts()[0].to_string()),
            ("frees_members", frees_counts()[1].to_string()),
            ("frees_drops", frees_counts()[2].to_string()),
            ("frees_held_drops", frees_counts()[3].to_string()),
            ("frees_not_ineligible", frees_counts()[4].to_string()),
            ("frees_not_past_the_cap", frees_counts()[5].to_string()),
            ("frees_not_recalled", frees_counts()[6].to_string()),
            ("frees_not_allocation_failed", frees_counts()[7].to_string()),
            ("frees_not_frees_stand", frees_counts()[8].to_string()),
            ("frees_longest_act_us", frees_counts()[9].to_string()),
            ("frees_application_us", frees_counts()[10].to_string()),
            (
                "frees_longest_application_us",
                frees_counts()[11].to_string(),
            ),
            // The backup trace's arm the same way, zeros without
            // `trace-backup-rig` (`crate::cycle::trace_backup::counts`): the
            // traces; their nanoseconds in all, in the fill and subtract, the
            // mark, the harvest and split, and finalization with reclamation;
            // the longest trace's; the entities walked and the members freed.
            ("tb_traces", trace_backup_counts()[0].to_string()),
            ("tb_trace_ns", trace_backup_counts()[1].to_string()),
            ("tb_fill_subtract_ns", trace_backup_counts()[2].to_string()),
            ("tb_mark_ns", trace_backup_counts()[3].to_string()),
            ("tb_components_ns", trace_backup_counts()[4].to_string()),
            ("tb_finalization_ns", trace_backup_counts()[5].to_string()),
            ("tb_longest_trace_ns", trace_backup_counts()[6].to_string()),
            ("tb_walked", trace_backup_counts()[7].to_string()),
            ("tb_freed", trace_backup_counts()[8].to_string()),
            ("tb_sweep_ns", trace_backup_counts()[9].to_string()),
            ("tb_swept", trace_backup_counts()[10].to_string()),
            ("split_sets", split_counts()[0].to_string()),
            ("split_requeued", split_counts()[1].to_string()),
            (
                "split_second_refusals_read_live",
                split_counts()[2].to_string(),
            ),
            ("split_dropped", split_counts()[3].to_string()),
            ("split_unreadable", split_counts()[4].to_string()),
            ("split_roots_read_live_again", split_counts()[5].to_string()),
            // The offers the mutators withdrew, by the section each stood in
            // (`testing::RigSection`: other, draw, build, poll, spin, wait,
            // end).
            (
                "offers_withdrawn_by_section",
                joined(&testing::withdrawals_by_section()),
            ),
            (
                "build_step_longest_us",
                longest_build_step().as_micros().to_string(),
            ),
            // Wave 3's readings (`PLAN.md`, S68.14), zeros without
            // `gc-window` (`testing::wave_three`): the polls that
            // reached the offer's reading and those a slice of drops held
            // back; the offers, their mean and largest ceiling and those at
            // the bound; the trace's expansions, at the frame, and at the
            // frame with a count of 0; and by the span from the mutator's
            // previous offer (under 1, 10, 100, 1,000 ms, then above), each
            // bucket's batches, roots, roots dead (proposed or at 0), roots
            // read live and roots unwalked, `;` between figures and `/`
            // between buckets. A ceiling is R's count read to the end of the
            // block that passes the bound, not the batch.
            ("polls_at_the_offer", wave[0].to_string()),
            ("polls_held_by_drops", wave[1].to_string()),
            ("offers_made", wave[2].to_string()),
            ("offer_ceiling_mean", wave[3].to_string()),
            ("offer_ceiling_largest", wave[4].to_string()),
            ("offers_at_the_bound", wave[5].to_string()),
            ("expansions", wave[6].to_string()),
            ("expansions_at_the_frame", wave[7].to_string()),
            ("expansions_at_the_frame_at_zero", wave[8].to_string()),
            ("batches_by_span", by_span),
            // Under `LL_RIG_ROOT_AGES`, by the age at the take
            // (`testing::wave_three::AGES`): the batches by their oldest
            // root's, as `batches_by_span`; the roots by their own, each
            // bucket's roots, dead, read live, unwalked, and those read live
            // by an earlier batch.
            ("batches_by_oldest_age", by_oldest_age),
            ("roots_by_own_age", by_own_age),
            // The unwalked entries written back into R, unmarked and marked.
            ("unwalked_written_back", written_back),
            // Roots a collector read live (`testing::read_live`): by site
            // (untracked, a live row, no row), and by the live readings the
            // root's header counted before (0, 1, 2, 3 or more, the last
            // two sites only); then the stale tags the Δ-test cleared, by
            // the windows they trail the frame, 1 to 254, `;` between bins.
            ("read_live_by_site", joined(&live_sites.map(|n| n as usize))),
            (
                "read_live_by_survived",
                joined(&live_survived.map(|n| n as usize)),
            ),
            (
                "stale_tag_ages",
                joined(
                    &stale_ages[1..255]
                        .iter()
                        .map(|&n| n as usize)
                        .collect::<Vec<_>>(),
                ),
            ),
        ]
    }

    /// [`JOURNAL_COLUMNS`] over the loop, then over the drain, and each
    /// window's records lost and threads never journaled.
    fn journal_fields(&self) -> Vec<(&'static str, String)> {
        let (over_the_loop, over_the_drain) = &self.journal;
        let mut fields = Vec::new();
        for (window, counts) in [over_the_loop, over_the_drain].into_iter().enumerate() {
            for column in JOURNAL_COLUMNS {
                let value = match (column.code, column.sum_of_b) {
                    (Some(code), false) => counts.records(column.kind, code),
                    (Some(code), true) => counts.sum_of_b(column.kind, code),
                    (None, false) => counts.records_of_kind(column.kind),
                    (None, true) => counts.sum_of_b_of_kind(column.kind),
                };
                fields.push((column.names[window], value.to_string()));
            }

            let lost = ["j_loop_lost", "j_drain_lost"][window];
            fields.push((lost, counts.lost.to_string()));
            let never = ["j_loop_never_journaled", "j_drain_never_journaled"][window];
            fields.push((never, counts.never_journaled.to_string()));
        }

        fields
    }

    /// A web load's figures (`WebCell`, `WebReading`), zero for a ring load:
    /// each summed over the mutators unless its name says otherwise.
    fn web_fields(&self, load: Load) -> Vec<(&'static str, String)> {
        let web = &self.web;
        let sum = |field: &dyn Fn(&WebReading) -> usize| -> String {
            web.mutators.iter().map(field).sum::<usize>().to_string()
        };
        let mean = |size_index: Option<usize>| -> String {
            let bytes: f64 = web
                .mutators
                .iter()
                .map(|reading| match size_index {
                    Some(size_index) => reading.garbage_mean[size_index],
                    None => reading.garbage_mean.iter().sum(),
                })
                .sum();
            format!("{bytes:.0}")
        };
        // Zero unless every mutator reached the requests the sum folds.
        let checksum = if web.mutators.iter().any(|reading| reading.checksum == 0) {
            0
        } else {
            web.mutators
                .iter()
                .fold(0u64, |sum, reading| sum.rotate_left(7) ^ reading.checksum)
        };
        let live_writes: usize = web
            .mutators
            .iter()
            .map(|reading| reading.blocks_the_writes_hold)
            .sum();
        let mut fields = vec![
            (
                "web_values",
                load.web.map_or(0, |web| web.values).to_string(),
            ),
            ("web_requests", sum(&|reading| reading.requests)),
            (
                "web_setup_ms_max",
                web.mutators
                    .iter()
                    .map(|reading| reading.setup.as_millis())
                    .max()
                    .unwrap_or(0)
                    .to_string(),
            ),
            ("web_draws_checksum", format!("{checksum:016x}")),
            ("web_garbage_mean_bytes", mean(None)),
            ("web_garbage_mean_bytes_64", mean(Some(0))),
            ("web_garbage_mean_bytes_128", mean(Some(1))),
            ("web_garbage_mean_bytes_512", mean(Some(2))),
            (
                "web_garbage_peak_bytes",
                sum(&|reading| reading.garbage_peak.1),
            ),
            (
                "web_garbage_peak_bytes_64",
                sum(&|reading| reading.garbage_peak.0[0]),
            ),
            (
                "web_garbage_peak_bytes_128",
                sum(&|reading| reading.garbage_peak.0[1]),
            ),
            (
                "web_garbage_peak_bytes_512",
                sum(&|reading| reading.garbage_peak.0[2]),
            ),
            (
                "web_garbage_at_the_drain_end_bytes",
                sum(&|reading| reading.garbage_at_the_drain_end),
            ),
            (
                "web_resident_at_the_drain_end_bytes",
                web.mutators
                    .iter()
                    .map(|reading| reading.resident_at_the_drain_end)
                    .max()
                    .unwrap_or(0)
                    .to_string(),
            ),
            (
                "web_deferred_at_the_drain_end",
                sum(&|reading| reading.deferred_at_the_drain_end),
            ),
            (
                "web_candidates_at_the_drain_end",
                sum(&|reading| reading.candidates_at_the_drain_end),
            ),
            ("web_hits", sum(&|reading| reading.counts.hits)),
            (
                "web_hits_registering",
                sum(&|reading| reading.counts.hits_registering),
            ),
            ("web_misses", sum(&|reading| reading.counts.misses)),
            (
                "web_evictions_silent",
                sum(&|reading| reading.counts.evictions_silent),
            ),
            (
                "web_evictions_registering",
                sum(&|reading| reading.counts.evictions_registering),
            ),
            (
                "web_sessions_replaced",
                sum(&|reading| reading.counts.sessions_replaced),
            ),
            (
                "web_session_writes",
                sum(&|reading| reading.counts.session_writes),
            ),
            ("web_silent_ends", sum(&|reading| reading.silent_ends)),
            ("web_registrations", sum(&|reading| reading.registrations)),
            (
                "web_reset_registrations",
                sum(&|reading| reading.reset_registrations),
            ),
            (
                "web_reset_values_registered",
                sum(&|reading| reading.reset_values_registered),
            ),
            (
                "web_values_standing_at_the_warm_up",
                sum(&|reading| reading.standing_at_the_warm_up.0),
            ),
            (
                "web_sessions_standing_at_the_warm_up",
                sum(&|reading| reading.standing_at_the_warm_up.1),
            ),
            (
                "web_values_standing_at_the_stop",
                sum(&|reading| reading.standing_at_the_stop.0),
            ),
            (
                "web_sessions_standing_at_the_stop",
                sum(&|reading| reading.standing_at_the_stop.1),
            ),
            (
                "web_retained_blocks_at_the_stop",
                web.retained_at_the_stop.to_string(),
            ),
            (
                "web_retained_blocks_of_live_writes",
                live_writes.to_string(),
            ),
            (
                "web_retained_garbage_blocks",
                web.retained_at_the_stop
                    .saturating_sub(live_writes)
                    .to_string(),
            ),
            (
                "web_retained_blocks_after_the_drains",
                web.retained_after_the_drains.to_string(),
            ),
            ("web_setup_rounds", web.setup.rounds.to_string()),
            (
                "web_setup_token_waits",
                web.setup.token_waits.waits.to_string(),
            ),
            (
                "web_setup_token_wait_longest_us",
                web.setup.token_waits.longest.as_micros().to_string(),
            ),
            ("web_batches_traced", web.batches_traced.to_string()),
            ("web_stub_trace_us", web.stub_trace.as_micros().to_string()),
            (
                "web_batches_a_second_min",
                format!(
                    "{:.1}",
                    web.batches_a_second
                        .iter()
                        .copied()
                        .fold(f64::INFINITY, f64::min)
                ),
            ),
            (
                "web_batches_a_second_mean",
                format!(
                    "{:.1}",
                    web.batches_a_second.iter().sum::<f64>()
                        / web.batches_a_second.len().max(1) as f64
                ),
            ),
            ("web_roots_a_second", format!("{:.0}", web.roots_a_second)),
            ("web_batches_recalled", web.recalled.0.to_string()),
            (
                "web_positions_a_recalled_batch_mean",
                (web.recalled.1 / web.recalled.0.max(1)).to_string(),
            ),
            (
                "web_positions_a_recalled_batch_most",
                web.recalled.2.to_string(),
            ),
            ("web_batches_completed", web.completed.0.to_string()),
            (
                "web_positions_a_completed_batch_mean",
                (web.completed.1 / web.completed.0.max(1)).to_string(),
            ),
            (
                "web_positions_a_completed_batch_most",
                web.completed.2.to_string(),
            ),
            ("web_widest_part_rows", web.widest_part.rows.to_string()),
            ("web_widest_part_blocks", web.widest_part.blocks.to_string()),
            (
                "web_widest_part_mark_positions",
                web.widest_part.mark_positions.to_string(),
            ),
            (
                "web_widest_part_scan_positions",
                web.widest_part.scan_positions.to_string(),
            ),
            (
                "web_widest_part_touched_blocks",
                web.widest_part.touched.to_string(),
            ),
            (
                "web_widest_part_wall_us",
                web.widest_part.wall.as_micros().to_string(),
            ),
        ];
        let withheld = &web.withheld;
        for stack in 0..WITHHELD_COLUMNS.len() {
            let mean = |all: usize, count: usize| (all / count.max(1)).to_string();
            let values = [
                withheld.crossings[stack].to_string(),
                mean(
                    withheld.held_at_the_crossings[stack],
                    withheld.crossings[stack],
                ),
                withheld.most_at_a_crossing[stack].to_string(),
                withheld.releases[stack].to_string(),
                mean(
                    withheld.held_at_the_releases[stack],
                    withheld.releases[stack],
                ),
                withheld.most_at_a_release[stack].to_string(),
            ];
            fields.extend(WITHHELD_COLUMNS[stack].into_iter().zip(values));
        }

        let stamping = web.stamping;
        let walks = stamping.walks.max(1);
        let values = [
            stamping.walks.to_string(),
            (stamping.stamps / walks).to_string(),
            stamping.stamps_most.to_string(),
            (stamping.wall.as_micros() / walks as u128).to_string(),
            stamping.longest.as_micros().to_string(),
        ];
        fields.extend(STAMPING_COLUMNS.into_iter().zip(values));
        // What a stamp the walks wrote saved the marks after it: the edges the
        // collector's marks left unexpanded, over the stamps written in the
        // same window.
        fields.push((
            "web_edges_pruned_per_stamp",
            format!(
                "{:.3}",
                self.scheme.collector_edges_pruned as f64 / stamping.stamps.max(1) as f64
            ),
        ));
        let (sets, members, members_most) = web.sets;
        let resets = web.resets;
        let resets_made = resets.resets.max(1) as u128;
        fields.extend([
            ("web_sets_posted", sets.to_string()),
            ("web_set_members_mean", (members / sets.max(1)).to_string()),
            ("web_set_members_most", members_most.to_string()),
            ("web_sets_dropped_at_a_return", web.sets_dropped.to_string()),
            ("web_collector_resets", resets.resets.to_string()),
            (
                "web_reset_sweep_us_mean",
                (resets.sweep.as_micros() / resets_made).to_string(),
            ),
            (
                "web_reset_sweep_longest_us",
                resets.sweep_longest.as_micros().to_string(),
            ),
            (
                "web_reset_give_back_us_mean",
                (resets.give_back.as_micros() / resets_made).to_string(),
            ),
            (
                "web_reset_give_back_longest_us",
                resets.give_back_longest.as_micros().to_string(),
            ),
            (
                "web_resets_under_the_claim",
                resets.under_the_claim.to_string(),
            ),
        ]);
        fields.extend([
            (
                "web_registrations_a_second",
                format!(
                    "{:.0}",
                    web.mutators
                        .iter()
                        .map(|reading| reading.registrations)
                        .sum::<usize>() as f64
                        / web.window.as_secs_f64().max(f64::MIN_POSITIVE)
                ),
            ),
            (
                "web_batches_through_the_state",
                web.batches_through_the_state.to_string(),
            ),
            (
                "web_frees_from_another_thread",
                web.frees_from_another_thread.to_string(),
            ),
            (
                "web_held_after_the_teardown_bytes",
                sum(&|reading| reading.held_after_the_teardown),
            ),
            (
                "web_teardown_passes_max",
                web.mutators
                    .iter()
                    .map(|reading| reading.teardown_passes)
                    .max()
                    .unwrap_or(0)
                    .to_string(),
            ),
        ]);
        fields
    }

    /// Per grant segment, in [`testing::SEGMENT_AROUND`]'s order: the takes
    /// whose wait met it and their wait, the returns withheld in it and their
    /// time withheld, and for the trace the collector's time in it, summed and
    /// at the longest.
    fn segment_fields(&self) -> Vec<(&'static str, String)> {
        const TAKES: [&str; testing::SEGMENTS] = ["token_waits_around", "token_waits_trace"];
        const TAKE_US: [&str; testing::SEGMENTS] = ["token_wait_us_around", "token_wait_us_trace"];
        const RETURNS: [&str; testing::SEGMENTS] =
            ["withheld_returns_around", "withheld_returns_trace"];
        const RETURN_US: [&str; testing::SEGMENTS] =
            ["withheld_return_us_around", "withheld_return_us_trace"];
        // The collector's time is taken inside the batch alone, so the
        // segment around it has no column of its own.
        const SEGMENT_US: [&str; testing::SEGMENTS - 1] = ["segment_us_trace"];
        const SEGMENT_LONGEST_US: [&str; testing::SEGMENTS - 1] = ["segment_longest_us_trace"];
        let mut fields = Vec::new();
        for segment in 0..testing::SEGMENTS {
            let returns = self.sum(|reading| reading.withheld_by_segment.returns[segment]);
            let returned: Duration = self
                .mutators
                .iter()
                .map(|reading| reading.withheld_by_segment.time[segment])
                .sum();
            fields.extend([
                (
                    TAKES[segment],
                    self.token_waits.waits_by_segment[segment].to_string(),
                ),
                (
                    TAKE_US[segment],
                    self.token_waits.total_by_segment[segment]
                        .as_micros()
                        .to_string(),
                ),
                (RETURNS[segment], returns.to_string()),
                (RETURN_US[segment], returned.as_micros().to_string()),
            ]);
            if segment > 0 {
                fields.extend([
                    (
                        SEGMENT_US[segment - 1],
                        self.segment_times.total[segment].as_micros().to_string(),
                    ),
                    (
                        SEGMENT_LONGEST_US[segment - 1],
                        self.segment_times.longest[segment].as_micros().to_string(),
                    ),
                ]);
            }
        }

        fields
    }
}

/// What the collections did since the last [`RoundFigures::take`]: the
/// figures a cell reads of its rounds, and of the mutators' dispositions of
/// what the rounds posted.
#[derive(Default)]
struct RoundFigures {
    outcomes: testing::Outcomes,
    rounds: usize,
    verdict_collections: testing::VerdictCollections,
    disposals: testing::VerdictCollections,
    scheme: testing::SchemeFigures,
    token_waits: testing::TokenWaits,
    segment_times: testing::SegmentTimes,
    recalls: (usize, usize),
    second_mark_recalls: usize,
    written_back: usize,
}

impl RoundFigures {
    /// The figures since the last take, each zeroed.
    fn take() -> Self {
        Self {
            outcomes: testing::take_outcomes(),
            rounds: testing::take_rounds(),
            verdict_collections: testing::take_verdict_collections(),
            disposals: testing::take_disposals(),
            scheme: testing::take_scheme_figures(),
            token_waits: testing::take_token_waits(),
            segment_times: testing::take_segment_times(),
            recalls: testing::take_recalls(),
            second_mark_recalls: testing::take_second_mark_recalls(),
            written_back: testing::take_written_back(),
        }
    }
}

/// Run `load` in `cell`: the collectors' pins and cap set, the figures
/// zeroed and births permitted, the mutators started together and stopped
/// together after the cell's wall time, the collectors retired. A dial a
/// caller set before the call stands through it and goes at the retire.
fn run(cell: &Cell, load: Load, class: *const Class) -> CellReading {
    let end = RetireOnDrop;
    let arrivals = load.web.is_some() && switch_from_env("LL_RIG_ARRIVALS");
    let spin_cost = if arrivals { SpinCost::fit() } else { None };
    // The collector's kinds from the setup on, so that the loop's first
    // records are counted; the readings below take the setup's out.
    // `LL_RIG_JOURNAL_KINDS`, a mask in hexadecimal, replaces the set: a cell
    // without the registrations reads what a record per decrement costs the
    // race between the mutator's recall and a trace.
    let kinds_before = crate::journal::kinds::enabled_kinds();
    crate::journal::kinds::set_enabled_kinds(std::env::var("LL_RIG_JOURNAL_KINDS").map_or(
        crate::journal::kinds::DEFAULT_KINDS | crate::journal::kinds::COLLECTOR_KINDS,
        |mask| {
            u64::from_str_radix(mask.trim_start_matches("0x"), 16)
                .expect("LL_RIG_JOURNAL_KINDS is a mask in hexadecimal")
        },
    ));
    testing::pin_collectors_to(&cell.collector_cpus);
    set_collector_cap(cell.cap);
    testing::time_the_withheld_returns(true);
    let _ = testing::take_spawns();
    let _ = testing::take_collectors_pinned();
    let _ = testing::take_collector_lives();
    let _ = testing::take_token_waits();
    let _ = testing::take_segment_times();
    let _ = testing::take_recalls();
    let _ = testing::take_written_back();
    let _ = testing::take_outcomes();
    let _ = testing::take_rounds();
    let _ = testing::take_verdict_collections();
    let _ = testing::take_disposals();
    let _ = testing::take_scheme_figures();
    testing::record_standings(switch_from_env("LL_RIG_STANDINGS"));
    #[cfg(feature = "gc-window")]
    testing::wave_three::record_root_ages(switch_from_env("LL_RIG_ROOT_AGES"));
    testing::permit_births(true);

    // The marks' switches before any mutator starts, so that every mark of the
    // cell reads them, the setup's included.
    let first_regions = switch_from_env("LL_RIG_FIRST_REGIONS");
    crate::cycle::mark::stop_at_candidates(first_regions);
    crate::cycle::mark::hold_nothing(switch_from_env("LL_RIG_HOLD_NOTHING"));
    crate::cycle::arena::keep_every_page(switch_from_env("LL_RIG_KEEP_EVERY_PAGE"));
    if let Ok(millis) = std::env::var("LL_RIG_EPOCH_MS") {
        crate::gc::ll_gc_set_epoch_interval(millis.parse().expect("milliseconds"));
    }
    // The shipped cut unless the run names one.
    crate::cycle::young_cut::set_young_cut(
        std::env::var("LL_RIG_YOUNG_CUT_MS")
            .map_or(crate::cycle::young_cut::shipped_cut(), |millis| {
                std::time::Duration::from_millis(millis.parse().expect("milliseconds"))
            }),
    );
    crate::gc::ll_gc_set_epoch_ratio(
        std::env::var("LL_RIG_SPENT_PER_PROOF")
            .map_or(0, |ratio| ratio.parse().expect("a whole ratio")),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let start = Arc::new(Barrier::new(cell.mutators.len() + 1));
    let stages = Arc::new(Barrier::new(cell.mutators.len() + 1));
    let classes = Sent(load.web.map(|_| WebClasses::new("Web")));
    let repeat = std::env::var("LL_RIG_REPEAT").map_or(1, |repeat| {
        repeat.parse().expect("LL_RIG_REPEAT is a count")
    });
    let threads: Vec<_> = cell
        .mutators
        .iter()
        .enumerate()
        .map(|(index, &cpu)| {
            let (stop, start, stages) =
                (Arc::clone(&stop), Arc::clone(&start), Arc::clone(&stages));
            let (class, classes) = (Sent(class), Sent(classes.0));
            let run_for = cell.run_for;
            std::thread::spawn(move || match load.web {
                Some(web) => a_web_mutator(
                    web,
                    index,
                    cpu,
                    classes.into_inner().expect("a web load's classes"),
                    &start,
                    &stages,
                    &stop,
                    repeat,
                    run_for,
                ),
                None => (
                    a_mutator(load, cpu, class.into_inner(), &start, &stop, run_for),
                    WebReading::default(),
                ),
            })
        })
        .collect();
    start.wait();
    let mut web = WebCell::default();
    web.stub_trace = millis_from_env("LL_RIG_STUB_TRACE_MS");
    let _stubbed = (!web.stub_trace.is_zero()).then(|| testing::stub_the_trace(web.stub_trace));
    let _batch_roots = std::env::var("LL_RIG_BATCH_ROOTS").ok().map(|roots| {
        testing::fix_the_batch_size(
            roots
                .parse()
                .expect("LL_RIG_BATCH_ROOTS is a count of roots"),
        )
    });
    if load.web.is_some() {
        // The setups' collections, apart from the loop's.
        web.setup = RoundFigures::take();
        let _ = crate::memory::heap::take_frees_from_another_thread();
        testing::read_traced_batches(switch_from_env("LL_RIG_TRACED_BATCHES"));
    }

    // The arrivals' window starts after the warm-up, where the driver takes
    // its counters' first readings; a
    // paced cell reads from the start barrier.
    let warm_up = if arrivals {
        seconds_from_env("LL_RIG_WARM_UP_SECONDS")
    } else {
        Duration::ZERO
    };
    std::thread::sleep(warm_up);
    if arrivals {
        web.warm_up = RoundFigures::take();
    }
    let journal_at_the_start = crate::journal::counts();
    let at_the_start = WindowEdge::now();
    let _ = testing::take_withheld_readings();
    let _ = crate::cycle::collector_stamps::testing::take_stamping();
    let _ = crate::cycle::posted_set::testing::take_sets_posted();
    let _ = crate::cycle::posted_set::testing::take_sets_dropped_at_a_return();
    let _ = crate::cycle::arena::take_reset_timing();
    let _ = testing::take_withdrawn_standings();
    let collector_cpu_at_the_start = testing::collector_cpu_to_now();
    std::thread::sleep(cell.run_for - warm_up);
    stop.store(true, Ordering::Relaxed);
    let collector_cpu_at_the_stop = testing::collector_cpu_to_now();
    let withdrawn = testing::take_withdrawn_standings();
    // A web load's mutators stop at three stages, each after the driver's
    // reading: their loop's end, where the retained blocks and the blocks the
    // live writes hold are read at one instant; the drains' end, where the
    // collections' figures are taken before any teardown collects; and the
    // release to the teardown.
    let mut round_figures = None;
    let mut journal_at_the_stop = None;
    let mut at_the_stop = None;
    let mut journal_after_the_drains = None;
    let mut after_the_drains = None;
    if let Some(load_web) = load.web {
        stages.wait();
        journal_at_the_stop = Some(crate::journal::counts());
        at_the_stop = Some(WindowEdge::now());
        if arrivals {
            // The window's collections, apart from the drain's.
            round_figures = Some(RoundFigures::take());
        }
        web.retained_at_the_stop = crate::memory::retained::retained_block_count();
        // The drain's batches are read on, for the widest part alone: a load
        // whose loop recalls every part completes one there.
        let batches = testing::take_traced_batches();
        dump_the_batches(&batches, at_the_start.at);
        web.batches_traced = batches.len();
        let stopped = at_the_stop.map_or_else(Instant::now, |edge| edge.at);
        web.window = stopped - at_the_start.at;
        web.read_the_cadence(&batches, at_the_start.at..stopped, cell.mutators.len());
        web.withheld = testing::take_withheld_readings();
        web.stamping = crate::cycle::collector_stamps::testing::take_stamping();
        web.sets = crate::cycle::posted_set::testing::take_sets_posted();
        web.sets_dropped = crate::cycle::posted_set::testing::take_sets_dropped_at_a_return();
        web.resets = crate::cycle::arena::take_reset_timing();
        let state = CORE_OBJECTS + VALUE_OBJECTS * load_web.values;
        web.batches_through_the_state = batches
            .iter()
            .filter(|batch| batch.rows_met >= state)
            .count();
        stages.wait();
        let drained = testing::take_traced_batches();
        dump_the_batches_to(&drained, at_the_start.at, ".drain");
        web.widest_part = batches
            .iter()
            .chain(drained.iter())
            .map(|batch| batch.widest_part)
            .max_by_key(|part| part.rows)
            .unwrap_or_default();
        testing::read_traced_batches(false);
        journal_after_the_drains = Some(crate::journal::counts());
        after_the_drains = Some(WindowEdge::now());
        web.retained_after_the_drains = crate::memory::retained::retained_block_count();
        web.frees_from_another_thread = crate::memory::heap::take_frees_from_another_thread();
        let drain = RoundFigures::take();
        round_figures.get_or_insert(drain);
        stages.wait();
    }

    // A ring load's mutators drain before they return: its loop is read at
    // the stop, and its drain at the join.
    let journal_at_the_stop = journal_at_the_stop.unwrap_or_else(crate::journal::counts);
    let at_the_stop = at_the_stop.unwrap_or_else(WindowEdge::now);
    let (mutators, web_mutators): (Vec<MutatorReading>, Vec<WebReading>) = threads
        .into_iter()
        .map(|thread| thread.join().expect("the mutator ran its loop"))
        .unzip();
    let journal_after_the_drains = journal_after_the_drains.unwrap_or_else(crate::journal::counts);
    let after_the_drains = after_the_drains.unwrap_or_else(WindowEdge::now);
    crate::journal::kinds::set_enabled_kinds(kinds_before);
    web.mutators = web_mutators;
    if let Ok(path) = std::env::var("LL_RIG_REQUESTS_TO") {
        write_the_requests(&path, &web.mutators);
    }
    assert_eq!(
        web.frees_from_another_thread, 0,
        "a web load frees nothing across threads"
    );
    // A ring load's rounds' figures before the retire, which zeroes them; the
    // rest after it, so that every collector life has ended and a birth the
    // last polls started has pinned itself.
    let (outcomes, rounds) = (testing::take_outcomes(), testing::take_rounds());
    drop(end);
    let figures = round_figures.unwrap_or_else(|| RoundFigures {
        outcomes,
        rounds,
        ..RoundFigures::take()
    });
    let born = testing::take_spawns();
    let (pinned, refused) = testing::take_collectors_pinned();
    assert_eq!(refused, 0, "the kernel pinned every collector born");
    if !cell.collector_cpus.is_empty() {
        assert_eq!(pinned, born, "every collector born pinned itself");
    }

    CellReading {
        mutators,
        collectors_born: born,
        collectors_pinned: pinned,
        collectors: testing::take_collector_lives(),
        collector_cpu_at_the_start,
        collector_cpu_at_the_stop,
        outcomes: figures.outcomes,
        rounds: figures.rounds,
        verdict_collections: figures.verdict_collections,
        disposals: figures.disposals,
        scheme: figures.scheme,
        token_waits: figures.token_waits,
        withdrawn,
        segment_times: figures.segment_times,
        recalls: figures.recalls,
        second_mark_recalls: figures.second_mark_recalls,
        written_back: figures.written_back,
        web,
        journal: (
            journal_at_the_stop.since(&journal_at_the_start),
            journal_after_the_drains.since(&journal_at_the_stop),
        ),
        spin_cost,
        edges: [at_the_start, at_the_stop, after_the_drains],
        ledger_peak: {
            let ledger = crate::memory::gc_metadata::stats();
            (ledger.peak_bytes(), ledger.peak_bytes_in_use())
        },
    }
}

/// Write the window's requests of every mutator to `path` as CSV, one line a
/// request, for `dev/tools/paired_excess.py` to pair across arms: the
/// mutator, the request's index among its arrivals, and its arrival, its
/// service's start and end, its drawn CPU and its drawn waits, each in
/// nanoseconds, the instants from the start barrier.
fn write_the_requests(path: &str, mutators: &[WebReading]) {
    use std::fmt::Write as _;
    let mut text = String::from("mutator,index,arrival_ns,start_ns,end_ns,cpu_ns,waits_ns\n");
    for (mutator, web) in mutators.iter().enumerate() {
        for record in &web.arrivals.records {
            let _ = writeln!(
                text,
                "{mutator},{},{},{},{},{},{}",
                record.index,
                record.arrival.as_nanos(),
                record.start.as_nanos(),
                record.end.as_nanos(),
                record.cpu.as_nanos(),
                record.waits.as_nanos()
            );
        }
    }
    std::fs::write(path, text)
        .unwrap_or_else(|error| panic!("the requests were written to {path}: {error}"));
}

/// The Δ-test's counts: sets proved, touched and weakly held.
fn tag_counts() -> (usize, usize, usize) {
    #[cfg(feature = "gc-window")]
    {
        let counts = crate::cycle::delta_test::tag_reading_counts();
        (counts.proved, counts.touched, counts.weakly_held)
    }
    #[cfg(feature = "gc-checkpoint")]
    (0, 0, 0)
}

/// The offers since the last call, taken: those taken, the standing of a
/// taken offer at the median, the 99th centile and the longest in
/// microseconds, and those withdrawn; zeros without the feature.
fn offer_counts() -> [u64; 5] {
    #[cfg(feature = "gc-window")]
    {
        let (mut taken, withdrawn) = testing::take_offer_standings();
        taken.sort_unstable();
        let at = |centile: usize| {
            taken
                .get((taken.len() * centile / 100).min(taken.len().saturating_sub(1)))
                .map_or(0, |nanos| nanos / 1_000)
        };
        [
            taken.len() as u64,
            at(50),
            at(99),
            taken.last().map_or(0, |nanos| nanos / 1_000),
            withdrawn.count as u64,
        ]
    }
    #[cfg(feature = "gc-checkpoint")]
    [0; 5]
}

/// [`crate::cycle::trace_backup::counts`], over the whole process as the
/// collector's own frees are read; zeros without the feature.
fn trace_backup_counts() -> [u64; 11] {
    #[cfg(feature = "trace-backup-rig")]
    {
        crate::cycle::trace_backup::counts()
    }
    #[cfg(not(feature = "trace-backup-rig"))]
    [0; 11]
}

/// [`crate::cycle::collector_frees::frees_counts`] flat, times in µs: sets,
/// members, drops, held drops, the five refusals, the longest act, the
/// applications in all and at the longest; zeros without the feature.
fn frees_counts() -> [u128; 12] {
    #[cfg(feature = "gc-window")]
    {
        let counts = crate::cycle::collector_frees::frees_counts();
        let [a, b, c, d, e] = counts.not_freed.map(|n| n as u128);
        [
            counts.sets as u128,
            counts.members as u128,
            counts.drops as u128,
            counts.held as u128,
            a,
            b,
            c,
            d,
            e,
            counts.longest_act.as_micros(),
            counts.applications.as_micros(),
            counts.longest_application.as_micros(),
        ]
    }
    #[cfg(feature = "gc-checkpoint")]
    [0; 12]
}

/// `counts` as one column, `;` between them.
fn joined(counts: &[usize]) -> String {
    counts
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(";")
}

/// [`testing::wave_three::take`] flat, the ceiling as a mean, and the
/// buckets as one column; zeros without the feature.
fn wave_three() -> ([u64; 9], String) {
    #[cfg(feature = "gc-window")]
    {
        let (polls, ceilings, expansions, by_span) = testing::wave_three::take();
        let mean = ceilings[1].checked_div(ceilings[0]).unwrap_or(0);
        let flat = [
            polls[0],
            polls[1],
            ceilings[0],
            mean,
            ceilings[2],
            ceilings[3],
            expansions[0],
            expansions[1],
            expansions[2],
        ];
        let buckets = by_span
            .iter()
            .map(|bucket| {
                bucket
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(";")
            })
            .collect::<Vec<_>>()
            .join("/");
        (flat, buckets)
    }
    #[cfg(feature = "gc-checkpoint")]
    ([0; 9], String::new())
}

/// [`testing::wave_three::take_ages`], each table as one column of
/// `;`-joined buckets split by `/`; empty without the feature.
fn root_ages() -> (String, String) {
    #[cfg(feature = "gc-window")]
    {
        let column = |table: &[[u64; 5]]| {
            table
                .iter()
                .map(|bucket| {
                    bucket
                        .iter()
                        .map(u64::to_string)
                        .collect::<Vec<_>>()
                        .join(";")
                })
                .collect::<Vec<_>>()
                .join("/")
        };
        let (oldest, own) = testing::wave_three::take_ages();
        (column(&oldest), column(&own))
    }
    #[cfg(feature = "gc-checkpoint")]
    (String::new(), String::new())
}

/// [`crate::cycle::split::split_counts`], zeros without the feature.
fn split_counts() -> [usize; 6] {
    #[cfg(feature = "gc-window")]
    {
        crate::cycle::split::split_counts()
    }
    #[cfg(feature = "gc-checkpoint")]
    [0; 6]
}

/// One cell's line, prefixed `rig,` for the driver, after the header's
/// line, prefixed `rig-header,`, which the driver writes once.
fn print(cell: &Cell, load: Load, reading: &CellReading) {
    let fields = reading.fields(cell, load);
    let column = |pick: fn(&(&'static str, String)) -> String| {
        fields.iter().map(pick).collect::<Vec<_>>().join(",")
    };
    println!("rig-header,{}", column(|field| field.0.to_string()));
    println!("rig,{}", column(|field| field.1.clone()));
}

#[test]
#[ignore = "measurement rig; run by dev/tools/rig.sh, one cell per process (release mode)"]
fn a_cell_of_the_rig() {
    let _g = test_guard();
    let _wait = testing::HeldRequestWait::crate_own();
    let cell = Cell::from_env();
    // The setting S68.8 reads under `gc-window`: the largest set
    // the collector frees itself.
    #[cfg(feature = "gc-window")]
    if let Ok(cap) = std::env::var("LL_RIG_MEMBER_CAP") {
        let _ = crate::cycle::collector_frees::set_member_cap_for_test(
            cap.parse().expect("LL_RIG_MEMBER_CAP is a count"),
        );
    }
    let class = member_class("RigNode");
    let loads: Vec<Load> = LOADS
        .into_iter()
        .filter(|load| cell.load.as_deref().is_none_or(|name| name == load.name))
        .map(|mut load| {
            // `LL_RIG_CHURN_GRAPHS` scales a churn load's rings an iteration.
            if load.churn_graphs > 0
                && let Ok(graphs) = std::env::var("LL_RIG_CHURN_GRAPHS")
            {
                load.churn_graphs = graphs.parse().expect("LL_RIG_CHURN_GRAPHS is a count");
            }

            load
        })
        .collect();
    assert!(
        !loads.is_empty(),
        "LL_RIG_LOAD names a load of the rig: {:?}",
        cell.load
    );
    for load in loads {
        print(&cell, load, &run(&cell, load, class));
    }
}

/// Every value up to 100,000 ns lands in a bucket whose ceiling is at or
/// above it and within an eighth of it, which is what a quantile read off
/// the ceiling inherits.
#[test]
fn a_latency_lands_in_a_bucket_within_an_eighth_above_it() {
    for nanos in 0..100_000u64 {
        let ceiling = Latencies::ceiling_of(Latencies::bucket(nanos));
        assert!(
            (nanos..=nanos + nanos / 8).contains(&ceiling),
            "{nanos} ns reads {ceiling}"
        );
    }

    let mut latencies = Latencies::default();
    for nanos in 1..=1000 {
        latencies.record(Duration::from_nanos(nanos));
    }

    for (share, exact) in [(0.5, 500), (0.99, 990), (0.999, 999)] {
        let read = latencies.quantile(share);
        assert!(
            (exact..=exact + exact / 8).contains(&read),
            "the {share} quantile of 1..=1000 reads {read}"
        );
    }
}

/// The load named `name`.
fn load_named(name: &str) -> Load {
    LOADS
        .into_iter()
        .find(|load| load.name == name)
        .expect("a load of the rig")
}

/// Each of the rig's figures read once on an input whose answer is known.
#[test]
#[ignore = "calibration of the rig's figures; run explicitly with --ignored (release mode)"]
fn the_rigs_figures_read_their_known_answers() {
    let _g = test_guard();
    let _record = super::record();
    let _wait = testing::HeldRequestWait::crate_own();
    let class = member_class("RigCalibrationNode");
    let cell = Cell {
        placement: "calibration".into(),
        load: None,
        mutators: vec![None; SMOKE_MUTATORS],
        collector_cpus: Vec::new(),
        cap: DEFAULT_COLLECTOR_CAP,
        run_for: Duration::from_millis(500),
    };

    // A spinning thread's CPU time is its wall; a sleeping one's is next to
    // nothing, and each sleep is a voluntary switch.
    let (cpu, wall) = std::thread::spawn(|| {
        let (cpu, from) = (testing::thread_cpu_time(), Instant::now());
        while from.elapsed() < Duration::from_millis(200) {
            std::hint::spin_loop();
        }

        (testing::thread_cpu_time() - cpu, from.elapsed())
    })
    .join()
    .unwrap();
    assert!(
        cpu >= wall * 9 / 10 && cpu <= wall + Duration::from_millis(1),
        "a spin of {wall:?} read {cpu:?} of CPU"
    );
    println!("calibration: a spin of {wall:?} read {cpu:?} of CPU");
    let (cpu, switches) = std::thread::spawn(|| {
        let (cpu, switches) = (
            testing::thread_cpu_time(),
            testing::thread_context_switches(),
        );
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(5));
        }

        (
            testing::thread_cpu_time() - cpu,
            testing::thread_context_switches().0 - switches.0,
        )
    })
    .join()
    .unwrap();
    assert!(
        cpu < Duration::from_millis(5) && switches >= 20,
        "twenty sleeps read {cpu:?} of CPU and {switches} voluntary switches"
    );
    println!("calibration: twenty sleeps read {cpu:?} of CPU and {switches} voluntary switches");

    // Ten million turns of a loop retire at least one instruction each, and
    // twenty sleeps retire next to none in user mode. A box whose kernel
    // refuses the counters reads zeros in the line and skips this.
    let counted = std::thread::spawn(|| {
        let counters = testing::ThreadCycles::open()?;
        let mut sum = 0u64;
        for turn in 0..10_000_000u64 {
            sum = std::hint::black_box(sum.wrapping_add(turn));
        }

        let (cycles, instructions, share) = counters.read();
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(5));
        }

        Some((
            cycles,
            instructions,
            share,
            counters.read().1 - instructions,
        ))
    })
    .join()
    .unwrap();
    if let Some((cycles, instructions, share, slept)) = counted {
        assert!(
            (10_000_000..100_000_000).contains(&instructions) && cycles > 0 && share == 1.0,
            "a loop of ten million turns read {instructions} instructions and {cycles} \
             cycles, the counters running {share} of it"
        );
        assert!(
            slept < 1_000_000,
            "twenty sleeps read {slept} user-mode instructions"
        );
        println!(
            "calibration: ten million turns read {instructions} instructions and {cycles} \
             cycles; twenty sleeps {slept} instructions"
        );
    }

    // No garbage frees nothing, withholds nothing and posts no set. The
    // collector is born at the first thread's start whatever the load
    // (`dev/DECISIONS.md`, "the collector thread is born at the first thread's
    // start, roots or none"), so its rounds take what R holds and find it live.
    let read = run(&cell, load_named("garbage-0"), class);
    assert_eq!(
        (
            read.sum(|reading| reading.garbage_members),
            read.sum(|reading| reading.freed_by_polls + reading.freed_at_the_end),
            read.standing_bytes(),
            read.time_to_free(),
            read.sum(|reading| reading.withheld_peak),
            read.written_back,
            read.collectors_born,
            read.verdict_collections.collections,
        ),
        (0, 0, 0, Duration::ZERO, 0, 0, 1, 0),
        "no garbage"
    );

    // All garbage, across turnovers, frees all of it, and the collectors'
    // CPU stays inside their wall.
    testing::advance_epochs_after(Some(Duration::from_millis(1)));
    let read = run(&cell, load_named("garbage-100"), class);
    let built = read.sum(|reading| reading.garbage_members);
    assert!(built > 0, "the loop built garbage");
    assert_eq!(
        read.sum(|reading| reading.freed_by_polls + reading.freed_at_the_end),
        built,
        "every garbage member built was freed"
    );
    assert!(
        read.mutators.iter().all(|reading| reading.turnovers > 0),
        "the epochs turned"
    );
    assert!(read.time_to_free() > Duration::ZERO);
    // Under a cap above zero the polls free by the collections over P alone,
    // one a batch at most, and a batch carries at most `BATCH_BOUND` roots.
    assert_eq!(
        read.verdict_collections.freed,
        read.sum(|reading| reading.freed_by_polls),
        "every member a poll freed, a collection over P freed"
    );
    assert!(
        read.verdict_collections.collections <= read.outcomes.batches
            && read.outcomes.roots_served <= read.outcomes.batches * BATCH_BOUND
            && read.outcomes.batches <= read.outcomes.grants,
        "{:?}, {} collections over P",
        read.outcomes,
        read.verdict_collections.collections
    );
    // The phases of the collections over P fit inside their whole time, each
    // and those of the longest alike, and a collection that freed spent time
    // in its commit.
    let collections = read.verdict_collections;
    let phases: Duration = collections.phases.iter().sum();
    let longest: Duration = collections.phases_of_the_longest.iter().sum();
    assert!(
        phases <= collections.total
            && longest <= collections.longest
            && (collections.freed == 0 || collections.phases[5] > Duration::ZERO),
        "{collections:?}"
    );
    println!(
        "calibration: garbage-100 built {built} and freed them all, {} by polls in {} \
         collections over P of {} batches, over {} turnovers, the mean member freed after \
         {:?}; {} collector lives, {:?} CPU in {:?}",
        read.sum(|reading| reading.freed_by_polls),
        read.verdict_collections.collections,
        read.outcomes.batches,
        read.mutators
            .iter()
            .map(|reading| reading.turnovers)
            .sum::<u64>(),
        read.time_to_free(),
        read.collectors.lives,
        read.collectors.cpu,
        read.collectors.wall,
    );
    assert!(
        read.collectors_born > 0
            && read.collectors.lives == read.collectors_born
            && read.collectors.cpu <= read.collectors.wall,
        "{:?} over {} births",
        read.collectors,
        read.collectors_born
    );

    // A load whose registrations fill no block of R still has the collector
    // born at the first thread's start (`dev/DECISIONS.md`, "the collector
    // thread is born at the first thread's start, roots or none"), whose
    // rounds take R as it stands: every member built is either freed by a
    // poll or standing at the loop's end.
    let read = run(&cell, load_named("one-large-root"), class);
    let built = read.sum(|reading| reading.garbage_members);
    let freed = read.sum(|reading| reading.freed_by_polls);
    assert_eq!(
        (
            read.collectors_born,
            freed * MEMBER_CLASS_BYTES + read.standing_bytes()
        ),
        (1, built * MEMBER_CLASS_BYTES),
        "every garbage member built was freed by a poll or stood at the loop's end"
    );
    println!(
        "calibration: one-large-root built {built}, {freed} freed by polls, {} bytes standing",
        read.standing_bytes()
    );

    // A take the mutator recalls at the start of its trace posts every root
    // `Unwalked`, and the collection over P writes each back into R once;
    // the take waits once, and the probe's own reading of that wait, from
    // inside the token's wait to the hold, is the rig's to within the edges
    // of the two, which differ by a few instructions each way.
    use super::what_a_take_costs::{OVERLAPPING, Reading, a_take};
    let _end = RetireOnDrop;
    super::reset_lanes();
    let mut taken = None;
    for _ in 0..4 {
        let _ = (testing::take_written_back(), testing::take_recalls());
        let _ = testing::take_token_waits();
        let (_, _, whole, waited) = a_take(OVERLAPPING, class, Reading::Wall, &mut None, true);
        if whole {
            taken = Some((
                waited.expect("the take waited"),
                testing::take_written_back(),
                testing::take_recalls(),
                testing::take_token_waits(),
            ));
            break;
        }
    }

    let (waited, written_back, recalls, waits) = taken.expect("a take carried the ring whole");
    // An `Unwalked` root goes round P → R → P once.
    assert_eq!(
        (written_back, recalls, waits.waits),
        (OVERLAPPING.roots(), (0, 1), 1),
        "every root once back for its retry, one recall by a take, one wait"
    );
    assert_eq!(
        (
            waits.waits_by_segment[usize::from(testing::SEGMENT_TRACE)],
            waits.total_by_segment[usize::from(testing::SEGMENT_TRACE)]
        ),
        (1, waits.total),
        "the take met the trace"
    );
    assert!(
        waits.total.abs_diff(waited) <= Duration::from_micros(100),
        "the rig read {:?} where the probe read {waited:?}",
        waits.total
    );
    println!(
        "calibration: a recalled take wrote back {written_back} of {} roots, recalls {recalls:?} \
         (mark, take), the wait {:?} against the probe's {waited:?}",
        OVERLAPPING.roots(),
        waits.total
    );
}

/// The split by grant segment, on a hold whose segment and length the case
/// sets: this thread claims its own token as the elder in the trace's
/// segment, withholds three returns under it and gives them back 20 ms
/// later, which reads three returns in that segment and at least 60 ms.
#[test]
fn the_split_by_segment_reads_a_hold_the_case_sets() {
    const HELD: Duration = Duration::from_millis(20);
    struct StopTiming;
    impl Drop for StopTiming {
        fn drop(&mut self) {
            testing::time_the_withheld_returns(false);
        }
    }

    let _g = test_guard();
    testing::time_the_withheld_returns(true);
    let _stop = StopTiming;
    let token = &unsafe { &*super::record() }.token;
    let _ = testing::take_withheld_by_segment();
    assert!(token.claim_for_test(ELDER), "the token was free");
    testing::note_serving_slot(ELDER);
    let from = Instant::now();
    {
        let _segments = testing::BatchSegments::open(testing::SEGMENT_TRACE);
        for _ in 0..3 {
            testing::note_a_return_withheld();
        }
        std::thread::sleep(HELD);
    }
    testing::note_the_returns_given_back();
    let held = from.elapsed();
    token.release_claim_to(ELDER, crate::cycle::token::FREE);

    let withheld = testing::take_withheld_by_segment();
    let trace = usize::from(testing::SEGMENT_TRACE);
    assert_eq!(withheld.returns, [0, 3]);
    assert!(
        withheld.time[trace] >= 3 * HELD && withheld.time[trace] <= 3 * held,
        "three returns held {held:?} read {:?}",
        withheld.time[trace]
    );
    assert_eq!(
        testing::segment_of_the_holder(token.read()),
        testing::SEGMENT_AROUND,
        "a free token names no segment"
    );
}
