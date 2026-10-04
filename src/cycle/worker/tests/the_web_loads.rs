//! The web loads' request (`dev/design/the-web-loads.md`, "`web-heap`, a
//! request"): the draws that shape one request, the request built
//! object by object along its drawn timeline, its end, and the garbage it
//! leaves, in bytes by size. The rig's loop and the arrivals, the spin and the
//! waits' durations drive what is here; the cases below read each figure once
//! on an input whose answer is known.
//!
//! **The shape.** A context cycle of [`CONTEXT`] objects holds the request's
//! external reference. Under it a payload tree, each object hung in a free
//! child slot of an earlier one, taken breadth first, at most [`FAN_OUT`] a
//! parent; 45 % of the tree's objects are closures, with an edge to a context
//! object (a cycle through the context) or, one time in two, to the core, and
//! 1 % of the others hold an edge into the core. Zero to three ORM
//! collections hang in the tree, each a head, a chain of segments of
//! [`SEGMENT_ENTITIES`] and its entities, every entity's edge back to its
//! head. The objects are of three sizes, 64, 128 and 512 bytes.
//!
//! **The timeline.** A request's life is read as a place in `[0, 1)` of its
//! drawn CPU, which its two drawn waits split into three phases: every birth
//! and every registration has its place on it, and [`Request::advance`] does
//! what stands before a place. The context is born at zero and the rest in
//! build order, uniformly; each registration falls at a uniform place, on an
//! object drawn uniformly among those born before it. A request's
//! [`LOOKUPS`] cache lookups fall at uniform places too.
//!
//! **The long-lived state** (`dev/design/the-web-loads.md`, "Long-lived
//! state per mutator") is [`LongLived`]: a core of one strongly
//! connected component the requests' edges point into, an LRU cache of
//! ten-object values and the sessions, the last two hung from the core by
//! directory trees, so that a trace from a core root reaches all of it. The
//! cache's recency and keys are kept by the rig outside the heap
//! ([`KeyedLru`]); the heap sees a hit as a registration of the value and a
//! miss as an eviction's non-final decrement and a new value. The setup runs
//! the cache to its steady state outside the heap before it builds the
//! values, and registers what a long-running server holds registered.
//!
//! **The two variants** ([`Variant`]). In `web-heap` a request's objects are
//! GC heap objects and it holds its session by a reference of its own. In
//! `web-arena` they are objects of the mutator's arena, which no collection
//! scans: they register nothing, and the request ends in
//! `promote::arena_reset_full`. Its heap references — the session, the
//! lookups' values and [`ARENA_CORE_EDGES`] edges into the core — are stores
//! into arena slots that the store barrier logs, released at the reset, and
//! 30 % of its requests write an object into the session, which escapes and
//! is promoted into a block the reset retains (`dev/design/the-web-loads.md`,
//! "`web-arena`, a request").

use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::queue::POLL_STRIDE;
use crate::cycle::testing::move_prop;
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::memory::context::LLContext;
use crate::memory::heap::{entity_bytes_held, entity_bytes_held_by_a_walk, size_class_index};
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{
    CANDIDATE_BIT, MemoryCategory, RcHeader, ll_release, ll_retain, mutator_flags,
};
use crate::test_support::{entity_checked, prop_offset, store_prop};
use std::time::{Duration, Instant};
use the_rig::register;

/// The objects of the context cycle, the request's first (Express's
/// `req.res`/`res.req`, Octane's `clone $this->app`).
pub(super) const CONTEXT: usize = 8;

/// The most children a payload object holds.
pub(super) const FAN_OUT: usize = 8;

/// An ORM collection's entities a segment object holds.
pub(super) const SEGMENT_ENTITIES: usize = 30;

/// The drawn object count's bound from above: one draw in about 9,000 lands
/// past it, and it keeps a request's registrations and bytes inside what a
/// cell can hold.
pub(super) const MOST_OBJECTS: usize = 400_000;

/// The registrations one [`Request::advance`] makes at most, half the poll's
/// stride, so that a poll between two advances always finds R within it.
pub(super) const REGISTRATIONS_AN_ADVANCE: usize = POLL_STRIDE / 2;

/// The births one step of a request's build makes before the rig polls
/// (`Request::advance_at_most`): a compiled build loop polls on its
/// back-edge, and `POLL_STRIDE` is the runtime's own stride for a loop it
/// cannot see inside (`ll_release_vector`). It bounds births, not time.
pub(super) const BIRTHS_AN_ADVANCE: usize = POLL_STRIDE;

/// The cache lookups of a request [A].
pub(super) const LOOKUPS: usize = 20;

/// The core's objects [A].
pub(super) const CORE_OBJECTS: usize = 5_000;

/// The sessions [A].
pub(super) const SESSIONS: usize = 1_000;

/// The objects of a cache value and of a session [A].
pub(super) const VALUE_OBJECTS: usize = 10;
pub(super) const SESSION_OBJECTS: usize = 20;

/// The keys a lookup draws over, per value the cache holds.
pub(super) const KEYS_A_VALUE: usize = 4;

/// The requests a session stands untouched before the next touch replaces it
/// [A]: with touches uniform over [`SESSIONS`], e^(−4.6) of them come after
/// a longer gap, which puts the replacements at 1 % of requests.
pub(super) const SESSION_IDLE_REQUESTS: u64 = 4_600;

/// The heap references a `web-arena` request stores into arena slots [A],
/// each logged for release at its reset: its session, its lookups' values and
/// the rest edges into the core.
pub(super) const LOGGED_REFERENCES: usize = 300;

/// The edges into the core a `web-arena` request makes: its logged
/// references less the session's and the lookups'.
pub(super) const ARENA_CORE_EDGES: usize = LOGGED_REFERENCES - LOOKUPS - 1;

/// The share of `web-arena`'s requests that write an object into their
/// session, which escapes the arena [A].
pub(super) const SESSION_WRITE_PERCENT: f64 = 30.0;

/// The share of sessions holding a write at the steady state: a session
/// lives a geometric count of touches, one in a hundred replacing it, and a
/// touch writes at [`SESSION_WRITE_PERCENT`], so the share holding none is
/// E[0.7^k] = 0.007 / 0.307.
pub(super) const SESSION_WRITE_STEADY_SHARE: f64 = 1.0 - 0.007 / 0.307;

/// The slot of a session's head a write takes: slot 0 chains the cycle and
/// slot 1 holds its core edge.
const SESSION_WRITE_SLOT: u32 = 2;

/// A request's drawn CPU: lognormal, median 4 ms, σ 1 [A]
/// (`dev/design/the-web-loads.md`, "The loads").
pub(super) const REQUEST_CPU_MEDIAN: Duration = Duration::from_millis(4);

/// Each of a request's two blocking waits: lognormal, median 3 ms, σ 1 [A].
pub(super) const WAIT_MEDIAN: Duration = Duration::from_millis(3);

/// The σ of both timing draws.
const TIMING_SIGMA: f64 = 1.0;

/// The longest a timing draw may be [A]: one draw in about ten million
/// passes it, and one that did would hold its mutator for the cell's
/// arrivals.
const LONGEST_TIMING: Duration = Duration::from_secs(1);

/// The share of the wall a worker is busy or waiting at the specification's
/// arrival rate [A].
pub(super) const BUSY_SHARE: f64 = 0.6;

/// A timing draw of `median` and [`TIMING_SIGMA`], clamped at
/// [`LONGEST_TIMING`].
fn draw_a_duration(draws: &mut Draws, median: Duration) -> Duration {
    Duration::from_secs_f64(draws.lognormal(median.as_secs_f64(), TIMING_SIGMA)).min(LONGEST_TIMING)
}

/// The specification's mean interarrival, E[CPU] + E[waits] over
/// [`BUSY_SHARE`], a lognormal's mean being its median times e^(σ²/2):
/// 27.48 ms. The protocol replaces it by a pilot's where a request's build
/// is not free (`dev/design/the-web-loads.md`, "Common to both").
pub(super) fn specified_interarrival() -> Duration {
    let mean_of =
        |median: Duration| median.as_secs_f64() * (TIMING_SIGMA * TIMING_SIGMA / 2.0).exp();
    Duration::from_secs_f64((mean_of(REQUEST_CPU_MEDIAN) + 2.0 * mean_of(WAIT_MEDIAN)) / BUSY_SHARE)
}

/// A mutator's open-loop Poisson arrivals: exponential interarrivals of mean
/// `mean`, from [`Purpose::Arrivals`], as offsets from the start barrier. The
/// same instants in every arm, the draws being seeded as the plans' are.
pub(super) struct Arrivals {
    draws: Draws,
    mean: Duration,
    last: Duration,
}

impl Arrivals {
    pub(super) fn new(mutator: u64, repeat: u64, mean: Duration) -> Self {
        Self {
            draws: Draws::new(mutator, repeat, Purpose::Arrivals),
            mean,
            last: Duration::ZERO,
        }
    }

    /// The next arrival's offset from the start barrier.
    pub(super) fn next(&mut self) -> Duration {
        self.last += Duration::from_secs_f64(-self.draws.unit().ln() * self.mean.as_secs_f64());
        self.last
    }
}

/// Where a request's objects live (`dev/design/the-web-loads.md`, "The loads"): in the
/// GC heap, `web-heap`, or in the mutator's arena, reset at the request's
/// end, `web-arena`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Variant {
    Heap,
    Arena,
}

/// The entries of one directory object: every slot of the largest class.
const DIRECTORY_FAN_OUT: usize = slots(LARGEST);

/// The three object sizes, in bytes, and their shares in percent.
pub(super) const SIZES: [usize; 3] = [64, 128, 512];
const SHARES_PERCENT: [usize; 3] = [50, 35, 15];

/// The index of the largest of [`SIZES`], which an ORM head and its segments
/// take.
const LARGEST: u8 = 2;

/// What a stream of draws serves. Each purpose draws from a stream of its
/// own, so that a step that adds draws of one purpose moves no request's
/// shape.
#[derive(Clone, Copy)]
pub(super) enum Purpose {
    Shape = 1,
    Registrations = 2,
    /// The instants requests arrive at (`Arrivals`).
    Arrivals = 3,
    /// A request's lookups: their places and keys.
    Cache = 4,
    /// The objects a miss or a session's replacement builds.
    Values = 5,
    /// A request's session.
    Sessions = 6,
    /// The long-lived state's setup: the core, the cache's run to its steady
    /// state, the values and sessions it builds, the sessions' idle ages.
    Core = 7,
    /// A request's drawn CPU and its two waits' durations.
    Timing = 8,
}

/// The streams a mutator's plans are drawn from, one a purpose.
pub(super) struct Streams {
    pub(super) shape: Draws,
    pub(super) registrations: Draws,
    pub(super) cache: Draws,
    pub(super) sessions: Draws,
    pub(super) timing: Draws,
}

impl Streams {
    pub(super) fn new(mutator: u64, repeat: u64) -> Self {
        Self {
            shape: Draws::new(mutator, repeat, Purpose::Shape),
            registrations: Draws::new(mutator, repeat, Purpose::Registrations),
            cache: Draws::new(mutator, repeat, Purpose::Cache),
            sessions: Draws::new(mutator, repeat, Purpose::Sessions),
            timing: Draws::new(mutator, repeat, Purpose::Timing),
        }
    }

    /// The four streams' checksums, folded.
    pub(super) fn checksum(&self) -> u64 {
        [
            &self.shape,
            &self.registrations,
            &self.cache,
            &self.sessions,
        ]
        .iter()
        .fold(0, |sum, draws| sum.rotate_left(13) ^ draws.checksum())
    }
}

/// SplitMix64 over a seed made of the mutator, the repeat and the purpose:
/// the same sequence in every arm of a comparison, which is what pairs its
/// cells. The checksum folds every value drawn, for the line to print.
pub(super) struct Draws {
    state: u64,
    checksum: u64,
}

impl Draws {
    pub(super) fn new(mutator: u64, repeat: u64, purpose: Purpose) -> Self {
        Self {
            state: mutator.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ repeat.wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
                ^ (purpose as u64).wrapping_mul(0xD1B5_4A32_D192_ED03),
            checksum: 0,
        }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        self.checksum = self.checksum.rotate_left(5) ^ z;
        z
    }

    /// Uniform in `(0, 1)`: never zero, so that its logarithm is finite.
    pub(super) fn unit(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    /// Uniform in `0..n`.
    pub(super) fn below(&mut self, n: usize) -> usize {
        ((u128::from(self.next()) * n as u128) >> 64) as usize
    }

    /// Uniform in `low..=high`.
    pub(super) fn between(&mut self, low: usize, high: usize) -> usize {
        low + self.below(high - low + 1)
    }

    /// Lognormal of `median` and `sigma`, the normal by Box–Muller.
    pub(super) fn lognormal(&mut self, median: f64, sigma: f64) -> f64 {
        let (u, v) = (self.unit(), self.unit());
        let normal = (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos();
        median * (sigma * normal).exp()
    }

    /// Whether a draw falls under `percent` of 100.
    fn percent(&mut self, percent: f64) -> bool {
        self.unit() * 100.0 < percent
    }

    /// Every value drawn so far, folded.
    pub(super) fn checksum(&self) -> u64 {
        self.checksum
    }
}

/// Zipf with exponent 1 over `0..n`: index `k` drawn in proportion to
/// `1 / (k + 1)`, by a search of the cumulative weights.
pub(super) struct Zipf {
    cumulative: Vec<f64>,
}

impl Zipf {
    pub(super) fn new(n: usize) -> Self {
        let mut sum = 0.0;
        let cumulative = (1..=n)
            .map(|rank| {
                sum += 1.0 / rank as f64;
                sum
            })
            .collect();
        Self { cumulative }
    }

    /// An index in `0..n`; `n` must not be zero.
    pub(super) fn draw(&self, draws: &mut Draws) -> usize {
        let total = *self.cumulative.last().expect("a Zipf over no index");
        let at = draws.unit() * total;
        self.cumulative
            .partition_point(|&sum| sum < at)
            .min(self.cumulative.len() - 1)
    }
}

/// The three classes of [`SIZES`]: a header and a class word of sixteen
/// bytes, and sixteen a Box property, as `member_class` lays one. Copied
/// freely: the classes live for the process.
#[derive(Clone, Copy)]
pub(super) struct WebClasses([*const Class; 3]);

impl WebClasses {
    /// Classes named after `prefix`, which a case makes its own.
    pub(super) fn new(prefix: &str) -> Self {
        Self(std::array::from_fn(|size_index| {
            let mut builder = ClassBuilder::new(&format!("{prefix}{}", SIZES[size_index]));
            for property in 0..slots(size_index as u8) {
                builder = builder.prop(&format!("p{property}"), true);
            }

            builder.build()
        }))
    }
}

/// The Box properties an object of size index `size_index` has.
pub(super) const fn slots(size_index: u8) -> usize {
    (SIZES[size_index as usize] - 16) / 16
}

/// The edge an object holds in its slot 0 besides its children, by the index
/// of its target.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ExtraEdge {
    None,
    /// To context object `i`: a closure over the request.
    Context(u32),
    /// To the core's object `k`.
    Core(u32),
    /// An ORM entity's edge back to its collection's head, the object at
    /// that index.
    BackTo(u32),
}

/// One object of a plan: its size index, the object and slot its creation
/// reference goes into (none for the context, which is linked as a ring),
/// its extra edge, and its place on the timeline. 32 bytes, so that a plan
/// of [`MOST_OBJECTS`] takes 13 MB.
#[derive(Clone, Copy, Debug)]
pub(super) struct Placement {
    pub(super) size_index: u8,
    pub(super) parent: Option<(u32, u8)>,
    pub(super) extra: ExtraEdge,
    pub(super) born_at: f64,
}

const _: () = assert!(size_of::<Placement>() == 32);

/// A request as its draws shape it, before anything is built: the objects in
/// build order, the context first; the registrations as (place, object), in
/// the order of their places; the lookups as (place, key), in the same
/// order; the session's slot; the ORM collections' entity counts; the two
/// waits' places; and whether the end's release lands on the registered
/// context root. A `web-arena` plan has no registrations and no silent end,
/// its objects registering nothing, and draws its session write instead.
pub(super) struct Plan {
    pub(super) variant: Variant,
    pub(super) objects: Vec<Placement>,
    pub(super) registrations: Vec<(f64, u32)>,
    pub(super) lookups: Vec<(f64, u32)>,
    pub(super) session: u32,
    pub(super) orm: Vec<usize>,
    pub(super) waits_at: [f64; 2],
    /// The request's drawn CPU, the synthetic work its three phases spin
    /// with its build and its polls inside, and its two waits' durations,
    /// slept without a poll at `waits_at`.
    pub(super) cpu: Duration,
    pub(super) waits: [Duration; 2],
    pub(super) silent_end: bool,
    /// The size index of the object a `web-arena` request writes into its
    /// session, none where it writes nothing.
    pub(super) session_write: Option<u8>,
}

/// What a plan draws its long-lived targets over: the core's objects and
/// the cache's keys, each Zipf (s = 1), and the session slots, uniform; and
/// the variant its requests are built in.
pub(super) struct Targets {
    core: Zipf,
    keys: Zipf,
    session_slots: usize,
    variant: Variant,
}

impl Targets {
    pub(super) fn new(shape: &LongLivedShape, variant: Variant) -> Self {
        Self {
            variant,
            core: Zipf::new(shape.core),
            keys: Zipf::new(shape.values * KEYS_A_VALUE),
            session_slots: shape.sessions,
        }
    }
}

impl Plan {
    /// A request of the specification's count ([`draw_the_count`]) over
    /// `targets`, those of the long-lived state it is built with.
    pub(super) fn draw(streams: &mut Streams, targets: &Targets) -> Self {
        let (orm, count) = draw_the_count(&mut streams.shape);
        Self::shaped(streams, targets, orm, count)
    }

    /// A request of `count` objects with drawn collections, for a case that
    /// fixes the size; `targets` as [`Plan::draw`] takes them.
    pub(super) fn with_count(streams: &mut Streams, targets: &Targets, count: usize) -> Self {
        let orm = draw_the_orm(&mut streams.shape);
        let count = count.max(CONTEXT + orm_objects(&orm));
        Self::shaped(streams, targets, orm, count)
    }

    fn shaped(streams: &mut Streams, targets: &Targets, orm: Vec<usize>, count: usize) -> Self {
        let variant = targets.variant;
        let mut objects =
            place_the_objects(&mut streams.shape, &targets.core, &orm, count, variant);
        draw_the_births(&mut streams.shape, &mut objects);
        let registrations = match variant {
            Variant::Heap => draw_the_registrations(&mut streams.registrations, &objects),
            Variant::Arena => Vec::new(),
        };
        let mut waits_at = [streams.shape.unit(), streams.shape.unit()];
        waits_at.sort_unstable_by(f64::total_cmp);
        let (silent_end, session_write) = match variant {
            Variant::Heap => (streams.shape.percent(50.0), None),
            Variant::Arena => (
                false,
                streams
                    .shape
                    .percent(SESSION_WRITE_PERCENT)
                    .then(|| draw_a_size_index(&mut streams.shape)),
            ),
        };
        let mut lookups: Vec<(f64, u32)> = (0..LOOKUPS)
            .map(|_| {
                let at = streams.cache.unit();
                (at, targets.keys.draw(&mut streams.cache) as u32)
            })
            .collect();
        lookups.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        Self {
            variant,
            objects,
            registrations,
            lookups,
            session: streams.sessions.below(targets.session_slots) as u32,
            orm,
            waits_at,
            cpu: draw_a_duration(&mut streams.timing, REQUEST_CPU_MEDIAN),
            waits: [
                draw_a_duration(&mut streams.timing, WAIT_MEDIAN),
                draw_a_duration(&mut streams.timing, WAIT_MEDIAN),
            ],
            silent_end,
            session_write,
        }
    }

    /// The bytes the plan's objects take, by size index.
    pub(super) fn bytes(&self) -> [usize; 3] {
        let mut bytes = [0; 3];
        for object in &self.objects {
            bytes[object.size_index as usize] += SIZES[object.size_index as usize];
        }

        bytes
    }
}

/// The context and the payload tree of `count` objects with the collections
/// `orm`, in build order, their places on the timeline still to draw. In the
/// arena variant [`ARENA_CORE_EDGES`] tree objects, or every one of a smaller
/// tree, drawn without replacement before the placement, hold the core edges
/// and draw no closure, and a closure's edge goes to the context.
fn place_the_objects(
    shape: &mut Draws,
    core: &Zipf,
    orm: &[usize],
    count: usize,
    variant: Variant,
) -> Vec<Placement> {
    let mut objects = Vec::with_capacity(count);
    for _ in 0..CONTEXT {
        objects.push(Placement {
            size_index: draw_a_size_index(shape),
            parent: None,
            extra: ExtraEdge::None,
            born_at: 0.0,
        });
    }

    // The free child slots, breadth first: (object, slot).
    let mut free_slots = std::collections::VecDeque::new();
    for (index, object) in objects.iter().enumerate() {
        // Slot 0 links the ring.
        for slot in 1..slots(object.size_index).min(FAN_OUT + 1) {
            free_slots.push_back((index as u32, slot as u8));
        }
    }

    let tree = count - CONTEXT - orm_objects(orm);
    let mut core_edges_left = match variant {
        Variant::Heap => 0,
        Variant::Arena => ARENA_CORE_EDGES.min(tree),
    };
    // The tree's positions at which each collection's head is placed.
    let mut heads_at: Vec<usize> = orm.iter().map(|_| shape.below(tree.max(1))).collect();
    heads_at.sort_unstable();
    let mut heads_at = heads_at.into_iter().zip(orm.iter().copied()).peekable();
    for position in 0..tree {
        while let Some(&(at, entities)) = heads_at.peek()
            && at == position
        {
            place_a_collection(shape, &mut objects, &mut free_slots, entities);
            heads_at.next();
        }

        let size_index = draw_a_size_index(shape);
        let extra = match variant {
            Variant::Heap => draw_an_extra_edge(shape, core),
            Variant::Arena => {
                draw_an_arena_extra_edge(shape, core, tree - position, &mut core_edges_left)
            }
        };
        let parent = free_slots
            .pop_front()
            .expect("every tree object adds a free slot at least");
        let index = objects.len() as u32;
        objects.push(Placement {
            size_index,
            parent: Some(parent),
            extra,
            born_at: 0.0,
        });
        let first = usize::from(extra != ExtraEdge::None);
        for slot in first..slots(size_index).min(first + FAN_OUT) {
            free_slots.push_back((index, slot as u8));
        }
    }

    for (_, entities) in heads_at {
        place_a_collection(shape, &mut objects, &mut free_slots, entities);
    }

    objects
}

/// A tree object's extra edge: a closure's, 45 %, to a context object or,
/// one time in two, to the core; 1 % of the others into the core.
fn draw_an_extra_edge(shape: &mut Draws, core: &Zipf) -> ExtraEdge {
    if shape.percent(45.0) {
        if shape.percent(50.0) {
            ExtraEdge::Core(core.draw(shape) as u32)
        } else {
            ExtraEdge::Context(shape.below(CONTEXT) as u32)
        }
    } else if shape.percent(1.0) {
        ExtraEdge::Core(core.draw(shape) as u32)
    } else {
        ExtraEdge::None
    }
}

/// A tree object's extra edge in the arena variant, `positions_left` tree
/// positions standing from this one: a core edge with the chance of the
/// `core_edges_left` still to place over those positions (Knuth's selection
/// sampling, algorithm S), else a closure's edge to the context at 45 %.
fn draw_an_arena_extra_edge(
    shape: &mut Draws,
    core: &Zipf,
    positions_left: usize,
    core_edges_left: &mut usize,
) -> ExtraEdge {
    if shape.below(positions_left) < *core_edges_left {
        *core_edges_left -= 1;
        ExtraEdge::Core(core.draw(shape) as u32)
    } else if shape.percent(45.0) {
        ExtraEdge::Context(shape.below(CONTEXT) as u32)
    } else {
        ExtraEdge::None
    }
}

/// The births after the context, uniform over the life and in build order,
/// so that a parent is always born before its child.
fn draw_the_births(shape: &mut Draws, objects: &mut [Placement]) {
    let mut births: Vec<f64> = (CONTEXT..objects.len()).map(|_| shape.unit()).collect();
    births.sort_unstable_by(f64::total_cmp);
    for (object, at) in objects[CONTEXT..].iter_mut().zip(births) {
        object.born_at = at;
    }
}

/// As many registrations as 10 % of the objects after the context, each at
/// a uniform place on an object born before it, in the order of their
/// places.
fn draw_the_registrations(registrations: &mut Draws, objects: &[Placement]) -> Vec<(f64, u32)> {
    let planned = (CONTEXT..objects.len())
        .filter(|_| registrations.percent(10.0))
        .count();
    let mut registered: Vec<(f64, u32)> = (0..planned)
        .map(|_| {
            let at = registrations.unit();
            let born = objects.partition_point(|object| object.born_at < at);
            (at, registrations.below(born) as u32)
        })
        .collect();
    registered.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    registered
}

/// The collections and the object count of the specification: lognormal,
/// median 10,000, σ 1, clamped from below at the context and the ORM objects
/// and from above at [`MOST_OBJECTS`].
fn draw_the_count(shape: &mut Draws) -> (Vec<usize>, usize) {
    let orm = draw_the_orm(shape);
    let least = CONTEXT + orm_objects(&orm);
    let count = (shape.lognormal(10_000.0, 1.0).round() as usize).clamp(least, MOST_OBJECTS);
    (orm, count)
}

/// Zero to three collections of 10 to 100 entities each.
fn draw_the_orm(shape: &mut Draws) -> Vec<usize> {
    (0..shape.between(0, 3))
        .map(|_| shape.between(10, 100))
        .collect()
}

/// The objects a set of collections takes: a head, its segments and its
/// entities each.
fn orm_objects(orm: &[usize]) -> usize {
    orm.iter()
        .map(|&entities| 1 + entities.div_ceil(SEGMENT_ENTITIES) + entities)
        .sum()
}

fn draw_a_size_index(shape: &mut Draws) -> u8 {
    let mut at = shape.below(100);
    for (size_index, &share) in SHARES_PERCENT.iter().enumerate() {
        if at < share {
            return size_index as u8;
        }

        at -= share;
    }

    unreachable!("the shares sum to 100")
}

/// A collection of `entities`: its head, of the largest size, in the tree's
/// next free slot; a chain of segments of the largest size from the head's
/// slot 1, each holding the next in its slot 0 and up to
/// [`SEGMENT_ENTITIES`] entities from its slot 1; each entity of a drawn size
/// with its edge back to the head in its slot 0. The collection is exempt
/// from the fan-out and adds no free slot to the tree.
fn place_a_collection(
    shape: &mut Draws,
    objects: &mut Vec<Placement>,
    free_slots: &mut std::collections::VecDeque<(u32, u8)>,
    entities: usize,
) {
    let head = objects.len() as u32;
    objects.push(Placement {
        size_index: LARGEST,
        parent: Some(free_slots.pop_front().expect("a free slot for the head")),
        extra: ExtraEdge::None,
        born_at: 0.0,
    });
    let mut holder = (head, 1);
    for segment in 0..entities.div_ceil(SEGMENT_ENTITIES) {
        let index = objects.len() as u32;
        objects.push(Placement {
            size_index: LARGEST,
            parent: Some(holder),
            extra: ExtraEdge::None,
            born_at: 0.0,
        });
        holder = (index, 0);
        let in_segment = (entities - segment * SEGMENT_ENTITIES).min(SEGMENT_ENTITIES);
        for slot in 1..=in_segment {
            objects.push(Placement {
                size_index: draw_a_size_index(shape),
                parent: Some((index, slot as u8)),
                extra: ExtraEdge::BackTo(head),
                born_at: 0.0,
            });
        }
    }
}

/// What a request is built with: the thread's context and arena, and the
/// long-lived state its edges, lookups and session reach.
pub(super) struct RequestBuild<'a> {
    pub(super) context: LLContext,
    pub(super) arena: *mut Arena,
    pub(super) long_lived: &'a mut LongLived,
}

impl RequestBuild<'_> {
    /// A new object of size index `size_index`, in the heap or the arena by
    /// `variant`.
    ///
    /// # Safety
    /// As [`Request::start`].
    unsafe fn new_object(&mut self, size_index: usize, variant: Variant) -> *mut Object {
        let category = match variant {
            Variant::Heap => MemoryCategory::GcHeap,
            Variant::Arena => MemoryCategory::RequestArena,
        };
        unsafe {
            new_constructed(
                &mut self.context,
                self.long_lived.classes.0[size_index],
                category,
            )
        }
    }

    /// Put `child`, just built, into `holder`'s empty slot `slot`: in the
    /// heap its creation reference moves into the slot, and in the arena a
    /// store counts and logs nothing.
    ///
    /// # Safety
    /// As [`Request::start`]; both are of `variant`.
    unsafe fn link(&self, holder: *mut Object, slot: u32, child: *mut Object, variant: Variant) {
        match variant {
            Variant::Heap => unsafe { move_prop(holder, prop_offset(slot), child) },
            Variant::Arena => unsafe { store_prop(self.arena, holder, prop_offset(slot), child) },
        }
    }
}

/// What one step of a request did: the bytes born and the bytes that stopped
/// being reachable, by size index, the registrations made and the lookups
/// done.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) struct Advanced {
    pub(super) born: [usize; 3],
    pub(super) ended: [usize; 3],
    pub(super) registered: usize,
    pub(super) looked_up: usize,
}

impl Advanced {
    fn add(&mut self, other: Advanced) {
        for size_index in 0..3 {
            self.born[size_index] += other.born[size_index];
            self.ended[size_index] += other.ended[size_index];
        }

        self.registered += other.registered;
        self.looked_up += other.looked_up;
    }
}

/// Advance `request` to `to` in steps of at most [`BIRTHS_AN_ADVANCE`]
/// births, `step` after each with what it did — the rig's account and poll,
/// answering the poll's wall — as a compiled build loop polls on its
/// back-edge; answer the walls summed and where the last step stopped. A
/// stop at the registrations' bound ([`Stop::Events`]) leaves the rest for
/// a later point, as one unbounded advance does. Each step's wall, the poll
/// apart, is kept for [`longest_build_step`].
///
/// # Safety
/// As [`Request::advance`].
pub(super) unsafe fn build_to(
    request: &mut Request,
    build: &mut RequestBuild,
    to: f64,
    mut step: impl FnMut(Advanced) -> Duration,
) -> (Duration, Stop) {
    let mut walls = Duration::ZERO;
    loop {
        crate::cycle::worker::testing::enter_the_rig_section(
            crate::cycle::worker::testing::RigSection::Build,
        );
        let began = Instant::now();
        let (advanced, stop) = unsafe { request.advance_at_most(build, to, BIRTHS_AN_ADVANCE) };
        LONGEST_BUILD_STEP_NANOS.fetch_max(began.elapsed().as_nanos() as u64, Ordering::Relaxed);
        walls += step(advanced);
        if stop != Stop::Births {
            return (walls, stop);
        }
    }
}

/// The longest step [`build_to`] made, in nanoseconds, since the process
/// started.
static LONGEST_BUILD_STEP_NANOS: AtomicU64 = AtomicU64::new(0);

/// The longest step of a request's build since the process started.
pub(super) fn longest_build_step() -> Duration {
    Duration::from_nanos(LONGEST_BUILD_STEP_NANOS.load(Ordering::Relaxed))
}

/// Where [`Request::advance_at_most`] stopped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Stop {
    /// Every event placed before its point is done.
    Reached,
    /// Its births' bound, a birth before its point still to come.
    Births,
    /// [`REGISTRATIONS_AN_ADVANCE`] registrations and lookups, the rest left
    /// for the next advance to a later point.
    Events,
}

/// What a request's end did: whether the external reference landed on a
/// registered object, read before the release — the silent death — the
/// request's heap bytes by size index, garbage from here on, and the roots
/// the end registered. A `web-arena` request's end is its reset: never
/// silent, no heap bytes, and its registrations those of the reset's logged
/// releases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Ended {
    pub(super) silent: bool,
    pub(super) bytes: [usize; 3],
    pub(super) registered: usize,
    /// Of a `web-arena` reset's registrations, the cache values: the
    /// distinct value heads its lookups stored standing no candidate before
    /// it; zero for `web-heap`, whose values register at their hits
    /// (`dev/design/the-web-loads.md`, the protocol's "distinct cache values
    /// registered").
    pub(super) values_registered: usize,
}

/// A request in flight: its plan, the objects born so far, where on the
/// timeline its births, registrations and lookups stand, and the session it
/// holds.
pub(super) struct Request {
    plan: Plan,
    objects: Vec<*mut Object>,
    registrations_done: usize,
    lookups_done: usize,
    /// The context object the request's external reference is on; null in
    /// the arena variant, whose objects the reset frees.
    externally_held: *mut Object,
    /// The head of the session: held by the request's own reference, or in
    /// the arena variant by slot 0 of [`Request::locals`].
    session: *mut Object,
    /// The arena variant's locals, an arena object of the largest class
    /// whose slot 0 holds the session and whose slots from 1 the lookups'
    /// values, each a logged heap reference; null in the heap variant.
    locals: *mut Object,
}

/// The next thing a request does on its timeline.
#[derive(Clone, Copy)]
enum Event {
    Birth,
    /// A registration of the object born at this index.
    Registration(u32),
    /// A lookup of this key.
    Lookup(u32),
}

impl Request {
    /// Start `plan`: its session opened and held, its context cycle built,
    /// the external reference taken — on the context's first object where
    /// the end is silent, its second otherwise — and the first object
    /// registered. In the arena variant the context is the arena's, and the
    /// session is held by slot 0 of the request's locals, with no external
    /// reference and no registration.
    ///
    /// # Safety
    /// `build` is the calling mutator's, at a point where it may allocate;
    /// `plan` was drawn over the targets of `build`'s long-lived state.
    pub(super) unsafe fn start(build: &mut RequestBuild, plan: Plan) -> (Self, Advanced) {
        let (session, mut advanced) =
            unsafe { build.long_lived.open_session(build.arena, plan.session) };
        let variant = plan.variant;
        let mut objects = Vec::with_capacity(plan.objects.len());
        for object in &plan.objects[..CONTEXT] {
            let size_index = object.size_index as usize;
            objects.push(unsafe { build.new_object(size_index, variant) });
            if variant == Variant::Heap {
                advanced.born[size_index] += SIZES[size_index];
            }
        }

        for index in 0..CONTEXT {
            unsafe { build.link(objects[index], 0, objects[(index + 1) % CONTEXT], variant) };
        }

        let (externally_held, locals) = match variant {
            Variant::Heap => {
                let externally_held = objects[usize::from(!plan.silent_end)];
                unsafe {
                    ll_retain(session as *mut RcHeader);
                    ll_retain(externally_held as *mut RcHeader);
                    register(objects[0]);
                }

                advanced.registered += 1;
                (externally_held, std::ptr::null_mut())
            }
            Variant::Arena => {
                let locals = unsafe { build.new_object(LARGEST as usize, variant) };
                unsafe { store_prop(build.arena, locals, prop_offset(0), session) };
                (std::ptr::null_mut(), locals)
            }
        };
        let request = Self {
            plan,
            objects,
            registrations_done: 0,
            lookups_done: 0,
            externally_held,
            session,
            locals,
        };
        (request, advanced)
    }

    /// Do what is placed before `to` on the timeline, births, registrations
    /// and lookups in the order of their places, until
    /// [`REGISTRATIONS_AN_ADVANCE`] registrations and lookups together, each
    /// able to register one root; the rest waits for the next advance. A
    /// `to` of 1 or more is the life's end, before which everything stands.
    ///
    /// # Safety
    /// As [`Request::start`], on the same `build`.
    pub(super) unsafe fn advance(&mut self, build: &mut RequestBuild, to: f64) -> Advanced {
        unsafe { self.advance_at_most(build, to, usize::MAX) }.0
    }

    /// [`Request::advance`] stopped before its `births + 1`-th birth: what it
    /// did, and where it stopped. Called again with the same `to` after a
    /// [`Stop::Births`], it goes on from there, the events in the order one
    /// unbounded advance makes them. `births` is at least one.
    ///
    /// # Safety
    /// As [`Request::advance`].
    pub(super) unsafe fn advance_at_most(
        &mut self,
        build: &mut RequestBuild,
        to: f64,
        births: usize,
    ) -> (Advanced, Stop) {
        debug_assert!(births > 0, "a bound of no births advances nothing");
        let mut advanced = Advanced::default();
        let (mut events, mut born) = (0, 0);
        while let Some(event) = self.next_event(to) {
            if matches!(event, Event::Birth) {
                if born == births {
                    return (advanced, Stop::Births);
                }

                born += 1;
            } else {
                if events == REGISTRATIONS_AN_ADVANCE {
                    return (advanced, Stop::Events);
                }

                events += 1;
            }

            match event {
                Event::Birth => {
                    let size_index = unsafe { self.give_birth(build) };
                    if self.plan.variant == Variant::Heap {
                        advanced.born[size_index] += SIZES[size_index];
                    }
                }
                Event::Registration(index) => {
                    let object = *self
                        .objects
                        .get(index as usize)
                        .expect("a registration's object is born before its place");
                    unsafe { register(object) };
                    self.registrations_done += 1;
                    advanced.registered += 1;
                }
                Event::Lookup(key) => {
                    // The arena variant's value goes into the next slot of
                    // its locals, from slot 1.
                    let into = (!self.locals.is_null())
                        .then_some((self.locals, 1 + self.lookups_done as u32));
                    advanced.add(unsafe { build.long_lived.look_up(build.arena, key, into) });
                    self.lookups_done += 1;
                }
            }
        }

        (advanced, Stop::Reached)
    }

    /// The next event placed before `to`, the earliest of the next birth,
    /// registration and lookup; at one place a birth goes first, then a
    /// registration.
    fn next_event(&self, to: f64) -> Option<Event> {
        let birth = self
            .plan
            .objects
            .get(self.objects.len())
            .map(|object| (object.born_at, Event::Birth));
        let registration = self
            .plan
            .registrations
            .get(self.registrations_done)
            .map(|&(at, index)| (at, Event::Registration(index)));
        let lookup = self
            .plan
            .lookups
            .get(self.lookups_done)
            .map(|&(at, key)| (at, Event::Lookup(key)));
        [birth, registration, lookup]
            .into_iter()
            .flatten()
            .filter(|&(at, _)| at < to || to >= 1.0)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, event)| event)
    }

    /// Build the next object of the plan in its parent's slot, with its
    /// extra edge. Answers its size index.
    ///
    /// # Safety
    /// As [`Request::advance`].
    unsafe fn give_birth(&mut self, build: &mut RequestBuild) -> usize {
        let object = self.plan.objects[self.objects.len()];
        let size_index = object.size_index as usize;
        let born = unsafe { build.new_object(size_index, self.plan.variant) };
        let (parent, slot) = object.parent.expect("a born object has a parent");
        unsafe {
            build.link(
                self.objects[parent as usize],
                u32::from(slot),
                born,
                self.plan.variant,
            )
        };
        let target = match object.extra {
            ExtraEdge::None => std::ptr::null_mut(),
            ExtraEdge::Context(index) | ExtraEdge::BackTo(index) => self.objects[index as usize],
            ExtraEdge::Core(index) => build.long_lived.core[index as usize],
        };
        if !target.is_null() {
            unsafe { store_prop(build.arena, born, prop_offset(0), target) };
        }

        self.objects.push(born);
        size_index
    }

    /// The objects born so far.
    pub(super) fn born(&self) -> usize {
        self.objects.len()
    }

    /// Whether every birth, registration and lookup is done.
    pub(super) fn is_complete(&self) -> bool {
        self.objects.len() == self.plan.objects.len()
            && self.registrations_done == self.plan.registrations.len()
            && self.lookups_done == self.plan.lookups.len()
    }

    /// The object born `index`-th, for a case that reads the request back.
    pub(super) fn object(&self, index: usize) -> *mut Object {
        self.objects[index]
    }

    /// The plan the request was started with.
    pub(super) fn plan(&self) -> &Plan {
        &self.plan
    }

    /// End the request: in the heap variant [`Request::end`], in the arena
    /// variant [`Request::reset`].
    ///
    /// # Safety
    /// As [`Request::start`], on the same `build`; the request is complete.
    pub(super) unsafe fn finish(self, build: &mut RequestBuild) -> Ended {
        match self.plan.variant {
            Variant::Heap => unsafe { self.end() },
            Variant::Arena => unsafe { self.reset(build) },
        }
    }

    /// Release the session and the external reference.
    ///
    /// # Safety
    /// As [`Request::start`]; the request is complete and of the heap
    /// variant.
    pub(super) unsafe fn end(self) -> Ended {
        assert!(self.is_complete(), "a request ends after its last event");
        assert_eq!(self.plan.variant, Variant::Heap);
        let silent = is_a_candidate(self.externally_held);
        let session_registers = !is_a_candidate(self.session);
        unsafe {
            assert!(
                !ll_release(self.session as *mut RcHeader),
                "the session's directory slot and its cycle hold its head"
            );
            assert!(
                !ll_release(self.externally_held as *mut RcHeader),
                "the context ring holds the object the reference was on"
            );
        }

        Ended {
            silent,
            bytes: self.plan.bytes(),
            registered: usize::from(!silent) + usize::from(session_registers),
            values_registered: 0,
        }
    }

    /// Write into the session where the plan says so, and reset the arena
    /// by `promote::arena_reset_full`: the write escapes and is promoted,
    /// its block retained, and each logged heap reference is released,
    /// registering its target where that was no candidate. The write the
    /// session held before dies of the store's release. The registrations
    /// are the thread's admissions across the reset.
    ///
    /// # Safety
    /// As [`Request::start`], on the same `build`; the request is complete
    /// and of the arena variant, and nothing else of the arena is live.
    pub(super) unsafe fn reset(self, build: &mut RequestBuild) -> Ended {
        assert!(self.is_complete(), "a request ends after its last event");
        assert_eq!(self.plan.variant, Variant::Arena);
        if let Some(size_index) = self.plan.session_write {
            unsafe {
                build
                    .long_lived
                    .write_into_session(build.arena, self.session, size_index)
            };
        }

        let mut values: Vec<*mut Object> = (1..=self.lookups_done as u32)
            .map(|slot| slot_of(self.locals, slot))
            .filter(|&value| !value.is_null() && !is_a_candidate(value))
            .collect();
        values.sort_unstable();
        values.dedup();
        let before = crate::refcount::admissions();
        unsafe { reset_the_arena(build.arena) };
        Ended {
            silent: false,
            bytes: [0; 3],
            registered: crate::refcount::admissions() - before,
            values_registered: values.len(),
        }
    }
}

/// The long-lived state's sizes: the core's objects, the values the cache
/// holds (N), and the sessions.
#[derive(Clone, Copy)]
pub(super) struct LongLivedShape {
    pub(super) core: usize,
    pub(super) values: usize,
    pub(super) sessions: usize,
}

impl LongLivedShape {
    /// The specification's, at a cache of `values`.
    pub(super) const fn specified(values: usize) -> Self {
        Self {
            core: CORE_OBJECTS,
            values,
            sessions: SESSIONS,
        }
    }
}

/// What the cache and the sessions did since the setup. The distinct values
/// registered since the setup are `hits_registering` and
/// `evictions_registering` together, each value registering once in its
/// life, at a hit or at its eviction.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) struct CacheCounts {
    pub(super) hits: usize,
    /// Hits that found the value no candidate, and registered it.
    pub(super) hits_registering: usize,
    pub(super) misses: usize,
    /// Evictions of a value that was a candidate already, whose death
    /// registers nothing.
    pub(super) evictions_silent: usize,
    pub(super) evictions_registering: usize,
    pub(super) sessions_replaced: usize,
    /// Objects written into a session, the setup's steady state included.
    pub(super) session_writes: usize,
}

/// An LRU of a fixed number of entries over the keys `0..keys`, kept
/// outside the heap: which entry holds each key, the recency order of the
/// entries as a doubly linked list, and whether each entry was hit since
/// its key was inserted. A miss fills the next unused entry until every
/// entry is used, and evicts the least recently used one after.
pub(super) struct KeyedLru {
    entry_of_key: Vec<u32>,
    key_of_entry: Vec<u32>,
    /// Toward the most recently used entry, and toward the least.
    newer: Vec<u32>,
    older: Vec<u32>,
    newest: u32,
    oldest: u32,
    used: usize,
    hit_since_insertion: Vec<bool>,
}

/// In [`KeyedLru`]'s tables: no entry, or no key.
const ABSENT: u32 = u32::MAX;

/// What a [`KeyedLru`] lookup did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum LookupOutcome {
    /// The key was held, by this entry.
    Hit(u32),
    /// The key went into an entry no key had used.
    Inserted,
    /// The key went into the least recently used entry, whose key left;
    /// whether that entry had been hit since its insertion.
    Evicted { entry: u32, was_hit: bool },
}

impl KeyedLru {
    /// An empty LRU of `entries` over the keys `0..keys`, at least as many.
    pub(super) fn new(entries: usize, keys: usize) -> Self {
        assert!(entries > 0 && entries <= keys && keys < ABSENT as usize);
        Self {
            entry_of_key: vec![ABSENT; keys],
            key_of_entry: vec![ABSENT; entries],
            newer: vec![ABSENT; entries],
            older: vec![ABSENT; entries],
            newest: ABSENT,
            oldest: ABSENT,
            used: 0,
            hit_since_insertion: vec![false; entries],
        }
    }

    /// Look `key` up, and make its entry the most recently used.
    pub(super) fn look_up(&mut self, key: u32) -> LookupOutcome {
        let entry = self.entry_of_key[key as usize];
        if entry != ABSENT {
            self.unlink(entry);
            self.push_newest(entry);
            self.hit_since_insertion[entry as usize] = true;
            return LookupOutcome::Hit(entry);
        }

        let (entry, outcome) = if self.used < self.key_of_entry.len() {
            self.used += 1;
            (self.used as u32 - 1, LookupOutcome::Inserted)
        } else {
            let entry = self.oldest;
            self.unlink(entry);
            self.entry_of_key[self.key_of_entry[entry as usize] as usize] = ABSENT;
            let evicted = LookupOutcome::Evicted {
                entry,
                was_hit: self.hit_since_insertion[entry as usize],
            };
            (entry, evicted)
        };
        self.entry_of_key[key as usize] = entry;
        self.key_of_entry[entry as usize] = key;
        self.hit_since_insertion[entry as usize] = false;
        self.push_newest(entry);
        outcome
    }

    /// The keys from the most recently used entry to the least.
    pub(super) fn keys_by_recency(&self) -> Vec<u32> {
        let mut keys = Vec::with_capacity(self.used);
        let mut entry = self.newest;
        while entry != ABSENT {
            keys.push(self.key_of_entry[entry as usize]);
            entry = self.older[entry as usize];
        }

        keys
    }

    fn unlink(&mut self, entry: u32) {
        let (newer, older) = (self.newer[entry as usize], self.older[entry as usize]);
        if newer == ABSENT {
            self.newest = older;
        } else {
            self.older[newer as usize] = older;
        }

        if older == ABSENT {
            self.oldest = newer;
        } else {
            self.newer[older as usize] = newer;
        }
    }

    fn push_newest(&mut self, entry: u32) {
        self.newer[entry as usize] = ABSENT;
        self.older[entry as usize] = self.newest;
        if self.newest == ABSENT {
            self.oldest = entry;
        } else {
            self.newer[self.newest as usize] = entry;
        }

        self.newest = entry;
    }
}

/// A tree of largest-class objects whose leaves hold a fixed number of
/// entries, [`DIRECTORY_FAN_OUT`] a leaf, each inner level built the same way
/// up to one root, and the bytes of the cycle each entry holds. Entry `i` is
/// slot `i % DIRECTORY_FAN_OUT` of leaf `i / DIRECTORY_FAN_OUT`.
struct Directory {
    leaves: Vec<*mut Object>,
    bytes: Vec<[u32; 3]>,
}

impl Directory {
    /// A directory of `entries`, empty. Answers it, its root with its
    /// creation reference for the caller to move into a slot, and the objects
    /// it took.
    ///
    /// # Safety
    /// `context` is the calling mutator's, at a point where it may allocate.
    unsafe fn new(
        context: &mut LLContext,
        classes: &WebClasses,
        entries: usize,
    ) -> (Self, *mut Object, usize) {
        let class = classes.0[LARGEST as usize];
        let mut objects = 0;
        let mut level = |count: usize| {
            objects += count;
            (0..count)
                .map(|_| unsafe { new_constructed(context, class, MemoryCategory::GcHeap) })
                .collect::<Vec<*mut Object>>()
        };
        let leaves = level(entries.div_ceil(DIRECTORY_FAN_OUT).max(1));
        let mut below = leaves.clone();
        while below.len() > 1 {
            let above = level(below.len().div_ceil(DIRECTORY_FAN_OUT));
            for (index, &child) in below.iter().enumerate() {
                unsafe {
                    move_prop(
                        above[index / DIRECTORY_FAN_OUT],
                        prop_offset((index % DIRECTORY_FAN_OUT) as u32),
                        child,
                    )
                };
            }

            below = above;
        }

        let directory = Self {
            leaves,
            bytes: vec![[0; 3]; entries],
        };
        (directory, below[0], objects)
    }

    /// The object and the slot entry `entry` stands in.
    fn slot(&self, entry: u32) -> (*mut Object, u32) {
        let entry = entry as usize;
        (
            self.leaves[entry / DIRECTORY_FAN_OUT],
            (entry % DIRECTORY_FAN_OUT) as u32,
        )
    }

    /// The head entry `entry` holds, null for none.
    fn head(&self, entry: u32) -> *mut Object {
        let (holder, slot) = self.slot(entry);
        slot_of(holder, slot)
    }
}

/// Which of the two directories an entry is in.
#[derive(Clone, Copy)]
enum DirectoryOf {
    Values,
    Sessions,
}

impl DirectoryOf {
    /// The objects of the cycle an entry holds.
    fn cycle_objects(self) -> usize {
        match self {
            Self::Values => VALUE_OBJECTS,
            Self::Sessions => SESSION_OBJECTS,
        }
    }
}

/// A mutator's long-lived state (`dev/design/the-web-loads.md`, "Long-lived
/// state per mutator"). Built by [`LongLived::new`] and
/// registered to its steady state by [`LongLived::register_the_steady_state`];
/// held by the creation reference on the core's first object until
/// [`LongLived::let_go`].
///
/// **The core.** [`LongLivedShape::core`] objects, the first of the largest
/// class and the rest of drawn sizes; slot 0 of each holds the next and the
/// last's holds the first, which makes the core one strongly connected
/// component; slot 1 of each holds another core object drawn uniformly, but
/// the first's slots 1 and 2 hold the roots of the cache's and the sessions'
/// directories.
///
/// **A value and a session.** A cycle of [`VALUE_OBJECTS`] or
/// [`SESSION_OBJECTS`] of drawn sizes ([`build_a_cycle`]), its head in a
/// directory slot, so that the head stands at two references.
///
/// **The draws.** The setup's from [`Purpose::Core`], the objects built
/// after it from [`Purpose::Values`], both seeded by the mutator and the
/// repeat; none depends on the collector, so every arm builds the same state
/// and makes the same hits and misses.
pub(super) struct LongLived {
    classes: WebClasses,
    targets: Targets,
    core: Vec<*mut Object>,
    values: Directory,
    lru: KeyedLru,
    sessions: Directory,
    /// The request count at each session's last touch.
    last_touch: Vec<u64>,
    /// Requests opened, from [`FIRST_REQUEST`].
    requests: u64,
    built_after_the_setup: Draws,
    /// The checksum of every draw the setup made.
    setup_checksum: u64,
    /// The bytes the state holds reachable, by size index.
    held: [usize; 3],
    pub(super) counts: CacheCounts,
}

/// The request count the setup stands at, above any drawn idle age.
const FIRST_REQUEST: u64 = 1 << 40;

impl LongLived {
    /// Build the state of `shape` for mutator `mutator` in repeat `repeat`,
    /// with classes `classes`: the core; the cache's keys run through its LRU
    /// outside the heap until it holds [`LongLivedShape::values`] keys and
    /// then until it has evicted as many, which is its steady state; the
    /// values its entries hold then; the sessions, each idle for an age drawn
    /// geometric with a mean of as many requests as there are sessions, the
    /// gap between two touches of one. For [`Variant::Arena`], each session
    /// then holds a write with the share [`SESSION_WRITE_STEADY_SHARE`],
    /// each promoted by an arena reset of its own so that each retains a
    /// block of its own, as a request's write does. Registers nothing.
    ///
    /// # Safety
    /// `arena` is the calling mutator's, at a point where it may allocate;
    /// here alone it also holds nothing live, the arena variant's setup
    /// resetting it.
    pub(super) unsafe fn new(
        classes: WebClasses,
        shape: LongLivedShape,
        variant: Variant,
        mutator: u64,
        repeat: u64,
        arena: *mut Arena,
    ) -> Self {
        assert!(shape.core > 0 && shape.values > 0 && shape.sessions > 0);
        let mut context = LLContext { arena };
        let mut setup = Draws::new(mutator, repeat, Purpose::Core);
        let targets = Targets::new(&shape, variant);
        let (core, held) = unsafe { build_the_core(arena, &classes, &mut setup, shape.core) };
        let lru = run_to_the_steady_state(shape.values, &targets.keys, &mut setup);
        let (values, values_root, value_directory_objects) =
            unsafe { Directory::new(&mut context, &classes, shape.values) };
        let (sessions, sessions_root, session_directory_objects) =
            unsafe { Directory::new(&mut context, &classes, shape.sessions) };
        unsafe {
            move_prop(core[0], prop_offset(1), values_root);
            move_prop(core[0], prop_offset(2), sessions_root);
        }

        let mut state = Self {
            classes,
            core,
            values,
            lru,
            sessions,
            last_touch: Vec::with_capacity(shape.sessions),
            requests: FIRST_REQUEST,
            built_after_the_setup: Draws::new(mutator, repeat, Purpose::Values),
            setup_checksum: 0,
            held,
            counts: CacheCounts::default(),
            targets,
        };
        state.held[LARGEST as usize] +=
            (value_directory_objects + session_directory_objects) * SIZES[LARGEST as usize];
        for entry in 0..shape.values as u32 {
            let _ = unsafe { state.fill(arena, DirectoryOf::Values, entry, Some(&mut setup)) };
        }

        // The logarithm of the chance that one request leaves a session
        // untouched.
        let ln_untouched = (1.0 - 1.0 / shape.sessions as f64).ln();
        for slot in 0..shape.sessions as u32 {
            let _ = unsafe { state.fill(arena, DirectoryOf::Sessions, slot, Some(&mut setup)) };
            let idle = (setup.unit().ln() / ln_untouched) as u64;
            state.last_touch.push(FIRST_REQUEST - idle);
        }

        if variant == Variant::Arena {
            unsafe { state.write_the_steady_sessions(arena, &mut setup) };
        }

        state.setup_checksum = setup.checksum();
        state
    }

    /// The setup's draws and those of what was built after it, folded: with
    /// [`Streams::checksum`], what tells two arms' draws apart.
    pub(super) fn draws_checksum(&self) -> u64 {
        self.setup_checksum.rotate_left(29) ^ self.built_after_the_setup.checksum()
    }

    /// The value heads and the session heads standing candidate: the
    /// registered stock of the state, read by a walk of their heads.
    pub(super) fn standing_candidate(&self) -> (usize, usize) {
        let count = |directory: &Directory| {
            (0..directory.bytes.len() as u32)
                .filter(|&entry| is_a_candidate(directory.head(entry)))
                .count()
        };
        (count(&self.values), count(&self.sessions))
    }

    /// Give each session a write with the share [`SESSION_WRITE_STEADY_SHARE`],
    /// its size drawn from `setup`, each promoted by an arena reset of its
    /// own so that each retains a block of its own, as a request's write
    /// does.
    ///
    /// # Safety
    /// As [`LongLived::new`].
    unsafe fn write_the_steady_sessions(&mut self, arena: *mut Arena, setup: &mut Draws) {
        for slot in 0..self.sessions.bytes.len() as u32 {
            if setup.unit() >= SESSION_WRITE_STEADY_SHARE {
                continue;
            }

            let session = self.sessions.head(slot);
            let size_index = draw_a_size_index(setup);
            unsafe {
                self.write_into_session(arena, session, size_index);
                reset_the_arena(arena);
            }
        }
    }

    /// Write a new arena object of size index `size_index` into slot
    /// [`SESSION_WRITE_SLOT`] of `session`'s head: an escape the next reset
    /// of `arena` promotes. The write the slot held dies of the store's
    /// release.
    ///
    /// # Safety
    /// `arena` is the calling mutator's, at a point where it may allocate;
    /// `session` is a session head of this state.
    unsafe fn write_into_session(
        &mut self,
        arena: *mut Arena,
        session: *mut Object,
        size_index: u8,
    ) {
        let mut context = LLContext { arena };
        let write = unsafe {
            new_constructed(
                &mut context,
                self.classes.0[size_index as usize],
                MemoryCategory::RequestArena,
            )
        };
        unsafe { store_prop(arena, session, prop_offset(SESSION_WRITE_SLOT), write) };
        self.counts.session_writes += 1;
    }

    /// The distinct blocks the sessions' writes hold: each write's own, and
    /// the block its survivor list stands in where that is another. What
    /// the live state keeps retained, apart from the garbage's share.
    pub(super) fn blocks_the_writes_hold(&self) -> usize {
        let mut blocks = std::collections::HashSet::new();
        for slot in 0..self.sessions.bytes.len() as u32 {
            let write = slot_of(self.sessions.head(slot), SESSION_WRITE_SLOT);
            if write.is_null() {
                continue;
            }

            let block = crate::memory::block_pool::BlockHeader::of_ptr(write as *const u8) as usize;
            blocks.insert(block);
            // SAFETY: a write stands in the session only once the reset
            // after it promoted it, so its block is a retained block, held
            // for the write while the write lives.
            let list = unsafe { crate::memory::retained::survivor_list_holder(block) };
            if list != 0 {
                blocks.insert(list);
            }
        }

        blocks.len()
    }

    /// Register what a long-running server holds registered: every core
    /// object, which the requests' dead edges into it register within the
    /// warm-up; every value hit since its insertion, or in the arena variant
    /// every value, which the reset of the request that stored it registers;
    /// and every session, which its first request registers. `poll` runs after each
    /// [`REGISTRATIONS_AN_ADVANCE`] registrations, within the poll's stride.
    /// Answers the registrations made.
    ///
    /// # Safety
    /// The state is this thread's, and no request is in flight.
    pub(super) unsafe fn register_the_steady_state(&mut self, poll: &mut dyn FnMut()) -> usize {
        let hit_values: Vec<*mut Object> = (0..self.values.bytes.len() as u32)
            .filter(|&entry| {
                self.targets.variant == Variant::Arena
                    || self.lru.hit_since_insertion[entry as usize]
            })
            .map(|entry| self.values.head(entry))
            .collect();
        let sessions = (0..self.sessions.bytes.len() as u32).map(|slot| self.sessions.head(slot));
        let mut registered = 0;
        for object in self.core.iter().copied().chain(hit_values).chain(sessions) {
            unsafe { register(object) };
            registered += 1;
            if registered % REGISTRATIONS_AN_ADVANCE == 0 {
                poll();
            }
        }

        registered
    }

    /// Look `key` up: a hit registers the value, a miss replaces the least
    /// recently used value by the key's ([`LongLived::replace`]). With
    /// `into`, an arena object and its empty slot, the value found or built
    /// is stored there in place of a hit's registration: a heap reference the
    /// arena's reset releases. Answers the step as [`Advanced`], one lookup.
    ///
    /// # Safety
    /// `arena` is the calling mutator's, at a point where it may allocate.
    pub(super) unsafe fn look_up(
        &mut self,
        arena: *mut Arena,
        key: u32,
        into: Option<(*mut Object, u32)>,
    ) -> Advanced {
        let (entry, mut advanced) = match self.lru.look_up(key) {
            LookupOutcome::Hit(entry) if into.is_some() => {
                self.counts.hits += 1;
                (entry, Advanced::default())
            }
            LookupOutcome::Hit(entry) => {
                let head = self.values.head(entry);
                let registers = !is_a_candidate(head);
                unsafe { register(head) };
                self.counts.hits += 1;
                self.counts.hits_registering += usize::from(registers);
                let advanced = Advanced {
                    registered: usize::from(registers),
                    ..Advanced::default()
                };
                (entry, advanced)
            }
            LookupOutcome::Evicted { entry, .. } => {
                let replaced = unsafe { self.replace(arena, DirectoryOf::Values, entry) };
                self.counts.misses += 1;
                if replaced.registered == 0 {
                    self.counts.evictions_silent += 1;
                } else {
                    self.counts.evictions_registering += 1;
                }

                (entry, replaced)
            }
            LookupOutcome::Inserted => unreachable!("the setup filled every entry"),
        };
        if let Some((holder, slot)) = into {
            let head = self.values.head(entry);
            unsafe { store_prop(arena, holder, prop_offset(slot), head) };
        }

        advanced.looked_up = 1;
        advanced
    }

    /// Open a request's session in `slot`: a session untouched for more than
    /// [`SESSION_IDLE_REQUESTS`] is replaced first ([`LongLived::replace`]).
    /// Answers the session's head, which the caller takes a reference on, and
    /// the step.
    ///
    /// # Safety
    /// `arena` is the calling mutator's, at a point where it may allocate.
    unsafe fn open_session(&mut self, arena: *mut Arena, slot: u32) -> (*mut Object, Advanced) {
        self.requests += 1;
        let mut advanced = Advanced::default();
        if self.requests - self.last_touch[slot as usize] > SESSION_IDLE_REQUESTS {
            advanced = unsafe { self.replace(arena, DirectoryOf::Sessions, slot) };
            self.counts.sessions_replaced += 1;
        }

        self.last_touch[slot as usize] = self.requests;
        (self.sessions.head(slot), advanced)
    }

    /// Replace the cycle entry `entry` of `of` holds: its slot nulled, a
    /// non-final decrement on the head its cycle still holds, which registers
    /// the head where it was no candidate; then a new cycle built in the
    /// slot. Answers the old cycle's bytes as ended, the new one's as born,
    /// and the registration.
    ///
    /// # Safety
    /// `arena` is the calling mutator's, at a point where it may allocate.
    unsafe fn replace(&mut self, arena: *mut Arena, of: DirectoryOf, entry: u32) -> Advanced {
        let directory = match of {
            DirectoryOf::Values => &self.values,
            DirectoryOf::Sessions => &self.sessions,
        };
        let (holder, slot) = directory.slot(entry);
        let registers = !is_a_candidate(slot_of(holder, slot));
        let ended = to_usize(directory.bytes[entry as usize]);
        unsafe { store_prop(arena, holder, prop_offset(slot), std::ptr::null_mut()) };
        for size_index in 0..3 {
            self.held[size_index] -= ended[size_index];
        }

        let born = to_usize(unsafe { self.fill(arena, of, entry, None) });
        Advanced {
            born,
            ended,
            registered: usize::from(registers),
            looked_up: 0,
        }
    }

    /// Build a cycle into the empty slot of `entry` in `of`, its sizes and
    /// its core edge drawn from `setup`, or from the stream of what is built
    /// after the setup where it is none. Answers its bytes by size index.
    ///
    /// # Safety
    /// `arena` is the calling mutator's, at a point where it may allocate.
    unsafe fn fill(
        &mut self,
        arena: *mut Arena,
        of: DirectoryOf,
        entry: u32,
        setup: Option<&mut Draws>,
    ) -> [u32; 3] {
        let draws = setup.unwrap_or(&mut self.built_after_the_setup);
        let core = self.core[self.targets.core.draw(draws)];
        let (head, bytes) =
            unsafe { build_a_cycle(arena, &self.classes, draws, of.cycle_objects(), core) };
        let directory = match of {
            DirectoryOf::Values => &mut self.values,
            DirectoryOf::Sessions => &mut self.sessions,
        };
        let (holder, slot) = directory.slot(entry);
        unsafe { move_prop(holder, prop_offset(slot), head) };
        directory.bytes[entry as usize] = bytes;
        for size_index in 0..3 {
            self.held[size_index] += bytes[size_index] as usize;
        }

        bytes
    }

    /// The targets a plan for this state draws over.
    pub(super) fn targets(&self) -> &Targets {
        &self.targets
    }

    /// The bytes the state holds reachable, by size index.
    pub(super) fn held(&self) -> [usize; 3] {
        self.held
    }

    /// Release the core's creation reference, after which the whole state is
    /// garbage: a non-final decrement, the ring holding the first object.
    /// Answers the bytes the state held, which stop being reachable.
    ///
    /// # Safety
    /// As [`LongLived::new`]; no request is in flight.
    pub(super) unsafe fn let_go(self) -> [usize; 3] {
        unsafe {
            assert!(
                !ll_release(self.core[0] as *mut RcHeader),
                "the core's ring holds its first object"
            );
        }

        self.held
    }
}

/// A core of `objects` ([`LongLived`]'s "The core" less the directories),
/// its sizes and its other edges drawn from `setup`. Answers the objects,
/// the first holding its creation reference, and their bytes by size index.
///
/// # Safety
/// `arena` is the calling mutator's, at a point where it may allocate.
unsafe fn build_the_core(
    arena: *mut Arena,
    classes: &WebClasses,
    setup: &mut Draws,
    objects: usize,
) -> (Vec<*mut Object>, [usize; 3]) {
    let mut context = LLContext { arena };
    let mut held = [0; 3];
    let core: Vec<*mut Object> = (0..objects)
        .map(|index| {
            let size_index = if index == 0 {
                LARGEST
            } else {
                draw_a_size_index(setup)
            } as usize;
            held[size_index] += SIZES[size_index];
            unsafe { new_constructed(&mut context, classes.0[size_index], MemoryCategory::GcHeap) }
        })
        .collect();
    for index in 1..objects {
        unsafe { move_prop(core[index - 1], prop_offset(0), core[index]) };
    }

    unsafe { store_prop(arena, core[objects - 1], prop_offset(0), core[0]) };
    for index in 1..objects {
        // Another object: one of the other `objects - 1`, counted on from
        // this one.
        let other = core[(index + 1 + setup.below(objects - 1)) % objects];
        unsafe { store_prop(arena, core[index], prop_offset(1), other) };
    }

    (core, held)
}

/// An LRU of `values` entries run on keys drawn from `keys` until it holds
/// `values` keys and then until it has evicted as many, its steady state by
/// an independent simulation (`dev/BENCHMARKS.md`, "readings the S65 and S67
/// stage notes held, carried at the stages' close", the web loads' cache).
fn run_to_the_steady_state(values: usize, keys: &Zipf, setup: &mut Draws) -> KeyedLru {
    let mut lru = KeyedLru::new(values, values * KEYS_A_VALUE);
    let mut evictions = 0;
    while evictions < values {
        if matches!(
            lru.look_up(keys.draw(setup) as u32),
            LookupOutcome::Evicted { .. }
        ) {
            evictions += 1;
        }
    }

    lru
}

/// Build a cycle of `objects` objects of drawn sizes: a chain through slot
/// 0 from the head, the last object's slot 1 holding the head, and the
/// head's slot 1 holding `core`. Answers the head, holding its creation
/// reference, and the bytes by size index.
///
/// # Safety
/// `arena` is the calling mutator's, at a point where it may allocate; `core`
/// is a live object of its heap.
unsafe fn build_a_cycle(
    arena: *mut Arena,
    classes: &WebClasses,
    draws: &mut Draws,
    objects: usize,
    core: *mut Object,
) -> (*mut Object, [u32; 3]) {
    assert!(
        objects >= 2,
        "the back-edge and the core edge take two objects"
    );
    let mut context = LLContext { arena };
    let mut bytes = [0; 3];
    let chain: Vec<*mut Object> = (0..objects)
        .map(|_| {
            let size_index = draw_a_size_index(draws) as usize;
            bytes[size_index] += SIZES[size_index] as u32;
            unsafe { new_constructed(&mut context, classes.0[size_index], MemoryCategory::GcHeap) }
        })
        .collect();
    for index in 1..objects {
        unsafe { move_prop(chain[index - 1], prop_offset(0), chain[index]) };
    }

    unsafe {
        store_prop(arena, chain[objects - 1], prop_offset(1), chain[0]);
        store_prop(arena, chain[0], prop_offset(1), core);
    }

    (chain[0], bytes)
}

/// Reset `arena` by `promote::arena_reset_full`, which severs no edge here:
/// the pool refuses no survivor cell a web load asks for.
///
/// # Safety
/// As `promote::arena_reset_full`.
unsafe fn reset_the_arena(arena: *mut Arena) {
    let severed = unsafe { crate::promote::arena_reset_full(arena) };
    assert_eq!(severed, 0, "the reset severed no edge");
}

/// Whether `object` stands registered as a candidate.
fn is_a_candidate(object: *mut Object) -> bool {
    let flags = unsafe { mutator_flags(object as *const RcHeader) };
    flags & CANDIDATE_BIT != 0
}

/// The value held in `object`'s slot `slot`, null for none.
fn slot_of(object: *mut Object, slot: u32) -> *mut Object {
    unsafe { entity_checked(&*Object::prop_at(object, prop_offset(slot))) as *mut Object }
}

fn to_usize(bytes: [u32; 3]) -> [usize; 3] {
    bytes.map(|bytes| bytes as usize)
}

/// The bytes this thread's entity heap holds for it in the classes of
/// [`SIZES`]. O(1).
pub(super) fn held_by_size() -> [usize; 3] {
    let held = entity_bytes_held();
    SIZES.map(|size| held[size_class_index(size).expect("a small size")])
}

/// A mutator's garbage: the bytes its entity heap holds less what it keeps
/// reachable, by size index, integrated over time at the events that change
/// it. Garbage moves at this mutator's polls, where the collections and their
/// teardowns free; at a request's end, where its bytes stop being reachable;
/// and at a step of a request that ends bytes of the long-lived state, an
/// eviction or a session's replacement ([`Advanced::ended`]). A birth moves
/// the held and the reachable bytes together, but for a birth whose
/// allocation collects — a refill that takes remote returns, which the web
/// loads make none of, or a pressure collection. So [`Garbage::read`] after
/// each of those events integrates it exactly. The count begins before the
/// long-lived state is built, whose bytes are then born into it.
pub(super) struct Garbage {
    /// What the heap held when the count began, taken as reachable.
    baseline: [usize; 3],
    reachable: [usize; 3],
    current: [usize; 3],
    /// The window's start and the last reading's instant.
    from: Instant,
    since: Instant,
    /// Bytes times nanoseconds since the window's start, by size index.
    integral: [u128; 3],
    /// The window's highest reading by size index, and of the sum over them.
    peak: [usize; 3],
    peak_sum: usize,
    /// Bytes that stopped being reachable over the window: the garbage made,
    /// the flow the frees answer.
    made: usize,
}

impl Garbage {
    /// Begin at `now` with the heap holding `held`, all of it reachable.
    pub(super) fn new(now: Instant, held: [usize; 3]) -> Self {
        Self {
            baseline: held,
            reachable: [0; 3],
            current: [0; 3],
            from: now,
            since: now,
            integral: [0; 3],
            peak: [0; 3],
            peak_sum: 0,
            made: 0,
        }
    }

    /// `bytes` were born reachable.
    pub(super) fn born(&mut self, bytes: [usize; 3]) {
        for size_index in 0..3 {
            self.reachable[size_index] += bytes[size_index];
        }
    }

    /// `bytes` stopped being reachable: a request ended, or the long-lived
    /// state let a value or a session go.
    pub(super) fn ended(&mut self, bytes: [usize; 3]) {
        for size_index in 0..3 {
            self.reachable[size_index] -= bytes[size_index];
        }
        self.made += bytes.iter().sum::<usize>();
    }

    /// The heap holds `held` at `now`: the garbage since the last reading is
    /// integrated at its value then, and read anew.
    pub(super) fn read(&mut self, now: Instant, held: [usize; 3]) {
        let elapsed = now.saturating_duration_since(self.since).as_nanos();
        for size_index in 0..3 {
            self.integral[size_index] += self.current[size_index] as u128 * elapsed;
            self.current[size_index] = held[size_index]
                .checked_sub(self.baseline[size_index] + self.reachable[size_index])
                .expect("the heap holds what is reachable");
            self.peak[size_index] = self.peak[size_index].max(self.current[size_index]);
        }

        self.peak_sum = self.peak_sum.max(self.current.iter().sum());
        self.since = now;
    }

    /// Start the window anew at `now`, the warm-up's end: the integral from
    /// zero and the peaks from the current reading.
    pub(super) fn restart(&mut self, now: Instant) {
        self.integral = [0; 3];
        self.made = 0;
        self.peak = self.current;
        self.peak_sum = self.current.iter().sum();
        self.from = now;
        self.since = now;
    }

    /// Bytes made garbage over the window.
    pub(super) fn made(&self) -> usize {
        self.made
    }

    /// The garbage now, by size index.
    pub(super) fn current(&self) -> [usize; 3] {
        self.current
    }

    /// Bytes times nanoseconds over the window, by size index.
    pub(super) fn integral(&self) -> [u128; 3] {
        self.integral
    }

    /// The mean over the window up to the last reading, by size index.
    pub(super) fn mean(&self) -> [f64; 3] {
        let window = self
            .since
            .saturating_duration_since(self.from)
            .as_nanos()
            .max(1) as f64;
        self.integral.map(|integral| integral as f64 / window)
    }

    /// The window's peaks: by size index, and of the sum over them.
    pub(super) fn peak(&self) -> ([usize; 3], usize) {
        (self.peak, self.peak_sum)
    }
}

/// A case's request-building state: the arena and a small long-lived state
/// of [`LongLivedShape`] `shape`, its sessions all touched at the setup, so
/// that no request replaces one unless the case ages it.
struct Fixture {
    arena: Box<Arena>,
    long_lived: LongLived,
}

impl Fixture {
    /// A fixture of `shape` in the heap variant, its classes named after
    /// `name`, on a thread whose lanes it resets. The caller holds the pool's
    /// guard.
    fn new(name: &str, shape: LongLivedShape) -> Self {
        Self::in_variant(name, shape, Variant::Heap)
    }

    /// [`Fixture::new`] in `variant`.
    fn in_variant(name: &str, shape: LongLivedShape, variant: Variant) -> Self {
        reset_lanes();
        let mut arena = Box::new(Arena::new());
        let arena_ptr: *mut Arena = &mut *arena;
        let mut long_lived =
            unsafe { LongLived::new(WebClasses::new(name), shape, variant, 0, 0, arena_ptr) };
        long_lived.last_touch.fill(long_lived.requests);
        Self { arena, long_lived }
    }

    /// A fixture of a core of `core` objects, ten values and two sessions.
    fn with_core(name: &str, core: usize) -> Self {
        Self::new(
            name,
            LongLivedShape {
                core,
                values: 10,
                sessions: 2,
            },
        )
    }

    /// A plan of `count` objects from the streams of `seed`, with no
    /// lookups, for a case that reads the request's own objects.
    fn plan(&self, seed: u64, count: usize) -> Plan {
        let mut plan =
            Plan::with_count(&mut Streams::new(seed, 1), self.long_lived.targets(), count);
        plan.lookups.clear();
        plan
    }

    fn build(&mut self) -> RequestBuild<'_> {
        let arena: *mut Arena = &mut *self.arena;
        RequestBuild {
            context: LLContext { arena },
            arena,
            long_lived: &mut self.long_lived,
        }
    }

    /// Let the state go and collect this thread's cycles, which frees it:
    /// after a turnover, since a collection that read the state live stamped
    /// it and the prune stops at a stamp of the current epoch, and with the
    /// roots read live re-offered from the deferred lane.
    fn let_go(self) {
        let _ = unsafe { self.long_lived.let_go() };
        crate::cycle::epoch::turn_this_threads_cell();
        crate::cycle::queue::reoffer_deferred_candidates();
        unsafe { crate::gc::ll_gc_collect_cycles() };
    }
}

/// Build `plan` whole on this thread, returning the request and what its
/// steps did together.
///
/// # Safety
/// As [`Request::start`].
unsafe fn build_whole(build: &mut RequestBuild, plan: Plan) -> (Request, Advanced) {
    let (mut request, mut advanced) = unsafe { Request::start(build, plan) };
    while !request.is_complete() {
        advanced.add(unsafe { request.advance(build, 1.0) });
    }

    (request, advanced)
}

/// The draws hold the specification's distributions: over 10,000 counts the
/// median and the log's σ, and over the objects of fixed-count plans the
/// class shares, the closures', the core edges', the registrations' with
/// their places, and the collections'.
#[test]
fn the_draws_hold_their_distributions() {
    let targets = Targets::new(&LongLivedShape::specified(40_000), Variant::Heap);
    let mut streams = Streams::new(1, 1);
    let mut logs: Vec<f64> = (0..10_000)
        .map(|_| (draw_the_count(&mut streams.shape).1 as f64).ln())
        .collect();
    logs.sort_unstable_by(f64::total_cmp);
    let median = logs[logs.len() / 2].exp();
    assert!((median / 10_000.0 - 1.0).abs() < 0.03, "median {median}");
    let mean = logs.iter().sum::<f64>() / logs.len() as f64;
    let sigma = (logs.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / logs.len() as f64).sqrt();
    assert!((sigma - 1.0).abs() < 0.05, "σ {sigma}");

    let (mut sizes, mut tree, mut closures, mut core_edges) = ([0usize; 3], 0, 0, 0);
    let (mut registered, mut places, mut payload) = (0, 0.0, 0);
    let (mut collections, mut entities) = (0, 0);
    for _ in 0..50 {
        let plan = Plan::with_count(&mut streams, &targets, 2_000);
        registered += plan.registrations.len();
        places += plan.registrations.iter().map(|&(at, _)| at).sum::<f64>();
        payload += plan.objects.len() - CONTEXT;
        collections += plan.orm.len();
        entities += plan.orm.iter().sum::<usize>();
        for object in &plan.objects {
            sizes[object.size_index as usize] += 1;
            if object.parent.is_none() || matches!(object.extra, ExtraEdge::BackTo(_)) {
                continue;
            }

            tree += 1;
            match object.extra {
                ExtraEdge::Context(_) => closures += 1,
                ExtraEdge::Core(_) => core_edges += 1,
                _ => {}
            }
        }
    }

    let objects: usize = sizes.iter().sum();
    for size_index in 0..3 {
        let share = sizes[size_index] as f64 * 100.0 / objects as f64;
        // Wider than the draw's own spread, because every ORM head and
        // segment is of the largest size.
        assert!(
            (share - SHARES_PERCENT[size_index] as f64).abs() < 1.5,
            "size {}: {share} %",
            SIZES[size_index]
        );
    }

    // A closure's edge goes to the context or the core one time in two, and
    // 1 % of the rest hold a core edge: 22.5 % context, 22.5 % + 0.55 % core.
    let context_share = closures as f64 * 100.0 / tree as f64;
    let core_share = core_edges as f64 * 100.0 / tree as f64;
    assert!(
        (context_share - 22.5).abs() < 1.0,
        "context {context_share} %"
    );
    assert!((core_share - 23.05).abs() < 1.0, "core {core_share} %");
    let registered_share = registered as f64 * 100.0 / payload as f64;
    assert!(
        (registered_share - 10.0).abs() < 0.5,
        "registered {registered_share} %"
    );
    let mean_place = places / registered as f64;
    assert!((mean_place - 0.5).abs() < 0.02, "mean place {mean_place}");

    for _ in 0..10_000 {
        let orm = draw_the_orm(&mut streams.shape);
        collections += orm.len();
        entities += orm.iter().sum::<usize>();
    }

    let per_request = collections as f64 / 10_050.0;
    let per_collection = entities as f64 / collections as f64;
    assert!(
        (per_request - 1.5).abs() < 0.05,
        "collections {per_request}"
    );
    assert!(
        (per_collection - 55.0).abs() < 1.5,
        "entities {per_collection}"
    );
}

/// Each class of [`SIZES`] takes a slot of its size in the entity heap, and
/// the heap's count of held bytes moves by that size at each birth and death
/// and reads what a walk of its blocks reads.
#[test]
fn the_heap_holds_each_class_at_its_size() {
    let _g = test_guard();
    let classes = WebClasses::new("HeldBytes");
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    for size_index in 0..3 {
        let before = held_by_size();
        let objects: Vec<*mut Object> = (0..100)
            .map(|_| unsafe {
                new_constructed(&mut context, classes.0[size_index], MemoryCategory::GcHeap)
            })
            .collect();
        for &object in &objects {
            let line = crate::memory::heap::describe_slot(object as usize);
            assert!(
                line.contains(&format!(" stride {} ", SIZES[size_index])),
                "size {}: {line}",
                SIZES[size_index]
            );
        }

        let mut expected = before;
        expected[size_index] += 100 * SIZES[size_index];
        assert_eq!(held_by_size(), expected, "size {}", SIZES[size_index]);
        assert_eq!(entity_bytes_held(), entity_bytes_held_by_a_walk());
        for &object in &objects {
            unsafe {
                assert!(ll_release(object as *mut RcHeader));
                ll_object_die(object);
            }
        }

        assert_eq!(
            held_by_size(),
            before,
            "size {} after the deaths",
            SIZES[size_index]
        );
        assert_eq!(entity_bytes_held(), entity_bytes_held_by_a_walk());
    }
}

/// A request built along its plan holds what the plan names: each object in
/// its parent's slot, each extra edge on its target, and the plan's bytes by
/// size in the heap.
#[test]
fn a_request_builds_what_its_plan_names() {
    let _g = test_guard();
    let mut fixture = Fixture::with_core("PlanNamed", 50);
    let plan = fixture.plan(7, 2_000);
    let bytes = plan.bytes();
    let before = held_by_size();
    let core = fixture.long_lived.core.clone();
    let mut build = fixture.build();
    let (request, advanced) = unsafe { build_whole(&mut build, plan) };
    assert_eq!(advanced.born, bytes);
    let after = held_by_size();
    for size_index in 0..3 {
        assert_eq!(
            after[size_index] - before[size_index],
            bytes[size_index],
            "size {}",
            SIZES[size_index]
        );
    }

    let plan = request.plan();
    for index in 0..CONTEXT {
        assert_eq!(
            slot_of(request.object(index), 0),
            request.object((index + 1) % CONTEXT)
        );
    }

    for (index, object) in plan.objects.iter().enumerate().skip(CONTEXT) {
        let (parent, slot) = object.parent.unwrap();
        assert_eq!(
            slot_of(request.object(parent as usize), u32::from(slot)),
            request.object(index)
        );
        let expected = match object.extra {
            ExtraEdge::None => continue,
            ExtraEdge::Context(target) | ExtraEdge::BackTo(target) => {
                request.object(target as usize)
            }
            ExtraEdge::Core(target) => core[target as usize],
        };
        assert_eq!(
            slot_of(request.object(index), 0),
            expected,
            "object {index}"
        );
    }

    assert_eq!(unsafe { request.end() }.bytes, bytes);
    unsafe { crate::gc::ll_gc_collect_cycles() };
    fixture.let_go();
}

/// The end's release lands on the registered first object and registers
/// nothing, or on the unregistered second and registers it. The session
/// stands registered, as the steady state leaves it, so that its release at
/// the end registers nothing either.
#[test]
fn a_silent_end_registers_nothing_and_the_other_one_root() {
    let _g = test_guard();
    let mut fixture = Fixture::with_core("SilentEnd", 10);
    unsafe { fixture.long_lived.register_the_steady_state(&mut || {}) };
    for silent in [true, false] {
        let mut plan = fixture.plan(3, 100);
        plan.silent_end = silent;
        let (request, _) = unsafe { build_whole(&mut fixture.build(), plan) };
        let before = crate::cycle::queue::candidate_count();
        let ended = unsafe { request.end() };
        let after = crate::cycle::queue::candidate_count();
        assert_eq!(ended.silent, silent);
        assert_eq!(after - before, usize::from(!silent), "silent {silent}");
        assert_eq!(ended.registered, after - before);
        unsafe { crate::gc::ll_gc_collect_cycles() };
    }

    fixture.let_go();
}

/// One advance registers at most half the poll's stride, the rest at the
/// next.
#[test]
fn an_advance_registers_at_most_half_the_stride() {
    let _g = test_guard();
    let mut fixture = Fixture::with_core("HalfStride", 10);
    let plan = fixture.plan(5, 30_000);
    let planned = plan.registrations.len();
    assert!(
        planned > REGISTRATIONS_AN_ADVANCE,
        "{planned} registrations"
    );
    let mut build = fixture.build();
    let (mut request, _) = unsafe { Request::start(&mut build, plan) };
    let first = unsafe { request.advance(&mut build, 1.0) };
    assert_eq!(first.registered, REGISTRATIONS_AN_ADVANCE);
    let second = unsafe { request.advance(&mut build, 1.0) };
    assert_eq!(second.registered, planned - REGISTRATIONS_AN_ADVANCE);
    assert!(request.is_complete());
    let _ = unsafe { request.end() };
    unsafe { crate::gc::ll_gc_collect_cycles() };
    fixture.let_go();
}

/// A bounded advance stops at its births and the next goes on from there:
/// the steps together build the whole plan, and only a step the bound
/// stopped says so.
#[test]
fn a_bounded_advance_stops_at_its_births_and_goes_on() {
    let _g = test_guard();
    let mut fixture = Fixture::with_core("BoundedAdvance", 10);
    let plan = fixture.plan(5, 3_000);
    let (births, bytes, registrations) =
        (plan.objects.len(), plan.bytes(), plan.registrations.len());
    let mut build = fixture.build();
    let (mut request, started) = unsafe { Request::start(&mut build, plan) };
    let mut built = started;
    let mut cuts = 0;
    while !request.is_complete() {
        let before = request.born();
        let (advanced, stop) = unsafe { request.advance_at_most(&mut build, 1.0, 500) };
        let born = request.born() - before;
        assert!(born <= 500, "a step of {born}");
        if stop == Stop::Births {
            assert_eq!(born, 500, "the bound stopped it");
            cuts += 1;
        }

        built.add(advanced);
    }

    assert_eq!(request.born(), births);
    assert_eq!(cuts, (births - CONTEXT - 1) / 500, "{births} births");
    assert_eq!(built.born, bytes);
    assert_eq!(
        built.registered,
        registrations + 1,
        "and the start's context root"
    );
    let _ = unsafe { request.end() };
    unsafe { crate::gc::ll_gc_collect_cycles() };
    fixture.let_go();
}

/// The build to a point steps at the births' bound and polls after each
/// step: every step but the last stopped by the bound, the polls' walls
/// summed, and the plan built whole. Red with the steps' loop gone, which
/// builds a slice's births past the bound in a later slice.
#[test]
fn a_build_to_a_point_polls_between_its_steps() {
    let _g = test_guard();
    let mut fixture = Fixture::with_core("BuildTo", 10);
    let plan = fixture.plan(5, 3 * BIRTHS_AN_ADVANCE + 100);
    let (births, bytes) = (plan.objects.len(), plan.bytes());
    let mut build = fixture.build();
    let (mut request, started) = unsafe { Request::start(&mut build, plan) };
    let mut built = started;
    let mut steps = 0;
    let (walls, stop) = unsafe {
        build_to(&mut request, &mut build, 1.0, |advanced| {
            built.add(advanced);
            steps += 1;
            Duration::from_micros(1)
        })
    };

    if stop == Stop::Events {
        let _ = unsafe {
            build_to(&mut request, &mut build, 1.0, |advanced| {
                built.add(advanced);
                Duration::ZERO
            })
        };
    } else {
        assert_eq!(stop, Stop::Reached);
        assert_eq!(
            steps,
            (births - CONTEXT).div_ceil(BIRTHS_AN_ADVANCE),
            "{births} births"
        );
        assert_eq!(
            walls,
            Duration::from_micros(steps as u64),
            "the walls summed"
        );
    }

    assert!(steps >= 3, "{steps} steps");
    assert!(request.is_complete());
    assert_eq!(built.born, bytes);
    assert!(longest_build_step() > Duration::ZERO);
    let _ = unsafe { request.end() };
    unsafe { crate::gc::ll_gc_collect_cycles() };
    fixture.let_go();
}

/// The garbage integrates at its events: a scripted sequence of readings by
/// hand, and a request's whole bytes through the real count from its end to
/// the collection that frees them.
#[test]
fn garbage_integrates_at_its_events() {
    let t0 = Instant::now();
    let at = |millis: u64| t0 + Duration::from_millis(millis);
    let mut garbage = Garbage::new(t0, [1_000, 0, 0]);
    garbage.born([640, 0, 512]);
    garbage.read(at(1), [1_640, 0, 512]);
    assert_eq!(garbage.current(), [0, 0, 0]);
    garbage.ended([640, 0, 512]);
    garbage.read(at(3), [1_640, 0, 512]);
    assert_eq!(garbage.current(), [640, 0, 512]);
    garbage.read(at(7), [1_320, 0, 0]);
    assert_eq!(garbage.current(), [320, 0, 0]);
    garbage.read(at(10), [1_000, 0, 0]);
    // 640 B for 4 ms and 320 B for 3 ms; 512 B for 4 ms.
    assert_eq!(
        garbage.integral(),
        [640 * 4_000_000 + 320 * 3_000_000, 0, 512 * 4_000_000]
    );
    assert_eq!(garbage.peak(), ([640, 0, 512], 1_152));
    let mean = garbage.mean();
    assert!((mean[0] - (640.0 * 4.0 + 320.0 * 3.0) / 10.0).abs() < 1e-6);
    garbage.restart(at(10));
    assert_eq!(garbage.peak(), ([0, 0, 0], 0));

    let _g = test_guard();
    let mut fixture = Fixture::with_core("Integrated", 10);
    let plan = fixture.plan(9, 500);
    let mut garbage = Garbage::new(t0, held_by_size());
    let (request, advanced) = unsafe { build_whole(&mut fixture.build(), plan) };
    garbage.born(advanced.born);
    garbage.read(at(1), held_by_size());
    assert_eq!(garbage.current(), [0, 0, 0]);
    let bytes = unsafe { request.end() }.bytes;
    garbage.ended(bytes);
    garbage.read(at(2), held_by_size());
    assert_eq!(garbage.current(), bytes);
    unsafe { crate::gc::ll_gc_collect_cycles() };
    garbage.read(at(12), held_by_size());
    assert_eq!(garbage.current(), [0, 0, 0]);
    assert_eq!(
        garbage.integral(),
        bytes.map(|bytes| bytes as u128 * 10_000_000)
    );
    fixture.let_go();
}

/// The same seed draws the same plan whatever the other purposes' streams
/// have drawn: a plan's shape and registrations stay when its cache and
/// session streams have drawn more, and its lookups and session when its
/// shape and registration streams have; another repeat draws another stream.
#[test]
fn a_seed_draws_one_plan_whatever_the_other_streams_draw() {
    let targets = Targets::new(
        &LongLivedShape {
            core: 100,
            values: 100,
            sessions: 10,
        },
        Variant::Heap,
    );
    // A plan from streams whose shape, registrations, cache and sessions
    // have drawn `ahead` values first.
    let draw = |ahead: [usize; 4]| {
        let mut streams = Streams::new(2, 3);
        let each = [
            &mut streams.shape,
            &mut streams.registrations,
            &mut streams.cache,
            &mut streams.sessions,
        ];
        for (stream, extra) in each.into_iter().zip(ahead) {
            for _ in 0..extra {
                let _ = stream.unit();
            }
        }

        let plan = Plan::draw(&mut streams, &targets);
        (plan, streams.shape.checksum())
    };
    let shape_of = |plan: &Plan| {
        let objects: Vec<_> = plan
            .objects
            .iter()
            .map(|object| {
                (
                    object.size_index,
                    object.parent,
                    object.extra,
                    object.born_at.to_bits(),
                )
            })
            .collect();
        (objects, plan.silent_end, plan.registrations.clone())
    };
    let (plain, plain_checksum) = draw([0; 4]);
    let (others_ahead, others_ahead_checksum) = draw([0, 0, 1_000, 1_000]);
    assert_eq!(shape_of(&others_ahead), shape_of(&plain));
    assert_eq!(others_ahead_checksum, plain_checksum);
    assert_ne!(others_ahead.lookups, plain.lookups);
    let (shape_ahead, _) = draw([1_000, 1_000, 0, 0]);
    assert_ne!(shape_of(&shape_ahead), shape_of(&plain));
    assert_eq!(
        (shape_ahead.lookups, shape_ahead.session),
        (plain.lookups, plain.session)
    );
    let mut this_repeat = Draws::new(2, 3, Purpose::Shape);
    let mut next_repeat = Draws::new(2, 4, Purpose::Shape);
    assert_ne!(
        this_repeat.unit(),
        next_repeat.unit(),
        "another repeat draws another stream"
    );
}

/// The objects of a directory of `entries`, counted level by level as
/// [`Directory::new`] builds them.
fn directory_objects(entries: usize) -> usize {
    let mut level = entries.div_ceil(DIRECTORY_FAN_OUT).max(1);
    let mut objects = level;
    while level > 1 {
        level = level.div_ceil(DIRECTORY_FAN_OUT);
        objects += level;
    }

    objects
}

/// The core is one ring through slot 0, the setup registers nothing, and the
/// heap holds the bytes the state reports; one core root's trace meets every
/// edge of the state, and a collection frees nothing while the core's
/// reference is held and every byte once it is let go.
#[test]
fn one_core_root_reaches_the_whole_state() {
    let _g = test_guard();
    let before = held_by_size();
    let shape = LongLivedShape {
        core: 20,
        values: 40,
        sessions: 5,
    };
    let fixture = Fixture::new("WholeState", shape);
    assert_eq!(crate::cycle::queue::candidate_count(), 0);
    let held = fixture.long_lived.held();
    let after = held_by_size();
    for size_index in 0..3 {
        assert_eq!(after[size_index] - before[size_index], held[size_index]);
    }

    let core = &fixture.long_lived.core;
    let mut at = core[0];
    for step in 1..=shape.core {
        at = slot_of(at, 0);
        assert_eq!(at, core[step % shape.core], "step {step}");
    }

    for &object in &core[1..] {
        let other = slot_of(object, 1);
        assert!(
            other != object && core.contains(&other),
            "another core object"
        );
    }

    // The ring, the other edges and the two directories' roots; each
    // directory's parent edges and its entries; each cycle's chain, its
    // back-edge and its core edge.
    let edges = shape.core + (shape.core - 1) + 2 + directory_objects(shape.values) - 1
        + shape.values
        + directory_objects(shape.sessions)
        - 1
        + shape.sessions
        + shape.values * (VALUE_OBJECTS + 1)
        + shape.sessions * (SESSION_OBJECTS + 1);
    unsafe { register(core[shape.core / 2]) };
    let _ = crate::cycle::row::take_edge_dispatches();
    let _ = crate::cycle::row::take_dispatches_in_mark_phase();
    unsafe { crate::gc::ll_gc_collect_cycles() };
    assert_eq!(
        crate::cycle::row::take_dispatches_in_mark_phase(),
        1 + edges,
        "the root's resolution and one per edge"
    );
    assert_eq!(held_by_size(), after, "a held state frees nothing");
    fixture.let_go();
    assert_eq!(held_by_size(), before, "a state let go is freed whole");
}

/// The cache on a scripted sequence: a miss evicts the least recently used
/// value by a decrement that registers it, or registers nothing where a hit
/// registered it already; a hit registers a value once; the garbage is the
/// evicted value's bytes until a collection frees them.
#[test]
fn a_hit_registers_and_an_eviction_is_silent_only_after_one() {
    let _g = test_guard();
    let t0 = Instant::now();
    let at = |millis: u64| t0 + Duration::from_millis(millis);
    let start = held_by_size();
    let mut garbage = Garbage::new(t0, start);
    let mut fixture = Fixture::new(
        "ScriptedCache",
        LongLivedShape {
            core: 5,
            values: 3,
            sessions: 1,
        },
    );
    garbage.born(fixture.long_lived.held());
    garbage.read(at(1), held_by_size());
    assert_eq!(garbage.current(), [0, 0, 0]);
    let arena: *mut Arena = &mut *fixture.arena;
    let resident = fixture.long_lived.lru.keys_by_recency();
    let absent: Vec<u32> = (0..(3 * KEYS_A_VALUE) as u32)
        .filter(|key| !resident.contains(key))
        .take(2)
        .collect();
    let look_up = |long_lived: &mut LongLived, key: u32| {
        let before = crate::cycle::queue::candidate_count();
        let advanced = unsafe { long_lived.look_up(arena, key, None) };
        let registered = crate::cycle::queue::candidate_count() - before;
        assert_eq!(advanced.registered, registered, "key {key}");
        (advanced, registered)
    };

    // A miss evicts the oldest, which no hit registered.
    let (miss, registered) = look_up(&mut fixture.long_lived, absent[0]);
    assert_eq!(registered, 1);
    assert_ne!(miss.ended, [0, 0, 0]);
    assert_ne!(miss.born, [0, 0, 0]);
    garbage.born(miss.born);
    garbage.ended(miss.ended);
    garbage.read(at(2), held_by_size());
    assert_eq!(garbage.current(), miss.ended);
    unsafe { crate::gc::ll_gc_collect_cycles() };
    garbage.read(at(3), held_by_size());
    assert_eq!(
        garbage.current(),
        [0, 0, 0],
        "the collection freed the value"
    );
    let held = held_by_size();
    assert_eq!(
        std::array::from_fn(|size_index| held[size_index] - start[size_index]),
        fixture.long_lived.held(),
        "the state holds the new value and not the evicted one"
    );
    assert_eq!(
        fixture.long_lived.lru.keys_by_recency(),
        [absent[0], resident[0], resident[1]]
    );

    // Hits register each value once, and make `resident[1]` the oldest.
    for key in [resident[1], absent[0], resident[0]] {
        assert_eq!(look_up(&mut fixture.long_lived, key).1, 1, "key {key}");
    }

    assert_eq!(
        look_up(&mut fixture.long_lived, resident[0]).1,
        0,
        "a candidate's hit registers nothing"
    );
    assert_eq!(
        fixture.long_lived.lru.keys_by_recency(),
        [resident[0], absent[0], resident[1]]
    );
    let (_, registered) = look_up(&mut fixture.long_lived, absent[1]);
    assert_eq!(registered, 0, "the evicted value was a candidate already");
    assert_eq!(
        fixture.long_lived.counts,
        CacheCounts {
            hits: 4,
            hits_registering: 3,
            misses: 2,
            evictions_silent: 1,
            evictions_registering: 1,
            sessions_replaced: 0,
            session_writes: 0,
        }
    );
    fixture.let_go();
}

/// The setup leaves every entry holding a value of its size, a chain closed
/// by its back-edge with its core edge on the head, and registers the core,
/// the values hit since their insertion and the sessions, polling between
/// each [`REGISTRATIONS_AN_ADVANCE`]; the sessions' idle ages have the mean
/// of [`SESSIONS`] requests.
#[test]
fn the_setup_builds_and_registers_the_steady_state() {
    let _g = test_guard();
    let shape = LongLivedShape {
        core: REGISTRATIONS_AN_ADVANCE + 1,
        values: 200,
        sessions: SESSIONS,
    };
    let mut fixture = Fixture::new("SteadyState", shape);
    let long_lived = &mut fixture.long_lived;
    assert_eq!(long_lived.lru.used, shape.values);
    let mut keys = long_lived.lru.keys_by_recency();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), shape.values);
    for (directory, entries, objects) in [
        (&long_lived.values, shape.values, VALUE_OBJECTS),
        (&long_lived.sessions, shape.sessions, SESSION_OBJECTS),
    ] {
        for entry in 0..entries as u32 {
            let head = directory.head(entry);
            assert!(long_lived.core.contains(&slot_of(head, 1)), "entry {entry}");
            let mut last = head;
            for _ in 1..objects {
                last = slot_of(last, 0);
            }

            assert_eq!(slot_of(last, 1), head, "entry {entry}");
        }
    }

    // Built on the fixture's thread with all its sessions touched, so the
    // ages are read off a fresh state of the same seed.
    let mut arena = Arena::new();
    let fresh = unsafe {
        LongLived::new(
            WebClasses::new("SteadyStateAges"),
            shape,
            Variant::Heap,
            0,
            0,
            &mut arena,
        )
    };
    let mean_age = fresh
        .last_touch
        .iter()
        .map(|&touch| (FIRST_REQUEST - touch) as f64)
        .sum::<f64>()
        / shape.sessions as f64;
    assert!(
        (mean_age / 1_000.0 - 1.0).abs() < 0.1,
        "mean age {mean_age}"
    );
    let _ = unsafe { fresh.let_go() };

    let hit = long_lived
        .lru
        .hit_since_insertion
        .iter()
        .filter(|&&hit| hit)
        .count();
    let before = crate::cycle::queue::candidate_count();
    let mut polls = 0;
    let registered = unsafe { long_lived.register_the_steady_state(&mut || polls += 1) };
    assert_eq!(registered, shape.core + hit + shape.sessions);
    assert_eq!(crate::cycle::queue::candidate_count() - before, registered);
    assert_eq!(polls, registered / REGISTRATIONS_AN_ADVANCE);
    for entry in 0..shape.values as u32 {
        assert_eq!(
            is_a_candidate(long_lived.values.head(entry)),
            long_lived.lru.hit_since_insertion[entry as usize],
            "entry {entry}"
        );
    }

    // Misses from here evict silently exactly the values hit since their
    // insertion.
    let arena: *mut Arena = &mut *fixture.arena;
    let long_lived = &mut fixture.long_lived;
    let absent: Vec<u32> = (0..(shape.values * KEYS_A_VALUE) as u32)
        .filter(|&key| long_lived.lru.entry_of_key[key as usize] == ABSENT)
        .take(shape.values)
        .collect();
    let mut silent = 0;
    for key in absent {
        let was_hit = long_lived.lru.hit_since_insertion[long_lived.lru.oldest as usize];
        let advanced = unsafe { long_lived.look_up(arena, key, None) };
        assert_eq!(advanced.registered, usize::from(!was_hit), "key {key}");
        silent += usize::from(was_hit);
    }

    assert!(silent > 0 && silent < shape.values, "{silent} silent");
    assert_eq!(long_lived.counts.evictions_silent, silent);
    fixture.let_go();
}

/// The cache's LRU reaches the steady state of an independent simulation of
/// the same Zipf and LRU (`dev/BENCHMARKS.md`, "readings the S65 and S67 stage
/// notes held, carried at the stages' close", the web loads' cache): at N =
/// 40k, a hit rate of 84.5 %, 31.6 % of evictions silent and 17,966 resident
/// values hit since their insertion, over 70,000 lookups after the setup's run.
#[test]
fn the_cache_reaches_the_simulated_steady_state() {
    let values = 40_000;
    let keys = Zipf::new(values * KEYS_A_VALUE);
    let mut draws = Draws::new(1, 1, Purpose::Core);
    let mut lru = run_to_the_steady_state(values, &keys, &mut draws);
    let (mut hits, mut misses, mut silent) = (0, 0, 0);
    for _ in 0..70_000 {
        match lru.look_up(keys.draw(&mut draws) as u32) {
            LookupOutcome::Hit(_) => hits += 1,
            LookupOutcome::Evicted { was_hit, .. } => {
                misses += 1;
                silent += usize::from(was_hit);
            }
            LookupOutcome::Inserted => unreachable!("the cache is full"),
        }
    }

    let hit_rate = hits as f64 / 70_000.0;
    let silent_share = silent as f64 / misses as f64;
    let resident_hit = lru.hit_since_insertion.iter().filter(|&&hit| hit).count();
    assert!((hit_rate - 0.845).abs() < 0.01, "hit rate {hit_rate}");
    assert!((silent_share - 0.316).abs() < 0.02, "silent {silent_share}");
    assert!(
        (resident_hit as f64 / 17_966.0 - 1.0).abs() < 0.03,
        "resident hit {resident_hit}"
    );
}

/// A session touched after more than [`SESSION_IDLE_REQUESTS`] of idleness
/// is replaced, its eviction registering the old head, and one touched at
/// that idleness is not; the heap then holds the new session, and a
/// collection frees the old one.
#[test]
fn a_session_idle_past_its_lifetime_is_replaced() {
    let _g = test_guard();
    let before = held_by_size();
    let mut fixture = Fixture::new(
        "IdleSession",
        LongLivedShape {
            core: 5,
            values: 3,
            sessions: 3,
        },
    );
    let arena: *mut Arena = &mut *fixture.arena;
    let long_lived = &mut fixture.long_lived;
    // The opening counts one request more, so these are idle for exactly
    // the lifetime and one request past it.
    long_lived.last_touch[2] = long_lived.requests + 1 - SESSION_IDLE_REQUESTS;
    let kept = long_lived.sessions.head(2);
    let (head, advanced) = unsafe { long_lived.open_session(arena, 2) };
    assert_eq!((head, advanced), (kept, Advanced::default()));
    long_lived.last_touch[1] = long_lived.requests - SESSION_IDLE_REQUESTS;
    let old = long_lived.sessions.head(1);
    let old_bytes = to_usize(long_lived.sessions.bytes[1]);
    let candidates = crate::cycle::queue::candidate_count();
    let (head, advanced) = unsafe { long_lived.open_session(arena, 1) };
    assert_ne!(head, old);
    assert_eq!(head, long_lived.sessions.head(1));
    assert_eq!(advanced.ended, old_bytes);
    assert_eq!(advanced.born, to_usize(long_lived.sessions.bytes[1]));
    assert_eq!(advanced.registered, 1);
    assert_eq!(crate::cycle::queue::candidate_count() - candidates, 1);
    assert_eq!(long_lived.counts.sessions_replaced, 1);
    assert_eq!(long_lived.last_touch[1], long_lived.requests);
    unsafe { crate::gc::ll_gc_collect_cycles() };
    let held = held_by_size();
    assert_eq!(
        std::array::from_fn(|size_index| held[size_index] - before[size_index]),
        long_lived.held(),
        "the old session freed, the new one held"
    );
    fixture.let_go();
}

/// A request holds its session by a reference of its own from its start to
/// its end, and the end's release registers a session no request had
/// registered yet: here one its start put in place of an idle one.
#[test]
fn a_request_holds_its_session_and_its_end_registers_a_new_one() {
    let _g = test_guard();
    let mut fixture = Fixture::with_core("HeldSession", 10);
    unsafe { fixture.long_lived.register_the_steady_state(&mut || {}) };
    let mut plan = fixture.plan(6, 100);
    plan.silent_end = true;
    let slot = plan.session;
    fixture.long_lived.last_touch[slot as usize] =
        fixture.long_lived.requests - SESSION_IDLE_REQUESTS;
    let (request, advanced) = unsafe { build_whole(&mut fixture.build(), plan) };
    assert_ne!(advanced.ended, [0, 0, 0], "the idle session was replaced");
    let head = fixture.long_lived.sessions.head(slot);
    let count = || unsafe { crate::refcount::header_refcount(head as *const RcHeader) };
    assert_eq!(count(), 3, "its slot, its back-edge and the request");
    let candidates = crate::cycle::queue::candidate_count();
    let ended = unsafe { request.end() };
    assert_eq!(count(), 2);
    assert!(ended.silent);
    assert_eq!(ended.registered, 1, "the new session");
    assert_eq!(crate::cycle::queue::candidate_count() - candidates, 1);
    unsafe { crate::gc::ll_gc_collect_cycles() };
    fixture.let_go();
}

/// An advance runs the lookups placed before its bound and leaves the rest,
/// and a whole request runs all of them.
#[test]
fn an_advance_runs_the_lookups_at_their_places() {
    let _g = test_guard();
    let mut fixture = Fixture::with_core("LookupPlaces", 10);
    let plan = Plan::with_count(&mut Streams::new(4, 1), fixture.long_lived.targets(), 200);
    let before_half = plan.lookups.iter().filter(|&&(at, _)| at < 0.5).count();
    assert!(before_half > 0 && before_half < LOOKUPS, "{before_half}");
    let mut build = fixture.build();
    let (mut request, _) = unsafe { Request::start(&mut build, plan) };
    assert_eq!(
        unsafe { request.advance(&mut build, 0.5) }.looked_up,
        before_half
    );
    assert_eq!(
        unsafe { request.advance(&mut build, 1.0) }.looked_up,
        LOOKUPS - before_half
    );
    assert!(request.is_complete());
    let counts = build.long_lived.counts;
    assert_eq!(counts.hits + counts.misses, LOOKUPS);
    let _ = unsafe { request.end() };
    unsafe { crate::gc::ll_gc_collect_cycles() };
    fixture.let_go();
}

/// A lookup counts toward an advance's cap as a registration does, hit or
/// miss, since either can register one root.
#[test]
fn a_lookup_counts_toward_an_advances_cap() {
    let _g = test_guard();
    let mut fixture = Fixture::with_core("LookupsCounted", 10);
    let plan = Plan::with_count(
        &mut Streams::new(5, 1),
        fixture.long_lived.targets(),
        30_000,
    );
    // The lookups among the first events of the two lists merged by place,
    // a registration first at a tie.
    let mut places: Vec<(f64, bool)> = plan
        .registrations
        .iter()
        .map(|&(at, _)| (at, false))
        .chain(plan.lookups.iter().map(|&(at, _)| (at, true)))
        .collect();
    places.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let lookups_first = places[..REGISTRATIONS_AN_ADVANCE]
        .iter()
        .filter(|&&(_, lookup)| lookup)
        .count();
    let mut build = fixture.build();
    let (mut request, _) = unsafe { Request::start(&mut build, plan) };
    let first = unsafe { request.advance(&mut build, 1.0) };
    assert!(first.looked_up > 0, "lookups fall among the registrations");
    assert_eq!(first.looked_up, lookups_first);
    assert_eq!(
        request.registrations_done + first.looked_up,
        REGISTRATIONS_AN_ADVANCE
    );
    let second = unsafe { request.advance(&mut build, 1.0) };
    assert!(request.is_complete());
    assert_eq!(first.looked_up + second.looked_up, LOOKUPS);
    let _ = unsafe { request.end() };
    unsafe { crate::gc::ll_gc_collect_cycles() };
    fixture.let_go();
}

/// Every draw of a plan folded into one number, for a case that pins a plan.
fn plan_digest(plan: &Plan) -> u64 {
    let mut digest = 0u64;
    let mut fold =
        |value: u64| digest = digest.rotate_left(7) ^ value.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for object in &plan.objects {
        fold(u64::from(object.size_index));
        fold(object.parent.map_or(u64::MAX, |(index, slot)| {
            (u64::from(index) << 8) | u64::from(slot)
        }));
        fold(match object.extra {
            ExtraEdge::None => 0,
            ExtraEdge::Context(index) => (1 << 32) | u64::from(index),
            ExtraEdge::Core(index) => (2 << 32) | u64::from(index),
            ExtraEdge::BackTo(index) => (3 << 32) | u64::from(index),
        });
        fold(object.born_at.to_bits());
    }

    for &(at, index) in plan.registrations.iter().chain(&plan.lookups) {
        fold(at.to_bits());
        fold(u64::from(index));
    }

    fold(u64::from(plan.session));
    plan.orm.iter().for_each(|&entities| fold(entities as u64));
    fold(u64::from(plan.silent_end));
    digest
}

/// The timing draws and the arrivals hold the specification's
/// distributions: over 10,000 plans the CPU's and the waits' medians and the
/// logs' σ, each within 3 % and 0.05; over 100,000 arrivals the mean
/// interarrival within 1 % of 27.48 ms and the coefficient of variation within
/// 2 % of an exponential's 1.
#[test]
fn the_timing_and_the_arrivals_hold_their_distributions() {
    let expected = specified_interarrival().as_secs_f64();
    assert!((expected - 0.027_48).abs() < 0.000_01, "{expected}");
    let mut timing = Draws::new(1, 1, Purpose::Timing);
    for (median, what) in [(REQUEST_CPU_MEDIAN, "CPU"), (WAIT_MEDIAN, "wait")] {
        let mut logs: Vec<f64> = (0..10_000)
            .map(|_| draw_a_duration(&mut timing, median).as_secs_f64().ln())
            .collect();
        logs.sort_unstable_by(f64::total_cmp);
        let drawn = logs[logs.len() / 2].exp();
        assert!(
            (drawn / median.as_secs_f64() - 1.0).abs() < 0.03,
            "{what}'s median {drawn}"
        );
        let mean = logs.iter().sum::<f64>() / logs.len() as f64;
        let sigma =
            (logs.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / logs.len() as f64).sqrt();
        assert!((sigma - TIMING_SIGMA).abs() < 0.05, "{what}'s σ {sigma}");
    }

    let mut arrivals = Arrivals::new(1, 1, specified_interarrival());
    let mut last = Duration::ZERO;
    let gaps: Vec<f64> = (0..100_000)
        .map(|_| {
            let at = arrivals.next();
            let gap = (at - last).as_secs_f64();
            last = at;
            gap
        })
        .collect();
    let mean = gaps.iter().sum::<f64>() / gaps.len() as f64;
    let deviation =
        (gaps.iter().map(|gap| (gap - mean).powi(2)).sum::<f64>() / gaps.len() as f64).sqrt();
    assert!((mean / expected - 1.0).abs() < 0.01, "mean {mean}");
    assert!(
        (deviation / mean - 1.0).abs() < 0.02,
        "CV {}",
        deviation / mean
    );
}

/// `web-heap`'s plans at a fixed seed fold to the digests pinned here, those of
/// the plans the loads' first build drew, so that the arena variant's draws, on
/// the same streams, move none of `web-heap`'s.
#[test]
fn a_heap_plan_folds_to_its_pinned_digest() {
    let targets = Targets::new(&LongLivedShape::specified(40_000), Variant::Heap);
    let mut streams = Streams::new(3, 1);
    let digests: Vec<u64> = (0..3)
        .map(|_| plan_digest(&Plan::draw(&mut streams, &targets)))
        .collect();
    assert_eq!(
        digests,
        [
            0x9d9b_a613_7c09_0a8b,
            0x96e2_c347_0cc8_af29,
            0x5cc9_cb3e_41fe_255e
        ]
    );
}

/// A `web-arena` plan holds [`ARENA_CORE_EDGES`] core edges, or one on every
/// tree object of a smaller tree, no registrations, closures to the context
/// alone, and a session write on 30 % of requests.
#[test]
fn an_arena_plan_holds_its_core_edges_and_no_registrations() {
    let targets = Targets::new(&LongLivedShape::specified(40_000), Variant::Arena);
    let mut streams = Streams::new(4, 1);
    let (mut closures, mut untaken) = (0, 0);
    for _ in 0..20 {
        let plan = Plan::with_count(&mut streams, &targets, 2_000);
        assert!(plan.registrations.is_empty());
        let mut core_edges = 0;
        for object in plan.objects.iter().filter(|object| object.parent.is_some()) {
            match object.extra {
                ExtraEdge::Core(_) => core_edges += 1,
                ExtraEdge::Context(_) => closures += 1,
                ExtraEdge::None => untaken += 1,
                ExtraEdge::BackTo(_) => {}
            }
        }

        assert_eq!(core_edges, ARENA_CORE_EDGES);
    }

    // A closure is drawn at 45 % among the tree objects the core edges left.
    let closure_share = closures as f64 * 100.0 / (closures + untaken) as f64;
    assert!(
        (closure_share - 45.0).abs() < 1.0,
        "closures {closure_share} %"
    );

    // A tree of 100 objects gives each one a core edge.
    let mut small = Plan::with_count(&mut streams, &targets, 100);
    while !small.orm.is_empty() {
        small = Plan::with_count(&mut streams, &targets, 100);
    }

    let tree = small.objects.len() - CONTEXT;
    let core_edges = small
        .objects
        .iter()
        .filter(|object| matches!(object.extra, ExtraEdge::Core(_)))
        .count();
    assert_eq!((tree, core_edges), (92, 92));

    let writes = (0..4_000)
        .filter(|_| {
            Plan::with_count(&mut streams, &targets, 20)
                .session_write
                .is_some()
        })
        .count();
    let share = writes as f64 * 100.0 / 4_000.0;
    assert!(
        (share - SESSION_WRITE_PERCENT).abs() < 2.0,
        "writes {share} %"
    );
}

/// A `web-arena` request logs a heap reference into its arena for its
/// session, each lookup and each core edge; its objects leave the heap's
/// held bytes where its misses put them; and its reset registers exactly
/// the distinct targets of those references that stood no candidate.
#[test]
fn an_arena_request_registers_at_its_reset_what_it_logged() {
    let _g = test_guard();
    let shape = LongLivedShape {
        core: 50,
        values: 10,
        sessions: 2,
    };
    let mut fixture = Fixture::in_variant("ArenaReset", shape, Variant::Arena);
    let core = fixture.long_lived.core.clone();
    let mut plan = Plan::with_count(&mut Streams::new(5, 1), fixture.long_lived.targets(), 2_000);
    plan.session_write = None;
    let before = held_by_size();
    let mut build = fixture.build();
    let (request, advanced) = unsafe { build_whole(&mut build, plan) };
    let mut expected = before;
    for size_index in 0..3 {
        expected[size_index] += advanced.born[size_index];
    }

    assert_eq!(
        held_by_size(),
        expected,
        "only the misses' values are heap bytes"
    );

    assert_eq!(slot_of(request.locals, 0), request.session);
    let mut targets: Vec<*mut Object> = (1..=LOOKUPS as u32)
        .map(|slot| slot_of(request.locals, slot))
        .collect();
    assert!(
        targets.iter().all(|value| !value.is_null()),
        "every lookup stored its value"
    );
    targets.push(request.session);
    for (index, object) in request.plan().objects.iter().enumerate() {
        if let ExtraEdge::Core(target) = object.extra {
            assert_eq!(slot_of(request.object(index), 0), core[target as usize]);
            targets.push(core[target as usize]);
        }
    }

    assert_eq!(targets.len(), LOGGED_REFERENCES);
    targets.sort_unstable();
    targets.dedup();
    let predicted = targets
        .iter()
        .filter(|&&target| !is_a_candidate(target))
        .count();
    let ended = unsafe { request.finish(&mut build) };
    assert_eq!(ended.registered, predicted);
    assert!(predicted > 0, "the case registers something at the reset");
    assert_eq!(
        held_by_size(),
        expected,
        "the reset's releases are none of them final"
    );
    fixture.let_go();
}

/// A session write escapes its request's arena: the reset promotes it into
/// the heap and retains its block, and the next write into the session
/// frees it, its block going home.
#[test]
fn a_session_write_survives_its_reset_and_the_next_frees_it() {
    let _g = test_guard();
    let shape = LongLivedShape {
        core: 10,
        values: 10,
        sessions: 2,
    };
    let mut fixture = Fixture::in_variant("SessionWrite", shape, Variant::Arena);
    let session = fixture.long_lived.sessions.head(0);
    let arena: *mut Arena = &mut *fixture.arena;
    let retained = crate::memory::retained::retained_block_count;
    // The setup's write, if the session holds one, is freed first.
    unsafe {
        store_prop(
            arena,
            session,
            prop_offset(SESSION_WRITE_SLOT),
            std::ptr::null_mut(),
        )
    };
    let before = retained();
    unsafe {
        fixture.long_lived.write_into_session(arena, session, 1);
        crate::promote::arena_reset_full(arena);
    }

    let first = slot_of(session, SESSION_WRITE_SLOT);
    assert!(
        !first.is_null(),
        "the session holds the write after the reset"
    );
    assert_eq!(
        unsafe { crate::refcount::entity_category(first) },
        MemoryCategory::GcHeap,
        "the reset promoted the write"
    );
    let first_block = crate::memory::block_pool::BlockHeader::of_ptr(first as *const u8);
    assert_eq!(retained(), before + 1);

    unsafe {
        fixture.long_lived.write_into_session(arena, session, 0);
        crate::promote::arena_reset_full(arena);
    }

    let second = slot_of(session, SESSION_WRITE_SLOT);
    assert_ne!(first, second);
    assert_ne!(
        crate::memory::block_pool::BlockHeader::of_ptr(second as *const u8),
        first_block
    );
    assert_eq!(retained(), before + 1, "the first write's block went home");
    fixture.let_go();
}

/// Sessions touched uniformly, replaced past their idle lifetime and
/// written at [`SESSION_WRITE_PERCENT`] of their touches hold a write in
/// the share [`SESSION_WRITE_STEADY_SHARE`] names, and the setup gives its
/// sessions writes in that share.
#[test]
fn the_session_writes_stand_at_their_steady_share() {
    let mut draws = Draws::new(9, 1, Purpose::Sessions);
    let mut last_touch = vec![0u64; SESSIONS];
    let mut written = vec![false; SESSIONS];
    let (mut sum, mut samples) = (0.0, 0);
    for request in 1..=400_000u64 {
        let slot = draws.below(SESSIONS);
        if request - last_touch[slot] > SESSION_IDLE_REQUESTS {
            written[slot] = false;
        }

        last_touch[slot] = request;
        written[slot] |= draws.percent(SESSION_WRITE_PERCENT);
        if request > 200_000 && request % 1_000 == 0 {
            sum += written.iter().filter(|&&written| written).count() as f64 / SESSIONS as f64;
            samples += 1;
        }
    }

    let simulated = sum / samples as f64;
    assert!(
        (simulated - SESSION_WRITE_STEADY_SHARE).abs() < 0.01,
        "simulated {simulated}"
    );

    let _g = test_guard();
    let shape = LongLivedShape {
        core: 10,
        values: 10,
        sessions: SESSIONS,
    };
    let fixture = Fixture::in_variant("SteadyWrites", shape, Variant::Arena);
    let share = fixture.long_lived.counts.session_writes as f64 / SESSIONS as f64;
    assert!(
        (share - SESSION_WRITE_STEADY_SHARE).abs() < 0.015,
        "setup {share}"
    );
    assert_eq!(
        fixture.long_lived.blocks_the_writes_hold(),
        fixture.long_lived.counts.session_writes,
        "each write holds a block of its own"
    );
    fixture.let_go();
}

/// After the arena variant's setup has registered its steady state, every
/// resident value stands candidate: no eviction registers, and a reset
/// registers the values its misses built and the sessions it replaced and
/// nothing else.
#[test]
fn the_arena_steady_state_leaves_no_resident_value_to_register() {
    let _g = test_guard();
    let shape = LongLivedShape {
        core: 50,
        values: 1_000,
        sessions: 2,
    };
    let mut fixture = Fixture::in_variant("ArenaSteady", shape, Variant::Arena);
    unsafe { fixture.long_lived.register_the_steady_state(&mut || {}) };
    let mut streams = Streams::new(6, 1);
    let (mut registered, mut built) = (0, 0);
    for _ in 0..40 {
        let plan = Plan::with_count(&mut streams, fixture.long_lived.targets(), 200);
        let counts_before = fixture.long_lived.counts;
        let mut build = fixture.build();
        let (request, _) = unsafe { build_whole(&mut build, plan) };
        registered += unsafe { request.finish(&mut build) }.registered;
        let counts = fixture.long_lived.counts;
        built += counts.misses - counts_before.misses + counts.sessions_replaced
            - counts_before.sessions_replaced;
    }

    let counts = fixture.long_lived.counts;
    assert!(counts.misses > 0, "the case evicts");
    assert_eq!(counts.evictions_registering, 0);
    assert_eq!(registered, built);
    fixture.let_go();
}

/// A buffer this thread allocated and another thread frees is one free from
/// another thread, the count the rig reads the web loads by.
#[test]
fn a_free_from_another_thread_is_counted_once() {
    let _g = test_guard();
    let buffer = crate::cycle::testing::Sent(unsafe { crate::memory::stdapi::ll_alloc(64, 8) });
    let _ = crate::memory::heap::take_frees_from_another_thread();
    std::thread::spawn(move || unsafe { crate::memory::stdapi::ll_free(buffer.into_inner()) })
        .join()
        .expect("the other thread freed the buffer");
    assert_eq!(crate::memory::heap::take_frees_from_another_thread(), 1);
}
