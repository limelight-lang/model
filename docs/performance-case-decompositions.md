# Hot-path decompositions: every instruction against its contract

Instruction-level accounting for the three mutator figures the
performance case quotes: the retain/release pair, the counted publish,
and the death branch. Each instruction of the shipped code is either
tied to a named contract sentence or listed as residue with the lead
that would remove it. A row with a citation is work the language
mandates; a residue row is the only place a claim of avoidable cost can
live.

> **The two build configurations these figures name were deleted on
> 2026-08-26.** `rc-walk` and `rc-trace` are gone from the crate, and the
> code that produced every measurement below is on the branch
> `archive/pre-rc-cycle`. The figures stand as the baseline `rc-cycle` is
> to be measured against, not as a description of what the tree builds
> today (`PLAN.md`, S30).


Produced from the release staticlib built with `-C debuginfo=2`
(`bench-external/README.md`, "Symbol resolution for profiling"), read through
`objdump -dSr`, on the HEAD of the entry "fresh brackets on one HEAD"
(`dev/BENCHMARKS.md`, 2026-08-16). The listings are x86-64. Figures are
not repeated here — `dev/BENCHMARKS.md` stays the normative home of
every number, and this file cites entries by title.

One distinction governs the whole file. The C ABI door is the
**generic** form: the memory category arrives as a runtime argument and
the category layer is a call. Production code inlines through merged
bitcode and receives **specialized** forms with the category constant
folded (`rfc/model/gc/strategies.md`, "the compiler emits a
*specialized* form with the category check gone"). Where a row exists
only in the generic form, the row says so.

## The pair, retain half: `ll_retain`

```
mov    0x4(%rdi),%ecx        ; one 4-byte load of the flags half
mov    %ecx,%edx
and    $0x3,%edx             ; memory-category field, bits 0-1
setne  %al
test   $0x400,%ecx           ; COW, bit 10
sete   %cl
cmp    $0x3,%edx             ; Immortal
je     ret
and    %cl,%al               ; category != 0 and not COW -> uncounted
jne    ret
incl   (%rdi)                ; the narrow 4-byte counter increment
ret
```

| instructions | contract |
|---|---|
| `mov 0x4(%rdi),%ecx` | the width rule: a header is read as narrowly as it is written (`dev/DECISIONS.md`, "a header is read as narrowly as it is written, and through the helpers only"; `rfc`'s `archive/pre-rc-cycle`, `model/gc/rc-walk.md`, "The narrow mutator") |
| `and $3` / `setne` | the category field, flags bits 0-1: a non-zero category is not lifetime-counted (`refcount.rs`, `MemoryCategory` contract; `rfc/model/classes.md`, "Flags layout") |
| `test $0x400` / `sete` | the COW exception: a COW entity always counts (`rfc/model/values.md`; `refcount.rs`, the `COW` flag's contract) |
| `cmp $3` / `je` | Immortal is never counted: the pinned-count rule of `rfc/model/values.md`; the consequence — a count sitting at 1 forever — is spelled out at `refcount.rs`, `cow_separation_needed`'s Immortal arm |
| `and %cl,%al` / `jne` | the two tests above fused into one branch — the row inherits the category and COW citations; the fusion itself is codegen |
| `incl (%rdi)` | the counter itself: one narrow store, no flags half, no read-modify-write atomic — the single-mutator guarantee (`rfc`'s `archive/pre-rc-cycle`, `model/gc/rc-walk.md`, "What the mutator pays") |

Residue: none. Twelve instructions, each carrying a citation.

## The pair, release half and the death branch: `ll_release`

```
mov    0x4(%rdi),%eax        ; the same four-test gate as retain
...the gate's flag computation...
xor    %eax,%eax             ; return-false default, set before the gate branches
...the gate's branches...
mov    (%rdi),%eax           ; narrow counter load
dec    %eax
mov    %eax,(%rdi)           ; narrow counter store
or     %ecx,%eax             ; count == 0 AND category == GcHeap, fused
je     death
xor    %eax,%eax             ; non-final: report "do not tear down"
ret
death:
movzbl HANDSHAKE_REQUESTED,%ecx  ; the checkpoint's ack test
mov    $0x1,%al
test   %cl,%cl
jne    ack                   ; cold tail below
ret                          ; caller owns teardown
ack:
push   %rax                  ; the cold tail's call frame
call   ack_handshake         ; .text.unlikely
mov    $0x1,%al              ; the return the call clobbered
add    $0x8,%rsp
ret
```

| instructions | contract |
|---|---|
| the gate | as in `ll_retain` |
| the early `xor %eax,%eax` | the uncounted paths report false, and codegen hoists the default above the gate's branches — register scheduling, no memory access |
| `mov` / `dec` / `mov` | the relaxed-atomic demand: the header is read by the collector's thread, so the counter halves compile as relaxed atomic load and store (`rfc`'s `archive/pre-rc-cycle`, `model/gc/rc-walk.md`, "One demand on codegen"). What is observed, not claimed of compilers in general: this build emits the pair unfused, while the rc-trace build — plain accesses over the same source shape — emits a narrow `decl [mem]` (`dev/BENCHMARKS.md`, 2026-07-27). The two extra instructions are the annotation's observed price, and the annotation is the contract |
| `or %ecx,%eax` / branch | only a `GcHeap` count reaching zero dies by count — arena entities die at reset (`rfc/model/memory/arena-reset.md`); the two conditions fuse into one flags-setting `or` |
| `movzbl` / `test` / `jne` | the checkpoint rides the death branch and acks the handshake only (`rfc`'s `archive/pre-rc-cycle`, `model/gc/rc-walk.md`, "Both ride the death branch of `ll_release`" and the ack/pickup split); its measured price is the ≈ 1.1 ns of the 2026-07-27 decomposition |
| the `ack` tail | out of line and cold — a handful per epoch, never per operation; its `push`/`add` frame and re-set return value execute only when a handshake is posted |
| `mov $1,%al` / `ret` | teardown is the caller's (`ll_object_die`), bounded by deaths, not by store traffic |

Residue: none on the branch itself. Two scope lines, so a reader
summing rows against measured figures knows what sits outside them:
teardown past the branch is not decomposed here, priced by garbage
found; and the free path's parking test — one load and a predicted
branch while an epoch is in flight (`rfc`'s `archive/pre-rc-cycle`, `model/gc/rc-walk.md`,
"Deferred physical release") — is inside every measured create+die
figure and inside no listing in this file, part of the ≈ 1 ns
factory/free share of the 2026-07-27 decomposition.

## The counted publish: `store_ptr`, generic form

The full generic body has three outcomes, and the listing shows all
three: publish `new`; publish the copy the category layer substituted,
giving `new`'s retain back; or refuse, un-retaining `new`.

```
test   %rcx,%rcx             ; PHP null publishes nothing
je     publish-null
...retain(new), the ll_retain body inlined...
call   store_category_barrier ; generic form only
test   %rax,%rax
je     refusal
cmp    %r14,%rax
je     publish               ; barrier kept new
...release(new), the ll_release body inlined...   ; barrier substituted a copy
publish:
mov    %rax,(%rbx)           ; the slot store: one 8-byte write
mov    $0x1,%al
ret
publish-null:
xor    %eax,%eax             ; the slot takes 0, nothing was retained
jmp    publish
refusal:
...release(new), the ll_release body inlined, ack tail included...
xor    %eax,%eax             ; report false, every slot unchanged
ret
```

| instructions | contract |
|---|---|
| `test %rcx,%rcx` | a pointer slot holds `0` for PHP `null` — "the `ptr` form counts simply when the pointer is non-null" (`rfc/model/gc/strategies.md`, "The store barrier, as micro-operations") |
| inlined retain | as `ll_retain`, row for row |
| `call store_category_barrier` | the category layer: cross-arena check, escape count, release log (`rfc/model/memory/arenas.md`). **Generic form only** — the specialized heap-into-heap form folds the category and loses the call and the layer |
| the `stored != new` release | the barrier may substitute an escape copy for a COW value crossing out of the arena, and the original's creation retain is given back (`dev/DECISIONS.md`, "a COW value is copied out of the arena, and the store barrier can say no") |
| the refusal path | a store refuses for either of two causes — a log record it cannot fund (`rfc/runtime/exceptions.md`, "The log reserve protocol") or an escape copy it cannot make (`dev/DECISIONS.md`, "a COW value is copied out of the arena, and the store barrier can say no") — and the un-retain is a full release, death branch included |
| `mov %rax,(%rbx)` | the publish itself: the slot is the only door a strategy observes mutation through (`rfc/model/gc/strategies.md`, "The store barrier, as micro-operations") |

Residue, both entries with the same lead, and the lead split into its
observed and its designed half: the `push`/`pop` frame (three
registers) and the call into `store_category_barrier` exist because
this is the out-of-line ABI door. **Observed**: `ll_retain` inlines
away after `opt -O2` over merged bitcode (`README.md`, "LLVM IR
export" — that verification ran on Rust 1.87 / LLVM 20.1,
`x86_64-pc-windows-msvc`, an earlier toolchain and another OS than
today's builds, and has not been re-run since). **Designed, not yet observed**: the
specialized store form with the category folded and the layer's call
gone (`rfc/model/gc/strategies.md`) — no compiler emits it yet, and
the case document must carry it as design, not fact. The canary entry
measured the door's bias direction meanwhile: through a real call the
pair reads 0.11–0.14 above the in-crate figure (`dev/BENCHMARKS.md`,
2026-08-16, "fresh brackets on one HEAD").

## What this file proves, and what it cannot

Every instruction on the three paths is named. Every named instruction
carries a contract citation, except three groups that carry an account
instead: the out-of-line doors' call frames (residue, with the
inlining lead), the cold ack tail's frame (executed only under a
posted handshake), and one hoisted return-false default (register
scheduling). On these paths there is no known avoidable work. The file
does not prove the contracts themselves cheap — that is the canary
bracket's job — and it does not price the paths: figures live in
`dev/BENCHMARKS.md`, with their instruments and resolutions beside
them.
