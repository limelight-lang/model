//! Counts what a path allocates, so a test can assert that it allocates
//! nothing.
//!
//! Two counters, because "allocates nothing" has two meanings on a path
//! that lives inside a memory manager. [`heap_allocations`] counts what
//! reaches the global allocator, which is what a `Vec`, a `Box` or a
//! `format!` would do. [`pool_requests`] counts calls into
//! [`BlockPool::get`](crate::memory::block_pool::BlockPool::get), which
//! allocates nothing when the thread cache serves it and still takes a
//! process-global mutex when it does not — and a lock on the enrolment
//! path is refused by the same clause as an allocation
//! (`rfc/model/gc/cycle/questions.md`, Y12 clause 3).
//!
//! Both are per thread, because the suite runs tests in parallel and a
//! shared counter would charge one test's allocations to another.
//!
//! # Why a global allocator rather than a wrapper at the call site
//!
//! The path under test calls no allocator by name — that is the point of
//! it. A probe that had to be threaded through would only see the calls
//! somebody remembered to thread it through, which is the same as
//! defining the growth path as not hot: the property would be assumed by
//! the instrument that was supposed to check it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    /// Allocations this thread has made through the global allocator.
    /// `Cell<usize>` and `const`-initialised, so touching it from inside
    /// the allocator neither allocates nor recurses.
    static HEAP: Cell<usize> = const { Cell::new(0) };
}

/// The global allocator of the test binary: `System`, plus a count.
///
/// Reallocation counts as an allocation and deallocation counts as
/// nothing, because what a test asks here is "did this path go and get
/// memory", and a free is not that.
struct CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        HEAP.with(|c| c.set(c.get() + 1));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        HEAP.with(|c| c.set(c.get() + 1));
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        HEAP.with(|c| c.set(c.get() + 1));
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static PROBE: CountingAlloc = CountingAlloc;

/// Global-allocator calls on this thread, and zero the count.
pub(crate) fn take_heap_allocations() -> usize {
    HEAP.with(|c| c.replace(0))
}

/// Pool requests on this thread, and zero the count. The counter itself
/// lives in `block_pool`, at the one entry a block comes out of.
pub(crate) fn take_pool_requests() -> usize {
    crate::memory::block_pool::take_pool_requests()
}

/// Both counters at once, zeroed. A test brackets the path it is
/// judging with two calls and compares.
pub(crate) fn take_all() -> (usize, usize) {
    (take_heap_allocations(), take_pool_requests())
}
