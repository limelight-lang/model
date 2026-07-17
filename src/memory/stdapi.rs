//! Standard allocator API: a `GlobalAlloc` implementation and C
//! `malloc`/`free`/`calloc`/`realloc`/`aligned_alloc` exports over the
//! Limelight memory manager.
//!
//! This lets the allocator (a) drop into any Rust program as
//! `#[global_allocator]`, (b) run the real C benchmark suites unchanged,
//! (c) be reused outside the runtime.
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
//!
//! The `+256` payload offset is 256-aligned, so any alignment ≤ 256 is
//! satisfied for free (covers malloc's 16-byte guarantee and
//! `aligned_alloc` up to 256).
//!
//! **Thread-safety:** all paths are now multi-threaded. Large paths use
//! the thread-safe pool/OS; the small path routes a cross-thread free to
//! the owning heap's lock-free stack (see `heap`). The one remaining
//! limit is thread-exit abandonment: blocks still holding objects when
//! their owning thread exits are leaked rather than adopted. Fine for
//! worker pools with long-lived threads.

use std::alloc::{GlobalAlloc, Layout};

use crate::memory::block_pool::{
    BLOCK_KIND_HEAP, BLOCK_KIND_LARGE, BLOCK_KIND_LARGE_RUN, BLOCK_MASK, BLOCK_PAYLOAD, BLOCK_SIZE,
    BlockHeader, BlockPool, LINE_SIZE,
};
use crate::memory::heap::{MAX_SMALL, with_thread_heap};

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

/// Round `n` up to a whole number of blocks.
#[inline]
fn round_up_blocks(n: usize) -> usize {
    (n + BLOCK_SIZE - 1) & !(BLOCK_SIZE - 1)
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
    unsafe { with_thread_heap(|h| h.alloc(size)) }
}

/// # Safety
/// Standard allocator contract.
#[cold]
#[inline(never)]
unsafe fn ll_alloc_large(size: usize, align: usize) -> *mut u8 {
    if align > MAX_ALIGN {
        return std::ptr::null_mut();
    }
    // A small size can still land here via an alignment above 16.
    if size <= MAX_SMALL {
        return unsafe { with_thread_heap(|h| h.alloc(size)) };
    }

    if size <= BLOCK_PAYLOAD {
        // One pooled block holds the object; payload at +256.
        let block = BlockPool::global().get() as *mut LargeHeader;
        unsafe {
            block.write(LargeHeader {
                kind: BLOCK_KIND_LARGE,
                _pad: 0,
                size,
                run_bytes: 0,
            });
            (block as *mut u8).add(LINE_SIZE)
        }
    } else {
        // Huge: OS-direct, block-aligned so the mask still finds us.
        let run_bytes = round_up_blocks(size + LINE_SIZE);
        let layout = Layout::from_size_align(run_bytes, BLOCK_SIZE).unwrap();
        let block = unsafe { std::alloc::alloc(layout) } as *mut LargeHeader;
        if block.is_null() {
            return std::ptr::null_mut();
        }
        unsafe {
            block.write(LargeHeader {
                kind: BLOCK_KIND_LARGE_RUN,
                _pad: 0,
                size,
                run_bytes,
            });
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
        BLOCK_KIND_HEAP => {
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
