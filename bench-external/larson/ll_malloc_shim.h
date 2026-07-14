// Shim linking the unmodified upstream larson.cpp to ll-model's real C ABI
// (src/memory/stdapi.rs / heap.rs), via the benchmark's own
// CUSTOM_MALLOC/CUSTOM_FREE extension hook -- no LD_PRELOAD available on
// this platform/allocator shape, so this is the intended substitution
// point instead.
#pragma once
#include <cstddef>

extern "C" {
    void *ll_malloc(size_t size);
    void ll_c_free(void *ptr);
    void ll_thread_init(void);
}

// ll_thread_init() must run once per thread before ll_malloc/ll_c_free
// (see heap.rs::ll_thread_init doc) -- Limelight owns its own worker
// threads and calls this at their startup hook; larson.cpp spawns raw
// OS threads it knows nothing about, so the shim does it here instead,
// with a native (not Rust-TLS) per-thread bool -- this check lives in
// the benchmark harness, not in the library's hot path.
inline void ll_shim_ensure_init() {
    thread_local bool inited = false;
    if (!inited) {
        ll_thread_init();
        inited = true;
    }
}

inline void *ll_malloc_shim(size_t size) {
    ll_shim_ensure_init();
    return ll_malloc(size);
}

inline void ll_free_shim(void *ptr) {
    ll_shim_ensure_init();
    ll_c_free(ptr);
}

#define CUSTOM_MALLOC ll_malloc_shim
#define CUSTOM_FREE ll_free_shim
