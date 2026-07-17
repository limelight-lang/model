// Shim linking the unmodified upstream larson.cpp to ll-model's real C ABI
// (src/memory/stdapi.rs / heap.rs), via the benchmark's own
// CUSTOM_MALLOC/CUSTOM_FREE extension hook.
//
// A direct #define, deliberately -- the same shape as mi_malloc_shim.h next
// door (`#define CUSTOM_MALLOC mi_malloc`). It used to wrap both calls in an
// `ll_shim_ensure_init()` that tested a `thread_local` on every malloc *and*
// every free, because ll_malloc required an explicit ll_thread_init() first.
// mimalloc paid nothing equivalent, so every comparison run through this file
// charged us for the harness and called the difference an allocator gap.
//
// ll_malloc/ll_c_free now self-initialise on a cold branch -- exactly as
// mi_malloc does (`test rcx,rcx; je _mi_malloc_generic`) -- and the library's
// own TLS guard calls ll_thread_exit when a thread unwinds, which larson
// needs: its exercise_heap respawns itself as a fresh OS thread every
// NumBlocks rounds, and a thread dying without that strands its blocks.
#pragma once
#include <cstddef>

extern "C" {
    void *ll_malloc(size_t size);
    void ll_c_free(void *ptr);
}

#define CUSTOM_MALLOC ll_malloc
#define CUSTOM_FREE ll_c_free
