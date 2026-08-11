use super::*;
use crate::class::ClassBuilder;
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::{Object, new_constructed};
use crate::value::{Tag, Value};
use std::sync::atomic::{AtomicUsize, Ordering};

static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
}

/// A static block is a bare allocation with an object's layout and
/// no header — exactly what the compiler will emit for a class's
/// `static` properties.
fn static_block(layout: *const Class) -> *mut u8 {
    let size = unsafe { (*layout).object_size } as usize;
    let p =
        unsafe { std::alloc::alloc_zeroed(std::alloc::Layout::from_size_align(size, 16).unwrap()) };

    assert!(!p.is_null());
    p
}

unsafe fn free_static_block(p: *mut u8, layout: *const Class) {
    let size = unsafe { (*layout).object_size } as usize;
    unsafe { std::alloc::dealloc(p, std::alloc::Layout::from_size_align(size, 16).unwrap()) };
}

/// A static block holds ordinary references, so the pass releases
/// what each slot holds: a heap object takes its last release here,
/// an arena escapee loses the hold-count the escape barrier put on
/// it, and a target whose weak cell has already died dies at exit
/// like any other.
mod what_the_exit_pass_gives_back {
    use super::*;

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
    #[test]
    fn a_weak_referenced_object_held_by_a_static_notifies_at_thread_exit() {
        let _g = crate::memory::block_pool::test_guard();
        static SEEN: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn record(_o: *mut Object) {
            SEEN.fetch_add(1, Ordering::Relaxed);
        }

        SEEN.store(0, Ordering::Relaxed);

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
                // Let the cell go too; the static is the last holder of
                // the target, so its death at exit is what nulls it.
                assert!(crate::refcount::ll_release(weak as *mut RcHeader));
                crate::object::ll_entity_die(weak as *mut RcHeader);
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
    }
}

/// A worker thread registers a block and simply ends; the TLS guard
/// reaches `ll_thread_exit`. The order inside that function is
/// load-bearing, because a `__destruct` allocates and a static
/// string's payload is freed through the buffer arena's own
/// thread-local.
mod the_pass_nobody_calls_by_hand {
    use super::*;

    /// The wiring, and the case that matters in production: nobody
    /// calls the pass by hand. A worker thread registers a static block
    /// and simply ends; the TLS guard reaches `ll_thread_exit`, and the
    /// static's reference must be gone before the thread's blocks go
    /// home — the destructor allocates, so the order is load-bearing.
    #[test]
    fn a_thread_that_just_ends_releases_its_static_blocks() {
        let _g = crate::memory::block_pool::test_guard();
        static RAN: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn record(_o: *mut Object) {
            RAN.fetch_add(1, Ordering::Relaxed);
        }

        RAN.store(0, Ordering::Relaxed);

        let cls = ClassBuilder::new("StaticOnAWorker")
            .destructor(record as *const ())
            .build();
        let layout = ClassBuilder::new("StaticsOfAWorker")
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

            // One ordinary entity death *in the thread's own body*, and
            // it is load-bearing. It is what first initializes the
            // per-thread structures the exit teardown then reaches —
            // the parked-free list under rc-walk, the candidate buffer
            // under rc-trace — so their TLS destructors are registered
            // after the exit guard's and therefore run *before* it. A
            // version of this test without this death passed while the
            // production shape aborted.
            let doomed = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
            unsafe {
                assert!(crate::refcount::ll_release(doomed as *mut RcHeader));
                crate::object::ll_object_die(doomed);
            }

            let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
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
            // Deliberately no `ll_thread_exit()`: the guard must do it,
            // and the teardown pass must ride that same path.
        })
        .join()
        .unwrap();

        assert_eq!(
            RAN.load(Ordering::Relaxed),
            2,
            "the in-body death and the static's release both ran"
        );
    }

    /// The same path with a **dynamic string** in the static, which is
    /// what made the buffer arena reachable from thread exit: its
    /// teardown frees the payload, and that free goes through the arena's
    /// thread-local. While that local was a `RefCell<BufferArena>` it had
    /// drop glue, so its key was registered after the exit guard's and
    /// destroyed before it; `with` then panicked with `AccessError`
    /// inside a destructor, which cannot unwind, and the process aborted
    /// — taking the whole suite with it, which is why this is a test
    /// about a thread and not about a string.
    ///
    /// The in-body dynamic string is load-bearing for the same reason as
    /// the in-body death above: it is what first touches the arena, so
    /// that under the old shape its destructor was registered at all.
    #[test]
    fn a_thread_that_just_ends_frees_a_static_strings_payload() {
        let _g = crate::memory::block_pool::test_guard();
        let layout = ClassBuilder::new("StaticsHoldingAString")
            .prop("kept", true)
            .build() as usize;

        std::thread::spawn(move || {
            let layout = layout as *const Class;
            crate::memory::heap::ll_thread_init();
            let mut arena = Arena::new();
            let mut ctx = LLContext { arena: &mut arena };

            // Touch the buffer arena from the thread body first.
            let scratch = unsafe {
                crate::string::ll_string_new_dynamic(
                    &mut ctx,
                    MemoryCategory::GcHeap,
                    b"in the body",
                    0,
                )
            };

            unsafe {
                assert!(crate::refcount::ll_release(scratch as *mut RcHeader));
                crate::object::ll_entity_die(scratch as *mut RcHeader);
            }

            let s = unsafe {
                crate::string::ll_string_new_dynamic(
                    &mut ctx,
                    MemoryCategory::GcHeap,
                    b"held by a static until the thread ends",
                    0,
                )
            };

            let block = static_block(layout);
            unsafe {
                assert!(crate::memory::barrier::store_box(
                    &mut arena,
                    MemoryCategory::LongLived,
                    block.add(16) as *mut Value,
                    Value::entity(Tag::String, s as *mut RcHeader),
                ));
                ll_static_block_register(block, layout);
                assert!(!crate::refcount::ll_release(s as *mut RcHeader));
            }

            arena.reset(|_| {});
            // Deliberately no `ll_thread_exit()`: the guard must do it.
        })
        .join()
        .expect("the exit path must not abort freeing a payload");
    }
}

/// Reverse registration order, as C++ tears down function-local
/// statics, and one block popped at a time rather than a drain held
/// across user code: a `__destruct` reached mid-pass can register a
/// block of its own, and that block is the newest.
mod the_order_within_the_pass {
    use super::*;

    /// Reverse initialization order, as C++ tears down function-local
    /// statics: the later block may hold a reference the earlier one's
    /// teardown would otherwise have invalidated first.
    #[test]
    fn blocks_are_torn_down_in_reverse_registration_order() {
        let _g = crate::memory::block_pool::test_guard();
        static ORDER: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());
        unsafe extern "C" fn record_first(_o: *mut Object) {
            ORDER.lock().unwrap().push(1);
        }

        unsafe extern "C" fn record_second(_o: *mut Object) {
            ORDER.lock().unwrap().push(2);
        }

        ORDER.lock().unwrap().clear();

        let first_cls = ClassBuilder::new("FirstStatic")
            .destructor(record_first as *const ())
            .build();
        let second_cls = ClassBuilder::new("SecondStatic")
            .destructor(record_second as *const ())
            .build();
        let layout = ClassBuilder::new("StaticsOfOrdering")
            .prop("kept", true)
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let mut register = |cls: *const Class| {
            let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
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

            block
        };

        let b1 = register(first_cls);
        let b2 = register(second_cls);

        run_thread_exit_teardown();
        assert_eq!(
            *ORDER.lock().unwrap(),
            vec![2, 1],
            "the block registered second is torn down first"
        );

        unsafe {
            free_static_block(b1, layout);
            free_static_block(b2, layout);
        }

        arena.reset(|_| {});
    }

    /// The reentrancy the pass's pop-one loop exists for: a
    /// `__destruct` reached mid-pass initializes a static block of its
    /// own, which registers while the loop is running. That block is
    /// the newest, so it must be torn down before the older ones the
    /// loop has not reached — which popping gives for free, and which a
    /// drain held across user code could not.
    #[test]
    fn a_block_registered_by_a_destructor_is_torn_down_within_the_same_pass() {
        let _g = crate::memory::block_pool::test_guard();
        static LATE: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());
        static ORDER: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
        static LATE_LAYOUT: AtomicUsize = AtomicUsize::new(0);
        static LATE_CLS: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn late_target(_o: *mut Object) {
            ORDER.lock().unwrap().push("late");
        }

        /// Runs while the pass is draining, and registers one more block.
        unsafe extern "C" fn registers_another(_o: *mut Object) {
            ORDER.lock().unwrap().push("first");
            let layout = LATE_LAYOUT.load(Ordering::Relaxed) as *const Class;
            let cls = LATE_CLS.load(Ordering::Relaxed) as *const Class;
            let block = static_block(layout);
            let mut arena = Arena::new();
            let mut ctx = LLContext { arena: &mut arena };
            let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
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

            LATE.lock().unwrap().push(block as usize);
            arena.reset(|_| {});
        }

        ORDER.lock().unwrap().clear();
        LATE.lock().unwrap().clear();
        let layout = ClassBuilder::new("StaticsOfReentrant")
            .prop("kept", true)
            .build();
        let late_cls = ClassBuilder::new("LateStaticTarget")
            .destructor(late_target as *const ())
            .build();
        let first_cls = ClassBuilder::new("RegistersAnother")
            .destructor(registers_another as *const ())
            .build();
        LATE_LAYOUT.store(layout as usize, Ordering::Relaxed);
        LATE_CLS.store(late_cls as usize, Ordering::Relaxed);

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe { new_constructed(&mut ctx, first_cls, MemoryCategory::GcHeap) };
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

        run_thread_exit_teardown();
        assert_eq!(
            *ORDER.lock().unwrap(),
            vec!["first", "late"],
            "the block a destructor registered was drained by the same pass"
        );

        unsafe { free_static_block(block, layout) };
        for &b in LATE.lock().unwrap().iter() {
            unsafe { free_static_block(b as *mut u8, layout) };
        }

        arena.reset(|_| {});
    }
}
