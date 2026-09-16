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

mod tests;

/// What the buffer arena hands out for `capacity` **in critical pressure
/// mode**, where an allocation searches the free lists of the owned chain
/// instead of bumping. A chunk a test is following coming back that way is
/// the free having happened; any other address is that chunk still out,
/// and a withheld one is out.
///
/// The chunk is given straight back, so the probe leaves the arena as it
/// found it. Two probes in a row therefore answer in reverse order of
/// freeing: a chunk goes to the head of its block's free list and the
/// critical search is first-fit, so the last one freed is the first one
/// offered.
pub(crate) fn chunk_from_the_free_list(capacity: usize) -> *mut u8 {
    use crate::memory::buffer::{PressureMode, set_pressure_mode};
    use crate::memory::buffer_arena::with_buffer_arena;

    let restore = crate::memory::buffer::pressure_mode();
    set_pressure_mode(PressureMode::Critical);
    let (chunk, granted) = with_buffer_arena(|arena| arena.alloc(capacity));
    set_pressure_mode(restore);
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
        "the instance fits a size class, so it exercises no large-entity allocation path"
    );
    assert!(
        fillers < RUN_FILLERS || size > crate::memory::block_pool::BLOCK_PAYLOAD,
        "the instance fits one block, so it takes no OS-direct run"
    );
    class
}

/// Entity pointer behind a Box slot, or null for scalar/null Boxes.
pub(crate) fn entity_checked(v: &Value) -> *mut RcHeader {
    if v.is_pointer() {
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

/// Run the test `case` — its full path — again in a child process with the
/// environment variable `marker` set, and assert that the child ended by
/// `SIGABRT` with `reason` on its stderr.
///
/// The signal is asserted rather than a failure: a panic in the child's
/// fixture would satisfy an unsuccessful exit, and the entry points this
/// serves end the process with no frame to report through. The reason is
/// asserted beside it, because an abort is what every last resort in the
/// crate answers with and only the reason says which one was reached. The
/// child's own count is read too: `--exact` with a name the harness cannot
/// match runs nothing and exits zero.
pub(crate) fn a_child_run_ends_by_abort_saying(case: &str, marker: &str, reason: &str) {
    use std::os::unix::process::ExitStatusExt;

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", case, "--nocapture"])
        .env(marker, "1")
        .output()
        .expect("the test binary runs the case as its own child");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("running 1 test"),
        "the child ran this case rather than none"
    );
    // Spelled out because the crate takes no `libc` dependency; 6 on every
    // unix this crate builds for.
    const SIGABRT: i32 = 6;
    assert_eq!(
        output.status.signal(),
        Some(SIGABRT),
        "the child did not abort; status {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(reason),
        "the child aborted for another reason than {reason:?}:\n{stderr}"
    );
}
