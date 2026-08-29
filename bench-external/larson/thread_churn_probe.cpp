// Does larson's thread churn explain the gap that scaling_probe.cpp can't see?
//
// scaling_probe (one stable thread, same workload shape, same C ABI) measures
// us at ~1.05x of mimalloc at a 5000-object live set. Real larson.cpp at the
// same live set measures ~1.25x. Same ABI, same pattern — the difference has
// to be something larson does that the probe does not.
//
// It does this (larson.cpp, end of exercise_heap):
//     pdea->finished = TRUE;
//     if (!stopflag) _beginthread(exercise_heap, 0, pdea);
// i.e. the worker respawns itself as a *new OS thread* every NumBlocks rounds,
// carrying the live set over. With num_rounds=100 chperthread=5000 that is a
// fresh thread every ~500k rounds — a few hundred over a 5-second run.
//
// heap.rs's own module doc says what that costs us: "Not yet handled:
// thread-exit abandonment. If a thread with live heap blocks exits, blocks
// still holding objects (and any later cross-thread frees into them) are
// leaked rather than adopted by another thread (mimalloc adopts abandoned
// pages)." So every respawn should strand that thread's blocks, and every
// free of a carried-over object should post to a dead heap's remote_free that
// nobody will ever drain.
//
// This isolates exactly that: identical work, run either on one thread or
// split across N sequential threads that hand the live set on. If the
// hypothesis holds, our throughput falls with thread count and our resident
// memory climbs, while mimalloc's does neither.
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <vector>
#include <windows.h>
#include <chrono>

#include <mimalloc.h>

extern "C" {
    void *ll_malloc(size_t size);
    void ll_c_free(void *ptr);
    bool ll_thread_init(void);
    void ll_thread_exit(void);
    struct MemoryStats {
        size_t regions_carved, resident_bytes, blocks_out, active_bytes, blocks_free;
    };
    void ll_memory_stats(MemoryStats *out);
}

struct Rng {
    uint64_t s;
    uint64_t next() { s ^= s << 13; s ^= s >> 7; s ^= s << 17; return s; }
    size_t size() { return 8 + next() % (1000 - 8); }
};

static const size_t LIVE_SET = 5000;
static const size_t TOTAL_ROUNDS = 4'000'000;

struct Work {
    std::vector<void *> *live;
    size_t rounds;
    uint64_t seed;
    bool use_mi;
};

static DWORD WINAPI worker(LPVOID p) {
    Work *w = (Work *)p;
    if (!w->use_mi && !ll_thread_init()) {
        fprintf(stderr, "ll_thread_init refused: the runtime did not start this thread\n");
        return 1;
    }
    // Symmetric with init: a thread that allocated must hand its blocks back
    // before it dies, or every block it owned is stranded. This is the whole
    // point of the probe.
    struct Exit { bool mi; ~Exit() { if (!mi) ll_thread_exit(); } } _e{w->use_mi};
    Rng rng{w->seed};
    auto &live = *w->live;
    for (size_t i = 0; i < w->rounds; i++) {
        size_t v = rng.next() % live.size();
        if (w->use_mi) {
            mi_free(live[v]);
            live[v] = mi_malloc(rng.size());
        } else {
            ll_c_free(live[v]);
            live[v] = ll_malloc(rng.size());
        }
        *(volatile char *)live[v] = 'a';
    }
    return 0;
}

// Run TOTAL_ROUNDS split across `threads` sequential OS threads, each handing
// the live set to the next -- larson's shape, with the churn rate as a knob.
static double run(size_t threads, bool use_mi, size_t *regions_out) {
    // A negative time is the refusal: the runtime would not start this
    // thread, and a probe that measured on anyway would time null
    // allocations as if they were allocations.
    if (!ll_thread_init()) {
        fprintf(stderr, "ll_thread_init refused: the runtime did not start this thread\n");
        return -1.0;
    }
    Rng rng{4141};
    std::vector<void *> live(LIVE_SET);
    for (auto &p : live) {
        p = use_mi ? mi_malloc(rng.size()) : ll_malloc(rng.size());
        *(volatile char *)p = 'a';
    }

    MemoryStats before{}, after{};
    ll_memory_stats(&before);

    auto t0 = std::chrono::high_resolution_clock::now();
    for (size_t t = 0; t < threads; t++) {
        Work w{&live, TOTAL_ROUNDS / threads, 4141 + t, use_mi};
        HANDLE h = CreateThread(nullptr, 0, worker, &w, 0, nullptr);
        WaitForSingleObject(h, INFINITE);
        CloseHandle(h);
    }
    auto t1 = std::chrono::high_resolution_clock::now();

    ll_memory_stats(&after);
    *regions_out = after.regions_carved - before.regions_carved;

    for (auto p : live) use_mi ? mi_free(p) : ll_c_free(p);
    return std::chrono::duration<double, std::nano>(t1 - t0).count() / double(TOTAL_ROUNDS);
}

int main() {
    printf("%zu rounds total, live set %zu, carried across N sequential threads\n\n",
           TOTAL_ROUNDS, LIVE_SET);
    printf("%8s %12s %12s %8s %14s\n", "threads", "ours", "mimalloc", "ratio", "our regions(+2MB)");
    for (size_t n : {1, 2, 4, 8, 16, 32, 64}) {
        size_t ro = 0, rm = 0;
        double o = run(n, false, &ro);
        double m = run(n, true, &rm);
        printf("%8zu %10.2fns %10.2fns %7.2fx %14zu\n", n, o, m, o / m, ro);
        fflush(stdout);
    }
    return 0;
}
