//! The web loads' request (`dev/plans/S67.md`, "`web-heap`, a request", and
//! its section S67.3): the draws that shape one request, the request built
//! object by object along its drawn timeline, its end, and the garbage it
//! leaves, in bytes by size. The rig's loop (S67.5) and the arrivals, the
//! spin and the waits' durations (S67.7) drive what is here; the cases below
//! read each figure once on an input whose answer is known.
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
//! object drawn uniformly among those born before it.

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
    Arrivals = 3,
    Cache = 4,
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
/// bytes, and sixteen a Box property, as `member_class` lays one.
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
/// the order of their places; the ORM collections' entity counts; the two
/// waits' places; and whether the end's release lands on the registered
/// context root.
pub(super) struct Plan {
    pub(super) objects: Vec<Placement>,
    pub(super) registrations: Vec<(f64, u32)>,
    pub(super) orm: Vec<usize>,
    #[expect(dead_code, reason = "S67.7's spin sleeps at them")]
    pub(super) waits_at: [f64; 2],
    pub(super) silent_end: bool,
}

impl Plan {
    /// A request of the specification's count ([`draw_the_count`]). `core`
    /// is a Zipf over the core the request is built with.
    pub(super) fn draw(shape: &mut Draws, registrations: &mut Draws, core: &Zipf) -> Self {
        let (orm, count) = draw_the_count(shape);
        Self::shaped(shape, registrations, core, orm, count)
    }

    /// A request of `count` objects with drawn collections, for a case that
    /// fixes the size; `core` as [`Plan::draw`] takes it.
    pub(super) fn with_count(
        shape: &mut Draws,
        registrations: &mut Draws,
        core: &Zipf,
        count: usize,
    ) -> Self {
        let orm = draw_the_orm(shape);
        let count = count.max(CONTEXT + orm_objects(&orm));
        Self::shaped(shape, registrations, core, orm, count)
    }

    fn shaped(
        shape: &mut Draws,
        registrations: &mut Draws,
        core: &Zipf,
        orm: Vec<usize>,
        count: usize,
    ) -> Self {
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

        let tree = count - CONTEXT - orm_objects(&orm);
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
            let extra = if shape.percent(45.0) {
                if shape.percent(50.0) {
                    ExtraEdge::Core(core.draw(shape) as u32)
                } else {
                    ExtraEdge::Context(shape.below(CONTEXT) as u32)
                }
            } else if shape.percent(1.0) {
                ExtraEdge::Core(core.draw(shape) as u32)
            } else {
                ExtraEdge::None
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

        // Births after the context, uniform over the life and in build
        // order, so that a parent is always born before its child.
        let mut births: Vec<f64> = (CONTEXT..objects.len()).map(|_| shape.unit()).collect();
        births.sort_unstable_by(f64::total_cmp);
        for (object, at) in objects[CONTEXT..].iter_mut().zip(births) {
            object.born_at = at;
        }

        // As many registrations as 10 % of the objects after the context,
        // each at a uniform place on an object born before it.
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
        let mut waits_at = [shape.unit(), shape.unit()];
        waits_at.sort_unstable_by(f64::total_cmp);
        let silent_end = shape.percent(50.0);
        Self {
            objects,
            registrations: registered,
            orm,
            waits_at,
            silent_end,
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

/// What a request is built with: the thread's context and arena, the
/// classes, and the core its edges point into.
pub(super) struct RequestBuild<'a> {
    pub(super) context: LLContext,
    pub(super) arena: *mut Arena,
    pub(super) classes: &'a WebClasses,
    pub(super) core: &'a [*mut Object],
}

/// What one [`Request::advance`] did: the bytes born by size index, and the
/// registrations made.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) struct Advanced {
    pub(super) born: [usize; 3],
    pub(super) registered: usize,
}

/// A request in flight: its plan, the objects born so far, and where on the
/// timeline its births and registrations stand.
pub(super) struct Request {
    plan: Plan,
    objects: Vec<*mut Object>,
    registrations_done: usize,
    /// The context object the request's external reference is on.
    externally_held: *mut Object,
}

impl Request {
    /// Start `plan`: its context cycle built, the external reference taken
    /// — on the context's first object where the end is silent, its second
    /// otherwise — and the first object registered. Answers the request and
    /// the context's bytes by size index.
    ///
    /// # Safety
    /// `build` is the calling mutator's, at a point where it may allocate;
    /// its core's objects are live GC-heap objects the caller keeps for the
    /// request's life, and `plan` was drawn over a Zipf of that core's
    /// length.
    pub(super) unsafe fn start(build: &mut RequestBuild, plan: Plan) -> (Self, [usize; 3]) {
        let mut objects = Vec::with_capacity(plan.objects.len());
        let mut born = [0; 3];
        for object in &plan.objects[..CONTEXT] {
            let size_index = object.size_index as usize;
            objects.push(unsafe {
                new_constructed(
                    &mut build.context,
                    build.classes.0[size_index],
                    MemoryCategory::GcHeap,
                )
            });
            born[size_index] += SIZES[size_index];
        }

        for index in 0..CONTEXT {
            unsafe {
                move_prop(
                    objects[index],
                    prop_offset(0),
                    objects[(index + 1) % CONTEXT],
                )
            };
        }

        let externally_held = objects[usize::from(!plan.silent_end)];
        unsafe {
            ll_retain(externally_held as *mut RcHeader);
            register(objects[0]);
        }

        let request = Self {
            plan,
            objects,
            registrations_done: 0,
            externally_held,
        };
        (request, born)
    }

    /// Do every birth placed before `to` on the timeline, and the
    /// registrations placed before it, at most [`REGISTRATIONS_AN_ADVANCE`];
    /// the rest wait for the next advance. A `to` of 1 or more is the life's
    /// end, before which everything stands.
    ///
    /// # Safety
    /// As [`Request::start`], on the same `build`.
    pub(super) unsafe fn advance(&mut self, build: &mut RequestBuild, to: f64) -> Advanced {
        let before = |at: f64| at < to || to >= 1.0;
        let mut advanced = Advanced::default();
        while let Some(object) = self.plan.objects.get(self.objects.len())
            && before(object.born_at)
        {
            let size_index = object.size_index as usize;
            let born = unsafe {
                new_constructed(
                    &mut build.context,
                    build.classes.0[size_index],
                    MemoryCategory::GcHeap,
                )
            };
            let (parent, slot) = object.parent.expect("a born object has a parent");
            unsafe {
                move_prop(
                    self.objects[parent as usize],
                    prop_offset(u32::from(slot)),
                    born,
                )
            };
            let target = match object.extra {
                ExtraEdge::None => std::ptr::null_mut(),
                ExtraEdge::Context(index) | ExtraEdge::BackTo(index) => {
                    self.objects[index as usize]
                }
                ExtraEdge::Core(index) => build.core[index as usize],
            };
            if !target.is_null() {
                unsafe { store_prop(build.arena, born, prop_offset(0), target) };
            }

            advanced.born[size_index] += SIZES[size_index];
            self.objects.push(born);
        }

        while advanced.registered < REGISTRATIONS_AN_ADVANCE
            && let Some(&(at, index)) = self.plan.registrations.get(self.registrations_done)
            && before(at)
        {
            let object = *self
                .objects
                .get(index as usize)
                .expect("a registration's object is born before its place");
            unsafe { register(object) };
            self.registrations_done += 1;
            advanced.registered += 1;
        }

        advanced
    }

    /// Whether every birth and registration is done.
    pub(super) fn is_complete(&self) -> bool {
        self.objects.len() == self.plan.objects.len()
            && self.registrations_done == self.plan.registrations.len()
    }

    /// The object born `index`-th, for a case that reads the request back.
    pub(super) fn object(&self, index: usize) -> *mut Object {
        self.objects[index]
    }

    pub(super) fn plan(&self) -> &Plan {
        &self.plan
    }

    /// Release the external reference. Answers whether it landed on a
    /// registered object, read before the release — the silent death — and
    /// the request's bytes by size index, garbage from here on.
    ///
    /// # Safety
    /// As [`Request::start`]; the request is complete.
    pub(super) unsafe fn end(self) -> (bool, [usize; 3]) {
        assert!(self.is_complete(), "a request ends after its last event");
        let silent =
            unsafe { mutator_flags(self.externally_held as *const RcHeader) } & CANDIDATE_BIT != 0;
        unsafe {
            assert!(
                !ll_release(self.externally_held as *mut RcHeader),
                "the context ring holds the object the reference was on"
            );
        }

        (silent, self.plan.bytes())
    }
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
/// teardowns free, and at a request's end, where its bytes stop being
/// reachable; a birth moves the held and the reachable bytes together, but
/// for a birth whose allocation collects — a refill that takes remote
/// returns, which the web loads make none of, or a pressure collection. So
/// [`Garbage::read`] after each of those events integrates it exactly.
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
        }
    }

    /// `bytes` were born reachable.
    pub(super) fn born(&mut self, bytes: [usize; 3]) {
        for size_index in 0..3 {
            self.reachable[size_index] += bytes[size_index];
        }
    }

    /// `bytes` stopped being reachable: a request ended.
    pub(super) fn ended(&mut self, bytes: [usize; 3]) {
        for size_index in 0..3 {
            self.reachable[size_index] -= bytes[size_index];
        }
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
        self.peak = self.current;
        self.peak_sum = self.current.iter().sum();
        self.from = now;
        self.since = now;
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

/// A case's request-building state: the arena, the classes and a strongly
/// connected core of 128-byte objects, each holding the next in its slot 0
/// and one drawn other in its slot 1, kept by the case's creation reference
/// on the first; the stand-in for S67.4's core.
struct Fixture {
    arena: Box<Arena>,
    classes: WebClasses,
    core: Vec<*mut Object>,
    zipf: Zipf,
}

impl Fixture {
    /// A fixture of `core_objects`, its classes named after `name`, on a
    /// thread whose lanes it resets. The caller holds the pool's guard.
    fn new(name: &str, core_objects: usize) -> Self {
        reset_lanes();
        let classes = WebClasses::new(name);
        let mut arena = Box::new(Arena::new());
        let arena_ptr: *mut Arena = &mut *arena;
        let mut context = LLContext { arena: arena_ptr };
        let core: Vec<*mut Object> = (0..core_objects)
            .map(|_| unsafe { new_constructed(&mut context, classes.0[1], MemoryCategory::GcHeap) })
            .collect();
        let mut draws = Draws::new(0, 0, Purpose::Cache);
        for index in 1..core_objects {
            unsafe { move_prop(core[index - 1], prop_offset(0), core[index]) };
            let other = core[draws.below(core_objects)];
            unsafe { store_prop(arena_ptr, core[index], prop_offset(1), other) };
        }

        unsafe { store_prop(arena_ptr, core[core_objects - 1], prop_offset(0), core[0]) };
        Self {
            arena,
            classes,
            zipf: Zipf::new(core.len()),
            core,
        }
    }

    /// A plan of `count` objects from the streams of `seed`.
    fn plan(&self, seed: u64, count: usize) -> Plan {
        Plan::with_count(
            &mut Draws::new(seed, 1, Purpose::Shape),
            &mut Draws::new(seed, 1, Purpose::Registrations),
            &self.zipf,
            count,
        )
    }

    fn build(&mut self) -> RequestBuild<'_> {
        let arena: *mut Arena = &mut *self.arena;
        RequestBuild {
            context: LLContext { arena },
            arena,
            classes: &self.classes,
            core: &self.core,
        }
    }
}

/// The value held in `object`'s slot `slot`, null for none.
fn slot_of(object: *mut Object, slot: u32) -> *mut Object {
    unsafe { entity_checked(&*Object::prop_at(object, prop_offset(slot))) as *mut Object }
}

/// Build `plan` whole on this thread, returning the request and its births.
///
/// # Safety
/// As [`Request::start`].
unsafe fn build_whole(build: &mut RequestBuild, plan: Plan) -> (Request, [usize; 3]) {
    let (mut request, mut born) = unsafe { Request::start(build, plan) };
    while !request.is_complete() {
        let advanced = unsafe { request.advance(build, 1.0) };
        for size_index in 0..3 {
            born[size_index] += advanced.born[size_index];
        }
    }

    (request, born)
}

/// The draws hold the specification's distributions: over 10,000 counts the
/// median and the log's σ, and over the objects of fixed-count plans the
/// class shares, the closures', the core edges', the registrations' with
/// their places, and the collections'.
#[test]
fn the_draws_hold_their_distributions() {
    let core = Zipf::new(5_000);
    let mut shape = Draws::new(1, 1, Purpose::Shape);
    let mut logs: Vec<f64> = (0..10_000)
        .map(|_| (draw_the_count(&mut shape).1 as f64).ln())
        .collect();
    logs.sort_unstable_by(f64::total_cmp);
    let median = logs[logs.len() / 2].exp();
    assert!((median / 10_000.0 - 1.0).abs() < 0.03, "median {median}");
    let mean = logs.iter().sum::<f64>() / logs.len() as f64;
    let sigma = (logs.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / logs.len() as f64).sqrt();
    assert!((sigma - 1.0).abs() < 0.05, "σ {sigma}");

    let mut registrations = Draws::new(1, 1, Purpose::Registrations);
    let (mut sizes, mut tree, mut closures, mut core_edges) = ([0usize; 3], 0, 0, 0);
    let (mut registered, mut places, mut payload) = (0, 0.0, 0);
    let (mut collections, mut entities) = (0, 0);
    for _ in 0..50 {
        let plan = Plan::with_count(&mut shape, &mut registrations, &core, 2_000);
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
        let orm = draw_the_orm(&mut shape);
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
    let mut fixture = Fixture::new("PlanNamed", 50);
    let plan = fixture.plan(7, 2_000);
    let bytes = plan.bytes();
    let before = held_by_size();
    let core = fixture.core.clone();
    let mut build = fixture.build();
    let (request, born) = unsafe { build_whole(&mut build, plan) };
    assert_eq!(born, bytes);
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

    let (_, ended) = unsafe { request.end() };
    assert_eq!(ended, bytes);
    unsafe { crate::gc::ll_gc_collect_cycles() };
}

/// The end's release lands on the registered first object and registers
/// nothing, or on the unregistered second and registers it.
#[test]
fn a_silent_end_registers_nothing_and_the_other_one_root() {
    let _g = test_guard();
    let mut fixture = Fixture::new("SilentEnd", 10);
    for silent in [true, false] {
        let mut plan = fixture.plan(3, 100);
        plan.silent_end = silent;
        let (request, _) = unsafe { build_whole(&mut fixture.build(), plan) };
        let before = crate::cycle::queue::candidate_count();
        let (landed_on_a_candidate, _) = unsafe { request.end() };
        let after = crate::cycle::queue::candidate_count();
        assert_eq!(landed_on_a_candidate, silent);
        assert_eq!(after - before, usize::from(!silent), "silent {silent}");
        unsafe { crate::gc::ll_gc_collect_cycles() };
    }
}

/// One advance registers at most half the poll's stride, the rest at the
/// next.
#[test]
fn an_advance_registers_at_most_half_the_stride() {
    let _g = test_guard();
    let mut fixture = Fixture::new("HalfStride", 10);
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
    let mut fixture = Fixture::new("Integrated", 10);
    let plan = fixture.plan(9, 500);
    let mut garbage = Garbage::new(t0, held_by_size());
    let (request, born) = unsafe { build_whole(&mut fixture.build(), plan) };
    garbage.born(born);
    garbage.read(at(1), held_by_size());
    assert_eq!(garbage.current(), [0, 0, 0]);
    let (_, bytes) = unsafe { request.end() };
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
}

/// The same seed draws the same plan and checksum whatever another purpose's
/// stream draws, and another repeat draws another stream.
#[test]
fn a_seed_draws_one_plan_whatever_the_other_streams_draw() {
    let core = Zipf::new(100);
    let draw = |extra: usize| {
        let mut other = Draws::new(2, 3, Purpose::Arrivals);
        for _ in 0..extra {
            let _ = other.unit();
        }

        let mut shape = Draws::new(2, 3, Purpose::Shape);
        let mut registrations = Draws::new(2, 3, Purpose::Registrations);
        let plan = Plan::draw(&mut shape, &mut registrations, &core);
        (
            plan.objects.len(),
            plan.registrations.len(),
            shape.checksum(),
        )
    };
    assert_eq!(draw(0), draw(1_000));
    let mut a = Draws::new(2, 3, Purpose::Shape);
    let mut b = Draws::new(2, 4, Purpose::Shape);
    assert_ne!(a.unit(), b.unit(), "another repeat draws another stream");
}
