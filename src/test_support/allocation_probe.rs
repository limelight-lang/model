//! Counts what a path allocates, so a test can assert that it allocates
//! nothing.
//!
//! Two counters, because "allocates nothing" has two meanings on a path
//! that lives inside a memory manager. [`take_heap_allocations`] counts
//! what reaches the global allocator, which is what a `Vec`, a `Box` or a
//! `format!` would do. [`take_pool_requests`] counts calls into
//! [`BlockPool::get`](crate::memory::block_pool::BlockPool::get), which
//! allocates nothing when the thread cache serves it and still takes a
//! process-global mutex when it does not — and a lock on the registration
//! path is refused by the same clause as an allocation
//! (`rfc/model/gc/cycle/questions.md`, Y12 clause 3).
//!
//! A third, [`take_heap_deallocations`], counts `dealloc` calls. A
//! structure torn down block by block allocates nothing while it does so,
//! so the two counters above read zero over it and a test built on them
//! alone certifies the very thing it was meant to catch. What the counter
//! does not measure is memory returned: a shrinking reallocation gives
//! bytes back and is charged to the allocation counter, so the question
//! it answers is "did this path call `free`", not "did this path get
//! smaller".
//!
//! All three are per thread, because the suite runs tests in parallel and
//! a shared counter would charge one test's allocations to another.
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
    /// the allocator neither allocates nor recurses. A `const` block with
    /// no destructor also has no lazily destroyed state, which is what
    /// makes `with` sound from inside a free running at thread exit.
    static HEAP: Cell<usize> = const { Cell::new(0) };

    /// Frees this thread has made through it, on the same terms.
    static FREED: Cell<usize> = const { Cell::new(0) };
}

/// The global allocator of the test binary: `System`, plus two counts.
///
/// A reallocation is charged to [`HEAP`] and never to [`FREED`]: whichever
/// way `System` serves it, the caller comes out holding one live block, so
/// counting the release inside it would report memory given back that the
/// caller still has. Only `dealloc` reaches [`FREED`].
struct CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        HEAP.with(|c| c.set(c.get() + 1));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        FREED.with(|c| c.set(c.get() + 1));
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

/// Global-allocator `dealloc` calls on this thread, and zero the count.
/// Calls, not bytes, and not memory returned — see the module doc.
pub(crate) fn take_heap_deallocations() -> usize {
    FREED.with(|c| c.replace(0))
}

/// Both allocation counters at once, zeroed. A test brackets the path it
/// is validating with two calls and compares.
///
/// What the path gave back is [`take_heap_deallocations`], which this does
/// not touch: a test asking whether a path allocates is not asking what it
/// freed, and folding the third number into this pair would put a
/// deallocation assertion into every existing comparison against `(0, 0)`.
pub(crate) fn take_allocations() -> (usize, usize) {
    (take_heap_allocations(), take_pool_requests())
}

#[cfg(test)]
mod tests;
