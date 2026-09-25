//! The rig: mutators running one load for a set wall time, each pinned to the
//! logical CPU the driver names, beside collectors pinned the same way. Its
//! placements are those of `dev/S64-GC-IMPROVEMENT-ANALYSIS.md`, "Какие опыты
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
//! plus the garbage no collection has freed yet.
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
//! round P → R → P each), the parts deferred past B with the retry spent or
//! past `B_max`, and the grants recalled by the mark and by a take; the
//! takes' waits for the token; and the collectors' lives, CPU, wall from
//! birth to end and context switches. Each is read once on an input whose
//! answer is known in [`the_rigs_figures_read_their_known_answers`], and the
//! count of deferred parts against the batch's own in `the_ceiling`.
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
//! - `LL_RIG_SECONDS` — how long the mutators run their loops.
//!
//! Run in a release build:
//! `cargo test --release --lib -- --ignored --exact
//! cycle::worker::tests::the_rig::a_cell_of_the_rig --test-threads=1 --nocapture`.

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
    /// members' entries in R however far the collector lags (`PLAN.md`
    /// S65.21). Only with one garbage graph.
    held: Graph,
}

impl Load {
    const fn roots_per_iteration(&self) -> usize {
        self.garbage_graphs * self.garbage.roots() + self.live_graphs * self.live.roots()
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
        live_graphs: ROOTS - garbage_rings,
        live: SMALL_RING,
    }
}

/// The loads of the S64 analysis's list. Change a name or add a load, and
/// change `LOADS` in `dev/tools/rig.sh` with it.
const LOADS: [Load; 15] = [
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
        live_graphs: 1,
        live: Graph {
            rings: 1,
            members: 2048,
            roots: 2048,
            shared: 0,
            fillers: 0,
        },
    },
    // One garbage ring of 2,048 members, every one of them registered: the
    // shape of S65.20's Critic, finding 1, where a batch proposes a part of
    // the ring's entries and the teardown kills members whose entries stand
    // in R behind it (`PLAN.md` S65.21).
    Load {
        name: "registered-ring",
        garbage_graphs: 1,
        garbage: REGISTERED_RING,
        held: Graph::NONE,
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
        live_graphs: ROOTS,
        live: SMALL_RING,
    },
    // Rings under and over a batch's bound of 1,024 roots.
    Load {
        name: "registered-ring-1000",
        garbage_graphs: 1,
        garbage: registered_ring(1000),
        held: Graph::NONE,
        live_graphs: 0,
        live: Graph::NONE,
    },
    Load {
        name: "registered-ring-4000",
        garbage_graphs: 1,
        garbage: registered_ring(4000),
        held: Graph::NONE,
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
        live_graphs: 0,
        live: Graph::NONE,
    },
];

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
        assert!(LOADS[index].roots_per_iteration() + LOADS[index].held.roots() < POLL_STRIDE);
        assert!(LOADS[index].live_members() < POLL_STRIDE);
        index += 1;
    }
};

/// Garbage members a mutator may have built and not yet seen freed before
/// its loop ends early: 512 MiB of members of [`MEMBER_CLASS_BYTES`]. A load
/// whose registrations never fill a block of R signals no collector, and its
/// garbage stands until the loop ends; the ceiling keeps such a cell off the
/// box's swap, and the line counts the mutators that reached it.
const OUTSTANDING_CEILING: usize = 1 << 22;

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
unsafe fn register(root: *mut Object) {
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
    }

    (graphs, keepers)
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
    /// The thread's CPU time over the loop.
    cpu: Duration,
    /// Context switches over the loop, voluntary and involuntary.
    switches: (u64, u64),
    /// Minor page faults over the loop.
    minor_faults: u64,
    /// Queue records the compactions read over the loop
    /// (`crate::cycle::queue::take_queue_work`).
    records_read: usize,
    /// The longest R stood at one iteration in [`R_SAMPLE_STRIDE`].
    ring_peak: usize,
    /// The wall of every iteration: the mutator's latency.
    latencies: Latencies,
    /// Members of the garbage graphs the loop built.
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
    /// The turnovers of the thread's epoch clock over the loop: a record
    /// another thread's life left keeps its count.
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
    let (cpu_from, switches_from) = (
        testing::thread_cpu_time(),
        testing::thread_context_switches(),
    );
    let faults_from = testing::thread_minor_faults();
    let _ = crate::cycle::queue::take_queue_work();
    let record = unsafe { &*mutator_record::this_thread_record() };
    let turnovers_from = record.turnovers();
    let from = Instant::now();
    let mut last = from;
    while !stop.load(Ordering::Relaxed) {
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

        for graph in &live {
            for &root in &graph.roots {
                unsafe { register(root) };
            }
        }

        reading.freed_by_polls += unsafe { crate::gc::ll_gc_maybe_collect() };
        reading.iterations += 1;
        let now = Instant::now();
        let outstanding = reading.garbage_members - reading.freed_by_polls;
        reading.latencies.record(now - last);
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

        if outstanding > OUTSTANDING_CEILING {
            reading.at_the_ceiling = true;
            break;
        }
    }

    reading.wall = from.elapsed();
    reading.withheld_by_an_entry_at_the_end = crate::cycle::queue::withheld_by_an_entry();
    reading.cpu = testing::thread_cpu_time() - cpu_from;
    let switches = testing::thread_context_switches();
    reading.switches = (switches.0 - switches_from.0, switches.1 - switches_from.1);
    reading.minor_faults = testing::thread_minor_faults() - faults_from;
    reading.records_read = crate::cycle::queue::take_queue_work().records_read;
    reading.turnovers = record.turnovers() - turnovers_from;
    unsafe { let_the_live_graphs_go(arena_ptr, &live, &keepers) };
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

/// Whether the cell runs form I of `PLAN.md` S65.21, the close's free of R's
/// front run: `LL_RIG_FRONT_RUN=1`.
fn front_run_from_env() -> bool {
    std::env::var("LL_RIG_FRONT_RUN").is_ok_and(|value| value == "1")
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
    /// The collectors' CPU at the instant the mutators were stopped.
    collector_cpu_at_the_stop: Duration,
    /// What the rounds did while the cell ran: batches and the roots they
    /// carried, grants, and the rounds themselves.
    outcomes: testing::Outcomes,
    rounds: usize,
    /// The mutators' collections over P.
    verdict_collections: testing::VerdictCollections,
    token_waits: testing::TokenWaits,
    /// Grants recalled by a stack's mark, and by a take.
    recalls: (usize, usize),
    written_back: usize,
    parts_deferred: usize,
    /// The GC ledger's high-water marks over the process so far: blocks
    /// reserved and bytes taken into use, both in bytes.
    ledger_peak: (usize, usize),
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

    fn cpu_an_operation(&self) -> Duration {
        let cpu: Duration = self.mutators.iter().map(|reading| reading.cpu).sum();
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
    /// not freed by a poll.
    fn standing_bytes(&self) -> usize {
        self.sum(|reading| reading.garbage_members - reading.freed_by_polls) * MEMBER_CLASS_BYTES
    }

    /// The line's fields by name, in the line's order: the header is their
    /// names, the line their values.
    fn fields(&self, cell: &Cell, load: Load) -> Vec<(&'static str, String)> {
        let listed = |cpus: Vec<String>| cpus.join(" ");
        let latencies = self.latencies();
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
            ("front_run", u8::from(front_run_from_env()).to_string()),
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
            ("parts_deferred", self.parts_deferred.to_string()),
            ("recalls_by_the_mark", self.recalls.0.to_string()),
            ("recalls_by_a_take", self.recalls.1.to_string()),
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
            (
                "freed_by_verdict_collections",
                self.verdict_collections.freed.to_string(),
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
                "live_bytes",
                (self.mutators.len() * load.live_members() * MEMBER_CLASS_BYTES).to_string(),
            ),
            (
                "mutators_at_the_ceiling",
                self.sum(|reading| usize::from(reading.at_the_ceiling))
                    .to_string(),
            ),
        ]
    }
}

/// Run `load` in `cell`: the collectors' pins and cap set, the figures
/// zeroed and births permitted, the mutators started together and stopped
/// together after the cell's wall time, the collectors retired. A dial a
/// caller set before the call stands through it and goes at the retire.
fn run(cell: &Cell, load: Load, class: *const Class) -> CellReading {
    let end = RetireOnDrop;
    testing::pin_collectors_to(&cell.collector_cpus);
    set_collector_cap(cell.cap);
    let _ = testing::take_spawns();
    let _ = testing::take_collectors_pinned();
    let _ = testing::take_collector_lives();
    let _ = testing::take_token_waits();
    let _ = testing::take_recalls();
    let _ = testing::take_written_back();
    let _ = testing::take_parts_deferred();
    let _ = testing::take_outcomes();
    let _ = testing::take_rounds();
    let _ = testing::take_verdict_collections();
    testing::permit_births(true);

    let stop = Arc::new(AtomicBool::new(false));
    let start = Arc::new(Barrier::new(cell.mutators.len() + 1));
    let threads: Vec<_> = cell
        .mutators
        .iter()
        .map(|&cpu| {
            let (stop, start, class) = (Arc::clone(&stop), Arc::clone(&start), Sent(class));
            std::thread::spawn(move || a_mutator(load, cpu, class.into_inner(), &start, &stop))
        })
        .collect();
    start.wait();
    std::thread::sleep(cell.run_for);
    stop.store(true, Ordering::Relaxed);
    let collector_cpu_at_the_stop = testing::collector_cpu_to_now();
    let mutators: Vec<MutatorReading> = threads
        .into_iter()
        .map(|thread| thread.join().expect("the mutator ran its loop"))
        .collect();
    // The rounds' figures before the retire, which zeroes them; the rest
    // after it, so that every collector life has ended and a birth the last
    // polls started has pinned itself.
    let (outcomes, rounds) = (testing::take_outcomes(), testing::take_rounds());
    drop(end);
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
        collector_cpu_at_the_stop,
        outcomes,
        rounds,
        verdict_collections: testing::take_verdict_collections(),
        token_waits: testing::take_token_waits(),
        recalls: testing::take_recalls(),
        written_back: testing::take_written_back(),
        parts_deferred: testing::take_parts_deferred(),
        ledger_peak: {
            let ledger = crate::memory::gc_metadata::stats();
            (ledger.peak_bytes(), ledger.peak_bytes_in_use())
        },
    }
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
    crate::cycle::queue::FRONT_RUN.store(front_run_from_env(), Ordering::Relaxed);
    let class = member_class("RigNode");
    let loads: Vec<Load> = LOADS
        .into_iter()
        .filter(|load| cell.load.as_deref().is_none_or(|name| name == load.name))
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

    // No garbage frees nothing, withholds nothing and signals no collector.
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
            (read.rounds, read.outcomes.batches),
            read.verdict_collections.collections,
        ),
        (0, 0, 0, Duration::ZERO, 0, 0, 0, (0, 0), 0),
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

    // A load whose registrations fill no block of R signals no collector,
    // and under a cap above zero no poll traces R whole: what was built
    // stands at the loop's end. (The count of deferred parts is read against
    // the batch's own in `the_ceiling`.)
    let read = run(&cell, load_named("one-large-root"), class);
    let built = read.sum(|reading| reading.garbage_members);
    assert_eq!(
        (
            read.collectors_born,
            read.sum(|reading| reading.freed_by_polls),
            read.standing_bytes()
        ),
        (0, 0, built * MEMBER_CLASS_BYTES),
        "every garbage member built stood at the loop's end"
    );
    println!(
        "calibration: one-large-root built {built}, none freed by a poll, {} bytes standing",
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
    assert_eq!(
        (written_back, recalls, waits.waits),
        (OVERLAPPING.roots(), (0, 1), 1),
        "one round P → R → P a root, one recall by a take, one wait"
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
