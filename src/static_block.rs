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
//! Each thread appends a block to a thread-local list the first time it
//! initializes that block, beside the initializer that already runs
//! there. At exit the list is walked in **reverse** initialization
//! order, and per block every counted slot is nulled and its former
//! occupant dropped through the barrier's `drop_ref` with `owner_cat =
//! LongLived`. Nothing here branches on what the slot held, because
//! `drop_ref` already decides.
//!
//! This module holds a base address and the descriptor that says where
//! the reference slots are; how a block is allocated and what its slots
//! mean are not its business — the release policy is the barrier's and
//! the teardown is `object`'s.
//!
//! Why the order is LIFO, why the drops go through `drop_ref`, and why
//! the process's last thread runs the pass in full like every other:
//! `rfc/model/classes.md`, "Teardown at thread exit", and
//! `dev/DECISIONS.md`, 2026-08-03.

use crate::class::Class;
use crate::refcount::{MemoryCategory, RcHeader};

type Registered = Vec<(*mut u8, *const Class)>;

thread_local! {
    /// Registered blocks in initialization order; torn down in reverse.
    ///
    /// A raw pointer in a `Cell`, not a `RefCell<Vec<..>>`: a `Vec` has
    /// drop glue, so its `thread_local` is registered for TLS
    /// destruction and can be gone before
    /// [`run_thread_exit_teardown`], which is itself reached **from** a
    /// TLS destructor — the heap guard's. No `thread_local!` this path
    /// can reach may have drop glue (`dev/DECISIONS.md`, 2026-08-03).
    ///
    /// The `Vec` behind it is owned here: allocated on first
    /// registration, freed when the pass drains it. A destructor
    /// running mid-pass may register another block, which is why the
    /// pass pops one at a time and frees only once the list is empty.
    static BLOCKS: std::cell::Cell<*mut Registered> =
        const { std::cell::Cell::new(std::ptr::null_mut()) };
}

/// Register `block`, laid out by `layout`, for teardown when this
/// thread exits.
///
/// Called once per class-block per thread, by the static initializer.
/// Registering the same block twice on one thread is a caller error;
/// it would tear the block down twice, and the second pass finds every
/// slot already null, so it costs a walk and releases nothing.
///
/// **Refuses rather than aborts** when the list cannot grow, like every
/// other growth point in this crate (`gc::buffer_candidate`'s
/// `try_reserve`, `ll_thread_init`'s hand-rolled allocation). The cost
/// of a refusal is named and bounded: that block's roots are not
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
    BLOCKS.with(|cell| {
        let mut list = cell.get();
        if list.is_null() {
            // Not `Box::new`: its failure mode is `handle_alloc_error`,
            // which aborts — the same reasoning as `ll_thread_init`'s
            // hand-rolled allocation, and the same refusal instead.
            let layout = std::alloc::Layout::new::<Registered>();
            let fresh = unsafe { std::alloc::alloc(layout) } as *mut Registered;
            if fresh.is_null() {
                return;
            }

            unsafe { fresh.write(Registered::new()) };
            list = fresh;
            cell.set(list);
        }

        let list = unsafe { &mut *list };
        if list.len() == list.capacity() && list.try_reserve(1).is_err() {
            return;
        }

        list.push((block, layout));
    });
}

/// Release every registered static block's roots, in reverse
/// registration order. Called from `ll_thread_exit` before the thread's
/// blocks go home, because the drops below run `__destruct` bodies that
/// allocate.
///
/// Idempotent, like the rest of that path: a second call finds the list
/// empty.
pub(crate) fn run_thread_exit_teardown() {
    // Pop one at a time rather than draining the vector: a `__destruct`
    // reached below may register a block of its own, and that block is
    // then the newest — it must be torn down before the older ones this
    // loop has not reached yet, which popping gives for free. Holding a
    // drain across user code would alias the list instead.
    loop {
        let next = BLOCKS.with(|cell| {
            let list = cell.get();
            if list.is_null() {
                return None;
            }

            match unsafe { (*list).pop() } {
                Some(entry) => Some(entry),
                None => {
                    // Drained: give the list back and leave the slot
                    // null, so a later registration starts a fresh one
                    // and a second call to this function finds nothing.
                    cell.set(std::ptr::null_mut());
                    unsafe { free_list(list) };
                    None
                }
            }
        });

        let Some((block, layout)) = next else { return };
        unsafe { tear_down(block, layout) };
    }
}

/// Give a registry list back, matching `register`'s hand-rolled
/// allocation: drop the `Vec` in place, then release the box.
unsafe fn free_list(list: *mut Registered) {
    unsafe {
        std::ptr::drop_in_place(list);
        std::alloc::dealloc(list as *mut u8, std::alloc::Layout::new::<Registered>());
    }
}

/// One block: sever its counted slots, then drop what came out.
///
/// Severing first and dropping after is the same discipline the drain
/// uses (`object::sever_counted_slots`): a drop runs user code that can
/// reach this very block, and it must find the slot already null rather
/// than a reference it could read a second time.
unsafe fn tear_down(block: *mut u8, layout: *const Class) {
    let mut displaced: Vec<*mut RcHeader> = Vec::new();
    unsafe { crate::object::sever_counted_slots(block, &*layout, &mut displaced) };
    for child in displaced {
        unsafe { crate::memory::barrier::drop_ref(MemoryCategory::LongLived, child) };
    }
}

#[cfg(test)]
mod tests;
