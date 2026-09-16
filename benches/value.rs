//! What the sixteen-byte box costs to read and to write in memory, before
//! and after the discriminating word (`rfc/model/values.md`, "ValueBox
//! Layout"; the reading is `dev/BENCHMARKS.md`, "S48.2 the box's price
//! after the relayout").
//!
//! What the numbers answer (`dev/BENCHMARKS.md`, "S48.0 the box's price
//! before the relayout"): the ruling that closed `rfc` A1 expected the
//! arithmetic on boxed `int` and `float` unmoved, the one-word type tests
//! unmoved, the decode of a box's tag to pay a second load, and a hash
//! lookup to pay one `test` and one `cmov` per hop of a collision chain
//! (`rfc/dev/DECISIONS.md`, "A1 closes on a discriminating word", the
//! cost line and the Critic paragraph). Each expectation has an arm here.
//!
//! - **arithmetic** — a loop over boxes in memory: read, unbox, add,
//!   box, store back; every box a `+0` payload read and a tag decode.
//!   The store-back is the representation's copy, and on a struct of four
//!   fields it compiles field-wise — payload, tag byte, flags byte, the
//!   reserved bytes through a stack temporary — which was 45 % of the
//!   arm's figure on 2026-09-14 (`dev/BENCHMARKS.md`, the S48.0 entry); a
//!   struct of two words copies as two qwords, so the arm is expected to
//!   fall by about that on the after-tree for a reason the ruling did not
//!   price. A separate copy arm was tried and refused: `black_box` on a
//!   sixteen-byte value spills it, and the arm measured the spill.
//! - **tag-only** — three arms over one mixed population, none reading a
//!   payload: the one-word tests (`is_null`, `is_int`), the decode
//!   (`tag()` under a `match`), and the truth test (`is_truthy_tag`). The
//!   ruling's "loads both words" is a claim about the decode alone, so
//!   the arms are kept apart.
//! - **lookup** — `get` on a hash whose integer keys stride by 64 and so
//!   land in one bucket at every table size the table grows through: a
//!   32-hop chain, one short of the collision defence's threshold, walked
//!   from the deepest key. The elements alternate a pointer-arm object
//!   and an immediate-arm integer in an aperiodic order, which is the same
//!   order on every lookup — so an arm test emitted as a branch is learned
//!   by the predictor within a few lookups and costs nothing here, while
//!   one emitted as a `cmov` joins the hop's load-to-use chain and costs
//!   its whole latency. The instrument sees a `cmov` and not a branch;
//!   which one was emitted is read from the disassembly of `Entry::link`,
//!   not from the clock. The one-hop lookup beside it is the same call on
//!   the shallowest key, and the reading is the slope,
//!   `(chain32 − chain1) / 31`, since the one-hop figure carries the
//!   call's own constant — the element copy and the reference decode of
//!   `element::get` — which moves for its own reasons.
//!
//! **A null pair runs beside every bench**: two arms whose code is the
//! same closure under two names. Their difference is zero by
//! construction, and the spread the run reports for it is the
//! instrument's own within one binary. It is not the bar between two
//! binaries: identical loops in two builds differed by 7–10 % on their
//! minimum through code placement alone on 2026-09-14, so a before/after
//! across builds carries the placement bar of `dev/BENCHMARKS.md`, Method
//! ("the placement bar") on top of this one.
//!
//! **Every loop bound passes through `black_box`**: with a constant trip
//! count visible to the optimizer two arms of one loop disagreed by
//! 0.11 ns per store on 2026-08-15 (`dev/BENCHMARKS.md`, "what the
//! release-at-reset record costs, and the statistic that decides the
//! answer").
//!
//! **The minimum is recorded beside criterion's median.** Criterion's
//! summary carries the mean and the median and no minimum, and the
//! statistic decides the answer where a probe is small (`dev/BENCHMARKS.md`,
//! same entry): each timed region here is timed on its own inside
//! `iter_custom`, and the least per-operation figure seen is printed
//! after the bench under `min:`.
//!
//! **What these numbers are not.** They are not the cost of the same
//! operations in compiled PHP, which inlines the accessors and knows most
//! arms statically; they are the cost of the work inside `Value` and the
//! table, and the comparison between the before-tree and the after-tree,
//! built and run in one sitting, is what they are for.
//!
//! ```
//! cargo bench --bench value
//! ```

use std::cell::Cell;
use std::time::{Duration, Instant};

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use ll_model::array::element;
use ll_model::array::entity::ll_array_new;
use ll_model::array::table::Key;
use ll_model::memory::arena::Arena;
use ll_model::memory::barrier::ref_store;
use ll_model::memory::context::{LLContext, set_current_context};
use ll_model::object::{ll_object_new, object_constructed};
use ll_model::refcount::{MemoryCategory, RcHeader, ll_release};
use ll_model::value::{Tag, Value};
use ll_model::{Class, ClassBuilder, Object};

/// Boxes in the arithmetic and tag-only populations. Sixteen KiB, which
/// is inside L1 on every target this crate is measured on, so the figure
/// is the instruction's and not the cache's.
const BOXES: usize = 1024;

/// Reads per timed region in the tag-only and arithmetic benches: the
/// population, walked this many times, so the clock read around the
/// region is well under a percent of it.
const PASSES: usize = 64;

/// Keys on the lookup's collision chain. One short of the table's
/// `CHAIN_LIMIT` (32): the insert counts the entries it walks before the
/// push, so the 32nd colliding key is admitted without drawing the
/// table's salt and the 33rd would not be.
const CHAIN: i64 = 32;

/// The stride that puts every key in one bucket: an unsalted table
/// indexes an integer key by its value under the slot mask, and the mask
/// is at most 63 while the table holds 32 entries.
const STRIDE: i64 = 64;

/// Lookups per timed region.
const LOOKUPS: usize = 1000;

thread_local! {
    /// The least per-operation time, in nanoseconds, a timed region of the
    /// current bench has shown. Reset by [`report_min`] after each bench
    /// prints it. A float, because a region's per-operation share is a
    /// fraction of a nanosecond and `Duration` would truncate it.
    static MIN: Cell<f64> = const { Cell::new(f64::INFINITY) };
}

/// Time `work` once per iteration and keep the least per-operation
/// figure, `ops` being the operations one call of `work` performs.
fn timed(iterations: u64, ops: usize, mut work: impl FnMut()) -> Duration {
    let mut elapsed = Duration::ZERO;
    for _ in 0..iterations {
        let start = Instant::now();
        work();
        let took = start.elapsed();
        elapsed += took;
        MIN.with(|m| {
            let per_op = took.as_secs_f64() * 1e9 / ops as f64;
            if per_op < m.get() {
                m.set(per_op);
            }
        });
    }

    elapsed
}

/// Print the least per-operation figure the bench `name` showed and
/// reset it for the next bench.
fn report_min(name: &str) {
    MIN.with(|m| {
        eprintln!("min: {name}: {:.3} ns/op", m.get());
        m.set(f64::INFINITY);
    });
}

/// A class with no properties: the entity the pointer-arm boxes name.
fn leaf_class(name: &str) -> *const Class {
    ClassBuilder::new(name).build()
}

/// A class with one Box property at offset 16: the slot the lookup's
/// array lives in, so every write to the array goes through the layer.
fn holder_class(name: &str) -> *const Class {
    ClassBuilder::new(name).prop("value", true).build()
}

/// Create and construct one object, as generated code does in two calls.
///
/// # Safety
/// `ctx` mounts a live arena.
unsafe fn object(ctx: *mut LLContext, class: *const Class) -> *mut Object {
    unsafe {
        let obj = ll_object_new(ctx, class, MemoryCategory::GcHeap);
        assert!(!obj.is_null(), "object creation refused: out of memory");
        assert!(object_constructed(ctx, obj), "construction refused");
        obj
    }
}

/// `int` and `float` boxes alternating, the shape a numeric loop over a
/// mixed array has.
fn numeric_population() -> Vec<Value> {
    (0..BOXES)
        .map(|i| {
            if i % 2 == 0 {
                Value::int(i as i64)
            } else {
                Value::float(i as f64)
            }
        })
        .collect()
}

/// Every tag class in turn — null, false, true, int, float, and a
/// pointer-arm object — so a tag switch takes every arm and a predictor
/// has a period of six to learn, which it does; the arms are compared
/// against each other on the same population, not against a random one.
fn mixed_population(leaf: *mut Object) -> Vec<Value> {
    (0..BOXES)
        .map(|i| match i % 6 {
            0 => Value::null(),
            1 => Value::bool(false),
            2 => Value::bool(true),
            3 => Value::int(i as i64),
            4 => Value::float(i as f64),
            _ => Value::entity(Tag::Object, leaf as *mut RcHeader),
        })
        .collect()
}

/// Read, unbox, add, box, store back — over the population, `PASSES`
/// times. What a `foreach` doing arithmetic over a mixed array pays per
/// element inside the box, the array's own walk excluded.
fn arithmetic_pass(boxes: &mut [Value]) {
    let n = black_box(boxes.len());
    for slot in boxes.iter_mut().take(n) {
        let v = *slot;
        *slot = match v.tag() {
            Tag::Int => Value::int(v.as_int().wrapping_add(5)),
            Tag::Float => Value::float(v.as_float() + 0.5),
            _ => v,
        };
    }
}

fn arithmetic(c: &mut Criterion) {
    let mut boxes = numeric_population();
    for name in ["value/arithmetic_x1024", "value/arithmetic_x1024_null_pair"] {
        c.bench_function(name, |b| {
            b.iter_custom(|iterations| {
                timed(iterations, BOXES * PASSES, || {
                    for _ in 0..black_box(PASSES) {
                        arithmetic_pass(&mut boxes);
                    }
                })
            })
        });
        report_min(name);
    }

    black_box(&boxes);
}

/// The one-word tests: `is_null` and `is_int` over the population,
/// counted so that neither is optimized out.
fn one_word_pass(boxes: &[Value]) -> usize {
    let n = black_box(boxes.len());
    let mut count = 0usize;
    for v in boxes.iter().take(n) {
        count += usize::from(v.is_null()) + usize::from(v.is_int());
    }

    count
}

/// The decode: the tag under a `match`, every arm counted.
fn decode_pass(boxes: &[Value]) -> [usize; 10] {
    let n = black_box(boxes.len());
    let mut counts = [0usize; 10];
    for v in boxes.iter().take(n) {
        counts[v.tag() as usize] += 1;
    }

    counts
}

/// The truth test, which reads no payload for null, false and true and
/// answers "decode the payload" for the rest.
fn truth_pass(boxes: &[Value]) -> usize {
    let n = black_box(boxes.len());
    let mut count = 0usize;
    for v in boxes.iter().take(n) {
        count += match v.is_truthy_tag() {
            Some(true) => 2,
            Some(false) => 1,
            None => 0,
        };
    }

    count
}

fn tag_only(c: &mut Criterion, ctx: *mut LLContext) {
    let leaf = unsafe { object(ctx, leaf_class("ValueBenchLeaf")) };
    let boxes = mixed_population(leaf);

    let arms: [(&str, fn(&[Value]) -> usize); 4] = [
        ("value/tag_only/one_word_x1024", |b| one_word_pass(b)),
        ("value/tag_only/one_word_x1024_null_pair", |b| {
            one_word_pass(b)
        }),
        ("value/tag_only/decode_x1024", |b| {
            decode_pass(b).iter().sum()
        }),
        ("value/tag_only/truth_x1024", |b| truth_pass(b)),
    ];
    for (name, pass) in arms {
        c.bench_function(name, |b| {
            b.iter_custom(|iterations| {
                timed(iterations, BOXES * PASSES, || {
                    let mut sink = 0usize;
                    for _ in 0..black_box(PASSES) {
                        sink = sink.wrapping_add(pass(&boxes));
                    }

                    black_box(sink);
                })
            })
        });
        report_min(name);
    }

    unsafe { ll_release(leaf as *mut RcHeader) };
}

/// The lookup's array: `CHAIN` keys striding by `STRIDE`, deepest first,
/// the elements alternating an object and an integer in an order a
/// predictor does not learn (module doc). Answers the holder's slot,
/// which names the array, and the leaves the pointer-arm elements name,
/// to be released with the array.
///
/// # Safety
/// `ctx` mounts a live arena; `arena` is that arena.
unsafe fn chained_array(ctx: *mut LLContext, arena: *mut Arena) -> (*mut Value, Vec<*mut Object>) {
    let holder = unsafe { object(ctx, holder_class("ValueBenchHolder")) };
    let slot = unsafe { Object::prop_at(holder, 16) };
    let array = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    unsafe {
        assert!(ref_store(
            arena,
            holder as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, array as *mut RcHeader),
        ));
        ll_release(array as *mut RcHeader);
    }

    let leaf = leaf_class("ValueBenchChainLeaf");
    let mut leaves = Vec::new();
    // An aperiodic pattern of pointer-arm and immediate-arm elements: a
    // Fibonacci-like sequence taken modulo a prime. It is the same walk on
    // every lookup, so a predictor learns it (module doc); what the
    // pattern buys is that no single arm test is constant over the chain.
    let mut pattern = (1u32, 1u32);
    for k in 0..CHAIN {
        let pointer_arm = pattern.0 % 7 < 3;
        pattern = (pattern.1, (pattern.0 + pattern.1) % 101);
        let value = if pointer_arm {
            let obj = unsafe { object(ctx, leaf) };
            leaves.push(obj);
            Value::entity(Tag::Object, obj as *mut RcHeader)
        } else {
            Value::int(k)
        };
        assert!(
            unsafe {
                element::set(
                    ctx,
                    MemoryCategory::GcHeap,
                    slot,
                    Key::Int(k * STRIDE),
                    value,
                )
            },
            "the store was refused"
        );
    }

    (slot, leaves)
}

fn lookup(c: &mut Criterion, ctx: *mut LLContext, arena: *mut Arena) {
    let (slot, leaves) = unsafe { chained_array(ctx, arena) };
    let deepest = Key::Int(0);
    let shallowest = Key::Int((CHAIN - 1) * STRIDE);

    let arms = [
        ("value/lookup/chain32_x1000", deepest),
        ("value/lookup/chain32_x1000_null_pair", deepest),
        ("value/lookup/chain1_x1000", shallowest),
    ];
    for (name, key) in arms {
        c.bench_function(name, |b| {
            b.iter_custom(|iterations| {
                let mut hits = 0usize;
                let elapsed = timed(iterations, LOOKUPS, || {
                    for _ in 0..black_box(LOOKUPS) {
                        hits +=
                            usize::from(unsafe { element::get(slot, black_box(key)) }.is_some());
                    }
                });
                assert_eq!(
                    hits,
                    LOOKUPS * iterations as usize,
                    "every lookup finds its key"
                );
                elapsed
            })
        });
        report_min(name);
    }

    for leaf in leaves {
        unsafe { ll_release(leaf as *mut RcHeader) };
    }
}

fn value(c: &mut Criterion) {
    // Criterion runs every arm on this thread, and the runtime serves a
    // thread once `ll_thread_init` has started it.
    assert!(
        ll_model::memory::heap::ll_thread_init(),
        "the runtime started the bench thread"
    );
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    arithmetic(c);
    tag_only(c, context_ptr);
    lookup(c, context_ptr, arena_ptr);

    set_current_context(std::ptr::null_mut());
}

criterion_group!(benches, value);
criterion_main!(benches);
