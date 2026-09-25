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
use std::time::Duration;

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
        live_graphs: ROOTS - garbage_rings,
        live: SMALL_RING,
    }
}

/// The loads of the S64 analysis's list. Change a name or add a load, and
/// change `LOADS` in `dev/tools/rig.sh` with it.
const LOADS: [Load; 10] = [
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
        live_graphs: 0,
        live: Graph::NONE,
    },
    // One live component of 2,048 members, every one of them registered
    // again at every iteration.
    Load {
        name: "large-live-core",
        garbage_graphs: 0,
        garbage: Graph::NONE,
        live_graphs: 1,
        live: Graph {
            rings: 1,
            members: 2048,
            roots: 2048,
            shared: 0,
            fillers: 0,
        },
    },
];

// An iteration registers its roots with no poll between them, and the
// teardown registers a member at every edge it nulls: both stay inside the
// poll's stride, past which a registration aborts the process
// (`crate::cycle::queue`, `POLL_STRIDE`).
const _: () = {
    let mut index = 0;
    while index < LOADS.len() {
        assert!(LOADS[index].roots_per_iteration() < POLL_STRIDE);
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

/// What one mutator's run read.
#[derive(Clone, Copy, Default)]
struct MutatorReading {
    iterations: usize,
    /// Members of the garbage graphs the loop built.
    garbage_members: usize,
    /// What the loop's polls freed.
    freed_by_polls: usize,
    /// What the collection after the loop freed, the live graphs gone.
    freed_at_the_end: usize,
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
    let mut reading = MutatorReading::default();
    start.wait();
    while !stop.load(Ordering::Relaxed) {
        for _ in 0..load.garbage_graphs {
            unsafe {
                build(&mut context, arena_ptr, class, load.garbage, &mut garbage);
                kill(&garbage.fillers);
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
        if reading.garbage_members - reading.freed_by_polls > OUTSTANDING_CEILING {
            reading.at_the_ceiling = true;
            break;
        }
    }

    unsafe { let_the_live_graphs_go(arena_ptr, &live, &keepers) };
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

/// Run `load` in `cell`: the collectors' pins and cap set and births
/// permitted, the mutators started together and stopped together after the
/// cell's wall time, the collectors retired. Returns the line the driver
/// reads; change its fields and change the header in `dev/tools/rig.sh`.
fn run(cell: &Cell, load: Load, class: *const Class) -> String {
    let end = RetireOnDrop;
    testing::pin_collectors_to(&cell.collector_cpus);
    set_collector_cap(cell.cap);
    let _ = testing::take_spawns();
    let _ = testing::take_collectors_pinned();
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
    let mutators: Vec<MutatorReading> = threads
        .into_iter()
        .map(|thread| thread.join().expect("the mutator ran its loop"))
        .collect();
    // Retired before the counts are read, so that a birth the last polls
    // started has pinned itself by then.
    drop(end);
    let born = testing::take_spawns();
    let (pinned, refused) = testing::take_collectors_pinned();
    assert_eq!(refused, 0, "the kernel pinned every collector born");
    if !cell.collector_cpus.is_empty() {
        assert_eq!(pinned, born, "every collector born pinned itself");
    }

    let listed = |cpus: &mut dyn Iterator<Item = String>| cpus.collect::<Vec<_>>().join(" ");
    let sum = |field: fn(&MutatorReading) -> usize| mutators.iter().map(field).sum::<usize>();
    format!(
        "rig,{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
        cell.placement,
        load.name,
        mutators.len(),
        listed(
            &mut cell
                .mutators
                .iter()
                .map(|cpu| cpu.map_or("-".into(), |cpu| cpu.to_string()))
        ),
        listed(&mut cell.collector_cpus.iter().map(usize::to_string)),
        cell.cap,
        cell.run_for.as_secs_f64(),
        sum(|reading| reading.iterations),
        mutators
            .iter()
            .map(|reading| reading.iterations)
            .min()
            .unwrap_or(0),
        sum(|reading| reading.garbage_members),
        sum(|reading| reading.freed_by_polls),
        sum(|reading| reading.freed_at_the_end),
        sum(|reading| usize::from(reading.at_the_ceiling)),
        born,
        pinned,
    )
}

#[test]
#[ignore = "measurement rig; run by dev/tools/rig.sh, one cell per process (release mode)"]
fn a_cell_of_the_rig() {
    let _g = test_guard();
    let _wait = testing::HeldRequestWait::crate_own();
    let cell = Cell::from_env();
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
        println!("{}", run(&cell, load, class));
    }
}
