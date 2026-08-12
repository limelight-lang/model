//! Reverse registration order, as C++ tears down function-local
//! statics, and one block popped at a time rather than a drain held
//! across user code: a `__destruct` reached mid-pass can register a
//! block of its own, and that block is the newest.

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
