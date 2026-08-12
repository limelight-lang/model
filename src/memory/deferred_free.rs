//! Deferred physical release: the GC activity bit of
//! `rfc/model/gc/heap-design.md`, demanded by the rc-walk collector
//! (`rfc/model/gc/rc-walk.md`, "Deferred physical release, and when an
//! epoch ends"). While a collection epoch is in flight, memory released
//! by ordinary refcount death is parked rather than recycled: the entity
//! dies on time, `__destruct` included, and only reuse waits.
//!
//! The queue's job is **identity**, which makes it soundness rather than
//! comfort: an id must name one entity from walk to drain, or the Phase 4
//! exact test can balance by coincidence on an object that was never
//! judged.
//!
//! Which kinds park, which do not, and why parking is out of band are in
//! `docs/memory-manager.md`, "Deferred free (rc-walk builds)" — the
//! document `memory/mod.rs` declares this module implements.
//!
//! Two obligations on the epoch protocol: the collector publishes the flag
//! **through a handshake before snapshotting**, since a mutator that has
//! not yet observed it may still recycle a slot the snapshot is about to
//! include; and the flush runs **on the owning thread** after the epoch
//! closes, the parked list being thread-local and the underlying frees
//! owner-bound.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

/// The GC activity bit: an epoch is in flight, park instead of freeing.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// A parked release: the pointer, and the free it owes.
#[derive(Clone, Copy)]
struct Parked {
    ptr: *mut u8,
    free: DeferredFree,
}

/// Which free a parked record replays, named rather than inferred from a
/// size: the three differ in what they hand back, and only one of them
/// needs a size at all.
#[derive(Clone, Copy)]
enum DeferredFree {
    /// Anything that reaches `ll_free`, which reads what it needs from
    /// the block header: an entity slot, a heap buffer, a pooled large
    /// block, an OS-direct run.
    Allocation,
    /// A buffer-arena chunk, with the granted capacity its free takes as
    /// an argument — a chunk carries no metadata of its own
    /// (`buffer_arena.rs`, the zero-metadata contract).
    Chunk { capacity: usize },
    /// A payload lying in a retained block. Nothing about the bytes
    /// waits, former arena memory having no free list; what waits is the
    /// block they pin ([`park_retained_payload`]).
    RetainedPayload,
}

thread_local! {
    /// This thread's parked allocations, in park order. Out-of-band:
    /// the parked memory itself is never touched (module doc).
    ///
    /// A raw pointer in a `Cell`, not a `RefCell<Vec<_>>`, and the
    /// reason is soundness rather than speed: a `Vec` has drop glue, so
    /// its key is registered for TLS destruction, and this list is
    /// reached **from** a TLS destructor — thread exit runs the
    /// static-block teardown (`static_block.rs`), whose deaths reach
    /// `flush_due` through the epoch checkpoint. TLS destructor order
    /// is unspecified and on glibc is reverse registration order, which
    /// puts the exit guard last precisely because it registers first,
    /// so this list is reliably already gone. `with` would then panic
    /// with `AccessError` inside a destructor, and a panic there cannot
    /// unwind: the process aborts. A `Cell<*mut _>` has no drop glue,
    /// is never registered, and stays readable for the whole life of
    /// the thread; [`dispose`] frees it explicitly.
    static PARKED: Cell<*mut Vec<Parked>> = const { Cell::new(std::ptr::null_mut()) };
}

/// This thread's list, allocated on first use.
fn parked_list() -> *mut Vec<Parked> {
    PARKED.with(|cell| {
        let mut list = cell.get();
        if list.is_null() {
            list = Box::into_raw(Box::new(Vec::new()));
            cell.set(list);
        }

        list
    })
}

/// The free-path test: one relaxed load, a predicted branch.
#[inline]
pub(crate) fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Open the deferral window. Caller (the epoch protocol) owes the
/// publication handshake before it snapshots — see the module doc.
pub(crate) fn begin_epoch() {
    let was = ACTIVE.swap(true, Ordering::Relaxed);
    debug_assert!(!was, "epochs do not nest (one verdict in flight, ever)");
}

/// Close the deferral window. Frees after this recycle immediately;
/// the parked backlog waits for [`flush`] on each owning thread.
pub(crate) fn end_epoch() {
    let was = ACTIVE.swap(false, Ordering::Relaxed);
    debug_assert!(was, "end_epoch without begin_epoch");
}

/// Park a freed allocation instead of recycling it. Writes nothing
/// into the allocation — a walker may still be reading it.
///
/// # Safety
/// `ptr` must be a just-freed allocation of a freeable block kind,
/// owned by this call (nothing may recycle it until [`flush`]
/// releases it for real).
pub(crate) unsafe fn park(ptr: *mut u8) {
    unsafe {
        (*parked_list()).push(Parked {
            ptr,
            free: DeferredFree::Allocation,
        })
    };
}

/// Park the free of a payload lying in a retained block. Nothing about
/// the bytes waits — former arena memory has no free list — but the block
/// they pin may become empty, and handing a block to the pool while a
/// walker still holds addresses inside it is what this queue exists to
/// prevent.
///
/// # Safety
/// `ptr` must be a live payload inside a retained block, freed by this
/// call and reachable by nothing until [`flush`] replays it.
pub(crate) unsafe fn park_retained_payload(ptr: *mut u8) {
    unsafe {
        (*parked_list()).push(Parked {
            ptr,
            free: DeferredFree::RetainedPayload,
        })
    };
}

/// Park a buffer-arena chunk, which does not pass `ll_free` and whose
/// free needs the granted capacity back. The whole call is parked, not
/// just the free-list link it would write: `BufferArena::free` also
/// decrements the block's live count, and an emptied block returns to
/// the global pool and can be re-stamped as another kind while the
/// walker still holds addresses inside it.
///
/// # Safety
/// `(ptr, capacity)` must be exactly one live chunk of this thread's
/// buffer arena, freed by this call and reachable by nothing until
/// [`flush`] releases it.
pub(crate) unsafe fn park_buffer_chunk(ptr: *mut u8, capacity: usize) {
    debug_assert!(capacity > 0, "a chunk's free is size-carrying");
    unsafe {
        (*parked_list()).push(Parked {
            ptr,
            free: DeferredFree::Chunk { capacity },
        })
    };
}

/// Release one parked record through the free path it names.
///
/// # Safety
/// Runs on the parking thread, with no epoch in flight.
unsafe fn release(p: Parked) {
    match p.free {
        DeferredFree::Allocation => unsafe { crate::memory::stdapi::ll_free(p.ptr) },
        DeferredFree::Chunk { capacity } => unsafe {
            crate::memory::buffer_arena::free_parked_chunk(p.ptr, capacity)
        },
        DeferredFree::RetainedPayload => {
            let block = crate::memory::block_pool::BlockHeader::of_ptr(p.ptr) as usize;
            if crate::memory::retained::payload_freed(block) {
                unsafe { crate::memory::retained::give_block_back(block) };
            }
        }
    }
}

/// Release this thread's parked backlog through the real free path.
/// Returns how many allocations were flushed, and **zero without
/// touching the backlog if an epoch is in flight**: flushing mid-epoch
/// would recycle the very slots the queue exists to pin, and the caller
/// cannot rule that out on its own (below). Frees in reverse park order,
/// so the free lists come
/// out as the intrusive-list draft left them (LIFO — tests and cache
/// behaviour rely on last-freed-first-reused).
///
/// # Safety
/// Must run on the thread that parked the allocations (the list and
/// the underlying frees are both thread-bound).
pub(crate) unsafe fn flush() -> usize {
    // Not an assertion, because the caller's [`flush_due`] cannot hold:
    // the bit is global, the collector raises it from its own thread, and
    // it flips between that read and this one often enough to be measured
    // (the regression test below). An epoch that opened in that window has
    // not been acked by this thread yet — `Epoch::open` raises the bit
    // before requesting the handshake, and the snapshot waits for the ack
    // — so nothing has read the slots yet and skipping is free: the
    // backlog goes at the next checkpoint. Recycling anyway would be the
    // one thing this queue exists to prevent.
    if active() {
        return 0;
    }

    let list = PARKED.with(|cell| cell.get());
    if list.is_null() {
        return 0;
    }

    let backlog = unsafe { std::mem::take(&mut *list) };
    for &parked in backlog.iter().rev() {
        unsafe { release(parked) };
    }

    backlog.len()
}

/// A closed epoch left parked memory on this thread: the checkpoint's
/// flush trigger. Two cheap reads (one global, one thread-local).
#[inline]
pub(crate) fn flush_due() -> bool {
    if active() {
        return false;
    }

    let list = PARKED.with(|cell| cell.get());
    !list.is_null() && unsafe { !(*list).is_empty() }
}

/// Give this thread's list back at thread exit, after the last flush.
///
/// Called from `ll_thread_exit` rather than from a TLS destructor,
/// which is the whole point (see the `PARKED` doc). Flushes what is
/// still parked when no epoch is in flight; when one is, the backlog
/// leaks, which is the known limit the module doc already declares —
/// freeing mid-epoch would recycle the very slots the queue pins.
///
/// Null-tolerant and idempotent: a thread that never parked, and a
/// second call, both find nothing.
pub(crate) fn dispose() {
    let list = PARKED.with(|cell| cell.replace(std::ptr::null_mut()));
    if list.is_null() {
        return;
    }

    let backlog = unsafe { Box::from_raw(list) };
    if !active() {
        for &parked in backlog.iter().rev() {
            unsafe { release(parked) };
        }
    }
}

/// This thread's parked backlog size (stats, tests).
#[cfg(test)]
pub(crate) fn parked_count() -> usize {
    let list = PARKED.with(|cell| cell.get());
    if list.is_null() {
        0
    } else {
        unsafe { (*list).len() }
    }
}

#[cfg(test)]
mod tests;
