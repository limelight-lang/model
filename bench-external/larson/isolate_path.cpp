// Isolates per-call PATH overhead (TLS + RefCell + closure + FFI boundary)
// from workload shape: fixed size, immediate alloc-then-free, no live set,
// no randomness. If our heap's gap here is close to the larson gap, the
// path is the dominant cost; if much smaller, larson's churn pattern
// (block refill / remote_free drain) is doing most of the damage instead.
#include <cstdio>
#include <cstdint>
#include <windows.h>

extern "C" {
    void *ll_malloc(size_t size);
    void ll_c_free(void *ptr);
    bool ll_thread_init(void);
    // Diagnostic-only: isolates TLS cost from FFI-boundary + algorithm cost.
    void *ll_diag_heap_new(void);
    void *ll_diag_alloc_raw(void *heap, size_t size);
    void ll_diag_free_raw(void *heap, void *ptr);
    size_t ll_diag_noop(size_t x);
    size_t ll_diag_size_class_only(size_t size);
    void *sn_malloc(size_t size);
    void sn_free(void *ptr, size_t size);
}
#include <mimalloc.h>

static double now_ms() {
    LARGE_INTEGER f, c;
    QueryPerformanceFrequency(&f);
    QueryPerformanceCounter(&c);
    return (double)c.QuadPart * 1000.0 / (double)f.QuadPart;
}

int main() {
    const size_t N = 20'000'000;
    const size_t SIZE = 64;

    // explicit, cold, one-time -- see heap.rs::ll_thread_init
    if (!ll_thread_init()) {
        fprintf(stderr, "ll_thread_init refused: the runtime did not start this thread\n");
        return 1;
    }

    {
        double t0 = now_ms();
        for (size_t i = 0; i < N; i++) {
            void *p = ll_malloc(SIZE);
            *(uint64_t *)p = i;
            ll_c_free(p);
        }
        double t1 = now_ms();
        printf("ours:     %.1f ms  (%.2f ns/op)\n", t1 - t0, (t1 - t0) * 1e6 / N);
    }
    {
        double t0 = now_ms();
        volatile size_t sink = 0;
        for (size_t i = 0; i < N; i++) {
            sink = ll_diag_noop(i);
        }
        double t1 = now_ms();
        printf("noop call:         %.1f ms  (%.2f ns/op)\n", t1 - t0, (t1 - t0) * 1e6 / N);
        (void)sink;
    }
    {
        double t0 = now_ms();
        volatile size_t sink = 0;
        for (size_t i = 0; i < N; i++) {
            sink = ll_diag_size_class_only(SIZE);
        }
        double t1 = now_ms();
        printf("size_class_index:  %.1f ms  (%.2f ns/op)\n", t1 - t0, (t1 - t0) * 1e6 / N);
        (void)sink;
    }
    {
        void *heap = ll_diag_heap_new();
        double t0 = now_ms();
        for (size_t i = 0; i < N; i++) {
            void *p = ll_diag_alloc_raw(heap, SIZE);
            *(uint64_t *)p = i;
            ll_diag_free_raw(heap, p);
        }
        double t1 = now_ms();
        printf("ours(raw,no-TLS): %.1f ms  (%.2f ns/op)\n", t1 - t0, (t1 - t0) * 1e6 / N);
    }
    {
        // Hypothesis: free_local() returns a block to the global pool the
        // instant used hits 0 (heap.rs:223-230). With only one live slot,
        // every free empties the block, so every next alloc re-carves a
        // fresh ~508-slot free list from scratch. Keep one anchor object
        // alive so `used` never reaches 0 and see if the cost collapses.
        void *heap = ll_diag_heap_new();
        void *anchor = ll_diag_alloc_raw(heap, SIZE); // never freed: used >= 1 always
        *(uint64_t *)anchor = 0;
        double t0 = now_ms();
        for (size_t i = 0; i < N; i++) {
            void *p = ll_diag_alloc_raw(heap, SIZE);
            *(uint64_t *)p = i;
            ll_diag_free_raw(heap, p);
        }
        double t1 = now_ms();
        printf("ours(anchored, used never 0): %.1f ms  (%.2f ns/op)\n", t1 - t0, (t1 - t0) * 1e6 / N);
    }
    {
        double t0 = now_ms();
        for (size_t i = 0; i < N; i++) {
            void *p = mi_malloc(SIZE);
            *(uint64_t *)p = i;
            mi_free(p);
        }
        double t1 = now_ms();
        printf("mimalloc: %.1f ms  (%.2f ns/op)\n", t1 - t0, (t1 - t0) * 1e6 / N);
    }
    {
        double t0 = now_ms();
        for (size_t i = 0; i < N; i++) {
            void *p = sn_malloc(SIZE);
            *(uint64_t *)p = i;
            sn_free(p, SIZE);
        }
        double t1 = now_ms();
        printf("snmalloc: %.1f ms  (%.2f ns/op)\n", t1 - t0, (t1 - t0) * 1e6 / N);
    }
    {
        double t0 = now_ms();
        for (size_t i = 0; i < N; i++) {
            void *p = malloc(SIZE);
            *(uint64_t *)p = i;
            free(p);
        }
        double t1 = now_ms();
        printf("system:   %.1f ms  (%.2f ns/op)\n", t1 - t0, (t1 - t0) * 1e6 / N);
    }
    return 0;
}
