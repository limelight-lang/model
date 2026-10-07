//! On one heap, the backup trace's verdict is trial deletion's with every
//! entity a candidate (`dev/design/the-general-algorithm.md`, "A third
//! collector: the backup trace", "A check in tests").
//!
//! Both run in this binary: the trace's verdict is read without a free, the
//! field cleared behind it, and then every entity the case built is registered
//! by hand — the release path registers nothing under `trace-backup-rig` — and
//! collected by `ll_gc_collect_cycles`, which is trial deletion's collection
//! over R. What it freed is what no longer stands. Each side is also held to
//! the answer the graphs were built to have.

use super::*;
use crate::memory::barrier::store_box_owned;
use crate::refcount::{CANDIDATE_BIT, OWNERSHIP_MARK, mutator_flags, update_header_flags};
use crate::value::{Tag, Value};

/// A node with two counted Box properties, `next` at `prop_offset(0)` and
/// `other` at `prop_offset(1)`, and no destructor.
fn two_edge_class(name: &str) -> *const Class {
    ClassBuilder::new(name)
        .prop("next", true)
        .prop("other", true)
        .build()
}

/// `count` fresh objects linked into a ring through `next`, every creation
/// reference spent.
fn linked_ring(arena: &mut Arena, class: *const Class, count: usize) -> Vec<*mut Object> {
    let members: Vec<*mut Object> = (0..count).map(|_| object(arena, class)).collect();
    unsafe {
        for (index, &member) in members.iter().enumerate() {
            store_prop(arena, member, prop_offset(0), members[(index + 1) % count]);
        }

        for &member in &members {
            assert!(!ll_release(member as *mut RcHeader));
        }
    }

    members
}

/// The graphs, and the members of them each verdict must read as garbage.
struct Graphs {
    built: Vec<*mut Object>,
    garbage: Vec<*mut Object>,
    /// The counted references the case holds as locals, to let go at its end.
    locals: Vec<*mut Object>,
}

fn build(arena: &mut Arena) -> Graphs {
    let class = two_edge_class("TbVerdictNode");
    let mut built = Vec::new();
    let mut garbage = Vec::new();
    let mut locals = Vec::new();

    // One: a garbage ring of three with a tail only it holds.
    let first = linked_ring(arena, class, 3);
    let tail = object(arena, class);
    unsafe {
        store_prop(arena, first[0], prop_offset(1), tail);
        assert!(!ll_release(tail as *mut RcHeader));
    }
    built.extend(&first);
    built.push(tail);
    garbage.extend(&first);
    garbage.push(tail);

    // Two: a ring held from a local, and a garbage ring with an edge into it.
    let held = linked_ring(arena, class, 2);
    unsafe { ll_retain(held[0] as *mut RcHeader) };
    locals.push(held[0]);
    let pointing = linked_ring(arena, class, 2);
    unsafe { store_prop(arena, pointing[1], prop_offset(1), held[1]) };
    built.extend(&held);
    built.extend(&pointing);
    garbage.extend(&pointing);

    // Three: a local holding a chain into a ring, and an object holding only
    // itself.
    let head = object(arena, class);
    locals.push(head);
    let reached = linked_ring(arena, class, 2);
    unsafe { store_prop(arena, head, prop_offset(1), reached[0]) };
    let alone = object(arena, class);
    unsafe {
        store_prop(arena, alone, prop_offset(0), alone);
        assert!(!ll_release(alone as *mut RcHeader));
    }
    built.push(head);
    built.extend(&reached);
    built.push(alone);
    garbage.push(alone);

    // Four: a garbage ring of three whose first edge is stored through the
    // owned form, so its second member carries the ownership mark.
    let owned: Vec<*mut Object> = (0..3).map(|_| object(arena, class)).collect();
    unsafe {
        let slot = Object::prop_at(owned[0], prop_offset(0));
        assert!(store_box_owned(
            &mut *arena,
            MemoryCategory::GcHeap,
            slot,
            Value::entity(Tag::Object, owned[1] as *mut RcHeader),
        ));
        store_prop(arena, owned[1], prop_offset(0), owned[2]);
        store_prop(arena, owned[2], prop_offset(0), owned[0]);
        for &member in &owned {
            assert!(!ll_release(member as *mut RcHeader));
        }
        assert_ne!(mutator_flags(owned[1] as *mut RcHeader) & OWNERSHIP_MARK, 0);
    }
    built.extend(&owned);
    garbage.extend(&owned);

    built.sort_unstable();
    garbage.sort_unstable();
    Graphs {
        built,
        garbage,
        locals,
    }
}

#[test]
fn the_verdict_is_trial_deletions_with_every_entity_a_candidate() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    let mut arena = Arena::new();
    let graphs = build(&mut arena);

    let mut by_the_trace: Vec<*mut Object> = unsafe { verdict() }
        .expect("a case's thread is outside every collection")
        .into_iter()
        .map(|entity| entity as *mut Object)
        .filter(|entity| graphs.built.binary_search(entity).is_ok())
        .collect();
    by_the_trace.sort_unstable();
    assert!(
        graphs.built.iter().all(|&entity| live(entity)
            && unsafe { crate::refcount::trace_field_load(entity as *mut RcHeader) } == 0),
        "the verdict freed nothing and cleared every field"
    );

    // Every entity a candidate, but one the owned form marked: the gate
    // refuses it as the release path would, and the trace reaches it through
    // its holder's edge.
    for &entity in &graphs.built {
        let header = entity as *mut RcHeader;
        if unsafe { mutator_flags(header) } & OWNERSHIP_MARK != 0 {
            continue;
        }

        unsafe {
            update_header_flags(header, |flags| flags | CANDIDATE_BIT);
            crate::cycle::queue::register_candidate(header);
        }
    }

    let freed = unsafe { crate::gc::ll_gc_collect_cycles() };
    let by_trial_deletion: Vec<*mut Object> = graphs
        .built
        .iter()
        .copied()
        .filter(|&entity| !live(entity))
        .collect();

    assert_eq!(
        by_the_trace, graphs.garbage,
        "the trace's verdict is the graphs' answer"
    );
    assert_eq!(
        by_trial_deletion, graphs.garbage,
        "trial deletion's verdict is the graphs' answer"
    );
    assert_eq!(
        freed,
        graphs.garbage.len(),
        "trial deletion freed those alone"
    );

    for &local in &graphs.locals {
        unsafe {
            if ll_release(local as *mut RcHeader) {
                crate::object::ll_entity_die(local as *mut RcHeader);
            }
        }
    }
    crate::cycle::queue::release_queue_segments();
}
