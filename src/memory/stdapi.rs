//! Standard allocator API: a `GlobalAlloc` implementation and C
//! `malloc`/`free`/`calloc`/`realloc`/`aligned_alloc` exports over the
//! Limelight memory manager, so the allocator can run the real C benchmark
//! suites unchanged and be reused outside the runtime.
//!
//! **Not yet usable as a Rust `#[global_allocator]`**, despite the impl at
//! the bottom of this file: regions come from `std::alloc::alloc` with
//! block alignment (`block_pool::carve_region`), so installing this
//! globally makes region carving re-enter `ll_alloc` with an alignment it
//! refuses, and every allocation reports null. It becomes true once
//! regions come from `VirtualAlloc` or `mmap` directly.
//!
//! **Size-less free works**, and the size split it routes on is
//! `docs/memory-manager.md`, "Layers". The small path additionally needs
//! an alignment of at most 16, which that section does not state. The
//! three rows are `BLOCK_KIND_HEAP`, `BLOCK_KIND_LARGE` and
//! `BLOCK_KIND_LARGE_RUN`, and a `LARGE_RUN` is handed back to the OS on
//! free: what that section says is never returned in phase 1 is the
//! region beneath it. One row is this module's own:
//!
//! - `BLOCK_KIND_ENTITY` (GC entities, allocated via `heap::entity_alloc`
//!   and never by `ll_alloc`) → the thread's entity heap; `ll_free` covers
//!   them so object teardown stays size-less.
//!
//! The `+256` payload offset is 256-aligned, so any alignment up to 256 is
//! satisfied for free, which covers malloc's 16-byte guarantee and
//! `aligned_alloc`.
//!
//! **Thread-safety:** all paths are multi-threaded, and what a
//! cross-thread free and a thread exit cost is `docs/memory-manager.md`,
//! "Cross-thread free" and "Thread exit: abandonment and adoption".

use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::AtomicU32;

use crate::memory::block_pool::{
    BLOCK_KIND_ENTITY, BLOCK_KIND_HEAP, BLOCK_KIND_LARGE, BLOCK_KIND_LARGE_RUN, BLOCK_MASK,
    BLOCK_PAYLOAD, BLOCK_SIZE, BlockHeader, BlockPool, LINE_SIZE, load_block_kind,
};
use crate::memory::heap::MAX_SMALL;

/// Largest alignment the `+256` payload guarantees.
const MAX_ALIGN: usize = LINE_SIZE;

/// Header for large (non-heap) allocations, written into the block's
/// first line. Shares offset 0 (`kind`) with the pool `BlockHeader`.
#[repr(C)]
struct LargeHeader {
    kind: AtomicU32,
    _pad: u32,
    /// Requested size (for `realloc` copy length).
    size: usize,
    /// Total OS allocation in bytes for `LARGE_RUN`; 0 for pooled LARGE.
    run_bytes: usize,
}

#[inline]
fn block_of(ptr: *mut u8) -> *mut u8 {
    ((ptr as usize) & !BLOCK_MASK) as *mut u8
}

/// Round `n` up to a whole number of blocks, or `None` if that would
/// overflow `usize` (a near-`usize::MAX` request must fail cleanly, not
/// wrap down to a tiny run and under-allocate).
#[inline]
fn round_up_blocks(n: usize) -> Option<usize> {
    n.checked_add(BLOCK_SIZE - 1).map(|x| x & !(BLOCK_SIZE - 1))
}

/// Allocate `size` bytes with alignment `align`. Returns null on
/// unsupported alignment (> 256) or OS failure.
///
/// The small path is the whole body here; everything else is a `#[cold]`
/// tail. Keeping the large-object branches in this function gave it a
/// stack frame (`push rsi; push rdi; sub rsp, 56`) that every small
/// `malloc` set up and tore down on its way to a tail call — see
/// [`Heap::alloc`](crate::memory::heap::Heap::alloc)'s doc for the full
/// reasoning.
///
/// # Safety
/// Standard allocator contract.
#[inline]
pub unsafe fn ll_alloc(size: usize, align: usize) -> *mut u8 {
    // Small path: the thread-local heap. Its slots are ≥16-aligned.
    //
    // No `size_class_index(size).is_some()` here: it returns `None` exactly
    // when `size > MAX_SMALL`, which `size <= MAX_SMALL` already decides.
    // Asking twice cost a second CLASS_LUT lookup on every malloc.
    if size <= MAX_SMALL && align <= 16 {
        // Self-initialising, via a cold branch on a null heap pointer.
        //
        // `ll_thread_init` remains the documented contract and is still the
        // right thing for an embedder to call; this is what makes skipping it
        // merely slower-once rather than undefined. It also makes the C ABI
        // callable exactly the way mimalloc's is, which matters for measuring
        // honestly: `mi_malloc` does this identical check inline
        // (`test rcx,rcx; je _mi_malloc_generic`) and self-initialises. Making
        // callers wrap us in an init check instead does not remove the cost —
        // it moves it into their wrapper and out of our number. That is not a
        // saving, it is a rigged comparison, and it was one: mimalloc-bench's
        // shim reaches mimalloc with `#define CUSTOM_MALLOC mi_malloc` while
        // ours went through a wrapper testing a `thread_local` on every malloc
        // *and* every free.
        let h = crate::memory::heap::thread_heap();
        if h.is_null() {
            return unsafe { ll_alloc_init(size) };
        }

        return unsafe { (*h).alloc(size) };
    }

    unsafe { ll_alloc_large(size, align) }
}

/// Cold tail: first allocation on this thread — build its heap, then retry.
///
/// # Safety
/// Standard allocator contract.
#[cold]
#[inline(never)]
unsafe fn ll_alloc_init(size: usize) -> *mut u8 {
    crate::memory::heap::ll_thread_init();
    // Building the heap can itself be refused, and then there is no heap
    // to allocate from — report it the same way as any other exhaustion.
    let h = crate::memory::heap::thread_heap();
    if h.is_null() {
        return std::ptr::null_mut();
    }

    unsafe { (*h).alloc(size) }
}

/// # Safety
/// Standard allocator contract.
#[cold]
#[inline(never)]
unsafe fn ll_alloc_large(size: usize, align: usize) -> *mut u8 {
    if align > MAX_ALIGN {
        return std::ptr::null_mut();
    }

    // A small size reaches here only via `align > 16`, which the heap
    // cannot honor (its slots are 16-aligned). Route every `align > 16`
    // request — small or large — through the pooled block path below, whose
    // payload sits at `+256` (256-aligned), so any alignment up to
    // `MAX_ALIGN` is satisfied. This also avoids touching the thread heap,
    // which may be null on a thread that never called `ll_thread_init`.

    if size <= BLOCK_PAYLOAD {
        // One pooled block holds the object; payload at +256.
        let block = BlockPool::global().get() as *mut LargeHeader;
        if block.is_null() {
            // The pool reports exhaustion rather than aborting, so this
            // path has to carry the report the rest of the way: writing
            // the header first would dereference null, which is how the
            // old abort came back as UB.
            return std::ptr::null_mut();
        }

        unsafe {
            // Field by field, and `kind` last through `store_block_kind`
            // (a release store): a collector enumerating blocks loads the
            // kind of every block in every carved region, and a pooled
            // block is in one. A struct store would cover that word with
            // a plain write, which is a data race by the model even
            // writing the value already there — the defect
            // `large_entity::commission` and `Heap::refill` were both
            // rewritten to avoid.
            (&raw mut (*block)._pad).write(0);
            (&raw mut (*block).size).write(size);
            (&raw mut (*block).run_bytes).write(0);
            crate::memory::block_pool::store_block_kind(&raw const (*block).kind, BLOCK_KIND_LARGE);
            (block as *mut u8).add(LINE_SIZE)
        }
    } else {
        // Huge: OS-direct, block-aligned so the mask still finds us.
        // `size` is caller-controlled ABI input; guard the header add and
        // the block round-up against overflow (either would wrap to a tiny
        // run and hand back a memory-unsafe under-allocation).
        let run_bytes = match size.checked_add(LINE_SIZE).and_then(round_up_blocks) {
            Some(n) => n,
            None => return std::ptr::null_mut(),
        };

        // `Layout` refuses a size past `isize::MAX`, which the checked
        // arithmetic above lets through — it only catches a wrap near
        // `usize::MAX`. Between the two lies the whole top half of the
        // range, and a caller that lost a sign lands in it. Unwrapping
        // here would panic across `extern "C"` and abort, where this
        // module's contract is to report null (`immortal_alloc_run` takes
        // the same call the same way).
        let layout = match Layout::from_size_align(run_bytes, BLOCK_SIZE) {
            Ok(layout) => layout,
            Err(_) => return std::ptr::null_mut(),
        };

        let block = unsafe { std::alloc::alloc(layout) } as *mut LargeHeader;
        if block.is_null() {
            return std::ptr::null_mut();
        }

        unsafe {
            // A run lies outside every region, so nothing reads its
            // kind across threads; the struct store is kept off the word
            // all the same, so that one rule covers both arms here.
            (&raw mut (*block)._pad).write(0);
            (&raw mut (*block).size).write(size);
            (&raw mut (*block).run_bytes).write(run_bytes);
            crate::memory::block_pool::store_block_kind(
                &raw const (*block).kind,
                BLOCK_KIND_LARGE_RUN,
            );
            (block as *mut u8).add(LINE_SIZE)
        }
    }
}

/// Free a pointer from [`ll_alloc`]. Dispatches on the owning block's
/// kind — no size needed.
///
/// Split fast/cold like [`ll_alloc`]: the heap path is the body, the
/// large and huge kinds are a `#[cold]` tail.
///
/// # Safety
/// `ptr` must be a live allocation from [`ll_alloc`] on this thread (for
/// small objects) and not already freed.
#[inline]
pub unsafe fn ll_free(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }

    let block = block_of(ptr);
    let kind = unsafe { load_block_kind(block as *const AtomicU32) };

    // An entity slot reaches the free list carrying the final
    // refcount-0 header, because that word is the occupancy test both
    // process-global enumerators apply (`heap::for_each_entity_slot`,
    // `heap::snapshot_entity_blocks`). A slot freed while its header
    // still reads a live count is enumerated as a live entity by every
    // later walk in the process, and for an object it is worse than an
    // over-count: the free-list link lands at bytes 8-15, where the
    // class pointer was, so a walk that believes the slot follows a
    // free-list link as a `*const Class`.
    //
    // Test-only, and it earned its place: killing an entity at
    // refcount 1 is a mistake with no local symptom, and the one it
    // does have surfaced as a census flake in an unrelated test on
    // another thread half an hour later (`dev/POSTMORTEM.md`, "an entity
    // killed at refcount 1").
    #[cfg(test)]
    if kind == BLOCK_KIND_ENTITY || crate::memory::large_entity::is_large_entity(kind) {
        let header = unsafe { *(ptr as *const u64) };
        assert_eq!(
            header & 0xffff_ffff,
            0,
            "entity freed with a live-looking header {header:#018x} at {:#x}",
            ptr as usize
        );
        // The same shape one field up: the cycle collector's candidate
        // buffer holds raw pointers, so a slot that reaches the free
        // list still claiming a place in it leaves a root aimed at
        // memory about to be handed out again. Every teardown door has
        // to clear it; this is where a door that forgot to says so.
    }

    // A reset in flight on this thread reads one header word of every
    // survivor it holds after its fixpoint, and one of every child their
    // slots still name, so a body whose free would return memory to the
    // system waits for it whether or not this reset ever promoted it.
    // Ahead of the collection arm below on purpose: parked for a
    // collection instead, the same body could be freed by that
    // collection's own flush while the reset still runs
    // (`memory::reset_window`).
    if crate::memory::large_entity::is_large_entity(kind)
        && unsafe { crate::memory::reset_window::park_large(ptr) }
    {
        return;
    }

    // A corpse of the reset in flight, in a block that has no occupant
    // index yet: the free is absorbed rather than deferred, `register`
    // having already declined to count it (`memory::reset_window`).
    if kind == crate::memory::block_pool::BLOCK_KIND_RETAINED
        && crate::memory::reset_window::absorbs_retained_free(block as usize)
    {
        return;
    }

    // An epoch-wide parking of every free that can put memory back in
    // circulation stood here while `rc-walk` ran, and went with it.
    // `rc-cycle` parks per slot rather than per epoch, on two windows
    // that are not the same width — a queue entry naming the slot, and a
    // collection in flight — and neither exists yet: S34.3 and S36.2
    // build them (`rfc/model/gc/rc-cycle.md`, "Death while enrolled").

    if kind == BLOCK_KIND_HEAP {
        let h = crate::memory::heap::thread_heap();
        if h.is_null() {
            // No heap on this thread means we cannot be the block's owner, so
            // this is by definition a cross-thread free. Post it and go — no
            // reason to build a heap for a thread that has never allocated.
            return unsafe { crate::memory::heap::free_foreign(ptr) };
        }

        return unsafe { (*h).free(ptr) };
    }

    // The entity population: same slot mechanics, its own heap instance.
    // This is object teardown's path (`ll_object_die` → here), not the C
    // `free` hot path, so the second compare costs nothing that matters.
    if kind == BLOCK_KIND_ENTITY {
        let h = crate::memory::heap::thread_entity_heap();
        if h.is_null() {
            return unsafe { crate::memory::heap::free_foreign(ptr) };
        }

        return unsafe { (*h).free(ptr) };
    }

    unsafe { ll_free_large(block, kind) };
}

/// # Safety
/// `block` must be the block header of a live non-heap allocation.
#[cold]
#[inline(never)]
unsafe fn ll_free_large(block: *mut u8, kind: u32) {
    match kind {
        BLOCK_KIND_LARGE => BlockPool::global().put(block as *mut BlockHeader),
        // One entity in a block of its own, which is the same physical
        // shape as the two kinds around it and a different population:
        // its own module owns the registry a run is enumerated from, and
        // the order in which that entry goes.
        crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE
        | crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE_RUN => unsafe {
            crate::memory::large_entity::free(block, kind)
        },
        BLOCK_KIND_LARGE_RUN => {
            let hdr = block as *mut LargeHeader;
            let run_bytes = unsafe { (*hdr).run_bytes };
            let layout = Layout::from_size_align(run_bytes, BLOCK_SIZE).unwrap();
            unsafe { std::alloc::dealloc(block, layout) };
        }
        crate::memory::block_pool::BLOCK_KIND_BUFFER => {
            // A buffer-arena chunk carries no metadata, so its size lives
            // with its owner and only `buffer_free_longlived_payload` has
            // it. Arriving here means a caller lost the capacity, and the
            // silent default arm below made that a leak nobody saw.
            debug_assert!(
                false,
                "a buffer-arena chunk frees through buffer_free_longlived_payload, which carries its size"
            );
        }
        crate::memory::block_pool::BLOCK_KIND_RETAINED => {
            // A promoted survivor died. The block it was promoted in is
            // former arena memory with no free list and no stride, so
            // nothing is recycled inside it; what the death changes is
            // the block's live-occupant count, and at zero the whole
            // block goes home. The registry drops the index before
            // saying so, because both enumerators dereference a
            // registered address without testing that its block still
            // exists (`retained.rs`, the readable-address contract).
            if crate::memory::retained::occupant_freed(block as usize) {
                unsafe { crate::memory::retained::give_block_back(block as usize) };
            }
        }

        // The two kinds that recycle nothing on a free: arena memory goes
        // at the reset and immortal memory never goes. Object teardown
        // funnels every category through here, so both arrive routinely.
        crate::memory::block_pool::BLOCK_KIND_ARENA
        | crate::memory::block_pool::BLOCK_KIND_IMMORTAL => {}
        // A free of a block already back in the pool: a double free, or a
        // pointer this allocator never handed out. Tolerated in every
        // build, because a C caller's mistake must not end the process,
        // and it is the only kind that reaches here from live code.
        crate::memory::block_pool::BLOCK_KIND_FREE => {}
        _ => {
            // Every other kind whose memory can go back into circulation
            // owes an arm above, and a missing one is a leak nothing
            // reports. Release still ignores it: the populations here are
            // reachable from the C ABI.
            //
            // Reading the arm list, `BLOCK_KIND_BUFFER` looks absent and
            // is not: it has an arm of its own above, asserting, because
            // a buffer-arena chunk carries no size and only
            // `buffer_free_longlived_payload` has one. No chunk reaches
            // this function at all — `body_free` routes the long-lived
            // categories there by category, never by kind.
            debug_assert!(false, "no free path for block kind {kind}");
        }
    }
}

/// The requested size of a live allocation, recovered from its block.
///
/// # Safety
/// `ptr` must be a live allocation from [`ll_alloc`].
unsafe fn ll_usable_size(ptr: *mut u8) -> usize {
    let block = block_of(ptr);
    let kind = unsafe { load_block_kind(block as *const AtomicU32) };
    match kind {
        BLOCK_KIND_HEAP | BLOCK_KIND_ENTITY => {
            // Heap slot: the class size (upper bound on the request).
            //
            // The word after `kind` is `HeapBlockHeader::size_class`, and
            // that adjacency is the whole of this read — the two are
            // neighbours because both are loaded from another thread, so
            // both had to leave the owner's private half
            // (`heap::HeapBlockHeader`). Moving either apart silently
            // makes this a size class of something else.
            use crate::memory::heap::SIZE_CLASSES;
            let ci = unsafe { load_block_kind((block as *const AtomicU32).add(1)) } as usize;
            SIZE_CLASSES[ci]
        }
        BLOCK_KIND_LARGE | BLOCK_KIND_LARGE_RUN => unsafe { (*(block as *const LargeHeader)).size },
        _ => 0,
    }
}

/// Reallocate: grow/shrink `ptr` to `new_size`. Allocates, copies
/// `min(old, new)`, frees the old.
///
/// # Safety
/// `ptr` must be a live allocation from [`ll_alloc`] or null.
pub unsafe fn ll_realloc(ptr: *mut u8, new_size: usize, align: usize) -> *mut u8 {
    if ptr.is_null() {
        return unsafe { ll_alloc(new_size, align) };
    }

    // An entity is never reallocated: it is a counted object whose
    // address other entities hold, and moving it would leave every one of
    // them pointing at freed memory. Copying it would be worse than
    // refusing — the copy lands under a raw-buffer kind, invisible to the
    // walk, while the original is freed with its children still counted
    // against it — so this refuses, and the two entity populations are
    // the only kinds that reach it.
    let kind = unsafe { load_block_kind(block_of(ptr) as *const AtomicU32) };
    if kind == BLOCK_KIND_ENTITY || crate::memory::large_entity::is_large_entity(kind) {
        debug_assert!(false, "an entity reached realloc");
        return std::ptr::null_mut();
    }

    let old_size = unsafe { ll_usable_size(ptr) };
    let new_ptr = unsafe { ll_alloc(new_size, align) };
    if new_ptr.is_null() {
        return std::ptr::null_mut();
    }

    unsafe {
        std::ptr::copy_nonoverlapping(ptr, new_ptr, old_size.min(new_size));
        ll_free(ptr);
    }

    new_ptr
}

// --- C ABI exports -------------------------------------------------------

/// # Safety
/// Standard `malloc` contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_malloc(size: usize) -> *mut u8 {
    unsafe { ll_alloc(size, 16) }
}

/// # Safety
/// Standard `free` contract; see the module thread note.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_c_free(ptr: *mut u8) {
    unsafe { ll_free(ptr) }
}

/// # Safety
/// Standard `calloc` contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_calloc(count: usize, size: usize) -> *mut u8 {
    let total = match count.checked_mul(size) {
        Some(t) => t,
        None => return std::ptr::null_mut(),
    };

    let p = unsafe { ll_alloc(total, 16) };
    if !p.is_null() {
        unsafe { std::ptr::write_bytes(p, 0, total) };
    }

    p
}

/// # Safety
/// Standard `realloc` contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_c_realloc(ptr: *mut u8, size: usize) -> *mut u8 {
    unsafe { ll_realloc(ptr, size, 16) }
}

/// # Safety
/// Standard `aligned_alloc` contract (align power of two ≤ 256).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_aligned_alloc(align: usize, size: usize) -> *mut u8 {
    unsafe { ll_alloc(size, align) }
}

// --- Rust GlobalAlloc -----------------------------------------------------

/// A `GlobalAlloc` over the Limelight memory manager. Install with
/// `#[global_allocator] static A: LimelightAlloc = LimelightAlloc;`.
/// Multi-threaded; the one limit is thread-exit abandonment (see the
/// module doc).
pub struct LimelightAlloc;

unsafe impl GlobalAlloc for LimelightAlloc {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { ll_alloc(layout.size(), layout.align()) }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        unsafe { ll_free(ptr) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe { ll_realloc(ptr, new_size, layout.align()) }
    }
}

#[cfg(test)]
mod tests;
