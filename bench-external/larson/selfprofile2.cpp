// Real-conditions profile: larson's actual workload shape (random sizes
// 8..1000, live-set churn: free a random victim, allocate a new random
// size in its place) but on ONE stable, long-lived thread -- unlike
// larson.cpp itself, which respawns a new OS thread every ~25ms in
// single-thread mode (exercise_heap re-`_beginthread`s itself after each
// batch), which defeats any thread-based sampling profiler (confirmed:
// both this project's own sampler and Very Sleepy's /mbt landed 95%+ of
// samples on the long-lived main thread's Sleep(), not the churning
// worker). Same size distribution and access pattern, no thread churn.
#include <cstdio>
#include <cstdint>
#include <windows.h>
#include <vector>
#include <atomic>

#ifdef PROFILE_MIMALLOC
#include <mimalloc.h>
#define ALLOC(sz) mi_malloc(sz)
#define FREE(p) mi_free(p)
#else
extern "C" {
    void *ll_malloc(size_t size);
    void ll_c_free(void *ptr);
    void ll_thread_init(void);
}
#define ALLOC(sz) ll_malloc(sz)
#define FREE(p) ll_c_free(p)
#endif

static std::atomic<bool> g_done{false};
static const size_t LIVE_SET = 5000;
static const size_t ROUNDS = 4'000'000;
static const size_t MIN_SIZE = 8, MAX_SIZE = 1000;

struct Rng {
    uint64_t s;
    uint64_t next() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        return s;
    }
    size_t size() { return MIN_SIZE + next() % (MAX_SIZE - MIN_SIZE); }
};

static DWORD WINAPI worker(LPVOID) {
#ifndef PROFILE_MIMALLOC
    ll_thread_init();
#endif
    Rng rng{4141};
    std::vector<void *> live(LIVE_SET);
    for (auto &p : live) {
        p = ALLOC(rng.size());
        *(volatile char *)p = 'a';
    }
    for (size_t i = 0; i < ROUNDS; i++) {
        size_t victim = rng.next() % LIVE_SET;
        FREE(live[victim]);
        void *p = ALLOC(rng.size());
        *(volatile char *)p = 'a';
        live[victim] = p;
    }
    for (auto p : live) FREE(p);
    g_done.store(true);
    return 0;
}

int main() {
    HANDLE hThread = CreateThread(nullptr, 0, worker, nullptr, CREATE_SUSPENDED, nullptr);
    if (!hThread) {
        printf("CreateThread failed: %lu\n", GetLastError());
        return 1;
    }

    std::vector<uint64_t> samples;
    samples.reserve(2'000'000);

    ResumeThread(hThread);

    CONTEXT ctx;
    ctx.ContextFlags = CONTEXT_CONTROL;
    while (!g_done.load()) {
        DWORD sc = SuspendThread(hThread);
        if (sc == (DWORD)-1) continue;
        if (GetThreadContext(hThread, &ctx)) {
            samples.push_back(ctx.Rip);
        }
        ResumeThread(hThread);
    }
    WaitForSingleObject(hThread, INFINITE);
    CloseHandle(hThread);

    printf("collected %zu samples\n", samples.size());

    FILE *f = fopen(
#ifdef PROFILE_MIMALLOC
        "samples2_mi.txt",
#else
        "samples2_ours.txt",
#endif
        "w");
    for (auto rip : samples) fprintf(f, "%llx\n", (unsigned long long)rip);
    fclose(f);
    return 0;
}
