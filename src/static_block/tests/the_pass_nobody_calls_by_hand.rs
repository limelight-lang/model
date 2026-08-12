//! A worker thread registers a block and simply ends; the TLS guard
//! reaches `ll_thread_exit`. The order inside that function is
//! load-bearing, because a `__destruct` allocates and a static
//! string's payload is freed through the buffer arena's own
//! thread-local.

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
