// Admin-free statistical sampling profiler: no ETW/wpr available on this
// account (not a local admin, confirmed), so this samples our own worker
// thread's RIP via SuspendThread/GetThreadContext/ResumeThread -- legal
// on a thread you own, no privilege needed. Link with /DYNAMICBASE:NO so
// sampled RIPs match dumpbin's static addresses directly (no need to
// compute the runtime module base / relocate at sample time).
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <windows.h>
#include <vector>
#include <atomic>

extern "C" {
    void *ll_malloc(size_t size);
    void ll_c_free(void *ptr);
    void ll_thread_init(void);
}

static std::atomic<bool> g_done{false};
static const size_t N = 8'000'000;
static const size_t OBJ_SIZE = 64;

static DWORD WINAPI worker(LPVOID) {
    ll_thread_init();
    for (size_t i = 0; i < N; i++) {
        void *p = ll_malloc(OBJ_SIZE);
        *(volatile uint64_t *)p = i;
        ll_c_free(p);
    }
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

    FILE *f = fopen("samples.txt", "w");
    for (auto rip : samples) {
        fprintf(f, "%llx\n", (unsigned long long)rip);
    }
    fclose(f);
    return 0;
}
