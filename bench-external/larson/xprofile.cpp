// Cross-process statistical sampler: launches a target exe (the real,
// unmodified larson.cpp binary against our allocator or a rival's),
// enumerates its threads, and repeatedly SuspendThread/GetThreadContext/
// ResumeThread every thread for a sampling window -- no admin needed,
// since it's a process we ourselves spawned (same user, same integrity
// level). This profiles the actual larson.cpp run under real conditions
// (varying sizes 8..1000, live-set churn), not a reimplementation.
#include <windows.h>
#include <tlhelp32.h>
#include <cstdio>
#include <cstdint>
#include <vector>
#include <string>

int main(int argc, char **argv) {
    if (argc < 4) {
        printf("usage: xprofile.exe <exe> <out.txt> <window_ms> [args...]\n");
        return 1;
    }
    const char *exePath = argv[1];
    const char *outPath = argv[2];
    int windowMs = atoi(argv[3]);

    std::string cmdline = std::string("\"") + exePath + "\"";
    for (int i = 4; i < argc; i++) {
        cmdline += " ";
        cmdline += argv[i];
    }
    std::vector<char> cmdbuf(cmdline.begin(), cmdline.end());
    cmdbuf.push_back('\0');

    STARTUPINFOA si{};
    si.cb = sizeof(si);
    PROCESS_INFORMATION pi{};
    if (!CreateProcessA(exePath, cmdbuf.data(), nullptr, nullptr, FALSE, 0, nullptr, nullptr, &si, &pi)) {
        printf("CreateProcess failed: %lu\n", GetLastError());
        return 1;
    }
    DWORD pid = pi.dwProcessId;

    // Give the target a moment to spawn its worker thread(s) (_beginthread).
    Sleep(300);

    std::vector<uint64_t> samples;
    samples.reserve(4'000'000);

    LARGE_INTEGER freq, t0, t1;
    QueryPerformanceFrequency(&freq);
    QueryPerformanceCounter(&t0);

    std::vector<DWORD> knownTids;
    std::vector<HANDLE> handles;
    LARGE_INTEGER lastEnum = t0;

    for (;;) {
        QueryPerformanceCounter(&t1);
        double elapsedMs = (double)(t1.QuadPart - t0.QuadPart) * 1000.0 / freq.QuadPart;
        if (elapsedMs > windowMs) break;

        double sinceEnumMs = (double)(t1.QuadPart - lastEnum.QuadPart) * 1000.0 / freq.QuadPart;
        if (handles.empty() || sinceEnumMs > 250.0) {
            // (Re-)enumerate threads: pick up any new worker threads,
            // drop stale handles. Kept infrequent -- this is the
            // expensive part (CreateToolhelp32Snapshot is a real syscall).
            for (auto h : handles) CloseHandle(h);
            handles.clear();
            knownTids.clear();

            HANDLE hSnap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if (hSnap != INVALID_HANDLE_VALUE) {
                THREADENTRY32 te{};
                te.dwSize = sizeof(te);
                if (Thread32First(hSnap, &te)) {
                    do {
                        if (te.th32OwnerProcessID != pid) continue;
                        HANDLE hThread =
                            OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT, FALSE, te.th32ThreadID);
                        if (!hThread) continue;
                        handles.push_back(hThread);
                        knownTids.push_back(te.th32ThreadID);
                    } while (Thread32Next(hSnap, &te));
                }
                CloseHandle(hSnap);
            }
            lastEnum = t1;
            fprintf(stderr, "[xprofile] re-enumerated: %zu threads for pid %lu\n", handles.size(), pid);
        }

        // Sample each thread, but SLEEP between passes -- suspending and
        // immediately re-suspending on the next loop iteration (no gap)
        // starves the target of any real run time between samples, which
        // is exactly what happened on the first attempt (100% of samples
        // landed in ntdll's thread-entry trampoline: the worker thread
        // never got far enough to run any real code). 1ms between passes
        // gives it a real slice to execute in before the next sample.
        for (auto hThread : handles) {
            DWORD sc = SuspendThread(hThread);
            if (sc == (DWORD)-1) continue;
            CONTEXT ctx;
            ctx.ContextFlags = CONTEXT_CONTROL;
            if (GetThreadContext(hThread, &ctx)) {
                samples.push_back(ctx.Rip);
            }
            ResumeThread(hThread);
        }
        Sleep(1);
    }
    for (auto h : handles) CloseHandle(h);

    // Let it finish naturally (or kill after grace period) -- we only
    // needed the sampling window, not the full run.
    TerminateProcess(pi.hProcess, 0);
    WaitForSingleObject(pi.hProcess, 2000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);

    printf("collected %zu samples over %dms\n", samples.size(), windowMs);
    FILE *f = fopen(outPath, "w");
    for (auto rip : samples) fprintf(f, "%llx\n", (unsigned long long)rip);
    fclose(f);
    return 0;
}
