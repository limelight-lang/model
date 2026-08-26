// Measurement probe: the retain/release pair against naive canaries,
// in one binary. What it measured and with which caveats:
// dev/BENCHMARKS.md, 2026-08-16, "the pair against its canaries"; the
// strategy behind it: dev/DECISIONS.md, "the performance case's
// external comparand is a canary, not a self-authored floor". The
// pair arms:
//
//   ll_pair       — the shipped pair through ll_retain/ll_release on a
//                   live ReferenceBox, count never reaching zero.
//   ll_pair_dup   — the same body compiled a second time at another
//                   address. Its difference from ll_pair is the
//                   instrument's measured zero: the threshold below
//                   which any other arm-to-arm difference in this
//                   binary is layout, not effect.
//   naive_pair    — a bare non-atomic pair: increment, decrement,
//                   branch to a never-taken cold death. A bound, not a
//                   runtime — it carries no flags test, no immortality
//                   gate, no null path. The pointer is laundered before
//                   each half so the compiler cannot fuse the pair —
//                   the shipped pair is two opaque calls, and the
//                   canary mirrors that shape rather than letting the
//                   optimizer delete it.
//   shared_pair   — the std::shared_ptr scope pattern a multi-threaded
//                   ARC pays: two locked counter operations plus the
//                   arm's own null-check destructor branch and
//                   dispatch, so the row prices the pattern, not
//                   atomics alone.
//   skeleton      — the loop with the cursor and the laundered pointer
//                   and no header op: bounds the harness term inside
//                   every figure above.
//
// A second group prices the entity lifecycle the same way —
// ll_create_die against malloc-init-free — and carries the plain-store
// canary whose counted comparand lives in the in-lib probe: the
// barrier cannot be reached from here, and the comment above the
// group says why.
//
// The ll arms cross the C ABI, so each half is a real call here while
// the production route inlines through merged bitcode (../../README.md,
// "LLVM IR export"): the bias runs against the ll arms, and a
// "within X of C" reading is conservative.
//
// Build (Linux):
//
//   cargo build --release
//   g++ -O2 -std=c++17 bench-external/canary/pair_canary.cpp \
//       -o bench-external/canary/pair_canary \
//       target/release/libll_model.a -lpthread -ldl -lm
//
// Acceptance is by disassembly, per arm — accept.sh beside this file,
// re-run after every rebuild: the naive arm keeps its inc, dec and
// branch; the skeleton keeps no header op; the ll arms keep both
// calls. A canary that lost its body to the optimizer reads as a
// floor and prices nothing.
//
// The harness copies src/memory/barrier/tests/
// what_a_store_costs_by_working_set.rs: 1000 pairs per timed region,
// 15 rounds after a discarded warm-up, arm order rotated by round
// index, min/median/max per arm, three passes bracketing drift. A
// harness rule changed there changes here too.

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <memory>
#include <thread>
#include <vector>

extern "C" {
void ll_thread_init();
void* ll_reference_new();
void ll_retain(void* entity);
bool ll_release(void* entity);
}

// No arm calls the store barrier, and the omission is a finding, not a
// choice: every barrier entry resolves a context and panics without
// one, and the C ABI has no door that constructs a context or an
// arena. Until it does, the counted publish is priced by the in-lib
// probe and the plain-store canary below pairs with it across
// instruments, both instruments' zeros being measured
// (dev/BENCHMARKS.md, 2026-08-16, "store and lifecycle canaries").

namespace {

constexpr size_t kPairs = 1000;
constexpr size_t kWide = 64;
constexpr size_t kRounds = 15;

// Hide a value from the optimizer, the trip-count rule of the in-lib
// probe: a constant bound is a different compilation.
size_t trip(size_t n) {
    asm volatile("" : "+r"(n));
    return n;
}

// Launder a pointer: the optimizer forgets what it knew about the
// pointee, so two halves of a pair stay two memory operations.
template <typename T>
T* launder_ptr(T* p) {
    asm volatile("" : "+r"(p));
    return p;
}

struct NaiveRc {
    uint32_t refcount;
    uint32_t flags;
};

// The never-taken death of the naive canary: out of line and cold, the
// shape a naive runtime gives it.
__attribute__((noinline, cold)) void naive_die(NaiveRc* o) {
    std::free(o);
}

using Nanos = double;

Nanos time_region(void (*body)(size_t)) {
    auto start = std::chrono::steady_clock::now();
    body(trip(kPairs));
    auto elapsed = std::chrono::steady_clock::now() - start;
    return std::chrono::duration<double, std::nano>(elapsed).count() / kPairs;
}

// Per-arm state: one array per arm, sized kWide, indexed by a mask so
// the single-child and wide sets run the same instructions.
void* ll_children[kWide];
void* ll_dup_children[kWide];
NaiveRc* naive_children[kWide];
std::shared_ptr<uint64_t> shared_children[kWide];
size_t g_mask;

void ll_pair_body(size_t pairs) {
    for (size_t i = 0; i < pairs; i++) {
        void* e = ll_children[i & g_mask];
        ll_retain(e);
        ll_release(e);
    }
}

// The duplicate of ll_pair_body, kept a distinct compilation so its
// figure differs from ll_pair only by code placement.
__attribute__((noinline)) void ll_pair_dup_body(size_t pairs) {
    for (size_t i = 0; i < pairs; i++) {
        void* e = ll_dup_children[i & g_mask];
        ll_retain(e);
        ll_release(e);
    }
}

void naive_pair_body(size_t pairs) {
    for (size_t i = 0; i < pairs; i++) {
        NaiveRc* o = launder_ptr(naive_children[i & g_mask]);
        o->refcount++;
        o = launder_ptr(o);
        if (--o->refcount == 0) {
            naive_die(o);
        }
    }
}

void shared_pair_body(size_t pairs) {
    for (size_t i = 0; i < pairs; i++) {
        std::shared_ptr<uint64_t> copy = shared_children[i & g_mask];
        copy.reset();
    }
}

// Compiles to the cursor and one pointer load per iteration — the empty
// asm markers fold, and that is the accepted shape (`accept.sh`): what
// this arm bounds is the loop skeleton every arm above carries, not the
// arms' two memory operations.
void skeleton_body(size_t pairs) {
    for (size_t i = 0; i < pairs; i++) {
        NaiveRc* o = launder_ptr(naive_children[i & g_mask]);
        asm volatile("" ::"r"(o));
        o = launder_ptr(o);
        asm volatile("" ::"r"(o));
    }
}

// ---- Store and lifecycle arms (figures: dev/BENCHMARKS.md,
// 2026-08-16, "store and lifecycle canaries").
// What the canaries do not carry, and therefore what the deltas price:
// no COW test, no destructor registration, no arena logging, no
// category test.

void* plain_store_slot;

// The counted store with the semantics stripped: one 8-byte write. The
// destination is laundered per iteration, or the compiler keeps only
// the last store of the region.
void plain_store_body(size_t stores) {
    for (size_t i = 0; i < stores; i++) {
        void** slot = launder_ptr(&plain_store_slot);
        *slot = naive_children[i & g_mask];
    }
}

// Create and kill a 24-byte entity through the factory and the full
// teardown path (kind-3 ReferenceBox: no destructor body, slot freed
// into the entity heap).
void ll_create_die_body(size_t cycles) {
    for (size_t i = 0; i < cycles; i++) {
        void* e = ll_reference_new();
        ll_release(e);
    }
}

// The lifecycle stripped to allocator work: same 24 bytes from glibc,
// a three-word init standing in for header, class and slot, and the
// free. No factory contract, no kind dispatch, no teardown order.
void malloc_free_body(size_t cycles) {
    for (size_t i = 0; i < cycles; i++) {
        uint64_t* p = static_cast<uint64_t*>(std::malloc(24));
        p = launder_ptr(p);
        p[0] = 1;
        p[1] = 0;
        p[2] = 0;
        std::free(p);
    }
}

struct Arm {
    const char* label;
    void (*body)(size_t);
};

Arm arms[] = {
    {"ll_pair", ll_pair_body},
    {"ll_pair_dup", ll_pair_dup_body},
    {"naive_pair", naive_pair_body},
    {"shared_pair", shared_pair_body},
    {"skeleton", skeleton_body},
    {"plain_store", plain_store_body},
    {"ll_create_die", ll_create_die_body},
    {"malloc_free", malloc_free_body},
};
constexpr size_t kArms = sizeof(arms) / sizeof(arms[0]);

// One pass: kRounds figures per arm after a discarded warm-up round,
// arm order rotated by round index, reduced to min/median/max. A null
// `pass` runs the whole pass and prints nothing — the process-level
// warm-up, so no discarded figure can be quoted.
void run_pass(size_t set, const char* pass) {
    std::vector<std::vector<Nanos>> taken(kArms);
    for (size_t round = 0; round <= kRounds; round++) {
        for (size_t position = 0; position < kArms; position++) {
            size_t index = (round + position) % kArms;
            Nanos figure = time_region(arms[index].body);
            if (round > 0) {
                taken[index].push_back(figure);
            }
        }
    }

    if (pass == nullptr) {
        return;
    }

    for (size_t a = 0; a < kArms; a++) {
        std::sort(taken[a].begin(), taken[a].end());
        std::printf(
            "pair_canary working_set=%zu pass=%s %s=%.3f ns/pair "
            "min=%.3f max=%.3f\n",
            set, pass, arms[a].label, taken[a][taken[a].size() / 2],
            taken[a].front(), taken[a].back());
    }
}

}  // namespace

int main() {
    // Leave the single-threaded world before anything is timed: glibc
    // branches shared_ptr's counter ops on __libc_single_threaded, and a
    // process that never spawned a thread would price the fast path
    // while the row claims the atomic pair. One spawned-and-joined
    // thread clears the flag for the rest of the process.
    std::thread([] {}).join();

    ll_thread_init();
    for (size_t i = 0; i < kWide; i++) {
        ll_children[i] = ll_reference_new();
        ll_dup_children[i] = ll_reference_new();
        naive_children[i] =
            static_cast<NaiveRc*>(std::calloc(1, sizeof(NaiveRc)));
        naive_children[i]->refcount = 1;
        shared_children[i] = std::make_shared<uint64_t>(i);
    }

    for (size_t set : {size_t{1}, kWide}) {
        g_mask = set - 1;
        run_pass(set, nullptr);
        run_pass(set, "1");
        run_pass(set, "2");
        run_pass(set, "3");
    }

    for (size_t i = 0; i < kWide; i++) {
        ll_release(ll_children[i]);
        ll_release(ll_dup_children[i]);
        naive_die(naive_children[i]);
    }

    return 0;
}
