// Sampling profile of the *production* Heap through the real C ABI, with
// in-process symbol+line resolution (DbgHelp), on larson's actual workload
// shape (random sizes 8..1000, live-set churn).
//
// Why this exists on top of selfprofile2.cpp: that one dumps raw RIPs to a
// 300 MB text file and leaves attribution to a manual dumpbin cross-walk.
// The open item in rfc/model/memory/heap-slot-allocation.md asks for
// attribution of the remaining ~2x gap against the fully-integrated Heap,
// which needs line-level answers, not addresses. Same
// SuspendThread/GetThreadContext technique (no local admin -> no ETW).
//
// Build (needs RUSTFLAGS="-C debuginfo=2" on the cargo build for lines):
//   cl /O2 /EHsc /std:c++17 /Zi selfprofile4.cpp /Fe:selfprofile4_ours.exe \
//      /link ll_model.lib dbghelp.lib ntdll.lib userenv.lib ws2_32.lib \
//      bcrypt.lib advapi32.lib
#include <cstdio>
#include <cstdint>
#include <cstring>
#include <windows.h>
#include <dbghelp.h>
#include <vector>
#include <atomic>
#include <map>
#include <string>
#include <algorithm>

#ifdef PROFILE_MIMALLOC
#include <mimalloc.h>
#define ALLOC(sz) mi_malloc(sz)
#define FREE(p) mi_free(p)
#else
extern "C" {
    void *ll_malloc(size_t size);
    void ll_c_free(void *ptr);
    bool ll_thread_init(void);
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
    if (!ll_thread_init()) {
        fprintf(stderr, "ll_thread_init refused: the runtime did not start this thread\n");
        return 1;
    }
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
    if (!hThread) { printf("CreateThread failed: %lu\n", GetLastError()); return 1; }

    std::vector<uint64_t> samples;
    samples.reserve(4'000'000);

    LARGE_INTEGER freq, t0, t1;
    QueryPerformanceFrequency(&freq);
    QueryPerformanceCounter(&t0);
    ResumeThread(hThread);

    CONTEXT ctx;
    ctx.ContextFlags = CONTEXT_CONTROL;
    while (!g_done.load()) {
        DWORD sc = SuspendThread(hThread);
        if (sc == (DWORD)-1) continue;
        if (GetThreadContext(hThread, &ctx)) samples.push_back(ctx.Rip);
        ResumeThread(hThread);
    }
    QueryPerformanceCounter(&t1);
    WaitForSingleObject(hThread, INFINITE);
    CloseHandle(hThread);

    double secs = double(t1.QuadPart - t0.QuadPart) / double(freq.QuadPart);
    // NB: wall time here is inflated by the sampler's own Suspend/Resume;
    // it is a sanity check, not the throughput number. Use larson for that.
    printf("collected %zu samples over %.2fs (sampler-perturbed)\n", samples.size(), secs);
    printf("workload: %zu rounds x (free+alloc), live set %zu, sizes %zu..%zu\n\n",
           ROUNDS, LIVE_SET, MIN_SIZE, MAX_SIZE);

    HANDLE proc = GetCurrentProcess();
    SymSetOptions(SYMOPT_LOAD_LINES | SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS);
    if (!SymInitialize(proc, nullptr, TRUE)) {
        printf("SymInitialize failed: %lu\n", GetLastError());
        return 1;
    }

    struct Bucket { size_t count = 0; std::string func; std::string file; DWORD line = 0; };
    std::map<std::string, Bucket> byLine;   // func+file+line
    std::map<std::string, size_t> byFunc;

    char symbuf[sizeof(SYMBOL_INFO) + MAX_SYM_NAME * sizeof(TCHAR)];
    PSYMBOL_INFO sym = (PSYMBOL_INFO)symbuf;
    sym->SizeOfStruct = sizeof(SYMBOL_INFO);
    sym->MaxNameLen = MAX_SYM_NAME;

    for (uint64_t rip : samples) {
        DWORD64 disp = 0;
        std::string fname = "<unresolved>";
        if (SymFromAddr(proc, rip, &disp, sym)) fname = sym->Name;

        IMAGEHLP_LINE64 li; li.SizeOfStruct = sizeof(li);
        DWORD ldisp = 0;
        std::string file; DWORD line = 0;
        if (SymGetLineFromAddr64(proc, rip, &ldisp, &li)) {
            const char *slash = strrchr(li.FileName, '\\');
            file = slash ? slash + 1 : li.FileName;
            line = li.LineNumber;
        }
        byFunc[fname]++;
        char key[512];
        snprintf(key, sizeof(key), "%s|%s:%lu", fname.c_str(), file.c_str(), line);
        auto &b = byLine[key];
        b.count++; b.func = fname; b.file = file; b.line = line;
    }

    auto pct = [&](size_t c) { return 100.0 * double(c) / double(samples.size()); };

    std::vector<std::pair<std::string, size_t>> fv(byFunc.begin(), byFunc.end());
    std::sort(fv.begin(), fv.end(), [](auto &a, auto &b) { return a.second > b.second; });
    printf("=== BY FUNCTION ===\n");
    for (size_t i = 0; i < fv.size() && i < 25; i++)
        printf("%6.2f%%  %8zu  %s\n", pct(fv[i].second), fv[i].second, fv[i].first.c_str());

    std::vector<Bucket> lv;
    for (auto &kv : byLine) lv.push_back(kv.second);
    std::sort(lv.begin(), lv.end(), [](const Bucket &a, const Bucket &b) { return a.count > b.count; });
    printf("\n=== BY SOURCE LINE ===\n");
    for (size_t i = 0; i < lv.size() && i < 40; i++)
        printf("%6.2f%%  %8zu  %s  (%s:%lu)\n", pct(lv[i].count), lv[i].count,
               lv[i].func.c_str(), lv[i].file.empty() ? "?" : lv[i].file.c_str(), lv[i].line);

    SymCleanup(proc);
    return 0;
}
