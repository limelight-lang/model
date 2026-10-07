//! The scan a collector runs over the edges its mark recorded, under
//! `gc-window`, against the default build's scan over the heap
//! (`crate::cycle::recorded_edges`; `dev/design/recycler-over-counts.md`,
//! §3.5).
//!
//! The shape is the one the record exists for: a live holder L whose edge
//! the mark subtracted from X, moved out of L after the mark and before the
//! scan with no count write on X — a count-free move, as an ARC-cancelled pair
//! or the runtime's own `Table::remove` makes one. Read off the heap, L no
//! longer reaches X and X's row, lowered by the edge, reads unreachable; read
//! off the record, L's run still names X and X is live.

use super::the_batch::served_by_a_collector;
use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::queue::collect_lane_tokens;
use crate::cycle::queue::verdicts::{Verdict, discard_standing_verdicts, standing_verdicts};
use crate::memory::arena::Arena;
use crate::memory::barrier::write_value_slot;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, ll_release};
use crate::test_support::{prop_offset, store_prop};
use crate::value::Value;
use std::sync::atomic::{AtomicUsize, Ordering};

fn node_class() -> *const Class {
    ClassBuilder::new("RecordedEdgeNode")
        .prop("next", true)
        .build()
}

unsafe fn object(arena: &mut Arena, class: *const Class) -> *mut Object {
    let mut context = LLContext { arena };
    unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) }
}

/// Where the hook put the reference it moved out of L: the local the move
/// left holding it.
static MOVED: AtomicUsize = AtomicUsize::new(0);

/// X, a batch's one root, is read live under the feature and proposed by the
/// default build when the edge from its live holder moved out between the
/// mark and the scan.
#[test]
fn an_edge_moved_out_after_the_mark_still_holds_its_target_live() {
    let _g = test_guard();
    reset_lanes();
    let node = node_class();
    let mut arena = Arena::new();
    // L keeps its creation reference, the case's local: L reads above zero.
    let l = unsafe { object(&mut arena, node) };
    let x = unsafe { object(&mut arena, node) };
    unsafe {
        store_prop(&mut arena, l, prop_offset(0), x);
        store_prop(&mut arena, x, prop_offset(0), l);
        assert!(
            !ll_release(x as *mut RcHeader),
            "L's edge holds X, registered at the non-final decrement"
        );
    }
    let mut in_r = Vec::new();
    collect_lane_tokens(&mut in_r);
    assert_eq!(in_r.len(), 1, "X alone is a root");
    unsafe { &*record() }.set_batch_size(1);

    let holder = l as usize;
    testing::between_the_next_phases(Box::new(move || unsafe {
        let slot = Object::prop_at(holder as *mut Object, prop_offset(0));
        MOVED.store((*slot).entity_ptr() as usize, Ordering::Relaxed);
        write_value_slot(slot, Value::null());
    }));
    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: 1,
            complete: true,
            backlog: false,
        }
    );
    assert_eq!(
        MOVED.load(Ordering::Relaxed),
        x as usize,
        "the hook moved X out of L"
    );

    let expected = if cfg!(feature = "gc-window") {
        Verdict::ReadLive
    } else {
        // The heap the default build's scan reads no longer has the edge;
        // the owner's exact validation, which re-reads the heap and finds
        // X's count held by the local, is what keeps X there.
        Verdict::Proposed
    };
    assert_eq!(standing_verdicts(), vec![(x as *mut RcHeader, expected)]);

    discard_standing_verdicts();
    reset_lanes();
    unsafe {
        store_prop(
            &mut arena,
            x,
            prop_offset(0),
            std::ptr::null_mut::<Object>(),
        );
        assert!(ll_release(l as *mut RcHeader), "the local was L's last");
        ll_object_die(l);
        assert!(
            ll_release(x as *mut RcHeader),
            "the moved reference was X's last"
        );
        ll_object_die(x);
    }
    reset_lanes();
}
