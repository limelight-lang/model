// Why does larson read 1.26x when scaling_probe reads 1.02x?
//
// Same allocator, same C ABI, same live set (5000), same size range (8..1000),
// one thread, larson run with num_rounds huge so it never respawns. The gap
// has to be inside the loop body. This bisects it: start from scaling_probe's
// loop, switch on larson's differences one at a time, and watch which one
// moves the ratio.
//
// larson's loop (exercise_heap), for reference:
//     victim = lran2(&rgen) % asize;
//     CUSTOM_FREE(array[victim]);
//     cFrees++;
//     blk_size = min_size + lran2(&rgen) % range;
//     array[victim] = CUSTOM_MALLOC(blk_size);
//     blksize[victim] = blk_size;
//     cAllocs++;
//     volatile char *chptr = array[victim];
//     *chptr++ = 'a';
//     volatile char ch = *array[victim];
//     *chptr = 'b';
//     if (stopflag) break;
// plus a warmup that allocates the live set and then randomly *permutes* it.
#include <cstdio>
#include <cstdint>
#include <cstring>
#include <vector>
#include <chrono>
#include <algorithm>
#include <mimalloc.h>

extern "C" {
    void *ll_malloc(size_t size);
    void ll_c_free(void *ptr);
    bool ll_thread_init(void);
}

// --- larson's RNG, verbatim -------------------------------------------------
#define LRAN2_MAX 714025l
#define IA 1366l
#define IC 150889l
struct lran2_st { long x, y, v[97]; };
static void lran2_init(struct lran2_st *d, long seed) {
    long x = (IC - seed) % LRAN2_MAX;
    if (x < 0) x = -x;
    for (int j = 0; j < 97; j++) { x = (IA * x + IC) % LRAN2_MAX; d->v[j] = x; }
    d->x = (IA * x + IC) % LRAN2_MAX;
    d->y = d->x;
}
static long lran2(struct lran2_st *d) {
    int j = (d->y % 97);
    d->y = d->v[j];
    d->x = (IA * d->x + IC) % LRAN2_MAX;
    d->v[j] = d->x;
    return d->y;
}

struct Xor { uint64_t s; uint64_t next() { s ^= s << 13; s ^= s >> 7; s ^= s << 17; return s; } };

static const size_t LIVE = 5000, ROUNDS = 3'000'000;
static const size_t MIN_SIZE = 8, MAX_SIZE = 1000;
static volatile int stopflag = 0;
static long g_cAllocs = 0, g_cFrees = 0;

struct Opts {
    bool lran;     // larson's RNG instead of xorshift
    bool permute;  // larson's warmup permutation of the live set
    bool blksize;  // larson's second array, written every round
    bool writes;   // larson's 2 writes + 1 read instead of 1 write
    bool counters; // larson's cAllocs/cFrees and the stopflag check
};

template <bool MI>
static double run(const Opts &o) {
    auto ALLOC = [](size_t s) { return MI ? mi_malloc(s) : ll_malloc(s); };
    auto FREE = [](void *p) { MI ? mi_free(p) : ll_c_free(p); };

    std::vector<void *> arr(LIVE);
    std::vector<size_t> bsz(LIVE);
    lran2_st lr; lran2_init(&lr, 4141);
    Xor xr{4141};
    auto rnd = [&](size_t m) -> size_t { return o.lran ? (size_t)(lran2(&lr) % (long)m) : (xr.next() % m); };

    for (size_t i = 0; i < LIVE; i++) {
        size_t s = MIN_SIZE + rnd(MAX_SIZE - MIN_SIZE);
        arr[i] = ALLOC(s); bsz[i] = s;
        *(volatile char *)arr[i] = 'a';
    }
    if (o.permute) {
        // larson's warmup: random permutation, then 4*N churn rounds.
        for (size_t c = LIVE; c > 0; c--) {
            size_t v = rnd(c);
            std::swap(arr[v], arr[c - 1]);
            std::swap(bsz[v], bsz[c - 1]);
        }
        for (size_t c = 0; c < 4 * LIVE; c++) {
            size_t v = rnd(LIVE);
            FREE(arr[v]);
            size_t s = MIN_SIZE + rnd(MAX_SIZE - MIN_SIZE);
            arr[v] = ALLOC(s); bsz[v] = s;
        }
    }

    auto t0 = std::chrono::high_resolution_clock::now();
    for (size_t i = 0; i < ROUNDS; i++) {
        size_t victim = rnd(LIVE);
        FREE(arr[victim]);
        if (o.counters) g_cFrees++;
        size_t s = MIN_SIZE + rnd(MAX_SIZE - MIN_SIZE);
        arr[victim] = ALLOC(s);
        if (o.blksize) bsz[victim] = s;
        if (o.counters) g_cAllocs++;
        if (o.writes) {
            volatile char *chptr = (volatile char *)arr[victim];
            *chptr++ = 'a';
            volatile char ch = *(volatile char *)arr[victim];
            (void)ch;
            *chptr = 'b';
        } else {
            *(volatile char *)arr[victim] = 'a';
        }
        if (o.counters && stopflag) break;
    }
    auto t1 = std::chrono::high_resolution_clock::now();
    for (auto p : arr) FREE(p);
    return std::chrono::duration<double, std::nano>(t1 - t0).count() / double(ROUNDS);
}

static void step(const char *name, Opts o) {
    double best_o = 1e18, best_m = 1e18;
    for (int r = 0; r < 3; r++) {
        best_o = std::min(best_o, run<false>(o));
        best_m = std::min(best_m, run<true>(o));
    }
    printf("%-34s %8.2fns %8.2fns  %6.2fx\n", name, best_o, best_m, best_o / best_m);
    fflush(stdout);
}

int main() {
    if (!ll_thread_init()) {
        fprintf(stderr, "ll_thread_init refused: the runtime did not start this thread\n");
        return 1;
    }
    printf("live %zu, %zu rounds, sizes %zu..%zu, one thread, best-of-3\n\n",
           LIVE, ROUNDS, MIN_SIZE, MAX_SIZE);
    printf("%-34s %10s %10s %8s\n", "variant", "ours", "mimalloc", "ratio");

    Opts base{false, false, false, false, false};
    step("scaling_probe loop (baseline)", base);

    Opts a = base; a.lran = true;      step("+ larson RNG (lran2)", a);
    Opts b = base; b.permute = true;   step("+ larson warmup permutation", b);
    Opts c = base; c.blksize = true;   step("+ blksize array write", c);
    Opts d = base; d.writes = true;    step("+ larson's 2 writes + read", d);
    Opts e = base; e.counters = true;  step("+ counters + stopflag", e);

    Opts all{true, true, true, true, true};
    step("ALL (= larson's loop)", all);
    return 0;
}
