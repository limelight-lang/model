//! Measurement probe, not a correctness test: what share of entity blocks
//! hold no entity a ring can pass through, under an allocation pattern that
//! interleaves the two kinds.
//!
//! Node B6 of `rfc/model/gc/walk/questions.md` asks whether the walk can
//! decline to touch a block at all. Its cheap shape counts the ring-capable
//! entities in each block and skips the block at zero, and what decides
//! whether that shape pays is how often a block comes out uniform. Entity
//! blocks are divided by block kind and then by size class
//! (`crate::memory::heap`), so a string and an object of the same size share
//! one, and consecutive allocation of the same kind is what makes a block
//! uniform by itself.
//!
//! B4 measured what a skip is worth: a leaf row costs the walk 40-54 ns and
//! an edge 43-47, and skipping a block saves both for every slot in it, plus
//! the storage-head read for any array.
//!
//! ## Method
//!
//! For each interleaving ratio, allocate one population of strings and
//! objects into a fresh heap, then read every entity block out of the global
//! registry and classify it: a block is **skippable** when no occupied slot
//! in it holds a kind that may close a cycle
//! (`crate::refcount::kind_may_close_a_cycle`, the predicate the design
//! already owns). Reported per ratio: blocks, skippable blocks, and the share
//! of live entities that sit in a skippable block — the last being what the
//! walk would actually stop reading.
//!
//! **The two kinds must land in one size class or the question is moot.**
//! The string's bytes are sized so its entity takes the same class as the
//! one-property object beside it; the reported class list is what says
//! whether that held, and a run whose two kinds occupy two classes measures
//! the allocator's separation instead of the interleaving.
//!
//! **This bounds the mechanism, not a program.** The ratios are chosen here,
//! and a real heap's are the corpus question of node A6. What the probe can
//! say is what the allocator does with a given interleaving, which is a
//! property of this code and not of anyone's PHP.
//!
//! ```
//! cargo test --release --lib -- --ignored measure_block_uniformity --nocapture
//! ```

use super::*;
use crate::memory::heap::snapshot_entity_blocks;
use crate::refcount::kind_may_close_a_cycle;
use crate::string::ll_string_new;

/// Entities allocated per ratio. Enough to fill many blocks of the size
/// class the two kinds share.
const POPULATION: usize = 20_000;

/// Objects per string in the interleaving, and the two pure ends. One
/// object per string is the worst case for uniformity; the pure ends say
/// what the mechanism can reach at best.
const RATIOS: [(usize, usize); 6] = [(1, 0), (0, 1), (1, 1), (1, 4), (1, 16), (4, 1)];

/// Same-kind run lengths, measured after the ratios show that any
/// interleaving at all contaminates every block. A run is that many strings
/// followed by that many objects, repeated. The block at this size class
/// holds 2 000 slots, so the interesting range brackets it.
const RUNS: [usize; 6] = [1, 100, 1_000, 2_000, 4_000, 10_000];

/// What one ratio produced.
struct Uniformity {
    blocks: usize,
    skippable: usize,
    entities: usize,
    entities_in_skippable: usize,
    /// Blocks holding both kinds at once — the shape the node is about.
    mixed: usize,
    /// Size class of every block seen, so a run says whether the two kinds
    /// could have shared a block at all.
    classes: Vec<usize>,
}

/// Classify every entity block in the global registry.
///
/// A slot whose count reads zero is free or mid-teardown and is not an
/// occupant; a block with no occupant at all counts as skippable, since the
/// walk would read nothing in it either way.
///
/// # Safety
/// No mutator is running: the caller owns the heap for the length of this
/// call.
unsafe fn classify() -> Uniformity {
    let mut out = Uniformity {
        blocks: 0,
        skippable: 0,
        entities: 0,
        entities_in_skippable: 0,
        mixed: 0,
        classes: Vec::new(),
    };

    for block in snapshot_entity_blocks() {
        let mut occupants = 0usize;
        let mut ring_capable = 0usize;
        for s in 0..block.slots {
            let slot = match &block.index {
                Some(index) => index[s] as *mut RcHeader,
                None => (block.payload + s * block.class_size) as *mut RcHeader,
            };

            let (refcount, flags) = unsafe { ((*slot).refcount, (*slot).flags) };
            if refcount == 0 {
                continue;
            }

            occupants += 1;
            if kind_may_close_a_cycle(flags) {
                ring_capable += 1;
            }
        }

        out.blocks += 1;
        out.entities += occupants;
        if !out.classes.contains(&block.class_size) {
            out.classes.push(block.class_size);
        }

        if ring_capable == 0 {
            out.skippable += 1;
            out.entities_in_skippable += occupants;
        } else if ring_capable < occupants {
            out.mixed += 1;
        }
    }

    out
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn measure_block_uniformity() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("BlockUniformity").prop("child", true).build();

    for (objects, strings) in RATIOS {
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let mut live: Vec<*mut RcHeader> = Vec::with_capacity(POPULATION);

        let group = objects + strings;
        for i in 0..POPULATION {
            let position = i % group;
            let entity = if position < objects {
                let o = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
                o as *mut RcHeader
            } else {
                // Fixed-length bytes so every string takes the same size
                // class; distinct so interning cannot fold the population.
                let bytes = format!("{i:06}");
                let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, bytes.as_bytes()) };
                s as *mut RcHeader
            };
            assert!(!entity.is_null(), "the probe's population must allocate");
            live.push(entity);
        }

        let seen = unsafe { classify() };
        let share = |part: usize, whole: usize| {
            if whole == 0 {
                0.0
            } else {
                100.0 * part as f64 / whole as f64
            }
        };

        println!(
            "block_uniformity objects_per_group={objects} strings_per_group={strings} \
             blocks={} skippable={} ({:.1} %) mixed={} entities={} in_skippable={} ({:.1} %) \
             classes={:?}",
            seen.blocks,
            seen.skippable,
            share(seen.skippable, seen.blocks),
            seen.mixed,
            seen.entities,
            seen.entities_in_skippable,
            share(seen.entities_in_skippable, seen.entities),
            seen.classes,
        );

        for entity in live {
            unsafe { crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, entity) };
        }

        arena.reset(|_| {});
    }

    for run in RUNS {
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let mut live: Vec<*mut RcHeader> = Vec::with_capacity(POPULATION);

        for i in 0..POPULATION {
            let entity = if (i / run) % 2 == 0 {
                let bytes = format!("{i:06}");
                let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, bytes.as_bytes()) };
                s as *mut RcHeader
            } else {
                let o = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
                o as *mut RcHeader
            };
            assert!(!entity.is_null(), "the probe's population must allocate");
            live.push(entity);
        }

        let seen = unsafe { classify() };
        let share = |part: usize, whole: usize| {
            if whole == 0 {
                0.0
            } else {
                100.0 * part as f64 / whole as f64
            }
        };

        println!(
            "block_uniformity run={run} blocks={} skippable={} ({:.1} %) mixed={} \
             entities={} in_skippable={} ({:.1} %) classes={:?}",
            seen.blocks,
            seen.skippable,
            share(seen.skippable, seen.blocks),
            seen.mixed,
            seen.entities,
            seen.entities_in_skippable,
            share(seen.entities_in_skippable, seen.entities),
            seen.classes,
        );

        for entity in live {
            unsafe { crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, entity) };
        }

        arena.reset(|_| {});
    }
}
