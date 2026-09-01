# Project terminology audit

Status: cross-repository audit and migration handoff, 2026-09-01. This report
covers the model crate and the active RFC tree. The detailed `cycle` mapping is
in `dev/CYCLE-TERMINOLOGY-AUDIT.md`.

Authority: `../rfc/dev/GLOSSARY.md`. Its canonical table governs new text;
deprecated terms are migration input. Historical decisions, dated plan
records, archived RFCs, and explicitly superseded analysis retain the words
they recorded.

## Findings

### 1. Allocation and operation results

`refusal` is the largest cross-cutting ambiguity. It currently denotes an
allocation failure, unsupported placement, hash-table admission denial,
capacity exhaustion, an unavailable journal, and a value that remains in its
source arena.

Required contextual names include:

| Current | Replacement |
| --- | --- |
| `InsertOutcome::RefusedForMemory` | `InsertOutcome::AllocationFailed` |
| `InsertOutcome::RefusedByLadder` | `InsertOutcome::AdmissionDenied` |
| `Placement::Refused` | `Placement::Unsupported` |
| `ExternalCarry::Refused` / `OutsideCarry::Refused` | a result stating that storage remains in its source block |
| `Window::Refused` | a journal-specific unavailable/unobserved-thread result |

`door` is similarly overloaded across allocation paths, ABI entry points,
barriers, channels, and OS resources. Replace it only after classifying each
site. The canonical alternatives are `allocation path`, `entry point`,
`store-barrier form`, `channel`, `ownership boundary`, and the exact OS
resource.

### 2. Candidate collection

The model crate still exposes `enrol`, `ENROLLED`, `enrolment gate`, `root
queue`, `escrow`, `floor`, `parking`, and judicial validation terms. The full
mapping is in `dev/CYCLE-TERMINOLOGY-AUDIT.md`; direct callers in `refcount`,
`gc`, `object`, `memory`, and current API maps must move in the same commit.

The RFC question graph mixes active contracts with dated reasoning and still
uses the retired vocabulary extensively. It cannot be globally rewritten:
quoted rulings and exact historical headings must remain searchable, while
normative clauses and unresolved questions must use canonical terms. The file
needs a section-aware S9.2 pass.

The glossary's earlier candidate-age definition was factually wrong. Candidate
age is not a count of epochs: it is the saturating component age assigned at a
validation commit, scoped by a distinct epoch stamp.

### 3. Lifecycle phases

`death`, `destructor`, `teardown`, `dispose`, `drop`, and `reclamation` are
used interchangeably in object, weak-reference, arena, FFI, and exception
documents. The vocabulary set is: zero-count transition, user destructor
(`__destruct`), field/resource teardown, weak-reference invalidation, and
storage reclamation. It is not one universal ordering.

Ordinary object teardown invalidates weak references before releasing child
fields. Cycle finalization invalidates weak references for every confirmed
member before the first user destructor, then revalidates before severing and
reclamation. Arena reset has its own ordering contract.

`cycle finalization` names the complete guarded protocol for a validated
unreachable component. An ABI identifier such as `dispose` may remain, but
prose must state that it orchestrates user-destructor invocation and
field/resource teardown. In particular, `drop` is not the "real destructor"
and `__destruct` is not a "pre-destructor".

### 4. Entity and FFI vocabulary

`entity` is reserved for managed, header-bearing allocations. Calling
`#[FFI]` an `unmanaged entity` contradicts that definition because the value
has no `RcHeader` or runtime class pointer.

Use `headerless FFI value`. State C layout or ABI compatibility separately;
`layout-transparent` is not a safe blanket replacement because it has a
narrower established ABI meaning. The existing `zero-abstraction.md` filename
and incoming links require a dedicated migration rather than an isolated file
rename.

### 5. Hash-table collision defense

`flood ladder`, `rung`, and `trigger` form an undocumented metaphorical
subsystem in `model/maps.md`, `model/arrays-hashtable.md`, the crate's table
code, and tests. Replace them with literal state and events:

- collision-defense state;
- chain-length threshold;
- equal-hash threshold;
- salted rebuild;
- keyed-hash escalation;
- terminal admission denial / collision-limit error.

This migration must preserve the difference between allocation failure and a
catchable denial of a new key.

### 6. Ownership vocabulary

Bare `owner` denotes at least a containing entity, owning mutator, heap-block
owner, lifetime anchor, and unique-ownership proof. Cross-module contracts must
qualify it. Local variables may remain short where the type makes the role
unambiguous.

### 7. Memory categories

`MemoryCategory::LongLived` is known to promise a lifetime and reclamation
mechanism the crate does not implement. Do not mechanically rename it. Its
replacement is gated by the design of region ownership and reclamation.

`arena promotion` remains a valid memory-management term. It must not be
globally replaced merely because candidate maturation formerly used
`promote`/`mature`.

### 8. Representation and platform words

`native` must be resolved to `machine code`, `standard PHP`, `machine stack`,
or `foreign code`. `scalar` remains valid for the PHP type family, but layout
text must use `immediate value`, `non-pointer value`, or the exact primitive
type when representation is the subject.

### 9. Active versus historical documents

Do not partially modernize historical material:

- completed and dated `progress`/`repair`/`done`/`handoff` records;
- `dev/DECISIONS.md` and benchmark records;
- archived or explicitly historical RFCs;
- exact citation headings.

`docs/architecture.md` in the model repository deliberately retains obsolete
collector diagrams behind a warning banner. It should eventually be redrawn
as a whole, not terminology-cleaned line by line.

The active RFC `model/gc/heap-design.md` still presents superseded collector
coordination as current. This is a status/architecture defect already recorded
as algorithm-audit issue B2; terminology cleanup must not make that section
look authoritative without resolving or removing it.

## Changes already made in the RFC working tree

- split the glossary into canonical, deprecated, and context-sensitive terms;
- corrected candidate age and exact-validation definitions;
- reserved `entity` for header-bearing managed allocations;
- added literal lifecycle, hash-defense, ownership, and FFI vocabulary;
- updated the active strategy summary to candidate registration, synchronous
  collection, allocation failure, collector worker, and deferred slot reuse;
- corrected the leading FFI definitions and lifecycle phase names;
- corrected unambiguous US-spelling and `native PHP` occurrences.

These changes do not close S9.1 or S9.2. The active RFC set still contains
deprecated terminology, especially the question graph, lifecycle documents,
and hash-table design.

## Migration order

1. Review and ratify the glossary; keep S9.1 open until every canonical term
   has an unambiguous definition and every deprecated term has a replacement.
2. Apply the `cycle` mapping to the model crate and current API maps.
3. Rewrite active candidate-collection RFC clauses, preserving historical
   quotations and headings.
4. Migrate allocation outcomes and `door` sites by semantic class.
5. Migrate lifecycle prose and headings, then repair links.
6. Rename the hash-table collision-defense state and outcomes in one RFC/code
   change.
7. Perform the FFI filename/link migration and the remaining platform-word
   cleanup.
8. Run source terminology audits, RFC link checking, the model test gate, and
   separate Sage/Critic review for each group.

No terminology commit may also change layout, allocation, synchronization, or
algorithm behavior.
