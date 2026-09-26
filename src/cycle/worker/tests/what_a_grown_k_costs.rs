//! What a threshold batch costs the mutator once `size_the_next_batch` has
//! grown K as far as it goes, on the two live disjoint shapes: the length of
//! the grant, the wait of a mutator that asks for its token at the start of
//! the trace, and the deaths a mutator freeing through the grant withholds
//! (`PLAN.md`, S65.13's measurement; `dev/DECISIONS.md`, "the trace in parts
//! waits for the recall and the stack marks").
//!
//! **K is grown by the batches themselves.** A collector thread of the case's
//! serves this thread's record once per batch, as a round does, and the
//! mutator consents between yields; K is read after each batch until it
//! repeats with a period of one or two batches, and the arms run on from
//! there with K still sized by every batch. A batch's roots are registered
//! again once its verdicts are discarded, so that R holds the same
//! [`RINGS`] roots whatever K asks for: the mutator's collection over P never
//! runs, and nothing stamps a ring.
//!
//! The file names nothing a tree before the batch in parts lacks, so that the
//! same probe runs on both sides of the step.
//!
//! Run in a release build:
//! `cargo test --release --lib -- --ignored what_a_grown_k_costs --test-threads=1 --nocapture`.

use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::deferred_slot_reuse::{DEATHS_MARK, foreign_withheld_counts};
use crate::cycle::queue::verdicts::{discard_standing_verdicts, standing_verdicts};
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::{Object, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, ll_release, ll_retain};
use crate::test_support::{prop_offset, store_prop};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

/// Rings built, one root each: K's bound, so that a batch at the bound takes
/// a root of every ring.
const RINGS: usize = BATCH_BOUND;

/// The members' size class, 128 bytes, a block holding 512 of them and a
/// touched block's row array about 2 KiB; and the counted properties that
/// make it.
const MEMBER_CLASS_BYTES: usize = 128;
const MEMBER_PROPS: usize = (MEMBER_CLASS_BYTES - 16) / 16;

/// Batches the growth may take before the case gives up on K settling.
const GROWTH_BATCHES: usize = 24;

/// Batches per arm, and the leading ones dropped.
const SAMPLES: usize = 21;
const WARM_UP: usize = 5;

/// Deaths a freeing mutator has in hand per batch: three marks, so that the
/// recall at the first mark is reached whatever the grant's length.
const DEATHS_IN_HAND: usize = 3 * DEATHS_MARK;

/// One shape: [`RINGS`] live rings of `members`, one member per block where
/// `spread`, and otherwise side by side, some 170 roots to a block.
#[derive(Clone, Copy)]
struct Shape {
    name: &'static str,
    members: usize,
    spread: bool,
}

/// The two shapes of the step's measurement, and a dense one whose blocks
/// hold some 170 roots each, a part's lookup of met roots walking its own
/// rows there rather than the block's roots.
const SHAPES: [Shape; 3] = [
    Shape {
        name: "disjoint-live",
        members: 5,
        spread: true,
    },
    Shape {
        name: "disjoint-wide-live",
        members: 20,
        spread: true,
    },
    Shape {
        name: "dense-live",
        members: 2,
        spread: false,
    },
];

/// Every object the rings are made of, which the case takes apart at its end.
struct Built {
    keepers: Vec<*mut Object>,
    members: Vec<*mut Object>,
    fillers: Vec<*mut Object>,
}

/// The rings of `shape`, each held by a keeper and registered at its first
/// member, every member alone in its block among fillers.
///
/// # Safety
/// A quiescent heap under the pool's guard; `class` has [`MEMBER_PROPS`]
/// counted properties.
unsafe fn build(arena: &mut Arena, class: *const Class, shape: Shape) -> Built {
    let arena_ptr: *mut Arena = arena;
    let mut context = LLContext { arena };
    let fillers_per_member = if shape.spread {
        crate::cycle::loads::slots_per_block(MEMBER_CLASS_BYTES) - 1
    } else {
        0
    };
    let mut built = Built {
        keepers: Vec::with_capacity(RINGS),
        members: Vec::with_capacity(RINGS * shape.members),
        fillers: Vec::with_capacity(RINGS * shape.members * fillers_per_member),
    };
    for _ in 0..RINGS {
        let ring: Vec<*mut Object> = (0..shape.members)
            .map(|_| unsafe {
                let member = new_constructed(&mut context, class, MemoryCategory::GcHeap);
                for _ in 0..fillers_per_member {
                    built.fillers.push(new_constructed(
                        &mut context,
                        class,
                        MemoryCategory::GcHeap,
                    ));
                }
                member
            })
            .collect();
        unsafe {
            for (position, &member) in ring.iter().enumerate() {
                crate::cycle::testing::move_prop(
                    member,
                    prop_offset(0),
                    ring[(position + 1) % shape.members],
                );
            }

            let keeper = new_constructed(&mut context, class, MemoryCategory::GcHeap);
            store_prop(arena_ptr, keeper, prop_offset(0), ring[0]);
            ll_retain(ring[0] as *mut RcHeader);
            assert!(!ll_release(ring[0] as *mut RcHeader), "the ring holds it");
            built.keepers.push(keeper);
        }

        built.members.extend(ring);
    }

    built
}

/// A collector thread of the case's, serving this thread's record once per
/// job.
struct Collector {
    jobs: Sender<()>,
    served: Receiver<Served>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Collector {
    fn start() -> Self {
        let (jobs, jobs_in) = std::sync::mpsc::channel::<()>();
        let (served_out, served) = std::sync::mpsc::channel();
        let sent = Sent(record());
        let thread = std::thread::spawn(move || {
            assert!(crate::memory::heap::ll_thread_init());
            let record = sent.into_inner();
            for () in jobs_in {
                let outcome = unsafe { testing::serve_alone(record) };
                served_out
                    .send(outcome)
                    .expect("the case reads the outcome");
            }
        });
        Self {
            jobs,
            served,
            thread: Some(thread),
        }
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        let (jobs, _) = std::sync::mpsc::channel();
        drop(std::mem::replace(&mut self.jobs, jobs));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What the mutator does once the batch's trace has started.
#[derive(Clone, Copy, PartialEq, Eq)]
enum During {
    /// Nothing: the batch is the grant's whole length.
    Nothing,
    /// Asks for its token, and the sample is what it waited.
    Asks,
    /// Asks for its token once this long has passed since the trace's start,
    /// and the sample is what it waited from the ask.
    AsksAfter(Duration),
    /// Frees dead slots through the grant, and the sample is the most the
    /// deaths' stack held.
    Frees,
}

/// What one batch measured: K before it, its trace, and the mutator's wait or
/// the most deaths it withheld, by the arm.
struct Sample {
    k: usize,
    traced: testing::TracedBatch,
    waited: Option<Duration>,
    withheld: Option<usize>,
}

/// Dead slots of 64 bytes, freed as a teardown frees them.
fn dead_slots(count: usize) -> Vec<*mut u8> {
    (0..count)
        .map(|_| {
            let slot = unsafe { crate::memory::heap::entity_alloc(64) };
            assert!(!slot.is_null(), "the heap served");
            let header = slot as *mut RcHeader;
            unsafe {
                header.write(RcHeader::new(
                    MemoryCategory::GcHeap,
                    crate::refcount::EntityKind::Array.to_flags(),
                ));
                crate::refcount::set_header_refcount(header, 0);
            }
            slot
        })
        .collect()
}

/// One batch, the mutator doing `during` from the trace's start, its roots
/// registered again after it.
fn a_batch(collector: &Collector, during: During) -> Sample {
    unsafe { &*record() }.clear_posted_for_test();
    let k = unsafe { &*record() }.batch_size();
    let (started_out, started) = std::sync::mpsc::channel::<()>();
    let waiting_from = std::sync::Arc::new(std::sync::Mutex::new(None));
    let stamp = std::sync::Arc::clone(&waiting_from);
    let token = unsafe { &raw const (*record()).token } as usize;
    testing::at_the_start_of_the_next_trace(Box::new(move || {
        let token = unsafe { &*(token as *const crate::cycle::token::TraceToken) };
        let before = token.waits();
        let _ = started_out.send(());
        if during != During::Asks {
            return;
        }

        while token.waits() == before {
            std::hint::spin_loop();
        }

        *stamp
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
    }));
    let mut slots = if during == During::Frees {
        dead_slots(DEATHS_IN_HAND)
    } else {
        Vec::new()
    };

    testing::read_traced_batches(true);
    collector.jobs.send(()).expect("the collector serves");
    let mut held_at = None;
    let mut withheld = 0;
    let served = loop {
        if let Ok(served) = collector.served.try_recv() {
            break served;
        }

        crate::cycle::token::read_and_act_on_this_thread();
        if started.try_recv().is_ok() {
            match during {
                During::Nothing => {}
                During::Asks => {
                    drop(crate::cycle::token::HeldToken::take_or_hold_posted());
                    held_at = Some(Instant::now());
                }
                During::AsksAfter(delay) => {
                    let started_at = Instant::now();
                    while started_at.elapsed() < delay {
                        std::hint::spin_loop();
                    }

                    let from = Instant::now();
                    drop(crate::cycle::token::HeldToken::take_or_hold_posted());
                    held_at = Some(Instant::now());
                    *waiting_from
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(from);
                }
                During::Frees => {
                    // One death stays in hand for the drain after the batch.
                    while slots.len() > 1 {
                        let slot = slots.pop().expect("more than one in hand");
                        unsafe { crate::memory::stdapi::ll_free(slot) };
                        withheld = withheld.max(foreign_withheld_counts().0);
                        if !unsafe { &*record() }.token.is_held() {
                            break;
                        }
                    }
                }
            }
        }

        std::thread::yield_now();
    };
    assert!(
        matches!(served, Served::Batch { .. }),
        "a batch was made: {served:?}"
    );
    let traced = testing::take_traced_batches();
    testing::read_traced_batches(false);

    // The batch's roots back into R, P emptied by hand: the collection over P
    // that would stamp or free them never runs. The deaths left in hand are
    // freed with the byte free, so that the first of them drains what the
    // grant withheld and the next consent finds no stack at its mark.
    let roots: Vec<*mut RcHeader> = standing_verdicts().iter().map(|&(root, _)| root).collect();
    discard_standing_verdicts();
    for root in roots {
        unsafe { crate::cycle::queue::register_candidate(root) };
    }

    for slot in slots {
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    let waited = held_at.map(|held_at| {
        let from = waiting_from
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .expect("the mutator stood in the token's wait");
        held_at - from
    });
    Sample {
        k,
        traced: *traced.last().expect("the batch was traced"),
        waited,
        withheld: (during == During::Frees).then_some(withheld),
    }
}

/// The least wall of sorting [`RINGS`] indices by the addresses of the
/// rings' roots, from a shuffled order, as the batch sorts its copy: the part
/// of the grant before the trace's first reading of the recall.
fn the_sort_of(built: &Built, shape: Shape) -> Duration {
    let roots: Vec<usize> = built
        .members
        .chunks(shape.members)
        .map(|ring| ring[0] as usize)
        .collect();
    let mut state = 0x9e37_79b9_u32;
    let mut shuffled: Vec<usize> = roots.clone();
    for index in (1..shuffled.len()).rev() {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        shuffled.swap(index, state as usize % (index + 1));
    }

    (0..SAMPLES)
        .map(|_| {
            let mut order: Vec<u16> = (0..RINGS as u16).collect();
            let from = Instant::now();
            order.sort_unstable_by_key(|&index| shuffled[usize::from(index)]);
            let wall = from.elapsed();
            std::hint::black_box(&order);
            wall
        })
        .min()
        .expect("samples")
}

/// Median and least of `samples`, sorted in place.
fn median_and_least<T: Ord + Copy>(samples: &mut [T]) -> (T, T) {
    samples.sort();
    (samples[samples.len() / 2], samples[0])
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn what_a_grown_k_costs() {
    let _g = test_guard();
    reset_lanes();
    let _wait = testing::HeldRequestWait::crate_own();
    let class = {
        let mut builder = ClassBuilder::new("GrownKNode");
        for property in 0..MEMBER_PROPS {
            builder = builder.prop(&format!("p{property}"), true);
        }

        builder.build()
    };

    for shape in SHAPES {
        let mut arena = Arena::new();
        let built = unsafe { build(&mut arena, class, shape) };
        assert_eq!(crate::cycle::queue::candidate_count(), RINGS);
        unsafe { &*record() }.set_batch_size(0);
        let collector = Collector::start();

        let mut sizes = vec![unsafe { &*record() }.batch_size()];
        for _ in 0..GROWTH_BATCHES {
            let _ = a_batch(&collector, During::Nothing);
            sizes.push(unsafe { &*record() }.batch_size());
            let settled = sizes.len() >= 4 && {
                let last = sizes.len() - 1;
                sizes[last] == sizes[last - 2] && sizes[last - 1] == sizes[last - 3]
            };
            if settled {
                break;
            }
        }

        println!("{}: K after each batch of the growth {sizes:?}", shape.name);
        let mut half_a_grant = Duration::ZERO;
        for arm in 0..4 {
            let during = match arm {
                0 => During::Nothing,
                1 => During::Asks,
                2 => During::AsksAfter(half_a_grant),
                _ => During::Frees,
            };
            // A batch P's room cut short of K prices another batch, so such a
            // batch is made and not kept.
            let mut samples = Vec::with_capacity(SAMPLES);
            let mut made = 0;
            while samples.len() < SAMPLES {
                let sample = a_batch(&collector, during);
                made += 1;
                assert!(made < 4 * SAMPLES, "P's room left whole batches");
                if sample.traced.roots == sample.k {
                    samples.push(sample);
                }
            }

            let samples: Vec<Sample> = samples.into_iter().skip(WARM_UP).collect();
            let mut roots: Vec<usize> = samples.iter().map(|s| s.traced.roots).collect();
            roots.sort_unstable();
            roots.dedup();
            let mut parts: Vec<usize> = samples.iter().map(|s| s.traced.parts).collect();
            let mut walls: Vec<Duration> = samples.iter().map(|s| s.traced.wall).collect();
            let complete = samples.iter().filter(|s| s.traced.complete).count();
            let (wall, least_wall) = median_and_least(&mut walls);
            if during == During::Nothing {
                half_a_grant = wall / 2;
            }

            let (median_parts, _) = median_and_least(&mut parts);
            let measured = match during {
                During::Nothing => String::new(),
                During::Asks | During::AsksAfter(_) => {
                    let mut waits: Vec<Duration> =
                        samples.iter().filter_map(|s| s.waited).collect();
                    let (median, least) = median_and_least(&mut waits);
                    format!("; the mutator waited {median:?}, least {least:?}")
                }
                During::Frees => {
                    let mut withheld: Vec<usize> =
                        samples.iter().filter_map(|s| s.withheld).collect();
                    let (median, least) = median_and_least(&mut withheld);
                    let most = *withheld.last().expect("samples");
                    format!(
                        "; the deaths' stack held at most {median} median, {least} least, \
                         {most} most"
                    )
                }
            };
            println!(
                "{} {}: roots per batch {roots:?}, parts {median_parts} median, {complete} of {} \
                 complete, the trace {wall:?} median, {least_wall:?} least{measured}",
                shape.name,
                match during {
                    During::Nothing => "grant".to_string(),
                    During::Asks => "wait".to_string(),
                    During::AsksAfter(delay) => format!("wait after {delay:?}"),
                    During::Frees => "free".to_string(),
                },
                samples.len(),
            );
        }

        println!(
            "{}: sorting a copy of {RINGS} of its roots by address, shuffled, {:?} least",
            shape.name,
            the_sort_of(&built, shape)
        );
        drop(collector);
        unsafe { let_the_rings_go(&mut arena, built) };
        drop(arena);
        reset_lanes();
    }
}

/// Take the rings apart: the keepers let go of them, and the mutator's
/// collection over R, which holds every root, frees them; the fillers die by
/// hand.
///
/// # Safety
/// `built` came from [`build`] on this thread and no collection runs.
unsafe fn let_the_rings_go(arena: &mut Arena, built: Built) {
    let arena_ptr: *mut Arena = arena;
    unsafe { &*record() }.clear_posted_for_test();
    unsafe {
        for &keeper in &built.keepers {
            store_prop(arena_ptr, keeper, prop_offset(0), std::ptr::null_mut());
        }

        let freed = crate::gc::ll_gc_collect_cycles();
        assert_eq!(freed, built.members.len(), "the rings were garbage");
        for object in built.keepers.into_iter().chain(built.fillers) {
            assert!(ll_release(object as *mut RcHeader));
            crate::object::ll_object_die(object);
        }
    }
}
