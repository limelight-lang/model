use super::*;
use crate::class::ClassBuilder;
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::new_constructed;
use crate::refcount::{MemoryCategory, ll_release};
use crate::test_support::{POOLED_FILLERS, RUN_FILLERS};
use crate::value::{Tag, Value};

/// Collect the addresses the walk currently yields. Tests assert
/// membership, never totals: the registry is process-global, and
/// other tests' leftovers (abandoned blocks with live objects) are
/// legitimately visible here.
fn walked_addresses() -> Vec<usize> {
    let mut seen = Vec::new();
    unsafe { for_each_entity_slot(|e| seen.push(e as usize)) };
    seen
}

// --- collect_cycles (build step 2) -------------------------------

use std::sync::atomic::{AtomicUsize, Ordering};

static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
static RESURRECTED: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
}

/// `$GLOBALS['keep'] = $this;` inside `__destruct`: an ordinary
/// counted store, so the component must be acquitted.
unsafe extern "C" fn resurrecting_destructor(obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
    unsafe { crate::refcount::ll_retain(obj as *mut RcHeader) };
    RESURRECTED.store(obj as usize, Ordering::Relaxed);
}

/// Tie `a.child = b` the way generated code leaves it after
/// `$a->child = $b; unset($b);` — the slot owns one reference.
unsafe fn tie(a: *mut Object, offset: u32, b: *mut Object) {
    unsafe {
        Object::prop_at(a, offset).write(Value::entity(Tag::Object, b as *mut RcHeader));
    }
}

/// [`crate::test_support::wide_class`] with the counting destructor,
/// so the only edge is the one the ring ties.
fn wide_ring_class(name: &str, fillers: usize) -> *const crate::class::Class {
    crate::test_support::wide_class(name, fillers, Some(counting_destructor as *const ()))
}

/// One walk that yields both the aggregate and the addresses behind
/// it, so a census that disagrees with its predecessor can name the
/// entities that came and went instead of only the count.
///
/// # Safety
/// As [`heap_census`]: a quiescent mutator.
unsafe fn census_with_addresses() -> (Census, Vec<(usize, u64)>) {
    let mut census = Census::default();
    let mut addresses = Vec::new();
    unsafe {
        for_each_entity_slot(|entity| {
            census.entities += 1;
            census.by_kind[entity_kind(entity) as usize] += 1;
            addresses.push((entity as usize, *(entity as *const u64)));
            trace_entity(entity, |_child| census.edges += 1);
        });
    }

    (census, addresses)
}

/// Print what left the census and what joined it, each address with
/// the state of the block the enumerator gates on.
///
/// The census flake behind this test (`dev/POSTMORTEM.md`, "an entity
/// killed at refcount 1") is entities
/// *leaving*: the count fails to grow because a live entity stopped
/// being enumerated, which is the direction that matters — a missed
/// entity contributes none of its out-edges, so its children read as
/// less rooted than they are.
fn report_census_drift(before: &[(usize, u64)], after: &[(usize, u64)]) {
    let before_set: HashSet<usize> = before.iter().map(|&(a, _)| a).collect();
    let after_set: HashSet<usize> = after.iter().map(|&(a, _)| a).collect();
    eprintln!(
        "census drift: {} before, {} after, expected +2",
        before.len(),
        after.len()
    );
    for addr in before_set.difference(&after_set) {
        eprintln!("  LEFT  {}", crate::memory::heap::describe_slot(*addr));
    }

    for addr in after_set.difference(&before_set) {
        eprintln!("  ADDED {}", crate::memory::heap::describe_slot(*addr));
    }
}

mod the_children_a_kind_has;
mod what_roots_a_component;
mod what_the_collection_reclaims;
mod what_the_drain_meets_when_it_arrives;
mod what_the_walk_enumerates;
