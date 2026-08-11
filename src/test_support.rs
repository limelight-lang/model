//! Fixtures more than one module's tests build.
//!
//! A shape belongs here once a second module needs it, and not before:
//! a fixture written twice is two fixtures that drift, and one written
//! once here is a dependency every reader of the test has to follow.

use crate::class::{Class, ClassBuilder};

/// Fillers whose instance no size class serves while one pooled block
/// still holds it, so the entity is found by the region scan both
/// process-global enumerators already perform.
pub(crate) const POOLED_FILLERS: usize = 600;

/// Fillers whose instance is past one block payload, so the entity is
/// an OS-direct run that no region contains and only
/// `memory::large_entity`'s registry names.
pub(crate) const RUN_FILLERS: usize = 4_200;

/// A class whose instance no size class serves: one counted Box slot
/// at +16 named `child`, `fillers` more Boxed properties left null, and
/// `destructor` if the caller has one to attach.
///
/// The filler count is the caller's because the two in use straddle
/// `BLOCK_PAYLOAD` — see [`POOLED_FILLERS`] and [`RUN_FILLERS`] — and a
/// test that wants both halves of the large-entity population asks for
/// one class of each.
pub(crate) fn wide_class(
    name: &str,
    fillers: usize,
    destructor: Option<*const ()>,
) -> *const Class {
    let mut builder = ClassBuilder::new(name).prop("child", true);
    if let Some(dispose) = destructor {
        builder = builder.destructor(dispose);
    }

    // Named separately because `prop` borrows the name for the build.
    let names: Vec<String> = (0..fillers).map(|i| format!("f{i}")).collect();
    for filler in &names {
        builder = builder.prop(filler, true);
    }

    let class = builder.build();
    let size = unsafe { (*class).object_size } as usize;
    assert!(
        size > crate::memory::heap::MAX_SMALL,
        "the instance fits a size class, so it exercises no large-entity door"
    );
    assert!(
        fillers < RUN_FILLERS || size > crate::memory::block_pool::BLOCK_PAYLOAD,
        "the instance fits one block, so it takes no OS-direct run"
    );
    class
}
