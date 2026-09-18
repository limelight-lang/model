//! A pool thread that runs `ll_thread_init` and `ll_thread_exit` per task
//! starts a new life with each init, and the registry is per life: the exit
//! closes it behind its pass, the next init opens it again.

use super::*;

static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
}

/// A block registered in a thread's second life is torn down by that life's
/// exit, as the first life's was by its own: the close that refuses a
/// registration after the exit's pass ends with the life, the way the exit
/// phase and the journal's ring do.
#[test]
fn a_second_life_registers_again_and_its_exit_tears_the_block_down() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("TwoLivesHeld")
        .destructor(counting_destructor as *const ())
        .build() as usize;
    let layout = ClassBuilder::new("StaticsOfTwoLives")
        .prop("kept", true)
        .build() as usize;

    let released_per_life = std::thread::spawn(move || {
        let cls = cls as *const Class;
        let layout = layout as *const Class;
        let mut released = [0; 2];
        let mut blocks = [std::ptr::null_mut(); 2];
        for (life, released) in released.iter_mut().enumerate() {
            assert!(
                crate::memory::heap::ll_thread_init(),
                "the runtime started life {life}"
            );
            let mut arena = Arena::new();
            let mut ctx = LLContext { arena: &mut arena };
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

            blocks[life] = block;
            arena.reset(|_| {});
            let before = DESTRUCTS.load(Ordering::Relaxed);
            crate::memory::heap::ll_thread_exit();
            *released = DESTRUCTS.load(Ordering::Relaxed) - before;
        }

        for block in blocks {
            unsafe { free_static_block(block, layout) };
        }

        released
    })
    .join()
    .unwrap();

    assert_eq!(
        released_per_life,
        [1, 1],
        "roots released by each life's exit: the second life's block was registered"
    );
}
