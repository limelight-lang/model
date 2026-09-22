//! What a take of a standing ring costs, by the shape of its roots
//! (`dev/BENCHMARKS.md`, "S64.5 what a take costs by the shape of its
//! roots"; the design's "Cost", `dev/design/a-standing-r-is-taken-after-n-rounds.md`).
//!
//! The budget ruling says a take's trace is one batch's — bounded by
//! [`TRACE_BLOCK_BUDGET`] blocks of the collector's arena — and that an
//! unwalked take shifts the mutator's trace rather than adding one
//! (`dev/DECISIONS.md`, "a take's trace is budgeted as one batch's, and an
//! unwalked take shifts the mutator's trace rather than adding one"). Two
//! shapes of the same sixty-three roots put the two halves of that claim
//! side by side:
//!
//! - **overlapping** — every root inside one component of 381 members, the
//!   corpus's median closure: the union of the closures is one closure, the
//!   trace completes inside the budget, and the mutator's collection over P
//!   reads the verdicts it posted;
//! - **disjoint** — every root the root of a ring of its own, one member per
//!   block, so that the row arrays the blocks reserve pass the budget: the
//!   trace is abandoned, every root comes back `Unwalked`, and the mutator
//!   traces them itself at its next poll.
//!
//! Each shape is read against a baseline: the same rings collected in line
//! over R whole with no take. The figure is the difference between the
//! mutator's collection after a take and that baseline.
//!
//! **The collector is retired before the timed collection in both arms**, so
//! that the two differ in what stands in P and in R and in nothing else.
//!
//! Run one at a time in a release build:
//! `cargo test --release --lib -- --ignored what_a_take_costs --test-threads=1`.
//! Under `perf stat --control`, `LL_PERF_CTL` and `LL_PERF_ACK` name the
//! fifos and `LL_TAKE_ARM` names the one arm to run, as
//! `dev/tools/take_perf.sh` sets them; the counting interval is opened
//! around the timed collection alone.

use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::census;
use crate::cycle::loads::slots_per_block;
use crate::cycle::queue::candidate_count;
use crate::cycle::testing::move_prop;
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, ll_release, ll_retain};
use crate::test_support::{prop_offset, store_prop};
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

/// Roots a standing sub-threshold ring carries at its largest: one short of
/// the threshold, which is what `batch` clamps a take to.
const ROOTS: usize = SOFT_THRESHOLD - 1;

/// The overlapping shape's component: the corpus's median closure, which is
/// the shape the design's "Cost" reads the take's own price on
/// (`dev/BENCHMARKS.md`, 2026-08-25).
const COMPONENT: usize = 381;

/// Members of each ring of the disjoint shape, one per block: five, so that
/// the 63 rings touch 315 blocks whose row arrays ask about 630 KiB, half
/// again what [`TRACE_BLOCK_BUDGET`] blocks of 64 KiB hold.
const RING_MEMBERS: usize = 5;

/// The members' size class: 128 bytes, so that a block holds 512 of them and
/// a touched block's row array is about 2 KiB (`dev/BENCHMARKS.md`, S40.3).
const MEMBER_CLASS_BYTES: usize = 128;

/// Reference-carrying properties of a class of [`MEMBER_CLASS_BYTES`]: the
/// header and the class word take sixteen bytes and each property sixteen.
const MEMBER_PROPS: usize = (MEMBER_CLASS_BYTES - 16) / 16;

/// Timed collections per arm, and the leading ones dropped: the first pays
/// the thread's workspace draw and the pool's first blocks.
const SAMPLES: usize = 21;
const WARM_UP: usize = 5;

/// The standing interval the arms run under: short of the crate's four
/// seconds so that the take falls inside the arm, and long against the
/// timer's cadence so that it is one round's work.
const INTERVAL: Duration = Duration::from_millis(50);

/// One shape of a take's roots. The members of every ring are linked
/// through one property; a garbage ring is held by nothing else and the
/// collection that reads it tears it down, a live one is held by a keeper
/// and every verdict over it reads live.
#[derive(Clone, Copy)]
struct Shape {
    name: &'static str,
    /// Rings built, each a component of its own.
    rings: usize,
    /// Members of each ring.
    members: usize,
    /// Members of each ring registered as candidates, which is what R holds.
    roots_per_ring: usize,
    /// Objects of the members' class allocated after each member, which is
    /// what puts one member in every block.
    fillers: usize,
    /// Whether a keeper holds each ring's first member, which makes the
    /// component live and the take's verdicts `ReadLive`.
    live: bool,
}

impl Shape {
    fn roots(&self) -> usize {
        self.rings * self.roots_per_ring
    }

    fn members(&self) -> usize {
        self.rings * self.members
    }

    /// What the timed collection frees: a garbage ring whole, and nothing at
    /// all of a live one — its roots go to the deferred lane.
    fn freed_by_the_collection(&self) -> usize {
        if self.live { 0 } else { self.members() }
    }
}

/// Every root of the take inside one component: the union of the roots'
/// closures is the closure of one, and the take costs the rows one root
/// costs.
const OVERLAPPING: Shape = Shape {
    name: "overlapping",
    rings: 1,
    members: COMPONENT,
    roots_per_ring: ROOTS,
    fillers: 0,
    live: false,
};

/// Every root the root of a ring of its own, one member per block: the sum
/// of the closures passes the trace's block budget, and the take comes back
/// unwalked.
const DISJOINT: Shape = Shape {
    name: "disjoint",
    rings: ROOTS,
    members: RING_MEMBERS,
    roots_per_ring: 1,
    fillers: slots_per_block(MEMBER_CLASS_BYTES) - 1,
    live: false,
};

/// The same two shapes with a keeper on every ring: the trace reads every
/// root live, and what the take saves the mutator is the walk it does not
/// have to make — the case the budget ruling's "`EmptyLane` when all read
/// live" names and the garbage shapes cannot show.
const OVERLAPPING_LIVE: Shape = Shape {
    name: "overlapping-live",
    live: true,
    ..OVERLAPPING
};

const DISJOINT_LIVE: Shape = Shape {
    name: "disjoint-live",
    live: true,
    ..DISJOINT
};

const _: () = assert!(OVERLAPPING.rings * OVERLAPPING.roots_per_ring == ROOTS);
const _: () = assert!(DISJOINT.rings * DISJOINT.roots_per_ring == ROOTS);

/// The rings of `shape`, built on this thread's heap and held by the case
/// only through the fillers: every member's creation reference is moved
/// into the slot of the member before it, so that the ring stands at one
/// reference per member and no member is registered by its construction;
/// the roots are then registered by a retain and a non-final release, which
/// is the state a candidate of a real collection is in.
///
/// # Safety
/// A quiescent heap under [`test_guard`], and `class` carries
/// [`MEMBER_PROPS`] Box properties.
unsafe fn build(arena: &mut Arena, class: *const Class, shape: Shape) -> Built {
    let arena_ptr: *mut Arena = arena;
    let mut context = LLContext { arena };
    let mut fillers = Vec::with_capacity(shape.rings * shape.members * shape.fillers);
    let mut keepers = Vec::with_capacity(if shape.live { shape.rings } else { 0 });
    let mut members = Vec::with_capacity(shape.members());
    for _ in 0..shape.rings {
        let ring: Vec<*mut Object> = (0..shape.members)
            .map(|_| unsafe {
                let member = new_constructed(&mut context, class, MemoryCategory::GcHeap);
                for _ in 0..shape.fillers {
                    fillers.push(new_constructed(&mut context, class, MemoryCategory::GcHeap));
                }
                member
            })
            .collect();

        unsafe {
            for (position, &member) in ring.iter().enumerate() {
                move_prop(member, prop_offset(0), ring[(position + 1) % shape.members]);
            }

            for &root in &ring[..shape.roots_per_ring] {
                ll_retain(root as *mut RcHeader);
                assert!(
                    !ll_release(root as *mut RcHeader),
                    "the ring's edge holds the root, so the release is not the last"
                );
            }

            if shape.live {
                // The keeper keeps its own creation reference, so nothing
                // registers it and the trace never reaches it; its edge is
                // the reference from outside that the trial deletion cannot
                // subtract, which is what makes the component live.
                let keeper = new_constructed(&mut context, class, MemoryCategory::GcHeap);
                store_prop(arena_ptr, keeper, prop_offset(0), ring[0]);
                keepers.push(keeper);
            }
        }

        members.extend(ring);
    }

    Built {
        fillers,
        keepers,
        members,
    }
}

/// What [`build`] left the case holding: the objects it must give back by
/// hand, the collection having no reason to free either.
struct Built {
    /// One per slot of every block a member stands in, holding the members
    /// apart.
    fillers: Vec<*mut Object>,
    /// One per ring of a live shape, holding the ring's first member; empty
    /// for a garbage shape.
    keepers: Vec<*mut Object>,
    /// Every member of every ring, which a live shape's teardown takes apart
    /// by hand; a garbage shape's are freed by the arm's own collection.
    members: Vec<*mut Object>,
}

/// Take a live shape's rings apart by hand — the caller's branch, a garbage
/// shape's rings having gone in the arm's own collection.
///
/// By hand and not by a collection: the arm's own collection read the
/// component live, and a commit ages what it reads, so a later trace prunes
/// at the members that were never registered and the ring waits for a
/// turnover it would take the case minutes to reach
/// (`crate::cycle::mark`, "The mature live core is not descended into").
/// The three loops are `cycle::testing::dismantle_ring`'s over a slice: every
/// member retained, so that no edge's null store frees a member the loop is
/// still walking; every edge nulled; every member released and died.
///
/// # Safety
/// `built` came from [`build`] on this thread, `arena` is this thread's, and
/// no collection is running.
unsafe fn let_the_rings_go(arena: &mut Arena, built: &Built) {
    let arena_ptr: *mut Arena = arena;
    unsafe {
        for &keeper in &built.keepers {
            store_prop(arena_ptr, keeper, prop_offset(0), std::ptr::null_mut());
        }

        for &member in &built.members {
            ll_retain(member as *mut RcHeader);
        }

        for &member in &built.members {
            store_prop(arena_ptr, member, prop_offset(0), std::ptr::null_mut());
        }

        for &member in &built.members {
            assert!(ll_release(member as *mut RcHeader), "the member's last");
            ll_object_die(member);
        }

        for &keeper in &built.keepers {
            assert!(
                ll_release(keeper as *mut RcHeader),
                "the keeper's edge is null, so its creation reference is its last"
            );
            ll_object_die(keeper);
        }
    }
}

/// Kill what held the members apart. A filler carries its creation
/// reference still, so its release is the last one and its death frees it.
///
/// # Safety
/// `fillers` came from [`build`] on this thread and no collection is
/// running.
unsafe fn kill(fillers: Vec<*mut Object>) {
    for filler in fillers {
        unsafe {
            assert!(ll_release(filler as *mut RcHeader), "the filler's last");
            ll_object_die(filler);
        }
    }
}

/// What the timed collection freed, what it cost, and what the census read
/// of it when a case armed one.
struct Collected {
    freed: usize,
    wall: Duration,
    report: Option<census::CollectionReport>,
}

/// Whether the arm reads the wall of its collection or the census of it:
/// the census walks the touched list at the scan's end, so a wall read
/// under one is the instrument's as much as the collection's.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reading {
    Wall,
    Census,
}

/// The mutator's collection, timed and counted: `collect` is the entry the
/// arm calls, `ll_gc_maybe_collect` after a take and `ll_gc_collect_cycles`
/// for the baseline.
fn the_collection(
    reading: Reading,
    control: &mut Option<Control>,
    collect: unsafe fn() -> usize,
) -> Collected {
    let armed = (reading == Reading::Census).then(census::arm);
    if let Some(control) = control.as_mut() {
        control.send("enable");
    }

    let from = Instant::now();
    let freed = unsafe { collect() };
    let wall = from.elapsed();
    if let Some(control) = control.as_mut() {
        control.send("disable");
    }

    let report = armed.map(|armed| {
        let report = census::take();
        drop(armed);
        report
    });
    Collected {
        freed,
        wall,
        report,
    }
}

/// One sample of the take arm: the elder takes the standing ring after the
/// interval, the collector is retired, and the mutator's collection over P
/// follows. The batch is the collector's own reading of its trace.
fn a_take(
    shape: Shape,
    class: *const Class,
    reading: Reading,
    control: &mut Option<Control>,
) -> (Vec<testing::TracedBatch>, Collected) {
    let mut arena = Arena::new();
    let built = unsafe { build(&mut arena, class, shape) };
    assert_eq!(
        candidate_count(),
        shape.roots(),
        "R holds the shape's roots and nothing else"
    );

    testing::take_standing_after(Some(INTERVAL));
    testing::read_traced_batches(true);
    born_over(&[record()]);
    let mut batches = Vec::new();
    assert!(
        wait_until(
            || {
                batches.extend(testing::take_traced_batches());
                !batches.is_empty()
            },
            A_BIRTH
        ),
        "the standing ring was taken inside the interval"
    );
    testing::read_traced_batches(false);
    testing::retire();
    testing::take_standing_after(None);

    let collected = the_collection(reading, control, || unsafe {
        crate::gc::ll_gc_maybe_collect()
    });
    assert_eq!(
        collected.freed,
        shape.freed_by_the_collection(),
        "the collection over P freed what the shape's liveness leaves it"
    );
    if shape.live {
        unsafe { let_the_rings_go(&mut arena, &built) };
    }

    unsafe { kill(built.fillers) };
    drop(arena);
    // The next sample starts from the queue this one started from: a live
    // shape's teardown nulls every edge, and each null store registers the
    // member it decrements (`cycle::testing::dismantle_ring`), so R would
    // carry the last sample's dead members into the next one's reading.
    reset_lanes();
    (batches, collected)
}

/// One sample of the baseline: the same rings, no collector, and the
/// mutator's own collection over R whole — the trace the take shifts rather
/// than adds.
fn collected_in_line(
    shape: Shape,
    class: *const Class,
    reading: Reading,
    control: &mut Option<Control>,
) -> Collected {
    let mut arena = Arena::new();
    let built = unsafe { build(&mut arena, class, shape) };
    assert_eq!(
        candidate_count(),
        shape.roots(),
        "R holds the shape's roots and nothing else"
    );

    let collected = the_collection(reading, control, || unsafe {
        crate::gc::ll_gc_collect_cycles()
    });
    assert_eq!(
        collected.freed,
        shape.freed_by_the_collection(),
        "the collection over R freed what the shape's liveness leaves it"
    );
    if shape.live {
        unsafe { let_the_rings_go(&mut arena, &built) };
    }

    unsafe { kill(built.fillers) };
    drop(arena);
    reset_lanes();
    collected
}

/// What the census read of one collection, in the terms the entry quotes:
/// the roots the trace attempted, the rows it met, the blocks its bump drew
/// and the members its exact validations walked.
///
/// The reading's dispatch counts are left out: they are taken at the scan's
/// end and count from the last take, so in an arm whose other collections
/// run unarmed they carry the arm's whole run rather than this collection.
fn census_line(report: &census::CollectionReport) -> String {
    let scan = report.scan.as_ref();
    let counters = &report.counters;
    format!(
        "roots {:?}, rows met {:?}, blocks touched {:?}, blocks drawn {}, \
         row arrays {}, validations {} over {} members, ending {:?}",
        scan.map(|scan| scan.roots),
        scan.map(|scan| scan.density.slotted.rows_met),
        scan.map(|scan| scan.density.slotted.blocks),
        counters.drawn_from_pool + counters.drawn_from_reserve,
        counters.row_arrays,
        counters.validations,
        counters.members_validated,
        report.close.as_ref().map(|close| close.ending),
    )
}

/// The `perf stat --control` pair, when the run is under one: a command
/// written to the control fifo and its acknowledgement read from the other,
/// so that the counting interval covers the timed collection and not the
/// build before it (`benches/census_driver.rs` opens the same pair).
struct Control {
    control: std::fs::File,
    ack: BufReader<std::fs::File>,
}

impl Control {
    /// The pair `dev/tools/take_perf.sh` names, or `None` when the run is
    /// not under `perf`.
    fn from_env() -> Option<Self> {
        let control = std::env::var("LL_PERF_CTL").ok()?;
        let ack = std::env::var("LL_PERF_ACK").ok()?;
        Some(Self {
            control: std::fs::OpenOptions::new()
                .write(true)
                .open(control)
                .expect("the control fifo opens"),
            ack: BufReader::new(std::fs::File::open(ack).expect("the ack fifo opens")),
        })
    }

    fn send(&mut self, command: &str) {
        writeln!(self.control, "{command}").expect("the command is written");
        self.control.flush().expect("and flushed");
        // perf writes `ack\n\0`, so the NUL of one acknowledgement opens the
        // next line and is trimmed with it.
        let mut line = String::new();
        self.ack.read_line(&mut line).expect("perf acknowledges");
        assert_eq!(
            line.trim_matches(|c: char| c.is_whitespace() || c == '\0'),
            "ack",
            "perf acknowledged {command:?} with {line:?}"
        );
    }
}

/// Whether the arm named by `LL_TAKE_ARM` is this one; every arm runs when
/// the variable is unset, which is how the probe is run without `perf`.
fn selected(shape: Shape, arm: &str) -> bool {
    match std::env::var("LL_TAKE_ARM") {
        Ok(named) => named == format!("{}:{arm}", shape.name),
        Err(_) => true,
    }
}

/// The middle of the samples, the arm sorted in place. The smallest is read
/// beside it at the printout, the two answering differently on a loaded box
/// (`dev/BENCHMARKS.md`, "the statistic that decides the answer").
fn median(samples: &mut [Duration]) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

/// One arm of the probe.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arm {
    /// The take, then the mutator's collection over P.
    Take,
    /// No take, and the mutator's collection over R whole.
    Baseline,
    /// The baseline a second time, whose difference from the first is the
    /// error bar every other difference is read against.
    Control,
}

impl Arm {
    fn name(self) -> &'static str {
        match self {
            Arm::Take => "take",
            Arm::Baseline => "baseline",
            Arm::Control => "control",
        }
    }
}

/// Run `arm` over `shape`: the timed samples, then one sample with the
/// census armed, whose counts are exact and whose wall is the instrument's.
fn the_arm(
    arm: Arm,
    shape: Shape,
    class: *const Class,
    control: &mut Option<Control>,
) -> (
    Vec<Duration>,
    Vec<testing::TracedBatch>,
    census::CollectionReport,
) {
    let mut walls = Vec::with_capacity(SAMPLES);
    let mut traced = Vec::new();
    for sample in 0..SAMPLES {
        let collected = match arm {
            Arm::Take => {
                let (batches, collected) = a_take(shape, class, Reading::Wall, control);
                if sample >= WARM_UP {
                    traced.extend(batches);
                }

                collected
            }
            Arm::Baseline | Arm::Control => collected_in_line(shape, class, Reading::Wall, control),
        };
        if sample >= WARM_UP {
            walls.push(collected.wall);
        }
    }

    let counted = match arm {
        Arm::Take => a_take(shape, class, Reading::Census, &mut None).1,
        Arm::Baseline | Arm::Control => collected_in_line(shape, class, Reading::Census, &mut None),
    };
    (walls, traced, counted.report.expect("the census was armed"))
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn what_a_take_costs_by_the_shape_of_its_roots() {
    let _g = test_guard();
    let _record = record();
    let _end = RetireOnDrop;
    reset_lanes();
    let _wait = testing::HeldRequestWait::crate_own();
    let mut control = Control::from_env();
    let class = {
        let mut builder = ClassBuilder::new("TakeCostNode");
        for property in 0..MEMBER_PROPS {
            builder = builder.prop(&format!("p{property}"), true);
        }

        builder.build()
    };

    for shape in [OVERLAPPING, DISJOINT, OVERLAPPING_LIVE, DISJOINT_LIVE] {
        for arm in [Arm::Take, Arm::Baseline, Arm::Control] {
            if !selected(shape, arm.name()) {
                continue;
            }

            let (mut walls, traced, report) = the_arm(arm, shape, class, &mut control);
            let smallest = walls.iter().copied().min().expect("the arm has samples");
            println!(
                "{} over the {} shape: the mutator's collection {:?}, least {:?} \
                 (of {} samples); the collector's batches {:?}; the census reads {}",
                arm.name(),
                shape.name,
                median(&mut walls),
                smallest,
                SAMPLES - WARM_UP,
                traced,
                census_line(&report),
            );
        }
    }
}
