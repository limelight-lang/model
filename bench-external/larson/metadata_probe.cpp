// Isolates ONE variable: where per-block metadata lives.
//
// Our Heap puts HeapBlockHeader at the start of each 32 KB block, so K live
// blocks mean K metadata cache lines scattered across K*32 KB of address
// space, one per 4 KB page. mimalloc puts every page's metadata in a dense
// array in its segment header (mi_segment_t.slices[]), so K pages' metadata
// is K*~80 contiguous bytes.
//
// Both layouts are measured here against the SAME data traffic (touch a
// random slot in the chosen block, like a real object access) and the same
// metadata work (read + write, like our `kind` load + free-list push +
// used--). The only difference between arm A and arm B is the address the
// metadata lives at. If the scattered layout is what costs us, A is slower
// than B, and the gap grows with K -- and that would say the fix is a dense
// per-region header array, not more micro-optimisation of the hot path.
//
// Build: cl /O2 /EHsc /std:c++17 metadata_probe.cpp /Fe:metadata_probe.exe
#include <cstdio>
#include <cstdint>
#include <vector>
#include <windows.h>
#include <chrono>
#include <algorithm>

static const size_t BLOCK_SIZE = 32 * 1024;
static const size_t ITERS = 20'000'000;

// 64 bytes: one cache line, close to HeapBlockHeader's real size (~56).
struct alignas(64) Meta {
    uint64_t kind, size_class, used, slots;
    uint64_t freep, owner, next, prev;
};

struct Rng {
    uint64_t s;
    uint64_t next() {
        s ^= s << 13; s ^= s >> 7; s ^= s << 17;
        return s;
    }
};

// arm A ("ours"): metadata at the head of each block, blocks 32 KB apart.
static double run_scattered(uint8_t *arena, size_t K, uint64_t *sink) {
    Rng rng{4141};
    auto t0 = std::chrono::high_resolution_clock::now();
    uint64_t acc = 0;
    for (size_t i = 0; i < ITERS; i++) {
        size_t k = rng.next() % K;
        uint8_t *block = arena + k * BLOCK_SIZE;
        Meta *m = (Meta *)block;              // <-- scattered: one per 32 KB
        m->used++;                            // metadata write
        acc += m->freep;                      // metadata read
        // data traffic: a random slot inside that block, like a real object
        uint8_t *slot = block + 256 + (rng.next() % 500) * 64;
        (*slot)++;
    }
    auto t1 = std::chrono::high_resolution_clock::now();
    *sink += acc;
    return std::chrono::duration<double, std::nano>(t1 - t0).count() / double(ITERS);
}

// arm B ("mimalloc"): identical data traffic, metadata in one dense array.
static double run_dense(uint8_t *arena, Meta *dense, size_t K, uint64_t *sink) {
    Rng rng{4141};
    auto t0 = std::chrono::high_resolution_clock::now();
    uint64_t acc = 0;
    for (size_t i = 0; i < ITERS; i++) {
        size_t k = rng.next() % K;
        uint8_t *block = arena + k * BLOCK_SIZE;
        Meta *m = &dense[k];                  // <-- dense: K*64B contiguous
        m->used++;
        acc += m->freep;
        uint8_t *slot = block + 256 + (rng.next() % 500) * 64;
        (*slot)++;
    }
    auto t1 = std::chrono::high_resolution_clock::now();
    *sink += acc;
    return std::chrono::duration<double, std::nano>(t1 - t0).count() / double(ITERS);
}

int main() {
    const size_t Ks[] = {20, 37, 106, 367, 1404};
    const size_t MAXK = 1404;

    uint8_t *arena = (uint8_t *)VirtualAlloc(nullptr, MAXK * BLOCK_SIZE,
                                             MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    Meta *dense = (Meta *)VirtualAlloc(nullptr, MAXK * sizeof(Meta),
                                       MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    if (!arena || !dense) { printf("alloc failed\n"); return 1; }
    uint64_t sink = 0;

    printf("metadata layout, same data traffic, %zu iters each\n", ITERS);
    printf("(K = live blocks; matches the block counts blocks_probe measured)\n\n");
    printf("%6s %10s %12s %12s %8s\n", "K", "span", "scattered", "dense", "ratio");

    for (size_t K : Ks) {
        double a = 1e18, b = 1e18;
        for (int r = 0; r < 3; r++) {
            a = std::min(a, run_scattered(arena, K, &sink));
            b = std::min(b, run_dense(arena, dense, K, &sink));
        }
        printf("%6zu %8zuKB %10.2fns %10.2fns %7.2fx\n",
               K, (K * BLOCK_SIZE) / 1024, a, b, a / b);
        fflush(stdout);
    }
    printf("\nsink %llu\n", (unsigned long long)sink);
    return 0;
}
