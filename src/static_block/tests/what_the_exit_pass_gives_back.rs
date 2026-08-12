//! A static block holds ordinary references, so the pass releases
//! what each slot holds: a heap object takes its last release here,
//! an arena escapee loses the hold-count the escape barrier put on
//! it, and a death a weak cell names reaches the weak table, which is
//! why that table is disposed of after this pass rather than before.

use super::*;

static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
}

/// The heap case: a static's reference is the object's last one, so
/// thread exit is what runs its `__destruct` and frees it.
#[test]
fn a_heap_reference_held_by_a_static_dies_at_thread_exit() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("StaticHeld")
        .destructor(counting_destructor as *const ())
        .build();
    let holder_layout = ClassBuilder::new("StaticsOfStaticHeld")
        .prop("kept", true)
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };

    let block = static_block(holder_layout);
    unsafe {
        let slot = block.add(16) as *mut Value;
        assert!(crate::memory::barrier::store_box(
            &mut arena,
            MemoryCategory::LongLived,
            slot,
            Value::entity(Tag::Object, obj as *mut RcHeader),
        ));
        ll_static_block_register(block, holder_layout);
    }

    // The static's store took the second reference; the local one goes.
    unsafe { assert!(!crate::refcount::ll_release(obj as *mut RcHeader)) };

    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        0,
        "still held by the static"
    );
    run_thread_exit_teardown();
    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        1,
        "the static let go at exit"
    );

    unsafe { free_static_block(block, holder_layout) };
    arena.reset(|_| {});
}

/// The arena case, and the one with no other decrement point: a
/// static holding a request-arena object placed an escape
/// hold-count on it, and only thread exit or an overwrite takes it
/// back off.
#[test]
fn an_arena_escapee_loses_its_hold_count_at_thread_exit() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("StaticEscapee").build();
    let holder_layout = ClassBuilder::new("StaticsOfEscapee")
        .prop("kept", true)
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };

    let block = static_block(holder_layout);
    unsafe {
        let slot = block.add(16) as *mut Value;
        assert!(crate::memory::barrier::store_box(
            &mut arena,
            MemoryCategory::LongLived,
            slot,
            Value::entity(Tag::Object, obj as *mut RcHeader),
        ));
        ll_static_block_register(block, holder_layout);
    }

    let held = unsafe { (*(obj as *mut RcHeader)).refcount };
    assert!(held >= 1, "the static's store is an escape hold");

    run_thread_exit_teardown();
    assert_eq!(
        unsafe { (*(obj as *mut RcHeader)).refcount },
        held - 1,
        "thread exit is the escape hold-count's other decrement point"
    );

    unsafe { free_static_block(block, holder_layout) };
    unsafe { crate::promote::arena_reset_full(&mut arena) };
}

/// A static holding an object that a weak cell also names. The
/// death at thread exit must reach the weak table — which is why
/// the table is disposed of last, and why swallowing the
/// notification would be worse than aborting: the cell's target
/// would dangle while `get()` still hands it out retained.
///
/// **The cell outlives the thread, and that is the instrument.**
/// Killed inside the thread it clears `HAS_WEAK_REFERENCES` on its
/// way out, so the target's death notifies nothing and the
/// ordering this test exists for cannot fail. Read after the join,
/// the cell answers null or the notification never ran.
#[test]
fn a_weak_referenced_object_held_by_a_static_notifies_at_thread_exit() {
    let _g = crate::memory::block_pool::test_guard();
    static SEEN: AtomicUsize = AtomicUsize::new(0);
    static CELL: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn record(_o: *mut Object) {
        SEEN.fetch_add(1, Ordering::Relaxed);
    }

    SEEN.store(0, Ordering::Relaxed);
    CELL.store(0, Ordering::Relaxed);

    let cls = ClassBuilder::new("StaticWeakTarget")
        .destructor(record as *const ())
        .build();
    let layout = ClassBuilder::new("StaticsOfWeakTarget")
        .prop("kept", true)
        .build();
    let cls = cls as usize;
    let layout = layout as usize;

    std::thread::spawn(move || {
        let cls = cls as *const Class;
        let layout = layout as *const Class;
        crate::memory::heap::ll_thread_init();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let weak = unsafe { crate::weak::ll_weakref_create(&mut ctx, obj as *mut RcHeader) };
        assert!(!weak.is_null());
        // Handed to the parent rather than killed here: the cell has
        // to be alive when the static pass releases the target.
        CELL.store(weak as usize, Ordering::Relaxed);

        let block = static_block(layout);
        unsafe {
            assert!(crate::memory::barrier::store_box(
                &mut arena,
                MemoryCategory::LongLived,
                block.add(16) as *mut Value,
                Value::entity(Tag::Object, obj as *mut RcHeader),
            ));
            ll_static_block_register(block, layout);
            assert!(!crate::refcount::ll_release(obj as *mut RcHeader));
        }

        arena.reset(|_| {});
    })
    .join()
    .unwrap();

    assert_eq!(
        SEEN.load(Ordering::Relaxed),
        1,
        "the target died at thread exit"
    );

    let weak = CELL.load(Ordering::Relaxed) as *mut crate::weak::LLWeakRef;
    assert!(
        unsafe { crate::weak::ll_weakref_get(weak) }.is_null(),
        "the death at thread exit never reached the weak table"
    );

    // The cell is this thread's to give back now; its target is
    // already gone, so the teardown touches no weak table.
    unsafe {
        assert!(crate::refcount::ll_release(weak as *mut RcHeader));
        crate::object::ll_entity_die(weak as *mut RcHeader);
    }
}
