// How many blocks does one alloc walk before it finds a free slot?
//
// `Heap::alloc` examines `available[ci]`; if that block has no room it hands
// off to a cold path that unlinks it and retries with the next one. Every
// block walked costs a cold load of its header, and `unlink` rewrites two
// *other* blocks' headers on top of that. A walk of 1.0 means the head block
// always had room and none of that happened.
//
// This number is what located the churn in
// `rfc/model/memory/heap-slot-allocation.md` ("Fix 5b"): it tracked the gap
// to mimalloc across every live-set size (1.00 where we were 1.2x behind,
// 1.41 where we were 1.8x behind).
//
// Needs the counters:
//   cargo build --release --features probe-counters
//   cl /O2 /EHsc /std:c++17 walk_probe.cpp /Fe:walk_probe.exe \
//      /link ll_model.lib ntdll.lib userenv.lib ws2_32.lib bcrypt.lib advapi32.lib
// Never time a build made with that feature — the counters are hot-path
// stores.
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <vector>

extern "C" {
    void *ll_malloc(size_t size);
    void ll_c_free(void *ptr);
    void ll_thread_init(void);
    // [entries, retries, unlinks, links]; entries counts block examinations,
    // retries counts the cold-path re-entries among them.
    void ll_probe_counters(uint64_t *out);
}

struct Rng {
    uint64_t s;
    uint64_t next() { s ^= s << 13; s ^= s >> 7; s ^= s << 17; return s; }
    size_t size() { return 8 + next() % (1000 - 8); }
};

int main(int argc, char **argv) {
    size_t ls = argc > 1 ? strtoull(argv[1], nullptr, 10) : 5000;
    const size_t ROUNDS = 2'000'000;
    ll_thread_init();

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

    double entries = double(a[0] - b[0]);
    double retries = double(a[1] - b[1]);
    double allocs = entries - retries; // real allocations, not re-entries
    printf("live_set %6zu | walk/alloc %5.3f | unlink/alloc %5.3f | link/alloc %5.3f\n",
           ls, entries / allocs, double(a[2] - b[2]) / allocs,
           double(a[3] - b[3]) / allocs);
    for (auto p : live) ll_c_free(p);
    return 0;
}
