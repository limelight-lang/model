//! Standard allocator API: a `GlobalAlloc` implementation and C
//! `malloc`/`free`/`calloc`/`realloc`/`aligned_alloc` exports over the
//! Limelight memory manager.
//!
//! This lets the allocator (a) run the real C benchmark suites
//! unchanged and (b) be reused outside the runtime.
//!
//! **Not yet usable as a Rust `#[global_allocator]`**, despite the impl
//! at the bottom of this file: regions come from `std::alloc::alloc`
//! with block alignment (`block_pool::carve_region`), so installing
//! this as the global allocator makes region carving re-enter
//! `ll_alloc` with an alignment it refuses — every allocation would
//! then report null. It becomes true once regions come from
//! `VirtualAlloc`/`mmap` directly.
//!
//! **Size-less free works** — the key that makes a standard `free(ptr)`
//! possible over our design: every allocation lives in a block-aligned
//! block, so `ptr & !BLOCK_MASK` finds the block header, whose `kind` routes
//! the free. No caller-provided size needed. Routing:
//!
//! - `≤ 8 KB`, align ≤ 16 → the small-object [`Heap`].
//! - `8 KB .. block payload` → one pooled block (`BLOCK_KIND_LARGE`).
//! - `> block payload` → an OS-direct block-aligned run
//!   (`BLOCK_KIND_LARGE_RUN`), returned to the OS on free.
//! - `BLOCK_KIND_ENTITY` (GC entities, allocated via
//!   `heap::entity_alloc`, never by `ll_alloc`) → the thread's entity
//!   heap; `ll_free` covers them so object teardown stays size-less.
//!
//! The `+256` payload offset is 256-aligned, so any alignment ≤ 256 is
//! satisfied for free (covers malloc's 16-byte guarantee and
//! `aligned_alloc` up to 256).
//!
//! **Thread-safety:** all paths are multi-threaded. Large paths use the
//! thread-safe pool/OS; the small path routes a cross-thread free to the
//! owning block's lock-free stack (see `heap`). A thread that exits hands
//! its blocks over automatically on every target — empty ones to the
//! pool, ones still holding objects to the global abandoned list, from
//! which the next thread needing that size class adopts them.
//!
//! Known limit: an abandoned block is reclaimed only when someone adopts
//! it, so a size class that goes permanently idle keeps its abandoned
//! blocks. Bounded by what was live at thread exit; no periodic trim
//! exists yet.

use std::alloc::{GlobalAlloc, Layout};

use crate::memory::block_pool::{
    BLOCK_KIND_ENTITY, BLOCK_KIND_HEAP, BLOCK_KIND_LARGE, BLOCK_KIND_LARGE_RUN, BLOCK_MASK,
    BLOCK_PAYLOAD, BLOCK_SIZE, BlockHeader, BlockPool, LINE_SIZE,
};
use crate::memory::heap::MAX_SMALL;

/// Largest alignment the `+256` payload guarantees.
const MAX_ALIGN: usize = LINE_SIZE;

/// Header for large (non-heap) allocations, written into the block's
/// first line. Shares offset 0 (`kind`) with the pool `BlockHeader`.
#[repr(C)]
struct LargeHeader {
    kind: u32,
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
/// # Safety
/// Standard allocator contract.
/// The small path is the whole body here; everything else is a `#[cold]`
/// tail. Keeping the large-object branches in this function gave it a stack
/// frame (`push rsi; push rdi; sub rsp, 56`) that every small `malloc` set
/// up and tore down on its way to a tail call — see [`Heap::alloc`]'s doc
/// for the full reasoning.
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
            block.write(LargeHeader {
                kind: 0,
                _pad: 0,
                size,
                run_bytes: 0,
            });
            // Kind last (release under rc-walk): the collector snapshot
            // reads every block's kind concurrently.
            crate::memory::block_pool::store_block_kind(&raw mut (*block).kind, BLOCK_KIND_LARGE);
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
        let layout = Layout::from_size_align(run_bytes, BLOCK_SIZE).unwrap();
        let block = unsafe { std::alloc::alloc(layout) } as *mut LargeHeader;
        if block.is_null() {
            return std::ptr::null_mut();
        }
        unsafe {
            block.write(LargeHeader {
                kind: 0,
                _pad: 0,
                size,
                run_bytes,
            });
            crate::memory::block_pool::store_block_kind(
                &raw mut (*block).kind,
                BLOCK_KIND_LARGE_RUN,
            );
            (block as *mut u8).add(LINE_SIZE)
        }
    }
}

/// Free a pointer from [`ll_alloc`]. Dispatches on the owning block's
/// kind — no size needed.
///
/// # Safety
/// `ptr` must be a live allocation from [`ll_alloc`] on this thread (for
/// small objects) and not already freed.
/// Split fast/cold like [`ll_alloc`]: the heap path is the body, the large
/// and huge kinds are a `#[cold]` tail.
#[inline]
pub unsafe fn ll_free(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    let block = block_of(ptr);
    let kind = unsafe { *(block as *const u32) };

    // While an rc-walk epoch is in flight, every freeable kind parks
    // instead of recycling — identity of walked slots and chased buffers
    // (`deferred_free`, one relaxed load + predicted branch, per
    // `rfc/model/gc/rc-walk.md`). The no-op kinds (arena, retained) fall
    // through: they recycle nothing, so identity holds without parking.
    #[cfg(feature = "rc-walk")]
    if crate::memory::deferred_free::active()
        && matches!(
            kind,
            BLOCK_KIND_HEAP | BLOCK_KIND_ENTITY | BLOCK_KIND_LARGE | BLOCK_KIND_LARGE_RUN
        )
    {
        return unsafe { crate::memory::deferred_free::park(ptr) };
    }

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
        _ => { /* not ours / double free — ignore in release, catch in tests */ }
    }
}

/// The requested size of a live allocation, recovered from its block.
///
/// # Safety
/// `ptr` must be a live allocation from [`ll_alloc`].
unsafe fn ll_usable_size(ptr: *mut u8) -> usize {
    let block = block_of(ptr);
    let kind = unsafe { *(block as *const u32) };
    match kind {
        BLOCK_KIND_HEAP | BLOCK_KIND_ENTITY => {
            // Heap slot: the class size (upper bound on the request).
            use crate::memory::heap::SIZE_CLASSES;
            let ci = unsafe { *((block as *const u32).add(1)) } as usize;
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
mod tests {
    use super::*;

    #[test]
    fn small_roundtrip() {
        let _g = crate::memory::block_pool::test_guard();
        unsafe {
            let p = ll_alloc(40, 16);
            assert!(!p.is_null());
            (p as *mut u64).write(0xDEAD_BEEF);
            assert_eq!(*(p as *mut u64), 0xDEAD_BEEF);
            ll_free(p);
        }
    }

    /// The pooled LARGE path is the middle band — bigger than a heap slot,
    /// smaller than a block payload — and it is the band the exhaustion
    /// contract was written for: null, never a dead process. It used to
    /// write the block header before looking at the pointer, so a refusal
    /// there was a null dereference.
    #[test]
    fn pooled_large_reports_exhaustion_instead_of_writing_through_null() {
        let _g = crate::memory::block_pool::test_guard();
        use crate::memory::block_pool::FORCE_OOM;
        use std::sync::atomic::Ordering;

        FORCE_OOM.store(true, Ordering::Relaxed);
        let p = unsafe { ll_alloc(20_000, 16) };
        let aligned = unsafe { ll_alloc(40, 64) }; // align > 16 routes here too
        FORCE_OOM.store(false, Ordering::Relaxed);

        assert!(p.is_null(), "exhaustion must report, not abort");
        assert!(aligned.is_null(), "the over-aligned route reports too");

        let q = unsafe { ll_alloc(20_000, 16) };
        assert!(!q.is_null(), "the path survived the refusal");
        unsafe { ll_free(q) };
    }

    #[test]
    fn large_single_block_roundtrip() {
        let _g = crate::memory::block_pool::test_guard();
        unsafe {
            let size = 20_000; // > 8 KB, < block payload
            let p = ll_alloc(size, 16);
            assert!(!p.is_null());
            // Writable across the whole request.
            std::ptr::write_bytes(p, 0xAB, size);
            assert_eq!(*p, 0xAB);
            assert_eq!(*p.add(size - 1), 0xAB);
            ll_free(p);
        }
    }

    #[test]
    fn aligned_alloc_over_16_honors_alignment() {
        let _g = crate::memory::block_pool::test_guard();
        unsafe {
            // align > 16 for a small size must be honored (the heap gives
            // only 16); several in a row so a mis-aligned heap slot would
            // show. Pooled payloads sit at +256, satisfying up to MAX_ALIGN.
            for align in [32usize, 64, 128, 256] {
                let ptrs: Vec<*mut u8> = (0..4).map(|_| ll_alloc(40, align)).collect();
                for &p in &ptrs {
                    assert!(!p.is_null());
                    assert_eq!((p as usize) % align, 0, "align {align} honored");
                }
                for p in ptrs {
                    ll_free(p);
                }
            }
            // Above MAX_ALIGN is unsupported → null.
            assert!(ll_alloc(40, 512).is_null());
        }
    }

    #[test]
    fn aligned_alloc_on_a_fresh_thread_does_not_deref_a_null_heap() {
        let _g = crate::memory::block_pool::test_guard();
        // A thread that never called `ll_thread_init`: an `align > 16` small
        // request must not route to the (null) thread heap.
        std::thread::spawn(|| unsafe {
            let p = ll_alloc(40, 64);
            assert!(!p.is_null());
            assert_eq!((p as usize) % 64, 0);
            ll_free(p);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn huge_size_overflow_returns_null_not_underallocation() {
        let _g = crate::memory::block_pool::test_guard();
        unsafe {
            // size + LINE_SIZE and the block round-up both overflow usize:
            // the request must be refused, never wrapped to a small run.
            assert!(ll_alloc(usize::MAX, 16).is_null());
            assert!(ll_alloc(usize::MAX - 100, 16).is_null());
        }
    }

    #[test]
    fn huge_os_direct_roundtrip() {
        let _g = crate::memory::block_pool::test_guard();
        unsafe {
            let size = 200_000; // > block payload -> OS-direct run
            let p = ll_alloc(size, 16);
            assert!(!p.is_null());
            assert_eq!(p as usize & BLOCK_MASK, LINE_SIZE, "run is 32K-aligned");
            std::ptr::write_bytes(p, 0xCD, size);
            assert_eq!(*p.add(size - 1), 0xCD);
            ll_free(p);
        }
    }

    #[test]
    fn realloc_grows_and_preserves() {
        let _g = crate::memory::block_pool::test_guard();
        unsafe {
            let p = ll_alloc(16, 16);
            std::ptr::copy_nonoverlapping(b"hello".as_ptr(), p, 5);
            let p2 = ll_realloc(p, 40, 16);
            assert_eq!(std::slice::from_raw_parts(p2, 5), b"hello");
            ll_free(p2);
        }
    }

    #[test]
    fn realloc_null_is_alloc() {
        let _g = crate::memory::block_pool::test_guard();
        unsafe {
            let p = ll_realloc(std::ptr::null_mut(), 32, 16);
            assert!(!p.is_null());
            ll_free(p);
        }
    }

    #[test]
    fn calloc_zeroes() {
        let _g = crate::memory::block_pool::test_guard();
        unsafe {
            let p = ll_calloc(10, 8);
            assert!(!p.is_null());
            for i in 0..80 {
                assert_eq!(*p.add(i), 0);
            }
            ll_c_free(p);
        }
    }

    #[test]
    fn global_alloc_drives_a_vec() {
        let _g = crate::memory::block_pool::test_guard();
        // Exercise the standard Rust interface end to end.
        let a = LimelightAlloc;
        unsafe {
            let layout = Layout::array::<u64>(1000).unwrap();
            let p = a.alloc(layout) as *mut u64;
            assert!(!p.is_null());
            for i in 0..1000 {
                p.add(i).write(i as u64);
            }
            assert_eq!(*p.add(999), 999);
            a.dealloc(p as *mut u8, layout);
        }
    }
}
