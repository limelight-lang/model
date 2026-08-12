//! One entry point answers for a class and for an interface; the
//! display and the itable it reads are `class.rs`'s.

use super::*;

#[test]
fn instanceof_covers_classes_and_interfaces() {
    let _g = crate::memory::block_pool::test_guard();
    extern "C" fn noop() {}

    let interface = ClassBuilder::interface("Speaks");
    let animal = ClassBuilder::new("Animal")
        .method("speak", noop as *const ())
        .implement(unsafe { &*interface }, vec![0])
        .build();
    let dog = ClassBuilder::new("Dog").parent(animal).build();
    let rock = ClassBuilder::new("Rock").build();

    with_ctx(|ctx| {
        let d = unsafe { new_constructed(ctx, dog, MemoryCategory::RequestArena) };
        let r = unsafe { new_constructed(ctx, rock, MemoryCategory::RequestArena) };
        unsafe {
            assert!(ll_instanceof(d, animal));
            assert!(ll_instanceof(d, dog));
            assert!(
                ll_instanceof(d, interface),
                "interface via inherited itable"
            );
            assert!(!ll_instanceof(r, animal));
            assert!(!ll_instanceof(r, interface));
        }
    });
}
