// Probe-only: dump ll_memory_stats at exit, to size what a workload actually
// holds. Built by force-include alongside ll_malloc_shim.h.
#pragma once
#include <cstdio>
#include <cstdlib>
extern "C" {
    struct LLMemStats { size_t regions_carved, resident_bytes, blocks_out, active_bytes, blocks_free; };
    void ll_memory_stats(LLMemStats *out);
}
static void ll_dump_stats() {
    LLMemStats s{};
    ll_memory_stats(&s);
    printf("\n[stats] regions carved: %zu  (%.1f MiB resident high-water)\n",
           s.regions_carved, s.resident_bytes / 1048576.0);
    printf("[stats] blocks out: %zu  (%.1f MiB held)\n",
           s.blocks_out, s.active_bytes / 1048576.0);
}
struct LLStatsHook { LLStatsHook() { atexit(ll_dump_stats); } };
static LLStatsHook ll_stats_hook_instance;
