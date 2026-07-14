// Real sampling profile of the bitmap+O(1) prototype (bitmap_proto.cpp's
// best variant) on the identical larson-shaped workload, to find the
// next bottleneck after the two already-measured fixes. Same
// SuspendThread/GetThreadContext sampling technique as selfprofile2.cpp.
#include <cstdio>
#include <cstdint>
#include <windows.h>
#include <intrin.h>
#include <vector>
#include <atomic>

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

struct BitmapClass {
    uint8_t *base = nullptr;
    size_t class_size = 0;
    size_t slot_count = 0;
    std::vector<uint64_t> bitmap;

    void init(size_t cs, size_t arena_bytes) {
        class_size = cs;
        slot_count = arena_bytes / cs;
        base = (uint8_t *)VirtualAlloc(nullptr, arena_bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
        bitmap.assign((slot_count + 63) / 64, ~0ULL);
    }

    void *alloc() {
        for (size_t w = 0; w < bitmap.size(); w++) {
            uint64_t word = bitmap[w];
            if (word == 0) continue;
            unsigned long bit;
            _BitScanForward64(&bit, word);
            bitmap[w] = word & (word - 1);
            size_t idx = w * 64 + bit;
            return base + idx * class_size;
        }
        return nullptr;
    }

    void free(void *p) {
        size_t idx = (size_t)((uint8_t *)p - base) / class_size; // <-- suspected: integer division
        bitmap[idx / 64] |= (1ULL << (idx % 64));
    }
};

struct BitmapAlloc {
    BitmapClass classes[NUM_CLASSES];
    void init() {
        for (size_t i = 0; i < NUM_CLASSES; i++) {
            classes[i].init(SIZE_CLASSES[i] + 8, 8 * 1024 * 1024);
        }
    }
    void *alloc(size_t size) {
        int ci = size_class_index_o1(size);
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
static const size_t ROUNDS = 8'000'000;

static std::atomic<bool> g_done{false};
static BitmapAlloc *g_ba;

static DWORD WINAPI worker(LPVOID) {
    Rng rng{4141};
    std::vector<void *> live(LIVE_SET);
    for (auto &p : live) {
        p = g_ba->alloc(rng.size());
        *(volatile char *)p = 'a';
    }
    for (size_t i = 0; i < ROUNDS; i++) {
        size_t victim = rng.next() % LIVE_SET;
        g_ba->free(live[victim]);
        void *p = g_ba->alloc(rng.size());
        *(volatile char *)p = 'a';
        live[victim] = p;
    }
    for (auto p : live) g_ba->free(p);
    g_done.store(true);
    return 0;
}

int main() {
    build_class_lut();
    BitmapAlloc ba;
    ba.init();
    g_ba = &ba;

    HANDLE hThread = CreateThread(nullptr, 0, worker, nullptr, CREATE_SUSPENDED, nullptr);
    std::vector<uint64_t> samples;
    samples.reserve(4'000'000);

    ResumeThread(hThread);
    CONTEXT ctx;
    ctx.ContextFlags = CONTEXT_CONTROL;
    while (!g_done.load()) {
        DWORD sc = SuspendThread(hThread);
        if (sc == (DWORD)-1) continue;
        if (GetThreadContext(hThread, &ctx)) samples.push_back(ctx.Rip);
        ResumeThread(hThread);
    }
    WaitForSingleObject(hThread, INFINITE);
    CloseHandle(hThread);

    printf("collected %zu samples\n", samples.size());
    FILE *f = fopen("samples3_bitmap.txt", "w");
    for (auto rip : samples) fprintf(f, "%llx\n", (unsigned long long)rip);
    fclose(f);
    return 0;
}
