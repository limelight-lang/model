//! The collector's reader has to find every population and every
//! kind's children: a template's values, counted by its shape rather
//! than by its class, so a class-driven walk finds none of them;
//! both halves of the large-entity population, the pooled block the
//! region scan reaches and the run only the registry names; and a
//! retained former-arena block, whose occupants come from the
//! reset's own index because nothing there can be strided. A run
//! freed mid-epoch stays addressable until the flush, its memory
//! being unmapped at the real free.

use super::*;

/// The concurrent collector reads cells relaxed-atomically and so
/// keeps its own copy of the slot stride — which means a template's
/// values, counted by its shape rather than by its class, have to be
/// found here too. A class-driven walk finds nothing on a template
/// (the class has no runs), and an under-counted in-degree makes the
/// target look rooted: a ring through a template would never be
/// collected.
#[test]
fn the_concurrent_tracer_sees_a_templates_values() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("InterpolatedString").template().build();
    // The shape lives in immortal memory, where a compiler-emitted
    // one lives: this walk recovers the shape word from a relaxed
    // integer load, so the address it casts back has to be one that
    // was exposed as an integer to begin with — Miri says so, and it
    // is right (`dev/WORKFLOW.md`, provenance).
    let parts = [crate::intern::intern_str(""), crate::intern::intern_str("")];
    let shape = crate::memory::immortal::immortal_alloc(size_of::<crate::template::TemplateShape>())
        as *mut crate::template::TemplateShape;
    unsafe {
        shape.write(crate::template::TemplateShape {
            value_count: 1,
            parts: parts.as_ptr(),
        })
    };

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let held = unsafe {
        crate::string::ll_string_new(&mut ctx, crate::refcount::MemoryCategory::GcHeap, b"v")
    };

    let values = [Value::entity(Tag::String, held as *mut RcHeader)];
    let t = unsafe {
        crate::template::ll_template_new(
            &mut ctx,
            cls,
            shape,
            &values,
            crate::refcount::MemoryCategory::GcHeap,
        )
    };

    let mut seen = Vec::new();
    unsafe {
        crate::walk::trace_cells::<crate::walk::RelaxedCells>(
            t as *mut RcHeader,
            EntityKind::Object as u32,
            |cell| seen.push(cell.child),
        )
    };

    assert_eq!(
        seen,
        vec![held as *mut RcHeader],
        "the template's value is invisible to the epoch's walk"
    );

    unsafe {
        if ll_release(t as *mut RcHeader) {
            crate::object::ll_entity_die(t as *mut RcHeader);
        }

        if ll_release(held as *mut RcHeader) {
            crate::object::ll_entity_die(held as *mut RcHeader);
        }
    }

    arena.reset(|_| {});
}

/// The epoch's own snapshot reaches a large entity, which is a
/// different question from the synchronous walk reaching one: this is
/// the arm that runs on the collector thread, and the rows it builds
/// are what `census_row` divides. Both halves of the population are
/// here — a pooled block found by the region scan, an OS-direct run
/// found only in the registry — and each contributes **one** slot.
/// A stride would fabricate rows out of the objects' own cells, and
/// fabricated edges can balance a live component into collection.
#[test]
fn the_epoch_snapshot_reaches_both_halves_of_a_large_entity_ring() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let a = unsafe {
        new_constructed(
            &mut ctx,
            wide_class("EpochPooled", POOLED_FILLERS),
            MemoryCategory::GcHeap,
        )
    };

    let b = unsafe {
        new_constructed(
            &mut ctx,
            wide_class("EpochRun", RUN_FILLERS),
            MemoryCategory::GcHeap,
        )
    };

    unsafe {
        tie(a, 16, b);
        tie(b, 16, a);
    }

    // The snapshot the epoch will take, read directly: one row per
    // object, at the object's own address, one slot wide.
    let rows = crate::memory::heap::snapshot_entity_blocks();
    for &entity in &[a as usize, b as usize] {
        let row = rows
            .iter()
            .find(|r| r.payload == entity)
            .expect("a large entity is missing from the epoch's snapshot");
        assert_eq!(row.slots, 1, "one occupant, whatever its size");
        assert!(
            row.index.is_none(),
            "and it is found by address, not by index"
        );
    }

    let first = stepped_epoch();
    assert!(first.stamped_new >= 2, "creation epoch: allocate-black");
    assert_eq!(first.confirmed, 0);

    let second = stepped_epoch();
    assert_eq!(
        second.confirmed, 1,
        "the ring is one confirmed component, across both halves"
    );
    assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
    let seen = walked_addresses();
    assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
    arena.reset(|_| {});
}

/// A run freed while an epoch is in flight stays addressable until
/// the flush, which is the whole reason both large-entity kinds park:
/// its memory is **unmapped** at the real free, and the snapshot
/// dereferences every registered address. The corpse reads refcount 0
/// and takes no row — what is being tested is that reading it is
/// sound at all.
#[test]
fn a_run_freed_mid_epoch_is_still_addressable_when_the_snapshot_reads_it() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let obj = unsafe {
        new_constructed(
            &mut ctx,
            wide_class("EpochDying", RUN_FILLERS),
            MemoryCategory::GcHeap,
        )
    };

    let block = (obj as usize) & !crate::memory::block_pool::BLOCK_MASK;
    assert!(crate::memory::large_entity::snapshot().contains(&block));

    let mut e = Epoch::open();
    checkpoint();
    unsafe {
        assert!(ll_release(obj as *mut RcHeader));
        crate::object::ll_entity_die(obj as *mut RcHeader);
    }

    assert!(
        crate::memory::large_entity::snapshot().contains(&block),
        "the free parked, so the run is still registered and still mapped"
    );

    e.snapshot();
    e.walk();
    e.judge();
    assert_eq!(e.stats.candidates, 0, "a corpse is no candidate");
    let _ = e.close();
    checkpoint();

    assert!(
        !crate::memory::large_entity::snapshot().contains(&block),
        "and the flush gives the run back"
    );
    arena.reset(|_| {});
}

/// The epoch reaches a retained former-arena block the same way it
/// reaches an entity block, though neither the walk nor the census
/// can stride there: the reset's object index supplies the slot
/// addresses, and the census resolves a child inside one by
/// searching that index instead of dividing
/// (`rfc/model/gc/retained-block-walk.md`). The ring is built and
/// promoted before any epoch, so it matures in the first and dies
/// in the second, exactly as a heap-born ring does.
#[test]
fn a_ring_promoted_into_a_retained_block_is_collected() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let node = ClassBuilder::new("EpochPromotedRing")
        .prop("child", true)
        .destructor(counting_destructor as *const ())
        .build();
    let holder_cls = ClassBuilder::new("EpochPromotedHolder")
        .prop("head", true)
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
    let a = unsafe { new_constructed(&mut ctx, node, MemoryCategory::RequestArena) };
    let b = unsafe { new_constructed(&mut ctx, node, MemoryCategory::RequestArena) };
    unsafe {
        tie(a, 16, b);
        tie(b, 16, a);
        assert!(crate::memory::barrier::ref_store(
            &mut arena,
            holder as *mut RcHeader,
            Object::prop_at(holder, 16),
            std::ptr::null_mut(),
            Value::entity(Tag::Object, a as *mut RcHeader),
        ));
    }

    unsafe { crate::promote::arena_reset_full(&mut arena) };
    assert_eq!(
        unsafe {
            (*crate::memory::block_pool::BlockHeader::of_ptr(a as *const u8))
                .kind
                .load(Ordering::Relaxed)
        },
        crate::memory::block_pool::BLOCK_KIND_RETAINED
    );
    unsafe {
        assert!(ll_release(holder as *mut RcHeader));
        crate::object::ll_object_die(holder);
    }

    let first = stepped_epoch();
    assert!(
        first.stamped_new >= 2,
        "the promoted pair is new to the collector"
    );
    let second = stepped_epoch();
    assert!(
        second.walked >= 2,
        "a retained block's occupants are walkable"
    );
    assert_eq!(
        second.confirmed, 1,
        "the promoted ring is one confirmed component"
    );
    assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
    assert!(!walked_addresses().contains(&(a as usize)));
}
