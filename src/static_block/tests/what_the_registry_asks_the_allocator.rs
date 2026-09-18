//! What the registry and the pass cost on the global allocator: nothing. The
//! entries sit in a chunk of the thread's buffer arena, so a growth the manager
//! refuses is a registration refused, and a registration after the exit's
//! pass has drained is refused too, since no pass would ever free the chunk it
//! drew; the pass drops each displaced child as it empties the slot, holding
//! none (`dev/DECISIONS.md`, "the reset window's memory comes from the
//! manager, and an allocation it cannot get is a refusal").

use super::*;
use crate::memory::buffer_arena::{FORCE_REFUSE_LONGLIVED, SERVE_BEFORE_REFUSING};
use crate::test_support::allocation_probe;

static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
}

/// A block holding one GC-heap object of `cls` in its first slot, the local
/// reference spent, so the block's slot is the object's last holder.
unsafe fn a_block_holding(
    arena: &mut Arena,
    cls: *const Class,
    layout: *const Class,
) -> (*mut u8, *mut Object) {
    let mut ctx = LLContext { arena: &mut *arena };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let block = static_block(layout);
    unsafe {
        assert!(crate::memory::barrier::store_box(
            arena,
            MemoryCategory::LongLived,
            block.add(16) as *mut Value,
            Value::entity(Tag::Object, obj as *mut RcHeader),
        ));
        assert!(!crate::refcount::ll_release(obj as *mut RcHeader));
    }

    (block, obj)
}

/// A first registration on a thread whose registry is empty makes no call
/// into the global allocator, and the pass that drains it gives the chunk
/// back to the arena: the registry reads empty afterwards. What the pass
/// itself asks of the global allocator is the next case's reading.
#[test]
fn a_first_registration_asks_the_global_allocator_for_nothing() {
    let _g = crate::memory::block_pool::test_guard();
    let (heap, held_after_registration, held_after_pass) = std::thread::spawn(|| {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the runtime started this thread"
        );
        let cls = ClassBuilder::new("RegistryFirstHeld").build();
        let layout = ClassBuilder::new("StaticsOfRegistryFirst")
            .prop("kept", true)
            .build();
        let mut arena = Arena::new();
        let (block, _obj) = unsafe { a_block_holding(&mut arena, cls, layout) };

        allocation_probe::take_heap_allocations();
        unsafe { ll_static_block_register(block, layout) };
        let heap = allocation_probe::take_heap_allocations();
        let held_after_registration = registry_holds();

        run_thread_exit_teardown();
        let held_after_pass = registry_holds();

        unsafe { free_static_block(block, layout) };
        arena.reset(|_| {});
        (heap, held_after_registration, held_after_pass)
    })
    .join()
    .unwrap();

    assert_eq!(heap, 0, "the registration reached the global allocator");
    assert_eq!(
        held_after_registration,
        (ENTRY_SIZE, ENTRY_SIZE),
        "(bytes used, bytes granted): one entry in a chunk of its own size"
    );
    assert_eq!(held_after_pass, (0, 0), "the pass left the chunk held");
}

/// The pass over a block with two displaced children makes no call into the
/// global allocator and no free through it: each child is dropped as its
/// slot is emptied, so the pass holds no list of them.
#[test]
fn the_pass_asks_the_global_allocator_for_nothing() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("PassDisplacedHeld")
        .destructor(counting_destructor as *const ())
        .build();
    let layout = ClassBuilder::new("StaticsOfPassDisplaced")
        .prop("first", true)
        .prop("second", true)
        .build();
    let mut arena = Arena::new();
    let (block, _first) = unsafe { a_block_holding(&mut arena, cls, layout) };
    let second = {
        let mut ctx = LLContext { arena: &mut arena };
        unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) }
    };
    unsafe {
        assert!(crate::memory::barrier::store_box(
            &mut arena,
            MemoryCategory::LongLived,
            block.add(32) as *mut Value,
            Value::entity(Tag::Object, second as *mut RcHeader),
        ));
        assert!(!crate::refcount::ll_release(second as *mut RcHeader));
        ll_static_block_register(block, layout);
    }

    let _ = allocation_probe::take_heap_allocations();
    let _ = allocation_probe::take_heap_deallocations();
    run_thread_exit_teardown();
    let heap = allocation_probe::take_heap_allocations();
    let freed = allocation_probe::take_heap_deallocations();

    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        2,
        "the pass released both children"
    );
    assert_eq!(
        (heap, freed),
        (0, 0),
        "(allocations, frees) the pass made through the global allocator"
    );

    unsafe { free_static_block(block, layout) };
    arena.reset(|_| {});
}

/// A growth the manager refuses leaves the block unregistered and the
/// registry as it was: the block registered before it is torn down by the
/// pass, and the refused block's root is still held after it.
#[test]
fn a_registration_the_manager_refuses_leaves_the_block_unregistered() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("RegistryRefusedHeld")
        .destructor(counting_destructor as *const ())
        .build();
    let layout = ClassBuilder::new("StaticsOfRegistryRefused")
        .prop("kept", true)
        .build();
    let mut arena = Arena::new();

    let (served, _) = unsafe { a_block_holding(&mut arena, cls, layout) };
    unsafe { ll_static_block_register(served, layout) };
    let held = registry_holds();
    assert_eq!(held.0, held.1, "the next entry needs a growth");

    let (refused, root) = unsafe { a_block_holding(&mut arena, cls, layout) };
    SERVE_BEFORE_REFUSING.store(0, Ordering::Relaxed);
    FORCE_REFUSE_LONGLIVED.store(true, Ordering::Relaxed);
    unsafe { ll_static_block_register(refused, layout) };
    FORCE_REFUSE_LONGLIVED.store(false, Ordering::Relaxed);
    assert_eq!(
        registry_holds(),
        held,
        "a refused growth changed the registry"
    );

    run_thread_exit_teardown();
    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        1,
        "the served block was torn down and the refused one was not"
    );
    assert_eq!(
        unsafe { crate::refcount::entity_refcount(root) },
        1,
        "the refused block's root is still held by its slot"
    );

    // The refused block's root is this test's to release now.
    unsafe {
        assert!(crate::refcount::ll_release(root as *mut RcHeader));
        crate::object::ll_object_die(root);
        free_static_block(served, layout);
        free_static_block(refused, layout);
    }

    arena.reset(|_| {});
}

/// The pointer to the block a destructor registers, and the layout: handed
/// to the destructor through statics, because a `dispose` takes one argument.
static LATE_BLOCK: AtomicUsize = AtomicUsize::new(0);
static LATE_LAYOUT: AtomicUsize = AtomicUsize::new(0);
/// Whether a destructor has registered already: one member of the ring does.
static LATE_REGISTERED: AtomicUsize = AtomicUsize::new(0);
/// The registry's `(len, capacity)` read right after that registration, on
/// the exiting thread: `(0, 0)` is a refusal, anything else an entry written.
static LATE_HELD: (AtomicUsize, AtomicUsize) = (AtomicUsize::new(0), AtomicUsize::new(0));
/// Deaths of the late block's root, apart from the other destructors' count.
static LATE_ROOT_DESTRUCTS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn late_root_destructor(_obj: *mut Object) {
    LATE_ROOT_DESTRUCTS.fetch_add(1, Ordering::Relaxed);
}

/// Runs inside the exit's collection, after the static pass has drained, and
/// registers a block there.
unsafe extern "C" fn registers_at_the_exits_collection(_o: *mut Object) {
    if LATE_REGISTERED.swap(1, Ordering::Relaxed) != 0 {
        return;
    }

    let block = LATE_BLOCK.load(Ordering::Relaxed) as *mut u8;
    let layout = LATE_LAYOUT.load(Ordering::Relaxed) as *const Class;
    unsafe { ll_static_block_register(block, layout) };
    let (len, capacity) = registry_holds();
    LATE_HELD.0.store(len, Ordering::Relaxed);
    LATE_HELD.1.store(capacity, Ordering::Relaxed);
}

/// A registration from the exit's collection — step 2 of `ll_thread_exit`,
/// the last step that runs user code — is refused: the registry holds no
/// entry for it, where an entry written there would be popped by no pass, and
/// the block's root is still held once the thread has ended, while the block
/// registered before the exit was torn down by its pass. The cost is the one
/// a refused growth already names, the block's roots leaking, where a chunk
/// drawn here would have kept its whole block on the abandoned list for the
/// life of the process.
///
/// The refusal is read off the registry rather than off a pool counter: an
/// accepted registration on a fresh thread can be served out of a block
/// another thread abandoned, with no pool request to count.
#[test]
fn a_registration_from_the_exits_collection_is_refused() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    LATE_ROOT_DESTRUCTS.store(0, Ordering::Relaxed);
    LATE_REGISTERED.store(0, Ordering::Relaxed);
    LATE_HELD.0.store(usize::MAX, Ordering::Relaxed);
    LATE_HELD.1.store(usize::MAX, Ordering::Relaxed);
    let early_cls = ClassBuilder::new("RegistryEarlyHeld")
        .destructor(counting_destructor as *const ())
        .build();
    let late_cls = ClassBuilder::new("RegistryLateTarget")
        .destructor(late_root_destructor as *const ())
        .build();
    let layout = ClassBuilder::new("StaticsOfRegistryLate")
        .prop("kept", true)
        .build();
    let ring_cls = ClassBuilder::new("RegistersAtTheExitsCollection")
        .prop("next", true)
        .destructor(registers_at_the_exits_collection as *const ())
        .build();
    let early_cls = early_cls as usize;
    let late_cls = late_cls as usize;
    let layout = layout as usize;
    let ring_cls = ring_cls as usize;

    let (early_block, root) = std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the runtime started this thread"
        );
        let early_cls = early_cls as *const Class;
        let late_cls = late_cls as *const Class;
        let layout = layout as *const Class;
        let ring_cls = ring_cls as *const Class;
        let mut arena = Arena::new();
        let (early_block, _) = unsafe { a_block_holding(&mut arena, early_cls, layout) };
        unsafe { ll_static_block_register(early_block, layout) };
        let (block, root) = unsafe { a_block_holding(&mut arena, late_cls, layout) };
        LATE_BLOCK.store(block as usize, Ordering::Relaxed);
        LATE_LAYOUT.store(layout as usize, Ordering::Relaxed);

        // A garbage ring left registered, which the exit's collection takes
        // and whose destructors run there.
        let _members = unsafe { crate::cycle::testing::ring(&mut arena, [ring_cls, ring_cls]) };
        arena.reset(|_| {});
        // No `ll_thread_exit()` here: the guard runs it, and the collection
        // is its second step.
        (early_block as usize, root as usize)
    })
    .join()
    .unwrap();

    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        1,
        "the block registered before the exit was torn down by its pass"
    );
    assert_eq!(
        LATE_REGISTERED.load(Ordering::Relaxed),
        1,
        "the ring's destructor ran at the exit"
    );
    assert_eq!(
        (
            LATE_HELD.0.load(Ordering::Relaxed),
            LATE_HELD.1.load(Ordering::Relaxed)
        ),
        (0, 0),
        "(len, capacity) of the registry after the late registration: an entry was written"
    );
    assert_eq!(
        LATE_ROOT_DESTRUCTS.load(Ordering::Relaxed),
        0,
        "the late block's root was torn down"
    );
    let root = root as *mut Object;
    assert_eq!(
        unsafe { crate::refcount::entity_refcount(root) },
        1,
        "the late block's root is still held by its slot"
    );

    // The root is this test's to release now, and both blocks too.
    unsafe {
        assert!(crate::refcount::ll_release(root as *mut RcHeader));
        crate::object::ll_object_die(root);
        free_static_block(early_block as *mut u8, layout as *const Class);
        free_static_block(
            LATE_BLOCK.load(Ordering::Relaxed) as *mut u8,
            LATE_LAYOUT.load(Ordering::Relaxed) as *const Class,
        );
    }
}
