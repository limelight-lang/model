// How many 32 KB blocks are live at each live-set size, and therefore how
// many distinct HeapBlockHeader cache lines (each on its own 4 KB page,
// since headers sit at the start of 32 KB-spaced blocks) a random free()
// has to reach into. Pairs with scaling_probe.cpp: that one shows *when*
// the gap to mimalloc opens, this one shows *how much metadata is in
// flight* at that point. Compare against the x86-64 L1 dTLB (~64 entries).
#include <cstdio>
#include <cstdint>
#include <vector>

extern "C" {
    void *ll_malloc(size_t size);
    void ll_c_free(void *ptr);
    void ll_thread_init(void);

    struct MemoryStats {
        size_t regions_carved;
        size_t resident_bytes;
        size_t blocks_out;
        size_t active_bytes;
        size_t blocks_free;
    };
    void ll_memory_stats(MemoryStats *out);
}

struct Rng {
    uint64_t s;
    uint64_t next() {
        s ^= s << 13; s ^= s >> 7; s ^= s << 17;
        return s;
    }
    size_t size() { return 8 + next() % (1000 - 8); }
};

// One live_set per process run: blocks retained in `empty_reserve` are
// never returned to the pool, so a delta measured across several phases in
// one process carries the previous phase's blocks and is meaningless
// (an earlier version of this probe reported "2 blocks" for 200 live
// objects that way). Absolute blocks_out at steady state, fresh process.
int main(int argc, char **argv) {
    if (argc < 2) { printf("usage: blocks_probe <live_set>\n"); return 1; }
    size_t ls = strtoull(argv[1], nullptr, 10);
    ll_thread_init();

    Rng rng{4141};
    std::vector<void *> live(ls);
    for (auto &p : live) {
        p = ll_malloc(rng.size());
        *(volatile char *)p = 'a';
    }
    // Churn so the block set reaches steady state.
    for (size_t i = 0; i < ls * 20; i++) {
        size_t v = rng.next() % ls;
        ll_c_free(live[v]);
        void *p = ll_malloc(rng.size());
        *(volatile char *)p = 'a';
        live[v] = p;
    }
    MemoryStats st{};
    ll_memory_stats(&st);

    // One header per block, each header its own cache line, and each block
    // is 32 KB-aligned -> every header lands on a distinct 4 KB page.
    printf("%8zu %8zu %9zuKB %8zu %9zu   %s\n", ls, st.blocks_out,
           (ls * 504) / 1024, st.blocks_out * 64 / 1024 + 1, st.blocks_out,
           st.blocks_out > 64 ? "EXCEEDS L1 dTLB" : "fits L1 dTLB");
    for (auto p : live) ll_c_free(p);
    return 0;
}
