// Isolated measurement: does a bitmap free-slot tracker beat our current
// intrusive-linked-list free/local_free on the REALISTIC (larson-shaped)
// workload -- varying sizes 8..1000, 5000-object live set, one stable
// thread? Not a rewrite of Heap -- a standalone prototype sharing only
// the same size-class table and the same workload generator, so the
// comparison isolates "linked list pop/push" vs "bitmap find-first-set
// + clear/set bit" and nothing else. Measure first, per the plan.
#include <cstdio>
#include <cstdint>
#include <cstring>
#include <windows.h>
#include <intrin.h>
#include <vector>
#include <chrono>

extern "C" {
    void *ll_malloc(size_t size);
    void ll_c_free(void *ptr);
    void ll_thread_init(void);
}
#include <mimalloc.h>

static const size_t SIZE_CLASSES[] = {
    16, 32, 48, 64, 80, 96, 112, 128,
    160, 192, 224, 256,
    320, 384, 448, 512,
    640, 768, 896, 1024,
    1280, 1536, 1792, 2048,
    2560, 3072, 3584, 4096,
    5120, 6144, 7168, 8192,
};
static const size_t NUM_CLASSES = sizeof(SIZE_CLASSES) / sizeof(SIZE_CLASSES[0]);

static inline int size_class_index(size_t size) {
    for (size_t i = 0; i < NUM_CLASSES; i++) {
        if (SIZE_CLASSES[i] >= size) return (int)i;
    }
    return -1;
}

// O(1) alternative: direct lookup table at 16-byte granularity. One array
// read, zero branches, instead of the current up-to-26-compare unrolled
// scan. 8192/16 = 512 entries, built once at startup.
static uint8_t CLASS_LUT[513];
static void build_class_lut() {
    for (size_t g = 0; g <= 512; g++) {
        size_t size = g * 16;
        int ci = 0;
        for (; ci < (int)NUM_CLASSES; ci++) {
            if (SIZE_CLASSES[ci] >= size) break;
        }
        CLASS_LUT[g] = (uint8_t)ci;
    }
}
static inline int size_class_index_o1(size_t size) {
    return CLASS_LUT[(size + 15) >> 4];
}

// --- Bitmap allocator prototype -------------------------------------------
// One generously-sized arena per class (no block-chaining machinery --
// this is a throwaway isolated perf test, not a production design).
// Free-slot state: one bit per slot, 1 = free. Claim = find first set bit
// (tzcnt over 64-bit words) + clear it. Release = set the bit back.
struct BitmapClass {
    uint8_t *base = nullptr;
    size_t class_size = 0;
    size_t slot_count = 0;
    std::vector<uint64_t> bitmap; // 1 = free

    void init(size_t cs, size_t arena_bytes) {
        class_size = cs;
        slot_count = arena_bytes / cs;
        base = (uint8_t *)VirtualAlloc(nullptr, arena_bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
        bitmap.assign((slot_count + 63) / 64, ~0ULL); // all free
    }

    void *alloc() {
        for (size_t w = 0; w < bitmap.size(); w++) {
            uint64_t word = bitmap[w];
            if (word == 0) continue;
            unsigned long bit;
            _BitScanForward64(&bit, word);
            bitmap[w] = word & (word - 1); // clear lowest set bit
            size_t idx = w * 64 + bit;
            return base + idx * class_size;
        }
        return nullptr; // arena exhausted (shouldn't happen at our test's scale)
    }

    void free(void *p) {
        size_t idx = (size_t)((uint8_t *)p - base) / class_size;
        bitmap[idx / 64] |= (1ULL << (idx % 64));
    }
};

struct BitmapAlloc {
    BitmapClass classes[NUM_CLASSES];
    // Map a live pointer back to its class: store class index just before
    // the payload (8 bytes header, matches the 8-byte RC header every
    // real object pays elsewhere in this codebase -- fair comparison).
    void init() {
        for (size_t i = 0; i < NUM_CLASSES; i++) {
            classes[i].init(SIZE_CLASSES[i] + 8, 8 * 1024 * 1024);
        }
    }
    void *alloc(size_t size, bool o1) {
        int ci = o1 ? size_class_index_o1(size) : size_class_index(size);
        void *p = classes[ci].alloc();
        *(uint32_t *)p = (uint32_t)ci;
        return (uint8_t *)p + 8;
    }
    void free(void *p) {
        void *base = (uint8_t *)p - 8;
        uint32_t ci = *(uint32_t *)base;
        classes[ci].free(base);
    }
};

// --- Workload (identical shape to selfprofile2.cpp) ------------------------
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

static const size_t LIVE_SET = 5000;
static const size_t ROUNDS = 4'000'000;

int main() {
    ll_thread_init();

    // --- our real allocator ---
    {
        Rng rng{4141};
        std::vector<void *> live(LIVE_SET);
        for (auto &p : live) {
            p = ll_malloc(rng.size());
            *(volatile char *)p = 'a';
        }
        auto t0 = std::chrono::high_resolution_clock::now();
        for (size_t i = 0; i < ROUNDS; i++) {
            size_t victim = rng.next() % LIVE_SET;
            ll_c_free(live[victim]);
            void *p = ll_malloc(rng.size());
            *(volatile char *)p = 'a';
            live[victim] = p;
        }
        auto t1 = std::chrono::high_resolution_clock::now();
        for (auto p : live) ll_c_free(p);
        double ms = std::chrono::duration<double, std::milli>(t1 - t0).count();
        printf("ours (real ll_malloc/ll_c_free, current impl): %.1f ms  (%.2f ns/op)\n", ms, ms * 1e6 / ROUNDS);
    }

    // --- mimalloc, identical workload, for a true apples-to-apples read
    // on how close bitmap+O(1) would actually get us ---
    {
        Rng rng{4141};
        std::vector<void *> live(LIVE_SET);
        for (auto &p : live) {
            p = mi_malloc(rng.size());
            *(volatile char *)p = 'a';
        }
        auto t0 = std::chrono::high_resolution_clock::now();
        for (size_t i = 0; i < ROUNDS; i++) {
            size_t victim = rng.next() % LIVE_SET;
            mi_free(live[victim]);
            void *p = mi_malloc(rng.size());
            *(volatile char *)p = 'a';
            live[victim] = p;
        }
        auto t1 = std::chrono::high_resolution_clock::now();
        for (auto p : live) mi_free(p);
        double ms = std::chrono::duration<double, std::milli>(t1 - t0).count();
        printf("mimalloc:           %.1f ms  (%.2f ns/op)\n", ms, ms * 1e6 / ROUNDS);
    }

    // --- bitmap prototype, linear-scan size lookup (same as `ours`) ---
    {
        BitmapAlloc ba;
        ba.init();
        Rng rng{4141};
        std::vector<void *> live(LIVE_SET);
        for (auto &p : live) {
            p = ba.alloc(rng.size(), false);
            *(volatile char *)p = 'a';
        }
        auto t0 = std::chrono::high_resolution_clock::now();
        for (size_t i = 0; i < ROUNDS; i++) {
            size_t victim = rng.next() % LIVE_SET;
            ba.free(live[victim]);
            void *p = ba.alloc(rng.size(), false);
            *(volatile char *)p = 'a';
            live[victim] = p;
        }
        auto t1 = std::chrono::high_resolution_clock::now();
        for (auto p : live) ba.free(p);
        double ms = std::chrono::duration<double, std::milli>(t1 - t0).count();
        printf("bitmap + scan:      %.1f ms  (%.2f ns/op)\n", ms, ms * 1e6 / ROUNDS);
    }

    // --- bitmap prototype, O(1) lookup table -- isolates the scan's cost ---
    {
        build_class_lut();
        BitmapAlloc ba;
        ba.init();
        Rng rng{4141};
        std::vector<void *> live(LIVE_SET);
        for (auto &p : live) {
            p = ba.alloc(rng.size(), true);
            *(volatile char *)p = 'a';
        }
        auto t0 = std::chrono::high_resolution_clock::now();
        for (size_t i = 0; i < ROUNDS; i++) {
            size_t victim = rng.next() % LIVE_SET;
            ba.free(live[victim]);
            void *p = ba.alloc(rng.size(), true);
            *(volatile char *)p = 'a';
            live[victim] = p;
        }
        auto t1 = std::chrono::high_resolution_clock::now();
        for (auto p : live) ba.free(p);
        double ms = std::chrono::duration<double, std::milli>(t1 - t0).count();
        printf("bitmap + O(1) LUT:  %.1f ms  (%.2f ns/op)\n", ms, ms * 1e6 / ROUNDS);
    }

    return 0;
}
