// Shim linking the unmodified upstream larson.cpp to ll-model's real C ABI
// (src/memory/stdapi.rs / heap.rs), via the benchmark's own
// CUSTOM_MALLOC/CUSTOM_FREE extension hook.
//
// larson spawns raw OS threads and gives them no start hook, and the runtime
// serves a thread once `ll_thread_init` has started it -- a malloc on a thread
// never started ends the process (`dev/DECISIONS.md`, "`ll_thread_init` is
// called once, and a refusal closes the thread"). So the malloc side carries
// the embedder's one initialisation behind a `thread_local` flag, tested on
// every malloc: that test is the price of an embedder with no thread start,
// and a comparison against mi_malloc, which starts its own thread on a cold
// branch inside the call, charges it to this shim rather than to the
// allocator. The free side needs nothing: a free on any thread routes the
// block to its owner. The library's own TLS guard calls ll_thread_exit when a
// thread unwinds, which larson needs: its exercise_heap respawns itself as a
// fresh OS thread every NumBlocks rounds, and a thread dying without that
// strands its blocks.
#pragma once
#include <cstddef>
#include <cstdio>
#include <cstdlib>

extern "C" {
    void *ll_malloc(size_t size);
    void ll_c_free(void *ptr);
    bool ll_thread_init(void);
}

static thread_local bool ll_shim_started = false;

static inline void *ll_shim_malloc(size_t size) {
    if (!ll_shim_started) {
        if (!ll_thread_init()) {
            // A refused thread never starts, and larson has no way to run
            // its work elsewhere.
            fprintf(stderr, "ll_thread_init refused: the runtime did not start this thread\n");
            abort();
        }
        ll_shim_started = true;
    }
    return ll_malloc(size);
}

#define CUSTOM_MALLOC ll_shim_malloc
#define CUSTOM_FREE ll_c_free
