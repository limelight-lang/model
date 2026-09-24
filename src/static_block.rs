//! Static blocks and their teardown at thread exit (A6).
//!
//! A static block holds a class's `static` properties for one thread.
//! It is **headerless** — no `RcHeader`, no class pointer at `+8` — so
//! it is not an entity, no collector walks it, and its reference slots
//! are roots for as long as the thread lives. Those roots need one
//! release point, and thread exit is it: the escape hold-count a static
//! places on a request-arena object has no other decrement, an overwrite
//! mid-request aside. Without this pass a worker pool accumulates every
//! request graph its statics ever touched, for the life of the process.
//!
//! # The pass
//!
//! Each thread appends a block to a thread-local registry — a chunk of
//! its buffer arena — the first time it initializes that block, beside
//! the initializer that already runs there. At exit the registry is
//! walked in **reverse** initialization order, and per block every
//! counted slot is nulled and its former occupant dropped through the
//! barrier's `drop_ref` with `owner_cat = LongLived`. Nothing here
//! branches on what the slot held, because `drop_ref` already decides.
//! The registry closes behind the exit's pass and refuses a registration
//! until the thread's next life reopens it ([`close_the_registry`],
//! [`reopen_the_registry`]); a refused block's roots leak, which
//! [`ll_static_block_register`] prices.
//!
//! This module holds a base address and the descriptor that says where
//! the reference slots are; how a block is allocated and what its slots
//! mean are not its business — the stride over the slots is `object`'s,
//! the emptying `cells`', and the release policy the barrier's.
//!
//! Why the order is LIFO, why the drops go through `drop_ref`, and why
//! the process's last thread runs the pass in full like every other:
//! `rfc/model/classes.md`, "Teardown at thread exit", and
//! `dev/DECISIONS.md`, "thread exit owns the order its per-thread state
//! dies in".

use std::cell::{Cell, UnsafeCell};

use crate::class::Class;
use crate::memory::buffer::Buffer;
use crate::memory::buffer_arena::{buffer_ensure_longlived, buffer_release_longlived};
use crate::refcount::MemoryCategory;

/// One registry entry: a block and the descriptor that lays it out.
#[derive(Clone, Copy)]
#[repr(C)]
struct Registered {
    block: *mut u8,
    layout: *const Class,
}

/// Bytes one entry takes in the registry's chunk.
const ENTRY_SIZE: usize = size_of::<Registered>();

thread_local! {
    /// Registered blocks in initialization order, torn down in reverse:
    /// [`ENTRY_SIZE`]-byte entries in a chunk of this thread's buffer
    /// arena, `len` and `capacity` in bytes as [`Buffer`] has them.
    ///
    /// A `Buffer` in an `UnsafeCell`, not a `RefCell<Vec<..>>`: a `Vec`
    /// has drop glue, so its `thread_local` is registered for TLS
    /// destruction and can be gone before
    /// [`run_thread_exit_teardown`], which is itself reached **from** a
    /// TLS destructor — the heap guard's. No `thread_local!` this path
    /// can reach may have drop glue (`dev/DECISIONS.md`, "thread exit owns
    /// the order its per-thread state dies in"). And
    /// a chunk of the arena rather than a `Vec` on the global heap, because
    /// a growth the manager refuses is answered to the registration, where
    /// a `Vec`'s is an abort (`dev/DECISIONS.md`, "the reset window's
    /// memory comes from the manager, and an allocation it cannot get is a
    /// refusal").
    ///
    /// The chunk is owned here: grown by the first registration that needs
    /// the room, released when the pass drains it. A destructor running
    /// mid-pass may register another block, which is why the pass pops one
    /// entry at a time and releases only once the registry is empty.
    static BLOCKS: UnsafeCell<Buffer> = const { UnsafeCell::new(Buffer::new()) };

    /// Whether the exit's pass has drained this registry for this life of
    /// the thread. Set by [`close_the_registry`], cleared by
    /// [`reopen_the_registry`] when the next life's init begins: between
    /// the two a registration would draw a chunk no pass pops, and the
    /// block that chunk sits in would never empty.
    static CLOSED: Cell<bool> = const { Cell::new(false) };
}

/// Register `block`, laid out by `layout`, for teardown when this
/// thread exits.
///
/// Called once per class-block per thread, by the static initializer.
/// Registering the same block twice on one thread is a caller error;
/// it would tear the block down twice, and the second pass finds every
/// slot already null, so it costs a walk and releases nothing.
///
/// **Refuses rather than aborts** when the registry cannot grow, like
/// every other growth point in this crate (`ll_thread_init`'s hand-rolled
/// allocation; the root queue refuses the same way), and refuses too once
/// the exit's pass has drained the registry ([`close_the_registry`]). The
/// cost of a refusal is named and bounded: that block's roots are not
/// released at thread exit, so its graph leaks for the life of the
/// process — the same outcome as before A6 existed, and better than
/// aborting a running server to reclaim memory.
///
/// # Safety
/// `block` must address a static block that stays valid until this
/// thread exits, laid out by `layout`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_static_block_register(block: *mut u8, layout: *const Class) {
    debug_assert!(!block.is_null() && !layout.is_null());
    // A static block is torn down through `object::for_each_body_cell`,
    // which strides the body alone; the group's own sever takes an
    // entity, and a block has no header to be one. So a layout carrying
    // the flag would keep cells nothing here empties and nothing here
    // frees (`dev/DECISIONS.md`, "a class with cells outside itself
    // carries one flag and one group of five").
    debug_assert_eq!(
        unsafe { Class::flags_of(layout) } & crate::class::CLASS_OUTSIDE_CELLS,
        0,
        "a static block's layout may not own cells outside the block"
    );
    if CLOSED.with(Cell::get) {
        return;
    }

    BLOCKS.with(|cell| {
        let registry = unsafe { &mut *cell.get() };
        let end = registry.len + ENTRY_SIZE;
        if buffer_ensure_longlived(registry, end, 0).is_null() {
            return;
        }

        // Aligned: a chunk is 8-aligned on every path the arena has, and an
        // entry is two words.
        unsafe {
            registry
                .data
                .add(registry.len)
                .cast::<Registered>()
                .write(Registered { block, layout })
        };
        registry.len = end;
    });
}

/// Release every registered static block's roots, in reverse
/// registration order. Called from `ll_thread_exit` before the thread's
/// blocks go home, because the drops below run `__destruct` bodies that
/// allocate.
///
/// Idempotent, like the rest of that path: a second call finds the
/// registry empty.
pub(crate) fn run_thread_exit_teardown() {
    // Pop one at a time rather than draining the registry: a `__destruct`
    // reached below may register a block of its own, and that block is
    // then the newest — it must be torn down before the older ones this
    // loop has not reached yet, which popping gives for free. Nothing read
    // off the registry survives the access that pops: a registration
    // inside `tear_down` may move the chunk.
    loop {
        let next = BLOCKS.with(|cell| {
            let registry = unsafe { &mut *cell.get() };
            if registry.len == 0 {
                // Drained: give the chunk back, so a later registration
                // grows a fresh one and a second call to this function
                // finds nothing. An empty registry holds no chunk to give.
                unsafe { buffer_release_longlived(registry) };
                return None;
            }

            registry.len -= ENTRY_SIZE;
            Some(unsafe { registry.data.add(registry.len).cast::<Registered>().read() })
        });

        let Some(Registered { block, layout }) = next else {
            return;
        };
        unsafe { tear_down(block, layout) };
    }
}

/// Refuse every registration this thread makes for the rest of this life.
/// Called by `ll_thread_exit` once its pass has drained the registry: the
/// exit's collection runs destructors after that, and a block one of them
/// registers would be popped by no pass — its chunk would keep the buffer
/// block it sits in out of the pool for the life of the process, where a
/// refused block leaks its roots alone.
pub(crate) fn close_the_registry() {
    CLOSED.with(|closed| closed.set(true));
}

/// Take registrations again: the next life of a pool thread, which runs
/// `ll_thread_init` and `ll_thread_exit` per task, registers its blocks
/// afresh and its own exit tears them down. Called by `ll_thread_init`
/// where it lowers the exit phase, and under the same two conditions — an
/// init inside an exit is that exit repairing itself, and a registration
/// there would be the one the close refuses.
pub(crate) fn reopen_the_registry() {
    CLOSED.with(|closed| closed.set(false));
}

/// The registry's `(len, capacity)` in bytes, for a test reading whether a
/// registration took room and whether the pass gave it back.
#[cfg(test)]
pub(crate) fn registry_holds() -> (usize, usize) {
    BLOCKS.with(|cell| {
        let registry = unsafe { &*cell.get() };
        (registry.len, registry.capacity)
    })
}

/// One block: empty each counted slot and drop its former occupant, slot
/// by slot, as `rfc/model/classes.md`, "Teardown at thread exit" writes the
/// pass ("runs `drop` on each"). The body's cells alone: a static block's
/// layout carries no cells outside the block, which `ll_static_block_register`
/// checks.
///
/// The slot is emptied before its occupant is dropped, so a `__destruct` the
/// drop runs — which can reach this very block — reads null there rather
/// than a reference to an entity being torn down; what such a destructor
/// sees of the other slots, and what a store it makes into one of them
/// costs, is the design's sentence at that heading. Nothing is held between
/// slots: thread exit runs outside a collection, and a list of the
/// displaced children would be a container on a runtime path
/// (`dev/DECISIONS.md`, "the reset window's memory comes from the manager,
/// and an allocation it cannot get is a refusal").
unsafe fn tear_down(block: *mut u8, layout: *const Class) {
    unsafe {
        let _ = crate::object::for_each_body_cell::<crate::cells::PlainCells>(
            block,
            layout,
            &mut |cell| {
                crate::cells::empty_cell(cell);
                crate::memory::barrier::drop_ref(MemoryCategory::LongLived, cell.child);
            },
        );
    };
}

#[cfg(test)]
mod tests;
