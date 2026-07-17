// Does our gap to mimalloc scale with the number of *live blocks*?
//
// Hypothesis under test: our per-block metadata (HeapBlockHeader) sits at
// the start of each 32 KB block, so K live blocks put K header cache lines
// on K distinct 4 KB pages, scattered across the address space. mimalloc
// keeps every page's metadata in one dense array at its segment start
// (mi_segment_t.slices[], 32 MB segment / 64 KB slices), so K pages' worth
// of metadata is a few contiguous KB. If that difference is what is left of
// the gap, then the gap should be small when few blocks are live (our
// headers stay hot) and widen as the live set grows past L1/dTLB reach.
//
// Same larson-shaped workload as selfprofile2/4 (random sizes 8..1000,
// free-a-random-victim + allocate-in-its-place), only LIVE_SET varies.
// No sampler attached: this measures wall time, not RIPs.
//
// Build:
//   cl /O2 /MD /EHsc /std:c++17 /I <mimalloc-include> scaling_probe.cpp \
//      /Fe:scaling_probe.exe /link ll_model.lib mimalloc.lib ...
#include <cstdio>
#include <cstdint>
#include <vector>
#include <chrono>
#include <algorithm>

extern "C" {
    void *ll_malloc(size_t size);
    void ll_c_free(void *ptr);
    void ll_thread_init(void);
}
#include <mimalloc.h>

struct Rng {
    uint64_t s;
    uint64_t next() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        return s;
    }
    size_t size() { return 8 + next() % (1000 - 8); }
};

static const size_t ROUNDS = 2'000'000;

template <typename A, typename F>
static double run(size_t live_set, A alloc, F freep) {
    Rng rng{4141};
    std::vector<void *> live(live_set);
    for (auto &p : live) {
        p = alloc(rng.size());
        *(volatile char *)p = 'a';
    }
    auto t0 = std::chrono::high_resolution_clock::now();
    for (size_t i = 0; i < ROUNDS; i++) {
        size_t victim = rng.next() % live_set;
        freep(live[victim]);
        void *p = alloc(rng.size());
        *(volatile char *)p = 'a';
        live[victim] = p;
    }
    auto t1 = std::chrono::high_resolution_clock::now();
    for (auto p : live) freep(p);
    return std::chrono::duration<double, std::nano>(t1 - t0).count() / double(ROUNDS);
}

int main() {
    ll_thread_init();
    const size_t sets[] = {50, 200, 1000, 5000, 20000, 80000};

    printf("larson-shaped workload, sizes 8..1000, %zu rounds each\n", ROUNDS);
    printf("avg object ~504B -> live bytes ~= live_set * 504\n\n");
    printf("%10s %12s %10s %10s %8s\n", "live_set", "live_bytes", "ours", "mimalloc", "ratio");

    for (size_t ls : sets) {
        // Best-of-3 each, to blunt scheduler noise on a dev laptop.
        double o = 1e18, m = 1e18;
        for (int r = 0; r < 3; r++) {
            o = std::min(o, run(ls, ll_malloc, ll_c_free));
            m = std::min(m, run(ls, mi_malloc, mi_free));
        }
        printf("%10zu %10zuKB %8.2fns %8.2fns %7.2fx\n",
               ls, (ls * 504) / 1024, o, m, o / m);
        fflush(stdout);
    }
    return 0;
}
