//! Fixtures more than one module's tests build.
//!
//! A shape belongs here once a second module needs it, and not before:
//! a fixture written twice is two fixtures that drift, and one written
//! once here is a dependency every reader of the test has to follow.

use crate::class::{Class, ClassBuilder};
use crate::memory::arena::Arena;
use crate::memory::barrier::ref_store;
use crate::object::Object;
use crate::refcount::RcHeader;
use crate::value::{Tag, Value};

pub(crate) mod allocation_probe;
pub(crate) mod outside_block;

/// What the buffer arena hands out for `capacity` **in critical pressure
/// mode**, where an allocation searches the free lists of the owned chain
/// instead of bumping. A chunk a test is following coming back that way is
/// the free having happened; any other address is that chunk still out,
/// and a parked one is out.
///
/// The chunk is given straight back, so the probe leaves the arena as it
/// found it. Two probes in a row therefore answer in reverse order of
/// freeing: a chunk goes to the head of its block's free list and the
/// critical search is first-fit, so the last one freed is the first one
/// offered.
pub(crate) fn chunk_from_the_free_list(capacity: usize) -> *mut u8 {
    use crate::memory::buffer::{PressureMode, set_pressure_mode};
    use crate::memory::buffer_arena::with_buffer_arena;

    set_pressure_mode(PressureMode::Critical);
    let (chunk, granted) = with_buffer_arena(|arena| arena.alloc(capacity));
    set_pressure_mode(PressureMode::Plenty);
    with_buffer_arena(|arena| unsafe { arena.free(chunk, granted) });
    chunk
}

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

/// Entity pointer behind a Box slot, or null for scalar/null Boxes.
pub(crate) fn entity_checked(v: &Value) -> *mut RcHeader {
    if v.is_refcounted() {
        v.entity_ptr()
    } else {
        std::ptr::null_mut()
    }
}

/// The offset of a class's `index`-th declared property, which is the
/// `offset` [`store_prop`] takes. A `ClassBuilder` property is one
/// `Value`: the header and the class word take the first sixteen bytes
/// of an object, and each property takes sixteen after them.
pub(crate) fn prop_offset(index: u32) -> u32 {
    16 + 16 * index
}

/// Store `value` into `holder`'s slot at `offset` through the real
/// barrier, as generated code would.
pub(crate) unsafe fn store_prop(
    arena: *mut Arena,
    holder: *mut Object,
    offset: u32,
    value: *mut Object,
) {
    unsafe {
        let slot = Object::prop_at(holder, offset);
        let old = entity_checked(&*slot);
        let new = if value.is_null() {
            Value::null()
        } else {
            Value::entity(Tag::Object, value as *mut RcHeader)
        };

        assert!(ref_store(arena, holder as *mut RcHeader, slot, old, new));
    }
}
