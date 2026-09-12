//! The loads S40.3 records, and the report each collection over them leaves.
//!
//! Ignored in the ordinary suite and run by hand:
//!
//! ```text
//! cargo test --lib census::tests::the_loads -- --ignored --nocapture
//! ```
//!
//! The numbers are in `dev/BENCHMARKS.md`; what stands here is the list the
//! Sage ruled on (`PLAN.md` S40.3) and the construction of each load, so a
//! later run can be compared against the same population. Every load runs on
//! a thread of its own under a pinned epoch, the deferred lane re-offered by
//! hand before each of eight collections, and every collection is printed on
//! its own line: the first is the one that draws the thread's workspace.
//!
//! What each reading is checked against is its construction, and the checks
//! are the calibration the loads carry: the roots, the rows met, the internal
//! edges, the dispatches, the freed count, the absence of pruning on a ring
//! whose every member is registered, and the bump's identity.

use super::*;
use crate::cycle::mark::take_edges_pruned;

/// The component sizes, 381 being the corpus's median closure.
const SIZES: [usize; 4] = [2, 16, 256, 381];

/// The design's four size classes.
const DESIGN_CLASSES: [usize; 4] = [32, 64, 128, 256];

/// Collections over each live load.
const COLLECTIONS: usize = 8;

/// One line of the record, and one more per touched block where the load
/// spans two populations, since the sums then mix a slotted block's `G` with
/// a retained block's.
fn report_line(collection: usize, collected: &Collected, pruned: usize) {
    let report = &collected.report;
    let Some(scan) = report.scan.as_ref() else {
        println!("  {collection:<2} scan absent");
        return;
    };
    let Some(close) = report.close else {
        println!("  {collection:<2} close absent");
        return;
    };
    let c = report.counters;
    let d = scan.density;
    let memory = collected.memory;
    println!(
        "  {:<2} roots {:<5} V {:<5}/{:<5}/{:<2} E {:<5} sat {:<2} mark {:<6} trace {:<6} pruned {:<2} \
         blocks {:<4} G {:<5} T {:<5} lines {:<5} rows_req {:<7} rows_gr {:<7} wl {:<7} comp {:<7} \
         drops {:<6} seg {}/{}/{} drawn {}/{} ret {:<2} tails {}/{:<6} rem {:<6} reoff {:<5} \
         val {}/{:<5} desc {}/{} gc_blocks {}/{} gc_bytes {}/{} ending {:?} freed {}",
        collection,
        scan.roots,
        d.slotted.rows_met,
        d.retained.rows_met,
        d.single_entity.rows_met,
        scan.internal_edges.recoverable_internal_edges,
        scan.internal_edges.saturated_rows,
        scan.mark_dispatches,
        scan.trace_dispatches,
        pruned,
        d.slotted.blocks + d.retained.blocks + d.single_entity.blocks,
        d.slotted.groups + d.retained.groups,
        d.slotted.groups_met + d.retained.groups_met,
        scan.lines,
        c.row_bytes_requested,
        c.granted.rows,
        c.granted.worklist,
        c.granted.components,
        c.granted.drops,
        close.worklist_segments,
        close.component_segments,
        close.drop_segments,
        c.drawn_from_pool,
        c.drawn_from_reserve,
        c.returned,
        c.tails_abandoned,
        c.tail_bytes_abandoned,
        close.remainder,
        c.reoffered,
        c.validations,
        c.members_validated,
        close.descent.vertices,
        close.descent.components,
        memory.current_blocks(),
        memory.peak_blocks(),
        memory.current_bytes_in_use(),
        memory.peak_bytes_in_use(),
        close.ending,
        collected.freed
    );
    if d.retained.blocks > 0 {
        let blocks: Vec<String> = scan
            .blocks
            .iter()
            .map(|block| {
                format!(
                    "{:?} V {} G {} T {}",
                    block.population, block.rows_met, block.groups, block.groups_met
                )
            })
            .collect();
        println!("     blocks: {}", blocks.join("; "));
    }
}

/// One collection over the load, with the pruned-edge count read beside the
/// report.
fn collect_and_report(collection: usize, expected_freed: usize) -> (usize, CollectionReport) {
    take_edges_pruned();
    let collected = collect_once();
    let pruned = take_edges_pruned();
    report_line(collection, &collected, pruned);
    let Collected { freed, report, .. } = collected;
    assert_eq!(
        freed, expected_freed,
        "collection {collection}: entities freed"
    );
    assert!(
        report.scan.is_some() && report.close.is_some(),
        "collection {collection}: both readings"
    );
    the_bump_balances(&report);
    (pruned, report)
}

/// A live ordinary ring: eight collections, each reading as the
/// construction, and then the ring let go and collected.
fn a_live_ordinary_ring(load: Load) {
    assert_eq!(load.population, LoadPopulation::Ordinary);
    let edges_per_member = if load.second_edge { 2 } else { 1 };
    println!(
        "\n== ordinary, class {}, {} members, {} fillers between{} ==",
        load.class_bytes,
        load.members,
        load.fillers,
        if load.second_edge {
            ", two edges per member"
        } else {
            ""
        }
    );
    on_a_fresh_thread(move || {
        release_queue_segments();
        let _epoch = epoch::pin(0);
        let mut built = unsafe { loads::build(load) };
        let _armed = arm();
        let mut first = None;
        for collection in 1..=COLLECTIONS {
            let (pruned, report) = collect_and_report(collection, 0);
            let scan = report.scan.clone().unwrap();
            let context = format!("collection {collection}");
            assert_eq!(pruned, 0, "{context}: a registered member is never pruned");
            assert_eq!(
                scan.roots, load.members,
                "{context}: every member is a root"
            );
            assert_eq!(
                scan.density.slotted.rows_met as usize, load.members,
                "{context}: rows met"
            );
            assert_eq!(
                scan.internal_edges.recoverable_internal_edges as usize,
                load.members * edges_per_member,
                "{context}: internal edges"
            );
            assert_eq!(
                scan.mark_dispatches,
                load.members * (1 + edges_per_member),
                "{context}: a root and its edges per member"
            );
            assert_eq!(
                scan.trace_dispatches,
                2 * scan.mark_dispatches,
                "{context}: the scan repeats the mark"
            );
            assert_eq!(
                report.counters.validations, 0,
                "{context}: a live ring proposes nothing"
            );
            match &first {
                None => first = Some((scan.density, report.counters.granted, scan.lines)),
                Some(first) => assert_eq!(
                    &(scan.density, report.counters.granted, scan.lines),
                    first,
                    "{context}: the same population reads the same"
                ),
            }
        }

        unsafe { loads::release_ring(&mut built) };
        let (_, report) = collect_and_report(COLLECTIONS + 1, load.members);
        assert_eq!(
            (
                report.counters.validations,
                report.counters.members_validated
            ),
            (1, load.members),
            "one exact validation over the whole ring"
        );
    });
}

/// The same ring built without a keeper's hold: one collection, which frees
/// it, read for the commit's own lookups.
fn a_garbage_ordinary_ring(load: Load) {
    println!(
        "\n== ordinary garbage, class {}, {} members, {} fillers between ==",
        load.class_bytes, load.members, load.fillers
    );
    on_a_fresh_thread(move || {
        release_queue_segments();
        let _epoch = epoch::pin(0);
        let mut built = unsafe { loads::build(load) };
        unsafe { loads::release_ring(&mut built) };
        let _armed = arm();
        let (pruned, report) = collect_and_report(1, load.members);
        assert_eq!(pruned, 0);
        let scan = report.scan.clone().unwrap();
        assert_eq!(scan.roots, load.members);
        assert_eq!(
            (
                report.counters.validations,
                report.counters.members_validated
            ),
            (1, load.members),
            "one exact validation, and no second: no destructor ran"
        );
        assert_eq!(
            report.close.unwrap().drop_segments,
            0,
            "the sever displaces no child out of a ring that holds only its members"
        );
    });
}

/// The retained ring: one root, the holder, and the members reached through
/// it until the prune stops the descent at the entry.
fn a_retained_ring(members: usize) {
    println!("\n== retained, class 256, {members} members ==");
    on_a_fresh_thread(move || {
        release_queue_segments();
        let _epoch = epoch::pin(0);
        let mut built = unsafe {
            loads::build(Load {
                class_bytes: 256,
                members,
                fillers: 0,
                population: LoadPopulation::Retained,
                second_edge: false,
            })
        };
        let _armed = arm();
        let threshold = crate::cycle::mark::TRAVERSAL_AGE_THRESHOLD as usize;
        for collection in 1..=COLLECTIONS {
            let (pruned, report) = collect_and_report(collection, 0);
            let scan = report.scan.clone().unwrap();
            let context = format!("collection {collection}");
            assert_eq!(scan.roots, 1, "{context}: the holder is the one root");
            assert_eq!(
                scan.density.slotted.rows_met, 1,
                "{context}: the holder's row"
            );
            // The entry's stamp gains one age per commit up to the threshold
            // and stops there, a target the descent does not reach being
            // left as it stands (`dev/BENCHMARKS.md`, 2026-09-12).
            assert_eq!(
                unsafe { crate::cycle::testing::stamp_of(built.members[0]) },
                (0, collection.min(threshold) as u32),
                "{context}: the entry's stamp"
            );
            if collection <= threshold {
                assert_eq!(pruned, 0, "{context}: the ring is below the threshold");
                assert_eq!(
                    scan.density.retained.rows_met as usize, members,
                    "{context}: every member met"
                );
                assert_eq!(
                    scan.mark_dispatches,
                    members + 2,
                    "{context}: the root, its edge, the ring"
                );
            } else {
                assert_eq!(pruned, 1, "{context}: the edge into the mature ring");
                assert_eq!(
                    scan.density.retained.rows_met, 0,
                    "{context}: the ring is not descended into"
                );
                assert_eq!(scan.mark_dispatches, 1, "{context}: the root alone");
            }
        }

        // The keeper lets go and dies; the first member is registered by the
        // null store and traced as a root, and the ring's second member is
        // mature and no candidate: the edge into it is pruned, the ring reads
        // live, and it waits for the turnover. The keeper's own record is
        // the second root, a dead one the trace expands to nothing.
        unsafe { loads::release_ring(&mut built) };
        let (pruned, report) = collect_and_report(COLLECTIONS + 1, 0);
        let scan = report.scan.clone().unwrap();
        assert_eq!(pruned, 1, "the edge from the first member into the second");
        assert_eq!(
            scan.roots, 2,
            "the dead keeper's record and the first member"
        );
        assert_eq!(
            scan.density.retained.rows_met, 1,
            "the first member's row alone"
        );
        assert_eq!(
            report.close.unwrap().ending,
            crate::cycle::collect::Ending::NothingProposed,
            "the pruned edge is never subtracted, so the scan reads the first member as held \
             from outside and proposes nothing"
        );
    });
}

/// The base matrix: dense over the four classes, one per block at class 256.
#[test]
#[ignore = "a measurement, recorded in dev/BENCHMARKS.md; run with --ignored"]
fn the_live_ring_dense_and_one_per_block() {
    let _g = test_guard();
    for class_bytes in DESIGN_CLASSES {
        for members in SIZES {
            a_live_ordinary_ring(Load {
                class_bytes,
                members,
                fillers: 0,
                population: LoadPopulation::Ordinary,
                second_edge: false,
            });
        }
    }

    for members in SIZES {
        a_live_ordinary_ring(Load {
            class_bytes: 256,
            members,
            fillers: loads::slots_per_block(256) - 1,
            population: LoadPopulation::Ordinary,
            second_edge: false,
        });
    }
}

/// The retained arm, over the four sizes.
#[test]
#[ignore = "a measurement, recorded in dev/BENCHMARKS.md; run with --ignored"]
fn the_retained_ring() {
    let _g = test_guard();
    for members in SIZES {
        a_retained_ring(members);
    }
}

/// Thirty-two members at class 256, consecutive against one per group: the
/// same `V/R` of 12.5 % with `T` of 4 against 32.
#[test]
#[ignore = "a measurement, recorded in dev/BENCHMARKS.md; run with --ignored"]
fn the_same_density_at_two_group_occupancies() {
    let _g = test_guard();
    for fillers in [0, 7] {
        a_live_ordinary_ring(Load {
            class_bytes: 256,
            members: 32,
            fillers,
            population: LoadPopulation::Ordinary,
            second_edge: false,
        });
    }
}

/// The full block at class 256 and at class 32: `V = R` and `T = G`.
#[test]
#[ignore = "a measurement, recorded in dev/BENCHMARKS.md; run with --ignored"]
fn the_full_block() {
    let _g = test_guard();
    for class_bytes in [256, 32] {
        let members = loads::slots_per_block(class_bytes);
        println!("\n(full block: {members} slots at class {class_bytes})");
        on_a_fresh_thread(move || {
            release_queue_segments();
            let _epoch = epoch::pin(0);
            let mut built = unsafe {
                loads::build(Load {
                    class_bytes,
                    members,
                    fillers: 0,
                    population: LoadPopulation::Ordinary,
                    second_edge: false,
                })
            };
            let _armed = arm();
            for collection in 1..=COLLECTIONS {
                let (pruned, report) = collect_and_report(collection, 0);
                let d = report.scan.as_ref().unwrap().density.slotted;
                assert_eq!(pruned, 0);
                assert_eq!(
                    d.blocks, 1,
                    "collection {collection}: the ring fills one block"
                );
                assert_eq!(d.rows_met, d.index_space, "collection {collection}: V = R");
                assert_eq!(d.groups_met, d.groups, "collection {collection}: T = G");
            }

            unsafe { loads::release_ring(&mut built) };
        });
    }
}

/// The ring of 381 with a second edge per member, both placements: the
/// difference against the base is the flat form's marginal lookup.
#[test]
#[ignore = "a measurement, recorded in dev/BENCHMARKS.md; run with --ignored"]
fn the_second_edge() {
    let _g = test_guard();
    for fillers in [0, loads::slots_per_block(256) - 1] {
        a_live_ordinary_ring(Load {
            class_bytes: 256,
            members: 381,
            fillers,
            population: LoadPopulation::Ordinary,
            second_edge: true,
        });
    }
}

/// The ring without its keeper, one collection per size and placement: the
/// commit's membership probes and the teardown's segments.
#[test]
#[ignore = "a measurement, recorded in dev/BENCHMARKS.md; run with --ignored"]
fn the_garbage_ring() {
    let _g = test_guard();
    for fillers in [0, loads::slots_per_block(256) - 1] {
        for members in SIZES {
            a_garbage_ordinary_ring(Load {
                class_bytes: 256,
                members,
                fillers,
                population: LoadPopulation::Ordinary,
                second_edge: false,
            });
        }
    }
}
