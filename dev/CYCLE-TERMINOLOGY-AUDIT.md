# Cycle terminology audit

Status: synchronized with `rfc/dev/GLOSSARY.md` at `rfc` commit `0075ef3` on
2026-09-01, under `model/PLAN.md` S41.1. The tables below state the ratified
names, so a rename step reads them directly — with one exception, the four
`refcount` rows under "Candidate registration in `refcount`", which S41.3
derived from a ratified row and which say so where they stand. This document changes no code.
Ratifying `rfc/dev/PLAN.md` S9.1 stays with that repository; the glossary is
authoritative here, and this mapping follows it if it moves again.

Scope: `src/cycle`, its tests, direct callers, and active API maps. Historical
records are out of scope.

## Rules

1. Name the state or operation, not a metaphor. `door`, `floor`, `escrow`,
   `debt`, `climb`, `meet`, and judicial language do not describe machine
   state.
2. Keep established collector terms: mark, scan, trial deletion, shadow row,
   working count, root, edge, component, trace, and candidate.
3. Use `ordinary allocation path`, `block pool`, and `critical reserve` for
   allocation. A request fails or returns null; a source is never a door.
4. Distinguish a logical row locator (block/index/population) from the
   shadow-row word address it resolves to.
5. Distinguish allocation failure from an algorithmic negative result.
6. Follow the RFC's US spelling in new names and prose.

## Glossary check, 2026-09-01

Every row of every table below was ruled against the glossary at `rfc` commit
`0075ef3`, except the four rows S41.3 derived, which that section marks. A row the glossary confirms carries no mark, and the tables state
the ratified name. Five rows were amended and the amendment is applied in
place; four outcomes have no glossary entry and stay here as gaps for
`rfc/dev/PLAN.md` S9.1.

Amended against the draft:

- `escrowed_count` takes `overflow_len`, the production name, rather than a
  second noun for one count (writing rule 3).
- the module `parking` becomes `deferred_slot_reuse`. The canonical term is
  *deferred slot reuse*, and `deferred_reuse` drops the object that
  `DEFERRED_SLOTS` and `deferred_slot_count` keep.
- `TraceWindow` becomes `ActiveTrace`. `TraceGuard` collides with the
  canonical *guard reference*, which the validation table spends on the `+1`
  strong reference held during cycle finalization.
- the `memory-manager door` bullet resolves to an allocation path or an entry
  point. The glossary's *door* entry is a closed list — allocation path, entry
  point, mailbox, channel, store-barrier form — and *ownership boundary* is
  not on it.
- `group_is_met` reads the *row-initialization bitmap*. "Visited bitmap" was a
  third name for a structure the glossary already names.

Gaps, for the glossary to answer rather than this audit:

- a result whose storage stayed in its source block, `ExternalCarry::Refused`
  and `OutsideCarry::Refused`. The glossary's *refusal* list covers a rejected
  design, an allocation failure, an admission denial, an unsupported placement
  and a capacity limit; this outcome is none of them.
- the journal's unobserved-thread result, `Window::Refused`
  (`src/journal/mod.rs`).
- `ResetWindow::escrow` (`src/memory/reset_window.rs`), which names deferred
  count corrections. *Escrow* is deprecated and its replacement, *overflow
  buffer*, is false for this sense.
- the prose sense of *enrolment* for attaching a block to the sweep list
  (`src/cycle/arena.rs`). Its replacement, *candidate registration*, names a
  different operation.

Identifiers the glossary reaches that no table below covers, and which the
rename steps carry anyway:

- `shadow::colour`, `shadow::recolour` and `testing::row_colour`. The US
  spelling rule reaches the functions, not only the type `Colour`.
- the row-initialization bitmap's own accessors — `groups`, `group_bit` and
  `group_bytes` in `shadow.rs` — and the phrase "met bitmap" in `arena.rs` and
  in the mark tests.

## Proposed mapping

### Candidate queue

The permanently held manager block is the queue's **base block**. Its first
cache line contains the owner state; the remainder is the bounded **overflow
buffer** required by the RFC glossary. `queue_base` is more precise than
`overflow_block`: the allocation owns the queue state as well as overflow
entries.

| Current | Proposed | Reason |
| --- | --- | --- |
| `floor_of` | `queue_base_of` | returns the owner state's base block |
| `draw_floor` | `try_ensure_queue_base` | idempotently returns an existing base or tries to allocate one |
| `draw_floor_or_abort` | `ensure_queue_base_or_abort` | states the terminal policy |
| `take_floor` | `initialize_queue_base` | thread initialization reports success |
| `release_floor` | `release_queue_base` | ends the lifetime-held allocation |
| test helper `floor` | `queue_base` | canonical noun |
| `ESCROW_ENTRIES` | `OVERFLOW_CAPACITY` | exact capacity of the overflow buffer |
| `escrowed` | `overflow_len` | initialized overflow entries |
| `escrow_entries` | `overflow_entries` | address of overflow storage |
| `escrow` | `append_to_overflow` | names the mutation |
| `drain_escrow` | `drain_overflow` | moves overflow entries into segments |
| `escrowed_count` | test-only `overflow_len` | the production name; one noun for one count |
| `live` | `write_segment` | the append target, not a liveness state |
| `filled` | `write_len` | initialized entries in `write_segment` |
| `held` | `spare_count` | initialized entries in `spares` |
| `OWNER` | `OWNER_STATE` | TLS stores a state locator |
| `owner` | `owner_state` | returns the state pointer |
| `owner_ref` | `owner_state_ref` | converts that pointer to a reference |
| `entries` | `segment_entries` | distinguishes segment and overflow arrays |
| `grow_and_write` | `append_with_new_segment` | links a segment and appends the candidate |
| `is_short` | `needs_spares` | condition tested by the poll |
| `replenish` | `refill_spares` | states what is refilled |
| `drain` | `release_queue_segments` | releases segments and spares, but not the base block |
| `spares_held` | test-only `spare_count` | matches production state |
| test helper `live_segment` | `write_segment` | matches production state |
| test helper `fill_live_segment` | `fill_write_segment` | matches production state |
| test helper `live_entry` | `write_segment_entry` | matches production state |
| `enrol` | `register_candidate` | RFC term is candidate registration |
| `ENROLLED` | `CANDIDATE_BIT` | prevents duplicate candidate registration |
| `enrolled_count` | test-only `candidate_count` | counts candidate entries |

Keep `OwnerCycleState`, `segment`, `spare`, and `POLL_STRIDE`. Renaming
`OwnerCycleState` should wait until the persistent per-thread GC state exists;
otherwise a queue-only name would immediately become false.

The module contract should describe exactly three storage paths:

1. append to the current write segment;
2. acquire a new segment from a spare or the critical reserve;
3. append to the base block's bounded overflow buffer.

### Candidate registration in `refcount`

**Derived under S41.3, not ratified with the tables above.** The glossary
rules on the bit and the verb; these four are that ruling carried to the gate
composed from the bit and to the three functions that reach it. `refcount`
cannot carry `register_candidate` beside `ENROLMENT_GATE_MASK` without leaving
half a rename, and the rename had to land in the same commit as the queue's.

| Current | Proposed | Reason |
| --- | --- | --- |
| `ENROLMENT_GATE_MASK` | `CANDIDATE_GATE_MASK` | the gate the bit is composed into |
| `may_enrol` | `may_become_a_candidate` | the answer is about the entity, not the caller |
| `is_enrolled` | `is_registered_candidate` | reads the bit, and matches the verb |
| `clear_enrolled` | `clear_candidate_bit` | takes the bit down, which is the narrower act |

### Row resolution

| Current | Proposed | Reason |
| --- | --- | --- |
| `Row` | `RowKey` | block/index/population identity, not a row address |
| `Population::Sole` | `Population::SingleEntity` | describes the population directly |
| `SOLE_OCCUPANT` | `SINGLE_ENTITY_INDEX` | matches the population variant |
| `Edge` | `EdgeTarget` | result of target classification |
| `Edge::Interior(Row)` | `EdgeTarget::Tracked(RowKey)` | a shadow row exists for the target |
| `Edge::External` | `EdgeTarget::Untracked` | also covers an unplaceable retained address |
| `edge_to` | `resolve_edge_target` | resolution, not navigation |

Keep `Slotted` and `Retained`. `Untracked` must retain the conservative rule:
stop descent and treat the edge as an external live reference.

### Trace scratch and shadow rows

| Current | Proposed | Reason |
| --- | --- | --- |
| `ShadowArena` | `TraceScratchArena` | RFC term; owns rows, bitmaps, and worklist segments |
| `Met` | `RowLookup` | result of ensuring a shadow row |
| `Met::Row` | `RowLookup::Ready` | row is initialized and usable |
| `Met::Unplaced` | `RowLookup::Untracked` | no safe row can be located; preserve conservatively |
| `Met::Refused` | `RowLookup::AllocationFailed` | both allocation paths returned no block |
| `first_reach` | `first_visit` | standard graph-traversal term |
| `ShadowArena::meet` | `TraceScratchArena::ensure_row` | initializes on first use, otherwise locates |
| `met_row` | `find_initialized_row` | read-only lookup; never initializes |
| private `ShadowArena::enrol` | `allocate_and_attach_row_array` | includes allocation, initialization, and list attachment |
| `sweep_touched` | `clear_touched_rows` | clears published shadow pointers/words |
| `RowArray::slots` | `RowArray::row_count` | retained arrays count occupants, not slots |
| `Colour` | `Color` | RFC US spelling |
| `Colour::Met` | `Color::Unclassified` | initialized working count before scan classification |
| `meet_group` | `ensure_group_initialized` | zeroes a group on first visit |
| `group_is_met` | `group_is_initialized` | reads the row-initialization bitmap |

Keep `Untouched`, `Live`, `shadow`, `row`, `working_count`, and `touched`.
Replace `Condemned` under exact-validation terminology below.

Do not call the current scratch arena `CycleWorkspace`. The planned persistent
per-thread workspace has a different owner and lifetime.

### Mark and scan

| Current | Proposed | Reason |
| --- | --- | --- |
| `Marked` | `MarkResult` | result type, not a past-tense entity |
| `Marked::Refused` | `MarkResult::AllocationFailed` | precise abort class |
| `Scanned` | `ScanResult` | result type, not a past-tense entity |
| `Scanned::Refused` | `ScanResult::AllocationFailed` | precise abort class |
| `meet_root` | `schedule_root_if_unvisited` | ensures its row and conditionally pushes it |
| `decide` | `classify_and_schedule_entity` | colors an entity and may push it |
| `from_live` | `reached_from_live` | boolean reads as a condition |
| `met_row_of` | `find_initialized_row_for_entity` | entity resolution plus row lookup |

Keep `mark`, `scan`, `visit_child`, `Complete`, `TraceStack`, `push`, `pop`,
and `reset`.

### Trace stack

| Current | Proposed | Reason |
| --- | --- | --- |
| `top` | `current` | current writable/readable segment |
| `used` | `current_len` | initialized entries in `current` |
| `below` | `previous` | link toward shallower segments |
| `above` | `next` | retained segment for the next deeper crossing |
| `climb` | `advance_segment` | acquires or reuses the next segment |
| `segments_held` | test-only `segment_count` | direct measurement name |

### Deferred slot reuse

The RFC glossary rejects `parking` for this lifecycle operation. The module
takes the canonical term whole and is named `deferred_slot_reuse`.

| Current | Proposed | Reason |
| --- | --- | --- |
| module `parking` | `deferred_slot_reuse` | the glossary's *deferred slot reuse* |
| `TraceWindow` | `ActiveTrace` | RAII holder of trace state and delayed reuse; *guard* is spent on guard references |
| `ACTIVE` | `TRACE_ACTIVE` | state being tested |
| `PARKED` | `DEFERRED_SLOTS` | delayed slot returns |
| `park_if_active` | `defer_reuse_if_tracing` | observable operation and condition |
| `parked_count` | test-only `deferred_slot_count` | direct measurement |
| `dispose` | `dispose_thread_state` | thread-exit lifecycle boundary |

This audit does not approve `Box<Vec<*mut u8>>`. Replacing it with
manager-owned GC memory is a separate structural change and must not be hidden
inside a rename commit. That change is `PLAN.md` S36.9 slice (c), and it
collapsed the two rows above it into one: the flag and the list were one state
in two declarations, and the head block's address is now both, under
`DEFERRED_RETURNS`. The rows record what S41 ratified; the crate's guard reads
the later name.

### Exact validation

| Current | Proposed | Reason |
| --- | --- | --- |
| module `exact` | `validation` | RFC term for owner-thread exact validation |
| `Judged` | `ValidationResult` | result of exact validation |
| `Judged::Condemned` | `ValidationResult::Unreachable` | component is eligible for finalization |
| `Judged::Corpse` | `ValidationResult::ZeroCountMember` | a member already reached zero |
| `Judged::Acquitted` | `ValidationResult::ExternallyReferenced` | current external reference exists |
| `judge` | `validate_component` | owner-thread validation operation |
| `discount` | `guard_refs_per_member` | guard references subtracted per member |
| `references` | `total_refcount` | sum being compared |
| `guards` | `guard_refcount` | total guard contribution |
| `every_member_holds_its_own_share` | `member_counts_cover_internal_edges` | checked inequality |
| `Colour::Condemned` | `Color::PotentiallyUnreachable` | scan proposal, not an exact-validation result |

`ValidationResult::Unreachable` is still a validation result, not permission
to reclaim without the existing finalization protocol. Comments must preserve
that boundary.

### Memory-manager terminology

The current `gc_metadata` implementation has aggregate block and byte
accounting; it has no per-role enum. Do not introduce or rename
`GcBlockRole::{QueueFloor, WorkspaceOverflow}` as part of this work.

Rewrite its remaining metaphors contextually:

- `memory-manager door` -> `memory-manager allocation path`, or
  `memory-manager entry point` where the site is the return API;
- `live position` -> `write position` when it names the queue segment;
- `escrow landing` -> `overflow-buffer append`;
- `floor control line` -> `queue-base control line`;
- `arena block` -> `trace-scratch block` where that is the owner.

The critical local comments are publication and accounting contracts, not the
old metaphors: accounting precedes GC-kind publication, and release accounting
precedes return across the manager boundary.

## Comment rewrite

Follow `dev/WORKFLOW.md`, "Comment standard". Comments should carry contracts
and local facts, not the history or argument that led to them.

Keep locally:

- safety preconditions and ownership/lifetime boundaries;
- no-allocation/no-lock candidate registration;
- candidate-bit publication before queue append;
- clear-shadow-before-slot-reuse ordering;
- pool-then-critical-reserve allocation order and exact failure result;
- GC kind publication and accounting order at manager boundaries;
- layout, capacity, alignment, and initialized-length facts;
- citations to the named RFC or decision section that owns an argument.

Delete or reduce to a citation:

- dates and change history (`used to`, `arrives at S...`, old measurements);
- repeated explanations of rejected alternatives;
- repeated pool/reserve prose already stated at module level;
- prose that merely restates a field or branch;
- rhetorical phrases such as `door`, `floor`, `escrow`, `debt`, `stands on`,
  `owes`, `acquit`, `condemn`, and `judge`.

Each module header should state, briefly: purpose; ownership/lifetime;
allocation/failure behavior; ordering invariants; named design references.
Item documentation should normally be one to five lines. Longer safety and
cross-change invariants are justified; historical narrative is not.

Test comments should describe the invariant and constructed state. Rename test
files, functions, helpers, and assertions that use retired production terms.
Do not globally replace ordinary English: `live`, `held`, and mathematical
`floor` remain correct in other contexts.

## Documentation boundary

Update only forward-looking, non-historical material:

- unchecked requirements and future handoffs in `PLAN.md`;
- current API maps in `dev/ARCHITECTURE.md` and `dev/INDEX.md`;
- active memory-manager documentation and API comments;
- direct callers in `gc`, `memory`, `object`, and `refcount`.

Do not rewrite:

- completed `[x]` plan items;
- dated `progress`, `repair`, `done`, and `handoff` records;
- `dev/DECISIONS.md` entries;
- benchmark records describing the measured code;
- archived documents;
- historical RFC rulings and their exact citation headings.

When active prose cites an old heading, preserve the heading exactly and add
the current name outside the citation.

`ResetWindow::escrow` is outside `cycle` and denotes deferred count
corrections, not queue overflow. It needs a separate glossary mapping such as
`deferred_corrections`; never apply `overflow_buffer` to it globally.

## Application order

1. Done 2026-09-01: the mapping is synchronized with the glossary (above).
   Ratifying `rfc/dev/PLAN.md` S9.1 stays with that repository.
2. Add a source-audit test for retired identifiers, allowing exact historical
   citation strings.
3. Done 2026-09-01 under S41.3: queue and memory-manager terminology, their
   direct callers, and the `refcount` rows derived above.
4. Done 2026-09-01 under S41.4: row, scratch-arena, mark/scan, stack,
   deferred-reuse and validation terminology, with the two module renames.
5. Rename tests and current API maps.
6. Rewrite production comments under the standard above.
7. Classify every remaining legacy occurrence as historical citation,
   unrelated English, or defect.
8. Run the complete gate from `dev/WORKFLOW.md`, then obtain a Critic review of
   terminology, preserved safety contracts, and accidental semantic changes.

The implementation commit must be rename/comment-only. Layout, allocation,
synchronization, and algorithm changes require separate review and commits.
