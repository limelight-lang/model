//! The synthetic loads S40.1 reports, and the reading each one leaves.
//!
//! Ignored in the ordinary suite and run by hand:
//!
//! ```text
//! cargo test --lib density::tests::the_loads -- --ignored --nocapture
//! ```
//!
//! The numbers themselves are in `dev/BENCHMARKS.md`; what stands here
//! is the construction that produced them, so a later run can be
//! compared against the same population rather than against a similar
//! one.
//!
//! # What these loads measure, and what they do not
//!
//! The fixture chooses the component sizes, the graph shape and the
//! allocation order. What the crate chooses — and the only thing
//! measured — is where the heap placed those entities across blocks and
//! which of each block's slots the trace's own arithmetic then met.
//!
//! It is not a corpus reading. A corpus arm needs a driver over this
//! crate's heap and is blocked with Phase D (`PLAN.md` S40.1).

use super::*;

use crate::cycle::row::Population as RowPopulation;

/// The size classes the design's own crossing is computed over
/// (`rfc/model/gc/rc-cycle.md`: "a 12 GiB heap of classes 32/64/128/256
/// at half occupancy"). A density is a share of a block's slots, and a
/// block's slots are `65,280 / class`, so the class is an input to every
/// figure here and a load on one class says nothing about another.
pub(super) const DESIGN_CLASSES: [usize; 4] = [32, 64, 128, 256];

/// Slots a block of `class_bytes` holds: the block payload over the
/// stride, which is what `heap::collector_block_slots` computes.
pub(super) const fn slots_per_block(class_bytes: usize) -> usize {
    crate::memory::block_pool::BLOCK_PAYLOAD / class_bytes
}

/// The component sizes S40.3 fixes, and this step reads the same ones so
/// that the two runs can be quoted in one sentence. 381 is the corpus's
/// median closure size; the rest bracket it.
const COMPONENT_SIZES: [usize; 4] = [2, 16, 256, 381];

/// Collections run over each load. Four is the least a `k` of three
/// needs; eight leaves room and costs one trace each.
pub(super) const COLLECTIONS: usize = 8;

/// One load's construction and the reading of each of its collections.
struct Load {
    /// The size class the component's entities take, which fixes how
    /// many slots a block holds and therefore the denominator.
    class_bytes: usize,
    members: usize,
    /// Entities allocated between two consecutive component members.
    /// Zero packs the component; one less than the block's slot count
    /// puts one member in every block.
    fillers_between: usize,
    collections: Vec<Reading>,
}

/// Build a component of `members` entities with `fillers_between`
/// unreferenced entities allocated between each pair, trace it
/// [`COLLECTIONS`] times, and answer every reading.
///
/// The fillers are ordinary occupants: allocated, never linked and never
/// registered, so they raise a block's occupancy without giving the
/// trace anything to reach. That is what separates the two arms — the
/// component is the same graph in both, and only the heap's placement
/// of it differs.
fn a_slotted_load(class_bytes: usize, members: usize, fillers_between: usize) -> Load {
    let name = format!("DensityLoad{class_bytes}c{members}x{fillers_between}");
    let class = a_class(&name, props_for(class_bytes));

    let mut positions = Vec::with_capacity(members);
    let mut count = 0;
    for _ in 0..members {
        positions.push(count);
        count += 1 + fillers_between;
    }

    let fixture = build(class, count, &positions);
    let collections = (0..COLLECTIONS).map(|_| collect()).collect();
    tear_down(fixture);

    Load {
        class_bytes,
        members,
        fillers_between,
        collections,
    }
}

/// One row of the table, as `dev/BENCHMARKS.md` records it.
///
/// Every collection is printed on its own line rather than averaged: the
/// first draws the thread's workspace and the rest do not, and that
/// asymmetry is what S36.12 slice (b) asks about, and what
/// `dev/BENCHMARKS.md`, "S43.1 the sweep's walk against the withheld chain"
/// reports one line at a time.
fn report(load: &Load) {
    println!(
        "\n== class {} ({} slots a block), {} members, {} fillers between ==",
        load.class_bytes,
        slots_per_block(load.class_bytes),
        load.members,
        load.fillers_between
    );
    println!(
        "  n  blocks  slots  occupied  met  met/slots  met/occupied  groups  groups_met  \
         saturated  blocks_drawn  mark_rows  trace_rows"
    );
    for (index, reading) in load.collections.iter().enumerate() {
        let d = reading.density.slotted;
        let per_slot = d.rows_met as f64 * 100.0 / d.index_space as f64;
        let per_occupant = d.rows_met as f64 * 100.0 / d.occupied as f64;
        println!(
            "  {:<2} {:<7} {:<6} {:<9} {:<4} {:<10.1} {:<13.1} {:<7} {:<11} {:<10} {:<13} {:<11} {}",
            index + 1,
            d.blocks,
            d.index_space,
            d.occupied,
            d.rows_met,
            per_slot,
            per_occupant,
            d.groups,
            d.groups_met,
            d.rows_saturated,
            reading.arena_blocks,
            reading.mark_resolutions,
            reading.trace_resolutions
        );
    }
}

/// Every collection of a load must read the same figures.
///
/// This is the control the `dev/BENCHMARKS.md` method asks for,
/// translated out of the clock: a count has no noise, so the second
/// reading of the same population is the control and a disagreement
/// voids the run. A density that moves between two traces of one
/// unchanged graph depends on state the fixture does not hold, and is
/// not a density.
fn every_collection_agrees(load: &Load) {
    let first = load.collections[0];
    // A reading of nothing is the shape this takes when the arena's row
    // sweep moves into the scan, which is S36.7's to wire: the walk
    // would then find a null touched head and every figure would be
    // zero, and every equality below would still hold.
    assert!(
        first.density.slotted.blocks > 0,
        "the load touched at least one block"
    );
    assert_eq!(
        first.density.slotted.rows_met, load.members as u64,
        "and met one row per component member"
    );
    for (index, reading) in load.collections.iter().enumerate().skip(1) {
        // The density and the row resolutions, and deliberately not
        // `newest_array`: a load that bumps past the workspace draws its
        // arrays from blocks the arena took this collection, so the
        // address moves between collections while every figure holds.
        assert_eq!(
            (
                reading.density,
                reading.mark_resolutions,
                reading.trace_resolutions,
                reading.arena_blocks
            ),
            (
                first.density,
                first.mark_resolutions,
                first.trace_resolutions,
                first.arena_blocks
            ),
            "collection {} of the {}-member load disagrees with the first",
            index + 1,
            load.members
        );
    }
}

/// The dense arm: the component's members allocated back to back, over
/// each of the design's four size classes.
///
/// The class is what makes this a table rather than a number. One
/// component of 381 members occupies 381 slots whatever the class, and
/// the block under it holds 2,040 slots at class 32 and 255 at class
/// 256 — so the same component reads a density that moves by a factor of
/// eight across the classes the crossing was computed over.
#[test]
#[ignore = "a measurement, recorded in dev/BENCHMARKS.md; run with --ignored"]
fn the_dense_arm() {
    let _g = test_guard();
    for class_bytes in DESIGN_CLASSES {
        for size in COMPONENT_SIZES {
            let load = on_a_fresh_thread(move || a_slotted_load(class_bytes, size, 0));
            every_collection_agrees(&load);
            report(&load);
        }
    }
}

/// The sparse arm: one component member per block, the rest of each
/// block filled by entities no root reaches.
///
/// Taken on class 256, the widest of the design's four, because one
/// member per block costs `slots − 1` fillers and the narrower classes
/// would cost thousands each. The figure it answers is `1 / slots` by
/// construction and is the least *this class* can read rather than a
/// measurement of the collector. It is not the least of the run: a
/// two-member component of class 32 reads 0.1 %, and the least any
/// design class can read is `1/2,040`.
#[test]
#[ignore = "a measurement, recorded in dev/BENCHMARKS.md; run with --ignored"]
fn the_sparse_arm() {
    let _g = test_guard();
    let class_bytes = 256;
    for size in COMPONENT_SIZES {
        let load = on_a_fresh_thread(move || {
            a_slotted_load(class_bytes, size, slots_per_block(class_bytes) - 1)
        });
        every_collection_agrees(&load);
        report(&load);
    }
}

/// The retained arm: a component the arena's reset moved into retained
/// blocks, whose index space is a survivor list rather than a slot
/// stride.
///
/// Reported apart from the two arms above and never averaged with them:
/// a survivor list holds no free position, so its density is structurally
/// higher and says nothing about an entity block's.
#[test]
#[ignore = "a measurement, recorded in dev/BENCHMARKS.md; run with --ignored"]
fn the_retained_arm() {
    let _g = test_guard();
    for size in COMPONENT_SIZES {
        let readings = on_a_fresh_thread(move || {
            let member_class = a_class(&format!("DensityRetained{size}"), props_for(256));
            let holder_class = a_class(&format!("DensityRetainedHolder{size}"), props_for(64));

            let mut arena = Arena::new();
            let mut context = LLContext { arena: &mut arena };
            let holder =
                unsafe { new_constructed(&mut context, holder_class, MemoryCategory::GcHeap) };
            let members: Vec<*mut Object> = (0..size)
                .map(|_| unsafe {
                    new_constructed(&mut context, member_class, MemoryCategory::RequestArena)
                })
                .collect();

            // A ring through the members, entered once from the holder:
            // the holder is the root, and every member is reached through
            // the retained block's own index space.
            for (position, &member) in members.iter().enumerate() {
                let next = members[(position + 1) % members.len()];
                unsafe { store_prop(&mut arena, member, prop_offset(0), next) };
            }

            unsafe { store_prop(&mut arena, holder, prop_offset(0), members[0]) };
            unsafe { crate::promote::arena_reset_full(&mut arena) };

            // The holder is the only registration: an arena entity takes
            // no candidate token, and the reading wanted is the rows of
            // the blocks its closure touches. Retained first, so the
            // release that registers it is not the last one — the holder
            // is reachable from this frame and from nowhere else.
            unsafe { ll_retain(holder as *mut RcHeader) };
            assert!(
                !unsafe { ll_release(holder as *mut RcHeader) },
                "the retain above stands, so this release is not the last"
            );

            let readings: Vec<Reading> = (0..COLLECTIONS).map(|_| collect()).collect();
            unsafe {
                store_prop(&mut arena, holder, prop_offset(0), std::ptr::null_mut());
                crate::refcount::clear_candidate_bit(holder as *mut RcHeader);
                assert!(
                    ll_release(holder as *mut RcHeader),
                    "and this one is: nothing else names the holder"
                );
                ll_object_die(holder);
            }

            readings
        });

        // The same control the other two arms take: a count has no noise,
        // so the repeat of the population stands in for the repeat of the
        // binary, and a disagreement voids the run.
        let first = readings[0];
        for (index, reading) in readings.iter().enumerate().skip(1) {
            assert_eq!(
                (
                    reading.density,
                    reading.mark_resolutions,
                    reading.trace_resolutions
                ),
                (
                    first.density,
                    first.mark_resolutions,
                    first.trace_resolutions
                ),
                "collection {} of the {size}-member retained load disagrees with the first",
                index + 1
            );
        }

        assert!(
            first.density.retained.blocks > 0,
            "the holder's closure reached a retained block"
        );
        assert_eq!(
            first.density.retained.rows_met, size as u64,
            "and met every member of the ring"
        );
        assert_eq!(
            first.density.retained.occupied, first.density.retained.index_space,
            "a survivor list carries no free position"
        );

        println!(
            "\n== retained, {size} members: mark rows {}, trace rows {}, blocks drawn {} ==",
            first.mark_resolutions, first.trace_resolutions, first.arena_blocks
        );
        for population in [
            (RowPopulation::Slotted, first.density.slotted),
            (RowPopulation::Retained, first.density.retained),
            (RowPopulation::SingleEntity, first.density.single_entity),
        ] {
            let (name, d) = population;
            if d.blocks == 0 {
                continue;
            }

            println!(
                "  {name:?}: blocks {} slots {} occupied {} met {} groups {} groups_met {}",
                d.blocks, d.index_space, d.occupied, d.rows_met, d.groups, d.groups_met
            );
        }
    }
}
