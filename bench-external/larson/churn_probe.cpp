// Why does a 64 KB block beat a 32 KB block?
//
// Two candidate mechanisms, and they imply different fixes:
//   (a) fewer full-block transitions. A bigger block holds more slots, so a
//       size class's head block runs out of slots less often, so link/unlink
//       churn (each touching prev+next headers, both cold) drops.
//   (b) fewer distinct blocks. Same live bytes over bigger blocks = fewer
//       block headers = fewer cold metadata lines in flight.
// If (a), the fix generalises: cut churn some other way and 32 KB catches up.
// If (b), block size itself is the lever and nothing else substitutes.
//
// Reports link+unlink per alloc (mechanism a) alongside live block count
// (mechanism b), for whatever BLOCK_SIZE the linked ll_model.lib was built
// with. Run against a 32 KB build and a 64 KB build and compare.
//
// Needs the counters:
//   cargo build --release --features probe-counters
// Never time a build made with that feature — the counters are hot-path
// stores.
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <vector>

extern "C" {
    void *ll_malloc(size_t size);
    void ll_c_free(void *ptr);
    bool ll_thread_init(void);
    // [entries, retries, unlinks, links]
    void ll_probe_counters(uint64_t *out);

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

int main(int argc, char **argv) {
    size_t ls = argc > 1 ? strtoull(argv[1], nullptr, 10) : 5000;
    const size_t ROUNDS = 2'000'000;
    if (!ll_thread_init()) {
        fprintf(stderr, "ll_thread_init refused: the runtime did not start this thread\n");
        return 1;
    }

    Rng rng{4141};
    std::vector<void *> live(ls);
    for (auto &p : live) { p = ll_malloc(rng.size()); *(volatile char *)p = 'a'; }

    uint64_t b[4], a[4];
    ll_probe_counters(b);
    for (size_t i = 0; i < ROUNDS; i++) {
        size_t v = rng.next() % ls;
        ll_c_free(live[v]);
        void *p = ll_malloc(rng.size());
        *(volatile char *)p = 'a';
        live[v] = p;
    }
    ll_probe_counters(a);
    MemoryStats st{};
    ll_memory_stats(&st);

    double allocs = double(a[0] - b[0]) - double(a[1] - b[1]); // entries - retries
    double unl = double(a[2] - b[2]);
    double lnk = double(a[3] - b[3]);
    printf("live_set %6zu | blocks %5zu | unlink/alloc %5.3f | link/alloc %5.3f | churn/alloc %5.3f\n",
           ls, st.blocks_out, unl / allocs, lnk / allocs, (unl + lnk) / allocs);
    for (auto p : live) ll_c_free(p);
    return 0;
}
