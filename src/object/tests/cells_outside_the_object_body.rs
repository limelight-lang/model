//! A class may own counted cells the runs cannot describe, and every
//! operation an ordinary property gets they get too: the tracing walk
//! finds them, the teardown releases what they hold, and the storage
//! behind them goes back with the object rather than after it.
//!
//! The class here declares no traced property at all, so a walk that
//! honoured only `ptr_runs` and `box_runs` would report an object with no
//! children — which is not a crash but a computed root, and a ring
//! through the block that never collects.

use super::*;
use crate::test_support::{chunk_from_the_free_list, outside_block};

/// The child of an object whose only counted cell is outside it. The
/// tracer is the same one every consumer goes through, so this is the
/// property the sever, the two collectors and the teardown all inherit.
#[test]
fn the_walk_finds_a_cell_the_runs_cannot_describe() {
    let _g = crate::memory::block_pool::test_guard();

    with_ctx(|ctx| unsafe {
        let arena = (*ctx).arena;
        let cls = outside_block::class("WakerTraced");
        let child_cls = ClassBuilder::new("WakerTracedChild").build();

        let obj = new_constructed(ctx, cls, MemoryCategory::GcHeap);
        assert!(!obj.is_null(), "the factory refused");
        assert!(
            (*cls).box_runs().is_empty() && (*cls).ptr_runs().is_empty(),
            "the class has a traced run, so the body could account for the child"
        );

        let block = outside_block::install_block(ctx, obj);
        assert!(!block.is_null(), "the object has no block to hold a cell");

        let child = new_constructed(ctx, child_cls, MemoryCategory::GcHeap);
        assert!(
            outside_block::store_cell(
                arena,
                obj,
                0,
                std::ptr::null_mut(),
                Value::entity(Tag::Object, child as *mut RcHeader),
            ),
            "the barrier refused the store this test is built on"
        );

        let mut seen = Vec::new();
        crate::walk::trace_entity(obj as *mut RcHeader, |c| seen.push(c));
        assert_eq!(
            seen,
            vec![child as *mut RcHeader],
            "the object's one counted child is the cell outside its body"
        );

        // The cell holds the only reference now, so the object's death
        // below is what kills the child.
        assert!(!ll_release(child as *mut RcHeader));
        assert!(ll_release(obj as *mut RcHeader));
        ll_entity_die(obj as *mut RcHeader);
    });
}

/// Teardown owes two things to a class with cells outside its body, and
/// the order between them is the contract: every cell is released first,
/// through the same drop the body's slots take, and only then does the
/// storage go back. Freeing first would drop the references with it.
///
/// The block coming back is read as the buffer arena handing the same
/// address out again under critical pressure, where an allocation
/// searches the free lists instead of bumping.
#[test]
fn a_dying_object_releases_its_outside_cells_and_frees_the_block() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);

    with_ctx(|ctx| unsafe {
        let arena = (*ctx).arena;
        let cls = outside_block::class("WakerTornDown");
        let child_cls = ClassBuilder::new("WakerTornDownChild")
            .destructor(counting_destructor as *const ())
            .build();

        let obj = new_constructed(ctx, cls, MemoryCategory::GcHeap);
        outside_block::install_block(ctx, obj);
        let (block, capacity) = outside_block::block_and_capacity(obj);
        let child = new_constructed(ctx, child_cls, MemoryCategory::GcHeap);
        assert!(outside_block::store_cell(
            arena,
            obj,
            0,
            std::ptr::null_mut(),
            Value::entity(Tag::Object, child as *mut RcHeader),
        ));

        assert!(!ll_release(child as *mut RcHeader), "the cell holds it");
        assert!(ll_release(obj as *mut RcHeader));
        ll_entity_die(obj as *mut RcHeader);

        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            1,
            "the child in the block outlived the object that held it"
        );

        assert_eq!(
            chunk_from_the_free_list(capacity),
            block,
            "the block was never given back"
        );
    });
}

/// The factory refuses the one category the group cannot serve. An arena
/// object never reaches full teardown, and promotion carries out-of-line
/// memory by entity kind, which for an object answers "none" — so the
/// storage of a corpse would never be freed and a survivor would keep a
/// pointer into memory the reset handed back. The carry that lifts the
/// refusal is its own stage (`dev/DECISIONS.md`, "a hooked class draws
/// its storage under its own category, and the arena carry waits").
#[test]
#[should_panic(expected = "no arena carry")]
#[cfg_attr(not(debug_assertions), ignore = "the tripwire is a debug assertion")]
fn the_factory_refuses_a_hooked_class_in_the_arena() {
    let _g = crate::memory::block_pool::test_guard();

    with_ctx(|ctx| unsafe {
        let cls = outside_block::class("WakerInAnArena");
        ll_object_new(ctx, cls, MemoryCategory::RequestArena);
    });
}

/// An instance that never took a block is the same class, and every
/// member of the group meets it: the walk yields nothing, the teardown
/// frees nothing, and neither reads the null as an address.
#[test]
fn an_instance_with_no_block_walks_and_dies_like_any_other() {
    let _g = crate::memory::block_pool::test_guard();

    with_ctx(|ctx| unsafe {
        let cls = outside_block::class("WakerEmpty");
        let obj = new_constructed(ctx, cls, MemoryCategory::GcHeap);
        assert!(outside_block::block_of(obj).is_null(), "the factory zeroed");

        let mut seen = 0;
        crate::walk::trace_entity(obj as *mut RcHeader, |_| seen += 1);
        assert_eq!(seen, 0, "an object with no block has no children");

        assert!(ll_release(obj as *mut RcHeader));
        ll_entity_die(obj as *mut RcHeader);
    });
}
