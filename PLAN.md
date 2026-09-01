# Plan

Implementation plan for `ll-model`, re-sorted 2026-07-24 against the RFC
after the 2026-07-22 object-layout redesign landed in the `rfc` repo.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/rc-cycle.md`, `model/gc/cycle/questions.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`.

The `rfc` repository carries its own plan at `dev/PLAN.md` for work that lands
in the specification rather than in this crate.

Updated: 2026-09-01 · Active: S41, and the rest of S36 after it — the sections
after S40 are the backlog

**Closed stages are deleted whole** (rule 23.1.3), and what outlived each
of them is in the journals rather than here: `dev/DECISIONS.md` for a
decision and its reason, `dev/POSTMORTEM.md` for a trap,
`dev/BENCHMARKS.md` for a measurement, `dev/INDEX.md` and
`dev/ARCHITECTURE.md` for the map. Deleted so far: S4 through S33, and
S35. A number is never reissued, so a
stage added later sits where it is to be done rather than where its
number falls, and the prose sections below are the backlog stages are
drawn from.

**`array::` is run under Miri in slices**, never whole — invocation and
thread cap in `dev/WORKFLOW.md`, Miri. What each slice costs, measured
2026-08-18 at two threads and quoted on Miri's own clock: `array::table`
without the flood ladder 32 tests in 79 s, the ladder's own module 14 in
273, and `array::entry` with the tracer and ring tests 13 in 179. All
clean. `array::entity` is the expensive one and is taken by test rather
than whole; the copy tests of that module ran 25 in 59 s.

**The crate collects no cycles.** S30 deleted `rc-walk`, `rc-trace` and
`rc-satb` on 2026-08-26 and the design in force is unbuilt, so a garbage ring
is retained and acyclic garbage dies by counting. The stages below build
`rc-cycle`; what the deletion took, what it kept and why is `dev/DECISIONS.md`
under that date, and the old code is on `archive/pre-rc-cycle`. S28 was
abandoned rather than closed by the same ruling, and S29 was split — its
second half is carried as S39.

**The stages below went through a Critic round and four Sage rulings on
2026-08-26**, on Edmond's instruction, and are the amended form. The rulings and their reasons are in `dev/DECISIONS.md`.

**Every cycle-GC improvement has two review gates.** Before the first code
edit, the Sage reviews its operation count, manager-allocation budget,
cache-working set, lifetime and refusal model, and records the pre-change
counter/benchmark baseline the step would otherwise erase. A red test is then seen failing;
after the implementation, the Critic reviews the repair and its mutations
before the step can be checked. Both findings are recorded in the step's
handoff. This applies to S36.9 onward and to the performance steps in S37/S40;
one broad review does not waive a later step's gate.

**Every byte owned for cycle collection comes from the memory manager and is
identifiable there as GC memory.** Production collection paths use no
allocator-owning Rust containers — no `Box`, `Vec`, `HashMap`, `BTreeMap`,
`Arc` backing allocation or hidden `GlobalAlloc`. Plain `#[repr(C)]` layouts,
fixed arrays, slices and raw links are representation, not ownership; their
backing blocks come through the manager. S36.9 makes this executable rather
than aspirational and audits the existing retained-index boundary before the
collector may cache it.

**Verification is one configuration** since 2026-08-26: the GC axis went with
the collectors, `hash-folding` and `debug-journal` are what remains, and
`cargo bench --no-run` is part of the gate because `cargo test --lib` builds
no bench target while `benches/lifecycle.rs` imports the GC ABI
(`dev/WORKFLOW.md`).

## Fog

A line here is an unresolved question rather than a step: it carries no
criterion, and it leaves when it gets one or when it is ruled on.

Whether `ll_default_dispose` nulls an object's cells or only releases them.
`mark` needs a live root, the queue can hold a zero-count corpse, and the
answer decides whether S36.7's driver must drop zero-count roots before marking
or may rest on the drain's sort (`rfc/model/gc/cycle/questions.md`, Y12 clause
5). Raised by `dev/CYCLE-COLLECTOR-REVIEW.md` and not checked there. The S42
Code Reviewer read `ll_default_dispose` on 2026-09-01: phase 2 drops each
counted child through `drop_ref` without nulling the cell, so a zero-count
root's cells are residue naming children whose counts no longer include those
edges, and a mark started from such a root would subtract below a live child's
count — the assertion S42.2 added fires in a test build. The driver skips
zero-count roots before `mark`, as `validation` already does for its input.

The six the review of 2026-09-01 raised over `52b2cbf` and `0416e83` left the
same day — four by Edmond's rulings, recorded in `dev/DECISIONS.md` and in the
`done:` clause of S38.3, and two by the repairs they prompted, recorded under
S36.9. The `dev/` sweep of the same day raised one more — `FORCE_OOM` against
the guard rule of `dev/POSTMORTEM.md`, 2026-08-13 — and it was fixed rather
than carried: the flag is raised only through `block_pool::force_oom`, whose
guard lowers it on the unwind as well as on the return.

- the glossary names no outcome for storage that stayed in its source block,
  none for the journal's unobserved thread, none for `ResetWindow::escrow` and
  none for the sweep-list sense of enrolment. S41.7 needs the first two, and
  all four belong to `rfc` S9.1 (`dev/CYCLE-TERMINOLOGY-AUDIT.md`, "Glossary
  check").
- `memory::reset_window` keeps a vocabulary of its own around the same gap —
  `CORPSE_WALKS` and `park_large` beside `ResetWindow::escrow` — and both
  S41 guards exempt the two by name, with the reason in the exemption. The
  rename waits on the glossary entry rather than inventing a third word, and
  no step owns it yet.

---

## S41 — The cycle vocabulary   *(before the rest of S36)*

Goal: every identifier and comment in `src/cycle` and its direct callers names
the state or the operation it denotes, and the crate agrees with the RFC
glossary rather than with metaphors that entered through a commit subject and
spread by agreement with the text already written.

Ordered by Edmond on 2026-09-01. Two audits stand behind it and neither is
repeated here: `dev/CYCLE-TERMINOLOGY-AUDIT.md` carries the `cycle` mapping,
the comment standard and the documentation boundary, and
`dev/PROJECT-TERMINOLOGY-AUDIT.md` carries the groups outside `cycle` and the
migration order across both repositories. Both are drafts against
`rfc/dev/GLOSSARY.md`, which wins on conflict. The `rfc` documents' own rename
stays with that repository's plan, under "The vocabulary" — the question
graph, the lifecycle documents, the hash-table design, the `zero-abstraction`
filename migration and `heap-design.md`'s status defect are all over there.
What this stage takes is the crate half of both audits.

Two things the project audit names are already owned and stay out: renaming
`MemoryCategory::LongLived`, which the backlog's "Rename the memory
categories" holds and which waits on region ownership, and redrawing
`docs/architecture.md`, which is S40.0.

**The stage runs before the rest of S36** so that S36.9's remaining slices and
S36.10 onward are written in the names that survive rather than renamed after
them. **Every commit here is rename and comment only**: layout, allocation,
synchronization and algorithm stay where they are, and a structural change the
audit happens to name — `cycle::deferred_slot_reuse`'s `Box<Vec<_>>` is the one — belongs
to the step that owns it.

- [x] S41.1 Decide: the mapping is synchronized with the RFC glossary
      done: every row of the audit's tables carries a verdict against
        `rfc/dev/GLOSSARY.md` — kept, amended to the glossary's word, or raised
        as a gap the glossary has to answer — and each verdict is recorded in
        the audit itself; a term the glossary does not cover is named rather
        than settled here
      tier: T2 · role: Sage
      Sage 2026-09-01: five rows amended against the draft — `escrowed_count`
        to the production `overflow_len`, the module `parking` to
        `deferred_slot_reuse`, `TraceWindow` to `ActiveTrace` because
        `TraceGuard` would collide with the canonical *guard reference*, the
        `memory-manager door` bullet to the glossary's closed list, and
        `group_is_met`'s reason to the row-initialization bitmap. Four
        outcomes have no glossary entry and stay gaps for `rfc` S9.1. Applied
        to both audits and verified against `src/` before recording. Final.
      handoff: the ratified mapping is the tables of
        `dev/CYCLE-TERMINOLOGY-AUDIT.md`, whose new "Glossary check" section
        carries the amendments, the four gaps and two identifier groups no
        table covered — the `colour` functions and the row-initialization
        bitmap's accessors. `door`, `escrow` and `floor` are defined by the
        glossary now, so the rename steps apply those entries rather than
        wait for them.
- [x] S41.2 The retired-identifier test, seen failing first
      done: a source audit over `src/` fails on any identifier the ratified
        mapping retires, allowing a string that quotes a document heading
        verbatim; it is seen red on this tree before the first rename and green
        after each one
      tier: T2 · role: —
      handoff: `cycle::tests::the_words_the_crate_retired` carries the ratified
        mapping as 72 entries, each scoped `Anywhere`, `Under` a subtree, or
        `Except` the subtrees owning a homonym — `replenish` is the reserve's
        and the critical cache's as well as the queue's. It reads identifiers
        and cuts a line's comment first, so prose stays S41.6's, and it drops a
        string literal naming a `.md` file, which is the citation exemption.
      handoff: **the guard stands over a shrinking debt list, not over the
        whole crate at once.** `STILL_TO_MIGRATE` names the 50 files that still
        offend; a retired name in any file outside it fails, a file leaves the
        list in the commit that renames it, and a second test refuses a file
        that has stopped offending, so the entry cannot be left behind to
        exempt it. This is the one departure from the `done:` clause above, and
        it is what lets every commit of the stage stay green: the clause's
        "green after each one" and `dev/WORKFLOW.md`'s per-commit gate cannot
        both hold while one test carries the whole mapping. Seen red before the
        list existed — 992 sites over those 50 files — and both guards were
        seen firing, on a file taken off the list and on a clean one put on it.
- [x] S41.3 The candidate queue and the manager boundary
      done: `src/cycle/queue.rs` and `src/memory/gc_metadata.rs` carry the
        ratified names, the module contract states the three storage paths the
        audit names, and no metaphor from the audit's list survives in either;
        the gate is green and no existing test changed an assertion to get
        there
      tier: T2 · role: Critic
      Critic 2026-09-01 round 1: sixteen findings, all vocabulary and comments
        — the mechanical rename-only check found no behaviour change. The
        load-bearing ones: `SPARE_SEGMENTS`'s doc kept "overflow" in the
        retired sense, three test messages described a different event than
        their assertion, `owes` and `stands on` survived the metaphor sweep,
        and the four `refcount` names decided in this session were in no table
        and in no guard row. All taken.
      Critic 2026-09-01 round 2: the round-1 fixes were applied to the lines
        quoted rather than to the classes they stood for — fourteen more
        old-sense uses of *overflow* and two more of *enrolment* in the same
        migrated files, and one fix that made its message false ("every
        allocation path but the base block refused", when that tier asks no
        allocation path). Also: the audit's Status line still claimed every
        row was ratified, and nothing tested the two `enrol` rows. All taken.
      handoff: the queue is `queue_base`/`overflow_*`/`write_segment`/
        `spare_count` and registration is `register_candidate`; the `enrol`
        family in `refcount` moved with it, and the four names that took —
        `CANDIDATE_GATE_MASK`, `may_become_a_candidate`,
        `is_registered_candidate`, `clear_candidate_bit` — are the audit's new
        "Candidate registration in `refcount`" section, marked as derived
        under this step rather than ratified under S41.1.
      handoff: `STILL_TO_MIGRATE` is 37 files, down from 50. Three of the
        thirteen left it without a rename: S41.2's guard counted
        `critical::replenish` and `reserve::replenish` at their callers as
        offences, which they are not, so its recorded 992 sites over 50 files
        overstates the debt. The guard now reads the `::` before a retired
        name and spares the owning module's own call, and `enrol` is two rows
        — `cycle/arena`'s is `allocate_and_attach_row_array`, which is what
        S41.4 has to apply there.
- [x] S41.4 Row resolution, the trace scratch, mark, scan, the stack, deferred reuse and validation
      done: `row`, `arena`, `shadow`, `mark`, `scan`, `stack`, `parking` and
        `exact` carry the ratified names, module by module with a compile at
        each boundary; `parking` becomes `deferred_slot_reuse` and `exact`
        becomes `validation`, and neither module changes what it does
      tier: T2 · role: Critic
      Critic 2026-09-01: the code half is complete and rename-only — the
        mechanical check found no difference beyond `rustfmt`'s line breaks.
        Every defect was in a comment or a string, and the two scripts that
        did that half caused them: three tokens carry two ratified names each
        (`Met`, `Condemned`, `Row`) and the rename took one, which put
        `RowLookup` where `Color::Unclassified` belonged in the scan's own
        doc; four string literals took an identifier where an English word
        stood; eleven doc comments still named an item the tree no longer
        declares. Taken, with the three ambiguous rows of the guard rewritten
        to name both senses in the failure message.
      handoff: `parking` and `exact` are gone as paths — `src/cycle/
        deferred_slot_reuse.rs` and `src/cycle/validation.rs`, with their test
        directories. `STILL_TO_MIGRATE` is empty from here, and that measures
        less than it looks: the guard reads identifiers with comments cut, so
        a file name, a name that merely contains a retired word
        (`condemned_from`, `met_first`) and every comment are outside it.
      handoff: the row-initialization bitmap's accessors (`groups`,
        `group_bit`, `group_bytes`) still have no ratified name, and the
        arena's `slots` parameter keeps its name on purpose: it counts the
        block's slots, which is what `RowArray::row_count` is derived from
        rather than what it holds.
- [x] S41.5 The tests and the current maps
      done: no test file name, test name or helper uses a retired term,
        measured by a guard of its own that reads the crate's **file names and
        item names** for the audit's metaphor list — condemn, acquit, corpse,
        judge, park, escrow, floor, climb, enrol, discount — as case-insensitive
        substrings, which is the axis `Where` cannot express and the reason the
        whole-token guard sees neither `condemned_from` nor
        `what_the_guard_discount_answers.rs`. An assertion message stays the
        whole-token guard's, which already reads string literals, and the
        `STILL_TO_MIGRATE` half of this clause is spent: S41.4 emptied the list.
        `dev/INDEX.md` and `dev/ARCHITECTURE.md` name what the code names;
        the historical records the audit lists as out of scope are untouched,
        and an active citation of an old heading keeps the heading exactly with
        the current name outside the quotation
      tier: T1 · role: —
      handoff: the second guard is
        `cycle::tests::the_metaphors_the_names_still_carry`, ten metaphors read
        as case-insensitive substrings over file names and declarations. It
        found 37 declarations and 6 file names; four names stay by exemption,
        each carrying its reason — the sibling guard's own test that names the
        token it is about, `memory::reset_window`'s `CORPSE_WALKS` and
        `park_large`, which wait on the glossary, and the hash table's
        `climbs_its_own_ladder`, which is S41.8's.
      handoff: `dev/INDEX.md` and `dev/ARCHITECTURE.md` name what the code
        names, and so do the open half of `PLAN.md` and its backlog. The dated
        records inside closed steps keep their vocabulary: they describe the
        day they were written, which is what the audit's documentation
        boundary asks.
- [x] S41.6 The comments, the residue and the gate
      done: every module header of `cycle` states purpose, ownership and
        lifetime, allocation and failure behaviour, ordering invariants and its
        design references; every remaining occurrence of a retired word is
        classified as a historical citation, unrelated English or a defect, and
        the defects are gone — measured over **comment text**, which no guard
        of this stage reads today, by S41.5's metaphor list run over comments
        with each survivor carrying either the document heading it quotes or a
        line naming why the word is not a metaphor there (`door` is not on the
        list until S41.7 classifies its sites); the full gate passes and a
        Critic has read the pass for terminology, for the safety contracts it
        had to preserve, and for an accidental change of meaning
      tier: T2 · role: Critic
      Critic 2026-09-01 round 2: five of the round-1 repairs were wrong or
        incomplete — a re-wrap cut the head off `scan.rs`'s ordering sentence,
        two headers claimed an exclusivity the queue's own aborts contradict,
        two comments were repaired into tautologies, and the wrap-aware
        citation rule could swallow a whole comment block behind one stray
        quote. Taken: the guard now reports an unbalanced quote rather than
        reading past it, reads `let (a, b)` destructuring, and states which
        surfaces no keyword can reach.
      Critic 2026-09-01: the word-level rewrite broke what a word-level
        rewrite breaks. Nine quoted headings were rewritten inside the
        quotation — every one of them a citation that wraps across two lines,
        which the first restoration pass could not see; three `# Safety`
        headings were re-wrapped into their own contracts; about eighteen
        sentences no longer parsed or no longer meant what they had; two
        module headers claimed what the code contradicts, one of them the
        `Box<Vec<_>>` this stage deliberately does not fix; and the new guard's
        citation rule was per line, so it could not see two of the survivors it
        exists to find. All taken.
      handoff: the third guard is
        `cycle::tests::the_metaphors_the_comments_still_carry`, the same ten
        metaphors over comment text. A quoted span is a citation and is spared,
        **and the quote state carries across lines**; seven exemptions name a
        subtree, a word and the reason, and the reasons are the reset window's
        window, the promote side of it, and the sweep-list sense of *enrolment*
        that `cycle/arena` keeps until the glossary answers.
      handoff: the second guard now reads `let` bindings too, which is where
        `condemned`, `judged` and `corpse` were still standing under comments
        that no longer said any of it. 239 comments were rewritten; the words
        that survive are citations, ordinary English (`noise floor` became
        *measurement noise*, the arithmetic `floor` a *lower bound*) or one of
        the seven exemptions.

- [ ] S41.7 Allocation outcomes, and the `door` sites by semantic class
      **blocked: two of its five outcomes have no glossary entry**
      (`ExternalCarry::Refused` / `OutsideCarry::Refused` and the journal's
      `Window::Refused`), which is `rfc` S9.1's to answer. What is not blocked
      is `Placement::Refused` → `Unsupported` and the `door` classification
      against the glossary's closed list; splitting the step is Edmond's call.
      `InsertOutcome`'s two rows landed with S41.8, which shares their enum.
      done: an allocation failure, an unsupported placement, an admission
        denial, a carry that left storage in its source block and an
        unobserved journal thread each carry a name of their own —
        `InsertOutcome`, `Placement`, `ExternalCarry`, `OutsideCarry` and the
        journal `Window` among them — and every `door` site is classified as
        an allocation path, an entry point, a mailbox, a channel or a
        store-barrier form, which is the glossary's closed list, with an OS
        resource named exactly instead; the difference between
        a memory failure and a result a caller can act on survives every
        rename, which a test asserts rather than a reading
      tier: T2 · role: Critic
      handoff: 76 sites over those five types, counted 2026-09-01 with `grep
        -rn` over `src/`. `InsertOutcome`'s two rows landed early, with S41.8,
        which shares an enum with them; what is left here is `Placement`, the
        two outcomes the glossary does not name, the journal's `Window` and
        the `door` classification.
      handoff: the `door` classification is done, by session L2 on
        2026-09-01 — `dev/design/door-sites.md`, 143 rows at `019618d`
        (140 occurrences by `grep -rnoi 'door' src/ | wc -l` and 3 file
        names; the 86 above was the whole-word singular line count at
        `27ffbf3`): allocation path 73, entry point 55, none of the five 14,
        mailbox, channel and store-barrier form 0. One ruling taken here:
        `element::set`, "the public door", is an entry point — a store
        function is the caller's way in, and *store-barrier form* names the
        forms a barrier takes, not the functions that reach it; the glossary
        line still defines neither, which is S9.1's. What is left of the
        step's unblocked half is the rename itself, run from the document;
        it also moves the `PLAN.md` lead-in `os.rs:136` quotes.
      handoff: the rename landed 2026-09-01, L2 — `c4a59a6` over 54 files,
        `door` in `src/` from 140 occurrences to the 3 the name guard's own
        entry carries, and `9ed63ae` for `Placement::Unsupported` with the
        distinction test. The unblocked half is spent; what remains is the
        blocked half — `ExternalCarry::Refused`, `OutsideCarry::Refused` and
        the journal's `Window::Refused` — and it waits on `rfc` S9.1.
- [x] S41.8 The hash table's collision defence
      done: the metaphor is gone from `src/array/` and its tests — collision-
        defence state, a chain-length threshold, an equal-hash threshold, a
        salted rebuild, a keyed-hash escalation and a terminal admission
        denial name what each was; the admission denial stays a result the
        caller can catch and does not become an allocation failure, which is
        the one distinction the rename can destroy silently
      tier: T2 · role: Critic
      handoff: `ladder`, `rung` and `trigger` were 176 occurrences in `src/`,
        counted 2026-09-01; the design half is `rfc`'s.
      Critic 2026-09-01 round 2: the citations are clean this time, checked by
        occurrence count over every file. What it found instead: the new test
        forced its refusal through `block_pool::force_oom`, which the crate's
        own notes call a coin flip for a `GcHeap` table — its storage comes
        from the buffer arena's long-lived side — so it now uses
        `FORCE_REFUSE_LONGLIVED` and proves the refusal through `REFUSALS`,
        which had no reader until now. Also taken: `trigger` had no guard on
        the surface four in five of its occurrences lived on, and is in the
        metaphor list with five exemptions; the `rung` row's replacement named
        nothing; and the `WORKFLOW.md` figure I "corrected" invented a cause.
      Critic 2026-09-01: no logic moved, and the added test is not vacuous —
        but two quoted headings were rewritten inside the quotation again,
        both of them wrapping onto a second line, which is the defect
        `dev/POSTMORTEM.md` had just been given an entry for. The checker that
        cleared the previous step compared citations by membership, so a
        heading damaged at one site and intact at another read as unchanged;
        it counts occurrences now. Also taken: two broken intra-doc links,
        `stage` used for four different things, a dead exemption the step was
        meant to retire, and a `>=` that could not catch an insert a refusal
        left behind.
      handoff: the vocabulary is the audit's six names — collision-defense
        state, the chain-length and equal-hash thresholds, the salted rebuild,
        the keyed-hash escalation and the terminal admission denial, in US
        spelling as rule 6 asks. Both guards carry the three words: the
        identifier guard scopes them `Under("array")`, which reads code and
        string literals, and the metaphor guards read the file names and the
        prose, where four in five of the `trigger` occurrences stood.
      handoff: `InsertOutcome::RefusedForMemory` became `AllocationFailed`
        here rather than in S41.7, because four match arms carry it beside the
        denial and half a rename inside one `|` reads as two vocabularies.
        S41.7 keeps `Placement`, the two glossary gaps, the journal `Window`
        and the `door` classification — 86 `door` sites and 17 `Refused`
        variants as of this commit.
- [x] S41.9 The lifecycle, ownership and platform words in the crate's prose
      done: `death`, `destructor`, `teardown`, `dispose`, `drop` and
        `reclamation` each name one of the five phases the project audit
        separates, and no comment presents them as one ordering; `native`
        resolves to machine code, standard PHP, the machine stack or foreign
        code at each of its sites; `owner` in a cross-module contract says
        which owner it means
      tier: T1 · role: —
      handoff: `native` was 14 occurrences in `src/` when this step was
        written and 2 when it ran — the earlier steps' rewrites took the rest.
        Both were the machine stack and say so now.
      handoff: the phase numbering is the defect the lifecycle half came down
        to. Three protocols number their phases from one — the object
        teardown's, the arena reset's and cycle finalization's — and each
        module now says whose numbering it means. `run_pre_destructor` is
        `run_user_destructor`: the audit rules that `__destruct` is not a
        *pre*-anything, and the name had spread to twelve prose sites.
      handoff: `owner` was qualified where a cross-module contract carries it —
        the containing entity in `barrier` and `array::table`, the owning
        thread in `block_pool` and `heap`, the holding entity in
        `buffer_arena`. A local `owner` whose type says what it is stays
        short, which is what the audit asks.
- [x] S41.10 The two maps say what the code says
      done: `dev/INDEX.md` and `dev/ARCHITECTURE.md` carry none of the
        audit's retired words as their own prose — `ladder`, `rung`,
        `trigger`, `corpse`, `condemn`, `parked`, `ENROLLED` among them — each
        replaced by the name the crate's code took for it, and a quoted
        heading or journal title keeps its old word inside the quotation;
        a grep over the two files for the retired words returns citations only
      tier: T1 · role: —
      note: S41.5's done clause promised the two maps and left about ten
        sites; found on 2026-09-01, ordered by Edmond the same day as a
        background job for a cheap agent
      handoff: done 2026-09-01 by a sonnet subagent in a worktree, checked
        here and amended once (`triggers` → `escalates to`). Two dead
        identifiers went with it, `ENROLMENT_GATE_MASK` at two sites, which
        the whole-word guard could not see through the underscore. What stays
        is citations, `memory::reset_window`'s own words at three sites, and
        two mentions of the deleted collector's parked lists, which no live
        name replaces.
- [ ] S41.11 Every cited heading exists in the document it names
      done: for every citation of the form `` `rfc/…md` `` or `` `docs/…md` ``
        followed by a quoted heading or bold lead-in, in `src/`, `benches/`,
        `docs/`, `dev/INDEX.md`, `dev/ARCHITECTURE.md` and `dev/WORKFLOW.md`,
        the quoted text is found in the named file — repointed to the section
        the fact moved to where the `rfc` rewrite of 2026-08-30 renamed it
        (`55786e4`, `a2310c1`, `0075ef3`), and the check itself is written
        down as pass 1's heading-level form in `dev/WORKFLOW.md`, so a
        renamed heading is found by a command rather than by a reader
      tier: T1 · role: —
      note: found by the S42 Code Reviewer on 2026-09-01 — `rc-cycle.md` has
        no "Cycle teardown" (10 files cite it), no "Death while enrolled"
        (7 files), no "Who judges, and what a trace is worth"; pass 1 tests
        only that the file exists. Dated journals are records and stay.
## S34 — The root queue, enrolment and parking

Goal: candidates reach the collector without the mutator paying for a data
structure, and an entity that dies while enrolled leaves no dangling pointer.

- [x] S34.1 The queue against Y12's contract
      done: **all eight** clauses hold, and where a clause constrains a reader
        that does not exist yet "hold" means the code contradicts none of it and
        says which half it does not build — clause 6 is the one that needs
        saying, its reserve draw being built and its reserve *mode* not, there
        being no collection to walk a root; so a failed growth never drops a
        root, no allocation
        happens on the enrolling thread's hot path, proven by a `#[cfg(test)]`
        allocation counter bracketing the enrolment call rather than by defining
        the growth path as not hot, and a second reader is either supported or
        refused by construction rather than by a `debug_assert!`; clause 4's
        second half is superseded by S34.2 and the step says so; the one arm the
        runtime keeps is rebuilt here — a growth refusal or a reserve draw
        during enrolment sets the pending flag, and the poll fires at the next
        clean point, returning 0 until S36.7 wires the collection
      tier: T2 · role: Critic
      Critic 2026-08-27: eleven findings, and the two that changed the code are
        the first two. `critical::draw` is a `try_with` on a thread-local with
        drop glue, so its first touch registers a destructor and S34.1 put that
        call on the release path where nothing can report — `ll_thread_init`
        now fills all three reserves before the heap allocation that returns
        early, and the residue is S34.5. And thread exit strands `ENROLLED` on
        entities that outlive the thread through the abandoned list, which the
        gate then refuses for ever: a permanent miss and not the "loss of
        candidates" the comment called it, so the comment is rewritten and
        S39.1 is told what it is choosing against.
      Critic 2026-08-27: the rest were the instrument and the fixtures, and
        four are now mutation-checked regressions that were not detectable
        before — the drain test held no spare at the drain, the allocation
        probe bracketed the growth path twice and the hot path never,
        `FORCE_OOM` in the refusal test closed a door the path never knocks on
        and hid an "ask the pool" fallback, and `fill_live_segment` set a count
        over 8159 recycled words that S34.3's corpse rule would have
        dereferenced. Also taken: the second `arm()` was unreachable, so the
        draw and the refusal now arm independently and each has a test; a held
        spare read `BLOCK_KIND_FREE`, now stamped at acquisition; and three
        comments said what had stopped being true. Nothing was rejected.
      handoff: the clause this step could not have been built against was
        clause 3, and it was ruled on 2026-08-27 (`rfc/dev/PLAN.md` S8.2, and
        the entry it names in `rfc/dev/DECISIONS.md`). What it hands the code: a
        segment is one 64 KiB pool block and the queue is a chain of them; the
        owner holds two spares in two pointer cells filled at thread init and at
        every poll through the ordinary door, the live segment being a cell the
        first enrolment swaps in; an overflow with both cells null draws the
        critical reserve. What no ruling reaches is the reserve being spent too
        — `rfc/dev/PLAN.md` S8.5 — so this step builds the reserve draw and
        stops at its edge.
      handoff: closed 2026-08-27. `cycle::queue` is the owner's side of the
        contract: a chain of 64 KiB pool segments, two spare cells filled at
        `ll_thread_init` and at every `ll_gc_maybe_collect`, the live segment a
        cell the first enrolment swaps in, `critical::draw` when both cells are
        empty, and the enrolled bit undone when both doors refuse. `refcount`'s
        release path is its one caller and the edge is in `dev/ARCHITECTURE.md`.
      handoff: verified at 514 tests — one run, three at four threads,
        `hash-folding` 514, `debug-journal` 520 three times, release with no
        warnings, `cargo bench --no-run`, `fmt --check`. Miri over `cycle::queue`
        is clean at 10 tests, 10.73 s on its own clock. Every new test was seen
        failing against a mutation of what it names, fifteen mutations in all.
        Miri found the stage's one defect and it was the fixture's: a raw
        pointer taken before a `&mut` call, invalidated by the retag —
        `dev/WORKFLOW.md`'s rule about that was scoped to reentrancy tests and
        is widened.
      handoff: what this step did **not** build, and who owns it. The read side
        is the collection's, S36.7, and the accelerator's swap S38.1's, so
        nothing yet agrees
        with a detaching reader about the fill cell — `rfc/dev/PLAN.md` S8.7.
        The drain at thread exit returns blocks and drops entries, which S39.1
        turns into a chosen fate. The corpse rule and the marks a reader writes
        into an entry's low bits are S34.3's and the trace's; the four bits are free
        and nothing writes them today.
- [x] S34.5 Decide what the critical reserve's first touch may cost
      done: `memory::critical`'s thread-local is reachable from `ll_release`
        without the process depending on what registering a TLS destructor
        costs — either its payload loses its drop glue and the blocks a thread
        that never ran `ll_thread_exit` holds are returned another way, or the
        cost is measured on this platform and recorded as acceptable with the
        measurement named
      tier: T2 · role: Critic
      handoff: raised by the Critic on S34.1, 2026-08-27. `critical::draw` is a
        `try_with` on a `thread_local!` whose payload has a `Drop`, so its
        **first** touch on a thread registers a destructor, and on glibc that
        registration allocates and terminates the process when it cannot. S34.1
        put that call on the release path, where nothing can report.
      handoff: what S34.1 did about it, and what it did not. `ll_thread_init`
        now fills all three reserves before the heap allocation that can return
        early, so every thread it runs on has touched the reserve while a
        refusal was still reportable. What is left is the thread that never
        calls `ll_thread_init` at all — a population `Critical::drop` and
        `ThreadCache::drop` both say the crate serves — and for that one the
        first touch is still `ll_release`'s. The claim about glibc is the
        Critic's reading and is **not verified on this box**; verifying it is
        part of this step.
      handoff: S34.8 added a second registration on the same path, and it is
        the same question. The lazy floor draw asks
        `heap::thread_exit_will_run`, whose call is the arming of `EXIT_GUARD`
        — a `thread_local!` with drop glue, so its first touch registers a
        destructor exactly as `critical::draw`'s does, and from the same
        release path on a thread that never ran `ll_thread_init`.
      Critic 2026-08-29: five findings against the decision, not the code. The
        two that mattered were about the record: the entry claimed a
        registration death at init belongs to the class the floor's ruling
        accepts, when that ruling accepts a *thread* refusal and a registration
        failure kills the process; and it called the registration and the
        floor's abort "the same edge", when one answers an empty block pool and
        the other an empty glibc heap. Both rewritten. The census pin, the
        second test's framing and the unpriced `pthread_key` arm were the other
        three, all taken. One round: nothing was disputed.
      handoff: closed 2026-08-29, and the arm taken is the second — the cost is
        measured and recorded rather than removed. Verified on this box, not
        assumed: the binary carries a weak `__cxa_thread_atexit_impl`, the
        toolchain's `linux_like::register` discards its result, and Ubuntu
        GLIBC 2.39-0ubuntu8.7 `calloc`s 32 bytes there and calls `__libc_fatal`
        on a null — "Fatal glibc error: failed to register TLS destructor: out
        of memory". It never returns a failure, so there is nothing for Rust to
        have checked.
      handoff: what the decision rests on, in `dev/DECISIONS.md`, "what the
        first touch of a thread-local with drop glue may cost". Four
        thread-locals in this crate have drop glue and `ll_thread_init` touches
        all four, so the death is deterministic in place rather than scattered
        over the release path — it is **not** converted into a refusable one,
        and the entry says so. Three tests in
        `memory::critical::tests::where_the_first_touch_happens`, the third a
        census of every `thread_local!` in `src/` against a literal list, so a
        fifth cannot appear unnoticed.
      handoff: not priced, and named rather than dismissed: a guard built on a
        `pthread_key_create` key taken once at process start, where the failure
        is reportable. It would remove the unreportable class from both paths
        if `pthread_setspecific` allocates nothing per thread, which was not
        read. Whoever picks it up owns the per-target story too.
- [x] S34.6 Make the enrolment unfailable, and delete the undo
      done: `enrol` answers nothing and every door refusing lands the entry in
        an escrow the same thread-local holds; the release path has no branch
        left in which a set enrolled bit names no entry; the poll refills,
        drains the escrow and only then fires, and a drain with no room leaves
        the entries where they are rather than losing them or looping
      tier: T2 · role: —
      handoff: Edmond ruled on 2026-08-28 that nothing may be lost — the
        mutator either collects itself or waits for the collector — and the
        mechanism was ruled beside it (`rfc/dev/PLAN.md` S8.5, and the entry it
        names in `rfc/dev/DECISIONS.md`). S34.1 had shipped the branch the
        ruling forbids: it undid the bit and lost the edge, which is Y6's
        permanent miss whenever that decrement was the ring's last external
        release.
      handoff: the escrow is one segment's capacity, 65 280 bytes of
        thread-local per thread, sized on clause 3's own poll argument.
        Overflowing it aborts, which is the funded class's last resort and is
        the one state this module has no answer for.
      handoff: the cost is measured and it is eager. `readelf -S` puts the test
        binary's `.tbss` at 65 680 bytes, so the escrow is 99.4 % of the crate's
        zero-initialised TLS image, and glibc allocates and zeroes that image
        per thread. Trimming it is one constant and wants the ABI's poll bound
        first (`rfc/runtime/exceptions.md`).
      handoff: role `—` rather than Critic, unlike S34.1: the shape is a
        ruling's and not a choice, and what a Critic would attack here — the
        escrow's size and the abort behind it — is named in the ruling and in
        `dev/DECISIONS.md` rather than decided in this step.
      handoff: *(storage amended 2026-08-28.)* The escrow leaves the TLS image
        for one allocator-issued floor block held for the thread's life; the
        rework is S34.8, the ruling `rfc/dev/DECISIONS.md` "the escrow's floor
        is allocator-issued". The 99.4 % TLS figure above stands as the
        measurement that motivated the move.
- [x] S34.7 Give the bulk release loop a poll of its own
      done: `ll_release_vector` polls on its backedge every `POLL_STRIDE`
        iterations, so a run of any length refills the queue's funding and
        drains its escrow mid-run; a test releases a vector longer than the
        stride with every door spent and finds the escrow empty and every
        candidate queued at the end
      tier: T2 · role: —
      handoff: found by the consolidation pass on the ruling, not by a test.
        The escrow was sized on "a whole segment cannot fill between two
        polls", and that argument quantifies over loops the compiler emits;
        `ll_release_vector`'s count is the caller's and the compiler polls only
        after the call, so a container clear enrolled without bound. Before
        this, a clear of some ninety thousand shared elements aborted with
        memory free — eleven segments of funding and then the escrow.
      handoff: the backedge is a legal fire point because iteration `i - 1` has
        fully returned and `entities[i]` is unread, which is
        `rfc/model/gc/strategies.md`'s own "between mutator operations". It
        rests on a precondition `rfc/model/memory/bulk-operations.md` now
        states: the caller severs every traced edge to an entry before
        submitting the vector.
- [x] S34.8 Move the escrow out of the TLS image into an allocator-issued floor
      done: the escrow's storage is one 64 KiB pool block drawn at
        `ll_thread_init` before the best-effort reserve fills and returned in
        `retire_the_journal` after `queue::drain`; a refused draw fails thread
        init through `ll_thread_init`'s new status return; a thread that never
        ran init draws its floor lazily at first enrol, through the ordinary
        door, and its refusal aborts; the lazy draw checks the exit phase and
        aborts past `ll_thread_exit` instead of drawing; the block is stamped
        `BLOCK_KIND_ARENA`; `ESCROW_ENTRIES`, `POLL_STRIDE` and the poll order
        are unchanged, and the accounting tests name the floor block
      tier: T2 · role: Critic
      handoff: the ruling is `rfc/dev/DECISIONS.md` "the escrow's floor is
        allocator-issued" (2026-08-28): per-life floor, a re-birth refusal
        refuses the new life through the status return, and memory-hard
        thread creation is a recorded trade, not a derivation.
      Critic 2026-08-29 round 1: five findings, every one verified before it was
        executed. `draw_floor` was not re-entrancy-safe and leaked a block per
        registered thread under `debug-journal`; the ignore on the unregistered
        thread's test gave a false reason; the status return had no enforcing
        mechanism; a superseded TLS figure was quoted fresh in the module doc;
        the drain test passed for a shallower reason. All five repaired.
      Critic 2026-08-29 round 2, against those repairs: `ll_thread_init` funded
        the thread without first asking whether anything would run its
        teardown — the same question `draw_floor_or_abort` asks three lines
        away — so a thread past the destruction of its guard's slot kept a
        floor, two spares and two reserves for the life of the process; the new
        leak test's exact count was unstable under a full `debug-journal` run,
        the registry's deferred ring frees landing inside its bracket; and
        `#[must_use]` stops at the crate boundary, where every `bench-external`
        probe called the new signature bare. All three repaired; the device is
        dropped here, at two rounds.
      handoff: closed 2026-08-29 at 530 tests — three runs at four threads,
        `hash-folding` 530, `debug-journal` 535, release with no warnings,
        `cargo bench --no-run`, `cargo build --examples`, `cargo fmt --check`
        clean. Every new test was seen failing against a mutation of what it
        names: the floor never returned, the floor unstamped, the drain taking
        it, init ignoring the refusal, the lazy draw arming nothing, the
        re-entrancy re-check deleted, the teardown check deleted.
      handoff: the measurement the ruling asked for is `dev/BENCHMARKS.md`,
        "the escrow's move out of TLS": `.tbss` 65 784 bytes before against 496
        after, both arms taken back to back on this box. What replaces them is
        one 64 KiB block per live thread, so every exact `blocks_out` counts one
        more per thread — the accounting tests say so where it matters.
      handoff: `ll_thread_init` now answers `bool`, and `false` has two causes:
        a refused floor, and a thread whose teardown will never run. Three
        self-initialising paths are exempt from reading it
        (`dev/DECISIONS.md`, "`ll_thread_init` answers"); everything else
        asserts. The re-entrancy trap that produced the round-one leak is
        `dev/POSTMORTEM.md`, "a draw re-entered itself through the journal".
- [x] S34.9 Take the pool's memory from the OS, and delete Rust's allocator from its path
      done: `carve_region` and the large-run path map their memory from the
        operating system — `mmap` on unix, `VirtualAlloc` on windows — and
        answer null when it refuses; no path reachable from `BlockPool::get`
        calls Rust's global allocator, the refill batch and the thread cache
        being fixed arrays and the region registry living in mappings
        of its own rather than in a growable collection; a test forces the operating system to refuse
        both mappings this path makes — the region and the registry's
        chunk — and reads a report instead of losing the process, and a
        refused carve is driven through `BlockPool::get` so the refill
        loop's own accounting runs on the refusal branch
      tier: T2 · role: Critic
      handoff: ruled by Edmond 2026-08-29. `BlockPool::get`'s own contract says
        "nothing on this path aborts", and three sites break it, because a
        `Vec` that cannot allocate calls `handle_alloc_error`:
        `Vec::with_capacity(REFILL_BATCH)` in `take_block`, the thread cache's
        `blocks.append`, and `regions.push` in `carve_region`.
        `memory/critical.rs` and `memory/heap.rs` already refuse that failure
        mode by hand and say why.
      handoff: it also pays a debt `memory/stdapi.rs` records in its own module
        doc: while regions came from `std::alloc::alloc`, this manager could not
        be installed as Rust's `#[global_allocator]`, because region carving
        would re-enter `ll_alloc` with an alignment it refuses.
      handoff: S34.8 depends on it. That step reports a refused floor block
        through `ll_thread_init`'s new status return, and the report is only
        true once the draw can refuse without killing the process.
      handoff: closed 2026-08-29 at 524 tests — three runs at four threads,
        `hash-folding` 524, `debug-journal` 530, release with no warnings,
        `cargo bench --no-run`, `cargo fmt --check` clean once rustfmt was
        installed on this toolchain. Every new test was seen failing against a
        mutation of what it names: the chain built newest-first, the registry
        refusal ignored, a refused region reported as carved, the `blocks_out`
        undo deleted, the short batch's remainder dropped.
      handoff: two Critic rounds on Fable, and the second found the first's
        repairs wanting, which is why the device is dropped here rather than
        after one. Round one: the `MAP_ANONYMOUS` fallback was wrong on
        android and solaris, the criterion's refusal test did not exist and
        had no seam to be written through, `bases()` rebuilt the `Vec` abort
        under the registry lock, and `CachedBlocks::extend` could fill the
        slot `put` needs. Round two, against those repairs: the visitor form
        ran arbitrary code under a lock the allocator takes — the rule
        `memory/large_entity.rs` states — so the registry's read path became
        lock-free, `len` and `next` atomics published by release stores;
        `mmap`'s `offset` was declared `i64` where 32-bit unix has a 32-bit
        `off_t`; linux mips defines `MAP_ANONYMOUS` as 0x800 and slipped
        through the gate built to stop it; the fault arming leaked on a panic
        and became RAII.
- [x] S34.10 Give Miri back a tree it can run
      done: a targeted Miri run over a module that carves a region completes
        instead of reporting undefined behaviour at `memory::os::map_aligned`,
        and `dev/WORKFLOW.md`'s Miri section states what the arm costs — which
        UB class Miri stops seeing in exchange
      tier: T2 · role: Critic
      handoff: found on 2026-08-29 while running Miri over S34.8's own pointer
        writes. `map_aligned` over-maps and trims the head and the tail with
        two partial `munmap`s, which is correct POSIX and which Miri's shim
        does not model: it reports "incorrect layout on deallocation" and stops
        the run. The first `BlockPool::get` of any test carves a region, so
        this reaches every test that allocates — `refcount::tests::`
        `who_may_read_a_header` passes only because it never asks the pool.
      handoff: it is S34.9's debt, found after that step closed. Before it,
        regions came from `std::alloc::alloc` and Miri ran; the WORKFLOW's Miri
        section still describes a capability the tree has not had since.
      handoff: what was tried and works, as a local patch that was **not**
        committed: `#[cfg(not(miri))]` around the two trims and a `#[cfg(miri)]`
        no-op `unmap`, which leaves the over-map in place and leans on
        `-Zmiri-ignore-leaks`, the flag the WORKFLOW already prescribes. Under
        it, `cycle::queue` ran 18 tests and `memory::heap` 20, both clean. The
        cost is that Miri stops seeing anything about unmapping, which is the
        trade the step has to state rather than assume.
      Critic 2026-08-29: four findings. The no-op `unmap` above would have
        disarmed three tests in `promote::tests::the_reset_reads_no_corpse`
        that name Miri as their whole regression, which is rule 4's
        prohibition applied to a tool rather than an assertion; the stated
        cost was incomplete, an untrimmed mapping leaving a 64 KiB readable
        apron at each end where an overrun past a region becomes invisible;
        the evidence exercised only the mapping half; and the `unmap`
        comment gave the wrong reason for the shim's refusal. All four taken.
      handoff: closed 2026-08-29, and **not** by the patch the line above
        describes. Under `cfg(miri)` the trims are skipped and a table in
        `os.rs` remembers `(aligned, base, over)`, so `unmap` hands back the
        whole mapping — an exact-layout deallocation the shim accepts, which
        keeps an unmap an unmap. Verified by running: `cycle::queue` 20 tests,
        `memory::stdapi` 14, `memory::large_entity` 5 — the last being the
        module that returns mappings, and neither an "incorrect layout" report
        nor the panic `unmap` raises on a missing table entry appeared.
      handoff: **what is not verified, and it is the Critic's first finding
        turned into a question.** The three `promote` tests can run again, but
        whether they still exhibit their defect is unknown: the reconcile one
        was run with `reset_window::park_large` returning false and passed in
        176 s. Either that is not the mutation their comments mean, or the
        arrangement needs a second half. They have been unrunnable under Miri
        since 2026-08-26, so the claim in their doc comments has been stale
        for three days and is not this arm's doing. Re-arming them is nobody's
        step.

- [x] S34.3 Parking a slot that dies enrolled
      done: death runs in full — weak cells cleared first, then `__destruct`,
        then children released — and the slot is withheld from the allocator
        while a queue entry names it; the retirement reads the refcount, clears
        the bit and returns the slot without touching the body; the return is
        the crate's single slot-return path and the block's `used` falls
        **there and never at the parking**, proven by a test that empties a
        block around a parked corpse and shows the block reaching the pool only
        at the return
      tier: T2 · role: —
      handoff: **the criterion was amended on 2026-08-29 and the clause that
        moved is named here.** It said "the drain reads the refcount, retires a
        zero-count entry"; what the drain is has no answer yet — the reader
        that drains a queue is the collection's, S36.7, and
        `cycle::queue::drain` is thread
        exit's, whose fate for an entry S39.1 owns. Wiring the retirement into
        that drain today would dereference entries, and the queue's own test
        fixture writes bare `RcHeader`s on the stack rather than allocated
        entities, deliberately and with a comment saying why: "nothing on this
        path dereferences the entry it writes". A drain that read them would
        read freed stack memory. So the mechanism is here and the wiring is
        S39.1's, together with the fixture change it needs.
      handoff: what landed. `memory::stdapi::ll_free` withholds an entity slot
        whose header carries `ENROLLED`, ahead of every route and after the
        reset window's own two guards — one door, because every slot return in
        the crate reaches that one. Nothing is recorded: the queue entry is the
        record, so `used` falls at the return and never at the parking, which
        is what keeps such a block out of the pool. `refcount::is_enrolled` and
        `clear_enrolled` are the two accessors, the second carrying an
        `expect(dead_code)` naming S39.1 until a caller arrives.
      handoff: two tests in `memory::stdapi::tests::`
        `the_slot_a_queue_entry_names`, both seen failing with the parking
        deleted: a block emptied around a parked corpse reaches the pool only
        when the last slot returns, and a parked body still reads the count the
        death left. Every `S34.3` citation in `src/`, `docs/` and the two maps
        was swept in the same commit — the sites that named two windows now
        name the one that is left, which is S36.2's.
- [x] S34.4 Prove the corpse rule against arena reuse
      done: a red-first test enrols, kills, resets the arena and drains, and the
        category-zero clause is what makes it pass
      tier: T1 · role: —
      handoff: closed 2026-08-29.
        `cycle::queue::tests::an_arena_entity_leaves_no_entry` builds a real
        arena object, takes it through the decrement the gate judges, resets
        the arena and watches a fresh one be handed the same block. Red against
        the gate with `MEMORY_CATEGORY_MASK` removed: an entry appears, naming
        a slot the reset then gives away.
      handoff: **the first fixture was green against that mutation** and had to
        be rebuilt. `release_word` returns before the decrement for a non-zero
        category unless `COW` is set, so a plain arena object never reaches the
        gate at all and the test proved nothing. The entity is shared on write
        for that reason, which is the construction `the_enrolment_gate` uses
        for the same clause; the test says so where it does it.
      handoff: what the clause is worth, in one line, since the step exists to
        record it: a GC-heap slot that dies enrolled is withheld by the free
        (S34.3), and an arena slot has no free to withhold it — `ll_arena_reset`
        returns the block whole — so an entry naming one would survive into the
        next request's memory.

- [ ] S34.2 The law: only the owner reduces state
      done: no dirty pass clears an enrolment bit, drops a queue entry or
        returns a slot; a reader may mark an entry a corpse and pass it on; the
        bit is cleared only by the owner consuming the one token that names a
        dead entity — the drain's corpse rule, or S36.5 commit for an in-flight
        root — and **never by death itself or at acquittal**, which supersedes
        Y12 clause 4's
        "cleared after the root is walked"; a test proves the acquittal case —
        ring A↔B with an external X→B that is released after the trace read the
        count — does not lose the ring, and the assertion is that a later
        collection reclaims it, not merely that the bit is still set
      tier: T2 · role: Sage → Critic
      handoff: clause 4 and the law of 2026-08-26 contradicted each other, and
        both were in the plan. The Sage ruled for the law: clearing on acquittal
        is the permanent miss, because enrolment is edge-triggered.
      handoff: the instant this step's test waits for was ruled on 2026-08-27
        (`rfc/dev/PLAN.md` S8.3, and the entry it names in
        `rfc/dev/DECISIONS.md`). An acquitted root parks in the owner's own
        suspects buffer with its bit set, and the first collection after the
        maturation epoch counter moves detaches that dormant lane beside the
        active lane as one composite in-flight batch. The test therefore forces
        the counter forward through a `#[cfg(test)]` shorthand, runs the poll,
        runs a collection and
        asserts the ring reclaimed, which is the assertion this step demands
        instead of "the bit is still set".
      handoff: **it waits on three later stages, and the plan's order had it
        first.** Every clause but one was about code that did not exist when
        the step was written: the dirty pass exists now — `cycle::mark` and
        `cycle::scan` — the corpse mark landed with S34.3, and commit's free
        is S36.5's.
        The one clause that is about today's code — the bit is never cleared at
        acquittal — holds vacuously, nothing in the crate clearing `ENROLLED`
        at all (checked 2026-08-29; `refcount.rs` only sets it). And the test
        the step demands needs a collection to assert reclamation, which
        `gc::ll_gc_collect_cycles` does not have until S36.7, plus the
        maturation counter of S37.1 and the suspects buffer of S37.4 for the
        instant it waits for. Moved last in the stage for that reason; the work
        order takes it after S37.4.

## S36 — Commit

Goal: only the owning thread frees. An in-line owner trace may commit what its
stable mark/scan proved; a speculative trace must first pass the owner's exact
test. The teardown is here in full: the Critic round of 2026-08-26 found the
stage claiming the frees while building none of them.

- [x] S36.1 The exact test on the owner's thread
      done: current fields are re-read on the owning thread before any free, and
        the test opens with the corpse rule — a member read at count zero drops
        the component whole before any guard or field write — exercised by a
        test in which tearing down one component releases into a second already
        judged white; the refusal path is exercised by a mutation racing the
        verdict, and by a positive control in which the same scenario without
        the mutation does free
      tier: T2 · role: Critic
      Critic 2026-08-29: eight findings. Taken — the safety contract names a
        member whose slot is still its own rather than a live entity, since the
        corpse rule reads a count of zero; the corpse test compares the state
        before the call against the state after it instead of asserting a
        residue the runtime never holds; the control arm builds the keeper it
        does not write into, so the two arms differ by the store alone; the
        sum's premise is checked member by member in a debug build, the sum
        being unable to see a defect that invents one in-edge and loses
        another; an empty component, a member outside the GC heap and a zero
        count under a guard are debug-asserted. Refused — naming the corpse in
        the answer: the disposition of an entry belongs to the queue drain,
        which sorts entries rather than members
        (`rfc/model/gc/cycle/questions.md`, Y12 clause 5). Verified and not a
        defect — `row::edge_to` places a `LongLived` entity on an interior row,
        which its own comment covers: the category is out of use.
      handoff: `src/cycle/exact.rs` is `judge(members, discount)`, and the
        tests are under `src/cycle/exact/tests/`; four source mutations were
        run and each was caught by the test that owns it. Two debts leave with
        this step. **Nothing derives a member list from the condemned rows**,
        no step of this plan owns that, and the design leaves the vehicle for
        that memory unnamed (`rfc/model/gc/rc-cycle.md`, "The release obliges
        a readership rule"). And a member of a kind other than an object or an
        array is untested — `Reference`, `Lazy`, a template, a class with cells
        outside itself.
- [x] S36.2 The trace-window parking
      note: S36.1's Critic round found the window this step has to cover. A
        member that never took a non-final decrement has no queue entry, so
        nothing names its slot; what keeps its header readable for the corpse
        rule is this step's trace window and not the entry.
      done: a slot freed while mark or scan may still address its row waits for
        the trace's end, releases into
        S34.3's single return path, and that path refuses while **either**
        window is open — a queue entry naming the slot, or a trace in
        flight; a red-first test shows the defect it prevents, a reused slot
        inheriting the dead occupant's row, and overlaps the two windows in both
        orders
      tier: T2 · role: —
      handoff: `cycle::parking::TraceWindow` owns the `ShadowArena`, so its
        drop order is not a caller convention: first reset and null every row,
        then lower the owner-local active flag, then replay the out-of-band
        returns through `stdapi::ll_free`. It is `#[must_use]`, cannot move to
        another thread, and a nested open fails in release as well as debug.
      handoff: all three row populations wait. A retained block has no slot
        free list, but its last occupant returns the whole block; pooled large
        entities return a block and OS-direct ones unmap a run. Seven tests
        cover those three routes, a slotted address inheriting a corpse's row,
        and both orders of the queue/trace windows. The retained work also
        repaired S34.3's older omission: its `ENROLLED` test had named only
        slotted and large entities. The block-return sentinel is deliberately
        distinguished from a retained entity pointer before any header read.
      handoff: this is the synchronous owner-side substrate only. S38.1/S38.3
        must replace the TLS active state and list with owner-addressable state
        before a worker traces another thread; the generation/handoff problem
        remains RFC audit A3 and is not claimed closed here.
      handoff: Critic and Sage reviews 2026-08-31 found the retained omission,
        the arena-before-replay ordering, an impossible first version of the
        reverse-overlap fixture, the movable/nestable guard and the exit order.
        All were taken; the old objection to a `Vec` allocation was withdrawn
        against the later decision that explicitly accepts the cold,
        trace-only allocation.
- [ ] S36.9 The GC-memory contract   *(before S36.3)*
      progress 2026-09-01 — S36.9a physical contract: the single
        `memory::gc_metadata` door owns pool/reserve adoption and return,
        `BLOCK_KIND_GC_METADATA` makes the bytes collection holds identifiable,
        and current/high-water block counts are observable. Queue control moved
        from TLS into one 64-byte floor line; TLS is one non-owning pointer,
        escrow capacity is 8,152 and `POLL_STRIDE` is re-derived as 4,076.
        This does not close S36.9: logical accounting and allocator-free
        parking, weak and retained storage remain.
      repair 2026-09-01 — the slice's `escrow` addressed the block through a
        `&OwnerCycleState`, which covers the control line alone; Miri fails the
        write on the `ll_release_vector` path and the parent tree passes. The
        floor pointer is threaded through `grow_and_write` and `escrow`
        instead, `escrow` is `unsafe` and states the precondition, and the
        overflow test that spawns a child now carries `cfg_attr(miri, ignore)`
        — without it Miri stopped at that test and ran none of `cycle::` after
        it. `dev/POSTMORTEM.md`, 2026-09-01. Miri over `cycle::`: 86 passed,
        0 failed, 1 ignored.
      repair 2026-09-01 — the slice's tests were rebuilt where they agreed
        with the code instead of constraining it. The capacity figures are
        asserted as the literals the documents name, and the escrow's last
        entry is asserted to end flush with the block, which is what makes the
        capacity exact rather than sufficient. The overflow test reads the
        child's signal rather than its exit status. The three refusals at the
        boundary have tests of their own — `BlockPool::put` and
        `critical::give_back` against a GC-stamped block, `adopt` against a
        source that is not the reserve — the reserve's aimed at the arm that
        keeps the block, since at capacity the pool answers first. Six source
        mutations were run and each was caught by the test that owns it.
      progress 2026-09-01 — S36.9b logical ledger: `gc_metadata::charge` and
        `discharge` keep one current and one high-water figure for the bytes in
        use inside the blocks collection owns. The figure moves at five
        structural transitions — a segment leaving the live position, an escrow
        landing, a floor's control line, an arena block leaving the bump, and
        the reset that publishes the block under the cursor before discharging
        the collection's whole total — so the enrolment write takes no added
        instruction. Two residues are documented granularity, each bounded by
        one payload: the live segment's own fill and the arena block still
        under the bump — and each is entered in the high-water figure by the
        transition that ends it, which is exact on one thread and can miss a
        maximum two threads stood in together. Thirteen source mutations were
        run and each was caught by the test that owns it.
        This does not close S36.9: allocator-free parking, weak and retained
        storage remain.
      Critic 2026-09-01: fifteen findings. Taken — `drain` released the live
        segment without charging its fill, so a thread that filled a segment
        and never overflowed was absent from the high-water figure while three
        documents called that figure exact; `drain_escrow`'s discharge was the
        one ledger site no test reached, and deleting it leaked eight bytes per
        entry for the life of the process; `current_bytes_in_use`'s contract
        stated a bound against the reservation that this step's own test
        contradicts, a spare segment and a block header being reservation; the
        peak assertions were absorbed by the process-global high-water, which a
        `#[cfg(test)]` door that lowers it to the current figure now makes
        exact; no test drove `enrol`'s ordinary write, the path the design
        exists to keep clear; `drain_escrow` took one read-modify-write per
        entry where `drain` took one for the run; `stats` could report a
        high-water figure below its own current one, and the byte axis was
        lifted — round two found the block axis still unlifted and it was
        lifted there; the
        payload charge rested on a fullness invariant with no assertion;
        `draw_floor`'s comment gave a reason that was not the reason, and the
        arena's claimed a single publish site it does not have. Refused —
        nothing: the two remaining findings are the `expect` on the discharge,
        which the Critic could not fire against the five sites, and the
        citation form, fixed in place. Known gap: the journal's re-entry into
        `draw_floor` is driven by no test, so a charge moved above its
        installed check would go uncaught.
      Critic 2026-09-01 round 2: eight findings, all taken, two of them
        defects the first round's repairs introduced. Batching `drain_escrow`'s
        discharge into one operation for the run left the escrow's bytes
        standing over entries already re-enrolled, so a recovery inflated the
        high-water figure by a whole payload for the life of the process — the
        discharge went back to one per entry, and `dev/BENCHMARKS.md` records
        the batch as tried and refused. `drain`'s charge-and-discharge pair for
        the live segment's fill was observable by another thread as a current
        figure holding a segment already gone, so a peak-only
        `gc_metadata::mark_peak` replaced it. Of the rest: the block axis was
        never lifted the way the byte axis was, and the contract claimed both;
        the high-water figure carries a residue only from the transition that
        ends it, which is exact on one thread and not across threads, and five
        texts claimed it exact — each now states the bound; every enumeration
        of the charge sites was one short of the code; the contract explained
        the cross-axis excess by the wrong read order; and four of the new
        tests passed under a mutation of the line they were written for, which
        two rewritten tests and two added assertions now fail. Not taken
        further: the device stops at two rounds.
      progress 2026-09-01 — S36.9c manager-backed withheld returns: the
        physical returns an in-line trace withholds move out of a
        `Box<Vec<_>>` and into a chain of `gc_metadata` blocks. The first is
        drawn at `ActiveTrace::open`, which now answers `Option<Self>`, so both
        doors refusing is a collection that does not start rather than a
        refusal met with a slot in hand; a growth past that block that both
        doors refuse ends the process, as the overflow buffer's bound does.
        TLS keeps one non-owning pointer to the head block and a null one is
        the closed window, which retires `TRACE_ACTIVE`. Measured on the day:
        the withheld-return path made 2 global allocations and now makes 0
        (seen red before the first edit), `.tbss` 480 bytes to 464, `cycle::`
        under Miri 104 passed to 113 with 0 failed, and the `lifecycle` timed
        run void on its own A-A control (`dev/BENCHMARKS.md`). Three of
        S36.11's `done:` claims land here, and its clause names what is left
        to it. This does not close S36.9: weak and retained storage remain.
      progress 2026-09-01 — S36.9d the weak table and the streaming drain: the
        per-thread weak table leaves the global allocator for an open-addressed
        table in one long-lived buffer payload — the mutator's storage class,
        not `gc_metadata`'s, because the ledger counts what collection holds
        and a thread that never collects fills this table. Sixteen-byte rows
        carry the target and a tagged subscriber word, capacity is a power of
        two at a load of one half, and every fallible step of
        `ll_weakref_create` runs before it holds anything, so a refusal answers
        null. `drain_arena_weak_log` notifies inside the drain's own walk
        instead of collecting into a `Vec`. Measured on the day: the first
        create made 2 global allocations and now makes 0, 200 creates across
        three growths made 8 and now make 0, the reset's weak walk made 2 above
        its control arm and now makes none, `.tbss` 464 bytes to 472 with the
        crate's thread-local set unchanged. `weak::` under Miri went 7 tests to
        19 with 0 failed, measured before the second Critic round's repairs:
        from 2026-09-01 Miri runs at the close of a logical block rather than
        of a step (Edmond; `dev/WORKFLOW.md`), so the run that covers this
        tree is S36.9's and is owed there over `weak`, `cycle` and `memory`. This does not close S36.9: retained storage remains, and
        the composite deny run over a wired collection waits for S36.7.
      Sage 2026-09-01 (slice d gate): the buffer layer is the consumer and
        `gc_metadata` is refused, the block kind being the answer to whose
        memory a block is; `array::table`, a cell pointer in the object header
        and a slot-indexed side array are refused with reasons; null at the ABI
        entry is the refusal answer, since `create` has a caller who can
        decline; the ledger gains no site and no residue; the deny gate is
        module-level zero and may not claim the collection-run leg; and no
        timed run is taken, this box's control having been void today. Taken
        whole, including the initial 64 rows and the half load.
      Critic 2026-09-01 round 1 (slice d): nineteen findings, none of them in
        the table's arithmetic, which the Critic put through a fuzz of its own
        against a model in its scratch directory — an instrument of that review
        and not of this repository. The load-bearing ones: neither the
        growth's nor the disposal's return of the payload was constrained by
        any test, and Miri cannot see it because the chunk is pool memory
        rather than the global allocator; the ledger test compared against a
        process-global high-water figure it had not lowered, so a charge would
        have passed it; the growth probe never asserted that a growth happened
        and counted its own `collect` inside the window; and the streaming
        drain's stated reason — "runs no user code" — is weaker than the
        condition it needs, which is that the callback may not reach the
        `Arena` at all while the walk holds `&mut` on it. All taken. Not a
        defect: `find`'s tag mask is behaviourally dead while the only tag is
        zero.
      Critic 2026-09-01 round 2 (slice d): fifteen findings, five of them in
        round 1's own repairs. The ledger test lowered one high-water figure of
        two, so a table drawn through `gc_metadata` would still have passed it
        — the test-only door lowers both axes now; the growth assertion could
        not fail, and pins the capacity three growths reach instead; the
        `drain_weak_log` doc gained a second summary and an order the log does
        not have, newest segment first being the chain's; and "the refusal is
        taken before anything is built" is false of the cell's refusal after a
        growth has already taken hold, which the prose now states. Also taken:
        the payload-return tests held the address and not the size the free
        recorded, which a second request at twice the size now pins; the
        OS-direct boundary was asserted against a copy of the arithmetic; and
        `chunk_from_the_free_list` restored the pressure mode it found rather
        than `Plenty`. The device stops at two rounds.
      Sage 2026-09-01 (slice c gate): the records live in a chain of manager
        blocks of their own, drawn at the open rather than at the first
        withheld return, because a refusal is answerable only before a slot is
        in hand; the trace arena is refused as the store, its own reset
        returning the record blocks before the replay reads them, and the
        queue base block is refused, its payload being exactly full and its
        lifetime the thread's. Refusal model: `None` at the open, no failure
        within capacity, process abort on a refused growth. Taken whole. Where
        the implementation departs: the append reads three loads rather than
        the ruling's two, the fill living in the block the cursor points into.
      Critic 2026-09-01 round 1 (slice c): thirteen findings, eleven taken.
        The load-bearing ones: an unwind out of the replay stranded the chain
        and left the ledger permanently inflated, `BlockPool::put` panicking on
        a poisoned mutex and nothing behind it re-entrant the way
        `arena::reset` is; `mark_peak` ran after the arena's reset, so the two
        residues of one collection were never in the ledger together while
        `gc_metadata` called the figure exact for one thread; the growth path's
        reserve accounting was constrained by no test; and the thread-exit call
        site still carried a comment about the `Vec` this step deleted.
        Refused: extracting the funding machinery this module now shares with
        `arena` and `queue` — that is S36.11's primitive, and building it here
        is that step done early.
      Critic 2026-09-01 round 2 (slice c): twelve findings; the first is a
        use-after-free round 1's own repair introduced. Moving the chain under
        its own `Drop` left the thread-local pointer naming it to the enclosing
        close, so an unwind out of the row sweep released the blocks with the
        window still standing, and the next free would have written a record
        through a stale cursor into memory the reserve had lent out again. The
        window is taken down by whoever releases the chain now, on both paths.
        Also taken: the ledger counted a later block's reserved control line as
        in use, against `current_bytes_in_use`'s own rule; the module cited
        Y14 for a claim Y14's 2026-08-26 amendment reverses; `critical`'s
        stamping enumeration was wrong in class and not only in count; and two
        prose claims contradicted the code they stood over. Refused: the
        finding that a test's failure message states the opposite of its
        contract — the message names the failure, which is what the crate's
        assertions do.
      Known gaps of slice c: no test drives an unwind out of the row sweep, so
        the second take-down of the window rests on reading; and nothing
        constrains `mark_peak` against a `charge`/`discharge` pair, which one
        thread cannot observe.
      Sage 2026-09-01: the charge belongs at a structural transition rather
        than per grant; the figure is bump consumption published at those
        transitions rather than a per-grant sum; the pre-change baseline is the
        gate, Miri over `cycle::`, `.tbss` and the enrolment operation count,
        the logical figure itself having no prior instrument. Taken whole —
        `dev/DECISIONS.md` carries the charge sites and the refused
        per-enrolment alternative, `dev/BENCHMARKS.md` the operation count and
        what `.tbss` cannot resolve.
      handoff: S36.9 is executed as separately reviewed slices: (a) physical
        block contract and queue state; (b) logical ledger and current arena
        instrumentation; (c) manager-backed parking plus ordinary/abort deny
        gate, done 2026-09-01; (d) weak-table ownership and streaming arena
        drain; (e) retained
        index/registry/snapshot ownership plus the direct-large registry audit.
        Only their composite source audit and deny test close this checkbox.
      done: every block owned by the candidate queue or a collection is drawn
        through one memory-manager wrapper, carries
        `BLOCK_KIND_GC_METADATA` while held, and is counted once — the kind is
        the whole answer to whose memory a block is, and collection is not
        split by use (Edmond, 2026-09-01, `dev/DECISIONS.md`); pool and
        critical-reserve handoffs restamp both directions, physical
        current/peak blocks and bytes are observable without double counting,
        and thread exit returns the direct count to zero. Beside it one pair
        of logical figures — bytes in use inside those blocks, current and
        high-water — which is what says how much of a reserved block is
        working memory. Which structure holds them is not carried in a
        production build (Edmond, 2026-09-01); the per-structure split is a
        build-time feature of its own, designed with
        `dev/design/debug-modes.md` axis A and owned by the backlog line
        below
      done: the production collection path contains no allocator-owning Rust
        container or hidden global allocation. The source audit covers
        `cycle`, its parking and deferred-drop storage, the weak registry and
        disposal path S36.3 reaches, and every registry the collector proposes
        to retain or cache; a collection run under a denying/counting global
        allocator covers ordinary, retained, weak, parking and abort paths and
        performs zero global allocations. A source/ownership audit catches
        backing allocated before the denying window opened
      tier: T2 · role: Sage → Critic
      handoff: this supersedes S36.2's acceptance of `Box<Vec>` parking. A
        manager-issued block stamped merely `ARENA` is not enough: the manager
        must be able to answer how many bytes GC owns.
        Control headers live in the manager block's header/payload; TLS holds
        only the non-owning pointer that finds the owner state. The present
        queue `Cell`s therefore move out of TLS into its floor header, and the
        queue capacity, poll stride and between-polls guarantee are re-derived
        and statically checked against the resulting layout.
      note 2026-09-01 — slice e is worked out in full before its code:
        `dev/CYCLE-COLLECTOR-REVIEW.md` finding 3 records that every production
        reader of the retained registry asks about one block whose address it
        already holds, and that its one enumeration has no production caller;
        `dev/design/retained-index-ownership.md` moves the index into the
        retained block's own collector line with the array in a per-thread
        chain of manager blocks, and names four questions for `rfc`. That is a
        design change rather than a backing move, so the fork below stands
        before the slice.
      handoff: the ownership audit must settle the retained registry before a
        cache is built. Its present `BTreeMap`, `Arc<[usize]>` and snapshot
        `Vec` may not be smuggled into the collection under the claim that an
        `Arc` clone itself allocates nothing. Either their backing moves under
        the manager or the registry is redesigned at its owning layer; there
        is no cycle-path exemption.
        The one site the composite audit exempts by name is
        `validation::member_counts_cover_internal_edges`'s in-degree `Vec`:
        it runs under `debug_assertions` alone, on the owning thread, and no
        release build allocates it (S42.1, 2026-09-01; the S42 Code Reviewer
        asked that the exemption stand here rather than in the closed stage).
- [ ] S36.14 Decide the retained index's owning layer   *(before S36.9's slice e)*
      done: the choice is recorded in `dev/DECISIONS.md` with the rejected side
        and its reason — either the present registry keeps its shape and only
        its backing moves under the manager, or the index moves into the
        retained block and the registry goes; and if the second wins, the four
        open questions of `dev/design/retained-index-ownership.md` are answered
        in `rfc` before any code, which is what makes them answerable at all:
        an owner word in the block, the index chain at thread exit, when a
        chain block is released, and what `for_each_entity_slot` may read
      tier: T2 · role: Sage
      note: the proposal's own working already refuses a block per index and
        an index beside its retained block, with reasons; what it does not
        settle is the block-owner word, which is the same prerequisite the
        collector worker waits on (`rfc/dev/ALGORITHM-AUDIT.md`, A4)

- [ ] S36.10 The persistent per-owner workspace   *(before S36.3)*
      done: thread init draws one mandatory 64 KiB workspace base from the
        ordinary block pool, after the queue floor and before registration is
        published; a refusal rolls the partial init back and refuses the
        thread. The base is rewound between collections and returned only at
        thread exit. It is never drawn permanently from the critical reserve;
        overflow asks the pool and then the reserve and returns after every
        commit or abort
      done: the workspace is a typed `Idle → Trace → Commit → Idle` ownership
        transition. Trace end filters members while rows are readable, sweeps
        every block shadow, lowers the active flag and replays parking, but
        does not rewind bytes the commit still names; commit or abort performs
        the final rewind. Nested use and a phase-invalid pointer fail in every
        build
      tier: T2 · role: Sage → Critic
      handoff: mandatory direct cycle memory becomes 131,072 bytes per
        registered thread — one 65,536-byte queue floor and one workspace
        base. The two best-effort queue spares make the nominal/maximum direct
        baseline 262,144 bytes when both are present. The separate critical
        reserve has capacity up to 524,288 bytes and is not guaranteed resident
        or workspace capacity. Initially exactly one workspace block is
        retained; retaining warm overflow waits on S40.3.
      handoff: a dense 381-entity shape needs 23,568 bytes (about 23.0 KiB)
        for the widest block's rows, one present-day stack segment and member
        pointers, so it fits the base by calculation; one entity in each of
        381 widest blocks reserves 6,251,448 row-array bytes, or 6,258,608
        bytes (about 5.97 MiB) with that stack and 381 member pointers. Neither
        number is a workload measurement.
- [ ] S36.11 The managed lists and the small worklist   *(before S36.3)*
      done: one manager-backed segmented primitive serves collection-owned
        pointer records with explicit `read`/`used` bounds and no drop glue;
        parking, condemned members and S36.5's deferred drops use it, while a
        small fixed worklist in the workspace serves leaf and small traces and
        grows into the same managed segments only on overflow
      done: a worklist entry carries the pair (entity, row pointer) rather
        than the entity alone, so the scan's pop reads the colour through the
        pointer instead of resolving the row a second time — the pointer and
        not the colour, because another path can recolour the row between push
        and pop (`dev/CYCLE-COLLECTOR-REVIEW.md`, finding 2). Mark reads no row
        at its pop and carries the pointer for one entry shape
      done: the trace arena bumps row arrays from a block's front and worklist
        segments from its back, growing when the two cursors meet, so the tail
        a 16,408-byte array leaves at the smallest size class — 24.6 % of the
        payload — is spent rather than abandoned; `residue` counts both ends
        (`dev/CYCLE-COLLECTOR-REVIEW.md`, finding 1)
      done: the Sage gate names the fixed small-worklist and pre-reserved
        parking capacities from the 65,280-byte payload before code begins;
        boundary tests exercise exactly capacity and capacity plus one, and
        the documented budget accounts for the other base-workspace residents
      done: the withheld returns take their base capacity from the workspace
        payload rather than from a block of their own, and the chain never
        writes a link into a corpse. What S36.9 slice c built and this step
        inherits rather than repeats: no `Box<Vec>`, an overflow that asks pool
        then critical, a documented hard failure rather than a lost physical
        return, and a replay through `stdapi::ll_free` after the row sweep
      tier: T2 · role: Sage → Critic
      handoff: the current first worklist push reserves 4,112 bytes for 512
        pointers even for a leaf, and current non-empty parking performs global
        allocation. Red tests show a small trace makes no manager overflow
        draw, two collections reuse the same base, corpse bytes remain intact,
        critical capacity is restored, and success and abort both return GC
        bytes to the per-thread baseline.
- [ ] S36.12 The in-flight batch and condemned membership   *(before S36.3)*
      done: collection detaches the active candidate chain as one in-flight
        batch whose bounds travel with it; every first-reached entity appends
        one manager-backed member record, all roots mark before any root scans,
        and final colours compact the records while rows are still readable;
        the resulting storage survives the trace close through commit
      done: refusal after detach or after any member append aborts the whole
        trace, sweeps its rows and restores every in-flight token to its source
        lane without allocation. No `CANDIDATE_BIT` bit is left without exactly one
        logical record and no record exists in two lanes
      tier: T2 · role: Sage → Critic
      handoff: choose and test the commit unit here. A single condemned batch
        is safe under the aggregate exact sum but resurrection in one connected
        part conservatively retains the others; if teardown promises
        per-component behaviour, this step must extract components instead of
        silently passing their union to `validation::validate_component`.
      handoff: an enrolled death does not clear the bit or retire its record.
        Teardown finishes and physical return waits; only the consumer that
        still owns the record may observe count zero, clear `CANDIDATE_BIT`, return
        the slot and retire the token. The reverse order creates a dangling
        queue pointer that can name a new occupant.
- [ ] S36.13 The retained-block visit   *(after S36.12, before S36.7)*
      done: the first reach into a retained block acquires its immutable
        occupant index once under the registry lock and records a
        manager-owned, trace-bounded visit; every later mark and scan lookup in
        that block searches it without the registry mutex, and no handle
        survives the trace token
      tier: T2 · role: Sage → Critic
      handoff: this starts only after S36.9 settles who owns the retained index;
        cloning the present `Arc` is not compliance. The counter proves one
        registry acquisition per touched retained block rather than today's
        approximate `2E + V + B` acquisitions for a retained-only trace. Its
        Sage gate records that old counter before changing the lookup.
- [ ] S36.3 The guard and the weak window
      done: after the exact test confirms, every member takes the teardown
        guard, then every weak cell naming any member is nulled, all members
        before any destructor; a condemned ring A↔B with a weak cell on A, whose
        `__destruct` on B loads that cell, reads null inside the destructor —
        seen red against the per-member order before the component-wide call
        lands; a component the exact test refuses leaves every cell resolving
      tier: T2 · role: Critic
      handoff: the guard is needed even single-threaded — a destructor releasing
        an internal edge would otherwise drop a member to zero and start
        ordinary death inside the teardown. The window is the one PEP 442 exists
        to close.
- [ ] S36.4 Destructors and the resurrection re-verify
      done: `__destruct` runs per object member on the owning thread; when any
        ran, the exact test runs again with the guard discount; a failure
        releases the guards through the counted path, so survivors keep true
        counts with destructors still ahead of them, and the component is
        abandoned with its cells nulled; a destructor that stores `$this` into
        an external root proves both the acquittal and the nulled cell — the
        divergence from PHP that `weak-references.md` records
      tier: T2 · role: Critic
      handoff: the re-verify survives the shortlist framing rather than
        contradicting it. Garbage is monotone only while no reference to the
        component exists outside it, and the destructor runs holding `$this`, a
        reference the teardown itself handed to user code.
- [ ] S36.5 Sever, free and the deferred drops
      done: internal edges are severed with external children collected; the
        guards come off through the counted release and each member reaching
        zero dies through the ordinary death path into S36.2's parking; the
        deferred-drop queue — severed children and the weak notify's displaced
        map values — drains only after the last member's free, the order proven
        by a test-only sequence probe; a weak cell re-created on a condemned
        member is cleared by the free-time `HAS_WEAK_REFERENCES` notification
      tier: T2 · role: Code Reviewer
- [ ] S36.6 Commit writes the maturation stamp
      done: on the owning thread, after judgement, each proven-live component is
        stamped as a unit — current epoch and `min(age) + 1` saturated at 3, one
        single-byte store per member, never inside a wider access; a condemned
        or unjudged component is never stamped; in the accelerator form the
        posted proven-live components are stamped by the owner at its drain, so
        the stamp byte has one writing thread in both forms; a test matures a
        live ring across two collections and shows the third pruning it, read
        off the S37.1 counter
      tier: T2 · role: —
      handoff: commit is the only writer because a mature stamp suppresses
        descent, which is a reduction of future suspicion and therefore the
        owner's by the law of S34.2 — and because the mark writes into no
        entity, which is what makes an aborted collection free.
- [ ] S36.7 Wire the collection into the ABI
      done: `ll_gc_collect_cycles` runs a collection and reports what it
        reclaimed, and `ll_gc_maybe_collect` fires on the armed pending flag and
        nowhere earlier; a test arms the flag, shows nothing collected before
        the next poll, and shows the collection at it — restaging the
        deferred-fire contract the dying `gc/tests/where_a_collection_may_fire.rs`
        carried
      tier: T2 · role: —
- [ ] S36.8 Elide the redundant exact test after an in-line owner trace
      done: when mark and scan run synchronously on the owning mutator at one
        consistent point, a condemned component proceeds directly to the owner
        commit — guard acquisition, weak invalidation and finalization — without
        `validation::validate_component` re-reading the same counts and fields first; the
        speculative/off-thread path still calls `validation::validate_component`, because its
        shortlist may combine observations from different instants; tests count
        exact-test entries and prove zero before teardown for the in-line path,
        one for a posted speculative result, and no behavioural difference on
        a ring with an external keeper; a benchmark records the saved member
        and edge reads for component sizes 2, 16 and 256
      tier: T2 · role: Critic
      handoff: this removes only the pre-teardown confirmation. S36.4's exact
        re-verify after any `__destruct` remains mandatory in both paths: user
        code ran with a guarded `$this` and may have resurrected a member. The
        in-line shortcut is valid only while the owner has not released the
        collection's consistency window between the final scan decision and
        guard acquisition; encode that boundary in the API so a future caller
        cannot pass a stale condemned list as an in-line proof.

## S37 — Maturation and the two class gates

Goal: the trace stops following the whole heap. On a booted Laravel corpus the
subgraph reachable from a median candidate root is 381 of 381 objects, so this
stage is what makes a trace affordable rather than what tunes it.

- [ ] S37.1 The maturation stamp is an edge-side prune
      done: mark's descent reads the stamp with one single-byte load; a member
        whose stamp epoch equals the heap's current epoch (mod 4) and whose age
        has reached `k` is treated as an **opaque live external and is not
        descended into**, the same test skipping a mature popped root entirely;
        a stale-epoch stamp reads as age 0 and is never cleared in place, so the
        trace writes no stamp; the per-thread-heap collection counter advances
        the epoch every 64 completed collections and `k = 3`, both named
        provisional after YRC's only known values, with the measurement owed at
        S40.1; `#[cfg(test)]` counters report edges pruned and roots skipped
        mature per collection
      tier: T2 · role: —
      handoff: the root-side reading — "traced only after it has stayed a
        candidate across `k` collections" — was struck from this step and from
        `rc-cycle.md`'s summary bullet on 2026-08-26. It is not a second
        mechanism: it filters which roots start a trace and does nothing to the
        closure, and its real content falls out of the prune at depth zero.
        Y9 calls the prune the only mechanism in this design that bounds the
        closure.
      handoff: carried from S31 before that stage was deleted. **Two producers
        hand a member a stamp byte nobody wrote.** A recycled `heap::FreeSlot`
        preserves the dead entity's final header, so the slot arrives carrying
        the previous occupant's byte 6; and promotion rewrites a survivor's
        category with a two-byte store, so the byte leaves the arena exactly as
        it went in. Either one reads here as a mature stamp of the current
        epoch and this step prunes a live subgraph permanently and silently.
        The zeroing belongs to S38.0; this step's counters are what would show
        it missing.
- [ ] S37.4 The suspects buffer and the turnover re-offer
      done: acquittal never clears the enrolment bit; a proven-live root parks
        its **one existing token** in the owner's dormant suspects queue, and
        every suspect is re-offered at the first collection after the heap's
        epoch advances by detaching the due dormant lane beside the active lane
        as one composite in-flight batch, never by enrolling or copying the
        entity a second time; red tests prove
        that a matured ring losing its last external reference mid-epoch is
        collected at that re-offer and not before, and that a ring whose mates
        carry unequal ages is likewise collected, so maturing apart costs recall
        rather than a permanent miss
      tier: T2 · role: Sage → Critic
      handoff: this is the backstop the withdrawn "retired on contact" clause
        was supposed to be and never was — eager clearing fires only when a
        trace touches the entity, and the stamp that wraps is exactly the one no
        trace touched for four epochs. It also collects YRC's 56 % saving on
        re-registration.
      handoff: `CANDIDATE_BIT` means exactly one logical token in exactly one state:
        `active → in-flight → dormant`, or consumer-retired after death;
        epoch turnover detaches active and due-dormant heads/tails in O(1), and
        an abort restores each sub-batch to the lane it came from without
        allocation. A decrement while dormant sees the standing bit and cannot
        add a duplicate. Store original enrolled
        roots only — adding every traced live member manufactures tokens, and
        collapsing two roots in one component can miss it after a later split.
      handoff: current queue segments cannot be spliced after filtering: only
        the live head has a fill bound and every segment behind it is assumed
        full. S36.12's per-batch/per-segment `read` and `used` bounds, or an
        equivalent in-place compaction restoring full segments, are a hard
        prerequisite. Tests count one token across active + in-flight + dormant
        after repeated decrements, acquittals, partial segments, abort and
        turnover; a dormant corpse keeps identity until its consumer retires it.
- [ ] S37.2 The acyclic gate
      done: the factory stamps bit 8 from the class's own answer — waits on
        `rfc` `model/classes.md` declaring a target per pointer slot
      tier: T2 · role: —
      handoff: blocked outside this repository; the step is listed so the
        dependency is visible rather than discovered.
- [ ] S37.3 The ownership mark
      done: a proven-owned entity never enters the candidate set, and the
        compiler's stamp is honoured at bit 9
      tier: T2 · role: —
      handoff: `refcount::ENROLMENT_GATE_MASK` already tests bit 9 and no
        compiler stamps it, so this step's remaining content is the
        factory-side and FFI-side write.

## S38 — The claim and concurrency

Goal: a collection runs either in a collector thread or in the mutator, never
both, and the losing side never deadlocks.

- [ ] S38.0 The collector's reader
      done: a second `CellReader` impl reads `word` and `ptr` as relaxed atomics
        and answers nothing else — no storage version and no give-up, because a
        torn read costs at most a phantom edge or a missed one, and a child
        mapping to no GC-heap block already ends the descent as an external live
        reference (`cycle::row::resolve_edge_target`); the collector-thread trace
        instantiates `trace_cells`
        with it, and a Miri slice drives it over an object with outside cells
        and over an array mid-move
      tier: T2 · role: Critic
      handoff: `RelaxedCells` and its re-check plumbing died at S30.2 because
        they existed for `rc-walk`'s precision. What the accelerator needs is
        strictly smaller, and the `CellReader` trait is the socket it plugs into.
      handoff: two debts carried from S31 before that stage was deleted. **The
        stale stamp byte** is this step's to zero, at both producers — a
        recycled `heap::FreeSlot` and a promoted survivor — because this is
        where the second thread arrives and the byte stops being inert; S37.1
        is what it breaks. And **`dev/WORKFLOW.md`'s ThreadSanitizer run has
        selected no test since 2026-08-26**, its only one having lived in the
        deleted `collector::`, so the instrument that reports
        plain-against-atomic is         unavailable until this step gives it a pairing
        to watch.
      handoff: the collector thread's birth is this step's to name — startup
        or first pressure — with its floor refusal following it; a mandatory
        floor drawn at first pressure is the worst moment
        (`rfc/dev/DECISIONS.md`, "the escrow's floor is allocator-issued").
- [ ] S38.1 The claim
      done: one flag **per mutator thread**, free or held, taken by CAS and
        released by one store; a waiter blocks on a mutex rather than spinning;
        it covers the **trace** over that thread's graph — the arena, the block
        triples and the touched list — while each owner's exact judgement runs
        at its own checkpoint; a test proves a held flag blocks collection
        entry on the claimed thread alone while enrolment, release, allocation
        **and a second collector's trace of another thread** proceed
      tier: T2 · role: Critic
      handoff: the word was per process until 2026-08-29, when Edmond ruled the
        exclusion per thread (`rfc/dev/DECISIONS.md`, "a trace stays inside the
        blocks of the thread it claimed"). What licenses the narrowing is the
        transfer rule: `thread_move` and `thread_clone` require the graph
        arriving in a thread to hold no reference to an object that stays in
        the source, so no thread names an entity in another thread's blocks,
        and a block belongs to one thread's heap. Two collectors therefore
        never meet in a block, a triple or a row.
      handoff: the mechanism is Edmond's, 2026-08-29: the flag is the mutator
        thread's own, a collector going to judge that mutator takes it, and CAS
        with a mutex to wait on is the whole of it. That settles a
        contradiction this step had carried since it was written — it asked for
        three states and a thread-local held flag, which the ruling of
        2026-08-27 (`rfc/dev/DECISIONS.md`, "the trace token covers the trace
        alone") had already rejected by name. The narrowing to one thread does
        not revive them: a thread meets its own flag held only by a collector,
        never by itself, because mark and scan run no user code and the
        teardown that does runs with the flag released and the entry gate shut.
- [ ] S38.4 The entry gate and the slow-path fire   *(before S38.2)*
      done: the GC-heap slot allocation slow path, on a forced refusal that
        names which allocation refused, waits on this thread's own claim while
        it is held, takes it, runs the in-line collection, retries once and
        reports null only after; a shortage at teardown depth ≥ 1 collects
        nothing and reports; a heap of one size class full of cyclic garbage
        serves the allocation with no explicit collect call
      tier: T2 · role: Critic
      handoff: Y14's clause "a thread that finds the token taken does not wait"
        was argued from the handshake deadlock, and the amendment of 2026-08-26
        deleted the handshake, so the Sage retired the clause with its reason
        and generalised the wait to any non-self holder. That generalisation is
        recorded in `rfc` (Y14 and `rc-cycle.md`, Concurrency) as well as in
        `dev/DECISIONS.md`; it is a decision of the round, not of the design of
        record as it stood.
      handoff: "any holder but itself" and "a claim this thread already holds"
        were the process-wide word's phrasing, where a holder could be another
        thread's collection. With the flag per thread (S38.1, Edmond
        2026-08-29) the only holder of this thread's flag is a collector
        judging it, so the wait needs no holder identity and the self-held arm
        has no entrant; what stops a collection at teardown depth is the gate,
        which is unchanged.
- [ ] S38.2 The working wait
      done: an in-line collection needs no verdict list, no handshake and no
        second phase — it is exact with respect to the counts because the owner
        re-reads its own current fields — and a mutator that cannot allocate
        while the claim is held in either state waits for the trace to end
        rather than preempting; the test's running collection is staged by
        S38.1's harness seizure and reaches the wait through S38.4's path, with
        a `#[cfg(test)]` counter past the wait asserted non-zero, because a test
        that merely terminates terminates most easily when the wait is never
        taken
      tier: T2 · role: Critic
- [ ] S38.3 Parking the mutator's frees during a trace
      note: S36.2 built the owner-side substrate for one thread, where nothing
        frees inside the window: mark and scan only read, and the trace window
        ends before the user-code teardown by the decision of 2026-08-31. The
        hazard is this step's, and it is the one rc-walk already paid for — a
        collector reading an entity another thread frees underneath it
        (Edmond, 2026-09-01).
      done: while a collection is in flight over a thread's blocks, that
        thread's frees park until it ends, whichever thread performs them, and
        the parking covers every address the trace holds rather than entity
        slots alone — an array's table storage in a buffer chunk
        (`cells::trace_cells` strides it, and
        `buffer_arena::buffer_free_longlived_payload` returns it past the
        gate), a retained payload whose block goes home through
        `retained::give_block_back`, and an OS-direct run; the cost is
        measured as the churn held across one collection
      tier: T2 · role: —

## S39 — Thread exit  (carried from S29.2)

- [ ] S39.1 Exit drains its own queue
      done: `ll_thread_exit` retires its queue before handing the heap over,
        which for a zero-count entry means reading the refcount, clearing
        `CANDIDATE_BIT` and returning the parked slot; and the fate of a **live**
        enrolled entity at exit is **chosen** — which of collect, hand over or
        leak, and why — rather than described in a comment, with the test that
        observes the chosen fate named; a red-first test kills a thread between
        enrolment and collection
      tier: T2 · role: —
      handoff: the corpse half arrived from S34.3 on 2026-08-29, which built
        the parking and the two accessors it needs
        (`refcount::clear_enrolled`, whose `expect(dead_code)` names this step)
        and could not wire them. Two obstacles, both this step's: a parked slot
        that is never retired leaks for the life of the process, and the
        queue's test fixture writes bare `RcHeader`s on the stack, so a drain
        that dereferenced entries would read freed stack memory — the fixture
        has to allocate real entities first, and `cycle/queue/tests.rs` says in
        its own words why it does not today.
      handoff: the criterion previously read "named rather than left to the
        reader", which a doc comment saying "these leak" satisfies with S29.2's
        defect intact.

## S40 — Measure the trace's density and decide the row form

Goal: the one number the design still lacks.

- [ ] S40.1 Measure
      done: the share of a touched block's slots that a real collection traces
        is measured on the corpus and on a synthetic load, with the denominator
        named — occupied slots or all slots, which differ by two at the design's
        assumed half occupancy — and with the instrument checked against a
        synthetic block whose traced share is fixed by construction; the same
        instrumented run records the pruned-edge share and the suspects re-offer
        volume at `k` of 1, 2 and 3, which is what settles S37.1's two
        provisional constants
      tier: T2 · role: Bench
      handoff: the corpus arm needs a driver over `ll-model`'s own heap. The
        recorded corpus instruments read PHP's heap, which has no blocks and no
        slots, so this arm is Phase-D-blocked in the same way S37.2 is blocked
        on `classes.md`; the synthetic arm is not, and it is what the row form
        can be decided on if Phase D is far.
- [ ] S40.0 Redraw `docs/architecture.md`'s diagrams
      done: the four modules the diagrams show that no longer exist are gone
        from them, `rc-cycle`'s own boundaries are drawn as built, and the
        banner added on 2026-08-26 comes off with them; `dev/ARCHITECTURE.md`
        stays the source of truth and the two agree
      tier: T1 · role: —
      handoff: the debt of S30.5, which left the diagrams standing under a
        banner rather than redrawing them — a diagram of an unbuilt collector
        reads as structure that exists. It sits here rather than earlier
        because the boundaries are not real until S38 closes; S40 is simply
        the last stage that will still be open.
- [ ] S40.3 Count the workspace and the cache traffic   *(before S40.2)*
      done: per collection counters report unique roots, `V`, `E`, touched
        blocks and groups, row bytes reserved and written, distinct row/group
        lines touched, worklist/member/parking high-water, arena requested,
        granted and abandoned-tail bytes, base hits, overflow draws and
        returns, suspect parks/re-offers, exact passes and probes, retained
        registry acquisitions, physical GC current/peak blocks by funding role,
        and logical current/high-water bytes by workspace consumer
      done: pinned back-to-back runs over component sizes 2, 16, 256 and 381,
        dense and one-entity-per-block, ordinary and retained, record
        instructions, cycles, L1D/LLC/dTLB and branch misses plus manager draws;
        the control protocol is `dev/BENCHMARKS.md`'s and every cache conclusion
        remains a hypothesis until these counters measure it
      tier: T2 · role: Sage → Critic
      handoff: a widest flat row array reserves 16,408 bytes, or 257
        line-equivalents and 257–258 physical cache lines depending on
        alignment. First touch is proven only to write 121 bytes; how many
        distinct lines those writes address is fixture-dependent and measured
        here. A persistent 64 KiB block removes manager churn, not cache fills
        and not the sparse-row cost. S40.2 changes representation only from
        these data.
- [ ] S40.2 Decide chunks or not
      done: below 29 % density the chunked form replaces the flat array and the
        measurement is quoted in the decision, with its denominator; above it
        the flat array stands and the alternative is recorded as refused with
        its number
      tier: T2 · role: —

---

## Cross-cutting (every stage)

- The old collectors are reachable at `archive/pre-rc-cycle` and nowhere else.
  Nothing is copied back without a decision entry.
- Every fix carries a regression test verified to fail on the bug
  (`dev/WORKFLOW.md`, Tests).
- Miri runs in slices, never whole (`dev/WORKFLOW.md`, Miri).
- A claim about speed is a measurement or it is not made.

## Then: arrays as a performance problem

Opened 2026-08-07 at Edmond's request. What was representation work in it
is built — the generic element write, the strategy tag in the head, the
32-byte entry with its collision link inside the element Box, and the
2 → 3 migration — and the reasoning behind each is in `dev/DECISIONS.md`
(2026-08-07 for the entry, 2026-08-11 for the head). What is left is
measurement.

**Four constants stand on borrowed or invented numbers**: the string-key
check's reversal threshold, the compaction threshold taken from Zend at
about 3 %, and the flood ladder's two, `EQUAL_HASH_LIMIT` and
`CHAIN_LIMIT`. None of the four can be settled on this box —
`dev/BENCHMARKS.md` puts its noise floor at 1.5–3 %, and every effect in
question is smaller than that. So this is not a task waiting for someone
to pick it up: it waits for a machine that can resolve it, and measuring
here would produce a number indistinguishable from noise and harder to
retract than to publish.

## The vocabulary

The crate's identifiers and test file names follow the glossary
`rfc/dev/PLAN.md` S9 builds; nothing here is renamed before S9.1 rules on it.
Two words are already marked for rename. Counted in this crate on 2026-08-29,
after commit 8208815: `door` at 110 occurrences in the code and 80 in the
documents, `escrow` at 88 and 34.
`ResetWindow::escrow` in `src/memory/reset_window.rs` names deferred count
corrections rather than the queue's overflow buffer and takes a different name.

## Beside the hashtable: the memory categories

Opened 2026-08-06, out of the same review chain, and independent of the
questions above. The routing item of that round is closed
(`memory/routing.rs`, and `dev/DECISIONS.md`); two are left, and the
second gates the first.

- [ ] **Rename the memory categories**, in the RFC where they are
  defined, through the documents that refer to them, and in the crate —
  **deferred 2026-08-06**, reasoning in `dev/DECISIONS.md`. `LongLived`
  is named after a duration rather than an owner, which is why its
  reclamation was never decided; `Region` would mark exactly the entities
  no region owns, a `#[Region]` class owning *arenas*; and `Arena` would
  make `arenas.md`'s "between two request arenas: forbidden" false before
  the mechanism justifying it exists. Meanwhile the category is marked
  out of use on the enum itself.
- [ ] **The region reset, and the refusal that waits on it.** The
  mechanism that would make a long-lived category mean something: what a
  region owns, when it resets, how the owner's O(1) death reaches its
  entities, and what promotion across a region boundary is.
  `rfc/model/memory/regions.md` is the starting point. It also gates
  `ll_string_new_dynamic`'s refusal of that category — today nothing
  would reclaim such a string. Blocked on design, not scheduled.

## What is left of the old phase lists

The A-chain of the 2026-07-24 status snapshot is finished but for two
items, and every rc-walk build step of Phase B is built, so both lists
were deleted with the snapshot that framed them. What survives is below,
each line verified against the code on 2026-08-13 rather than against its
own checkbox.

- [ ] **A3's factory half.** The descriptor carries `dispose`, and
  `ll_default_dispose` stands in until the compiler generates one.
  `factory` cannot be stood in for the same way: its signature is
  `factory(ctx, category)` with no class parameter, so it needs per-class
  generation, and the generic path stays
  `ll_object_new(ctx, class, category)`. `clone`, `deep_clone`,
  `thread_clone` and `thread_move` are reserved for the multi-threading
  future. "Only the GC reads `traced_runs` as data" holds once generated
  disposes replace the stand-in. `rfc/runtime/object-lifecycle.md`.
- [ ] **A7, no zeroing by default.** `ll_object_new` zero-fills the whole
  body unconditionally; which slots need a defined initial state is the
  factory's to decide (`rfc/BACKLOG.md`, deferred optimizations).
- [ ] **`Lazy` (code 1) and `Box` (code 10) have no producer.**
  `ll_entity_die`'s switch serves five; Box waits on the FFI surface and
  Lazy on the compiler, and each reaches a `debug_assert!` meanwhile.
  `Lazy` nevertheless answers yes to `EntityKind::closes_a_ring`, on the
  argument recorded in `dev/DECISIONS.md`, "a kind's ring classification is
  written at its declaration, before a factory stamps it". `StringDynamic`
  (code 9) is not carried here: `string::reserve` stamps it whenever the
  placement is out of line, so the kind has a producer.
- [ ] **The threshold arming policy and the collector-thread accelerator.**
  What is left of the old escalation ladder after S38.4 builds the entry gate
  and the slow-path fire. The arming policy is the compiler's
  (`rfc/model/gc/strategies.md`, arm/fire); the accelerator carries the third
  claim state's production entrant, the proposal machinery that turns a dirty
  trace into a shortlist, and the owner-checkpoint judgement that reads it
  (`rfc/model/gc/rc-cycle.md`). Gated on a measured in-line pause that a
  collector thread would shorten — until then the in-line collection at a
  failed allocation is the whole trigger, and that is the design in force
  rather than a stopgap (Y14, amended 2026-08-26).
- [ ] **The birth count and the unique-owner policy**
  (designed 2026-08-17 in the old collector's document, sections "The birth
  count" and "Unique ownership"; S30.5 moves the text into `rc-cycle`'s
  documents or it goes with the file) — gated on a Phase D measurement of
  the share of dynamic publications with compiler-provable targets; the
  move rule (copy, barrier, or a never-moved proof) is the open design
  question.
- [ ] **Per-structure GC memory, behind a feature.** Which structure holds
  collection's logical bytes — shadow rows, the trace worklist, a component's
  member list, parking, deferred drops, suspects — is not carried in a
  production build (Edmond, 2026-09-01). The breakdown is an axis A feature
  designed with `dev/design/debug-modes.md` §8; what it needs before it is
  built is a question that wants it.
- [ ] **Pure destructors, and the hand-off drain** — proposed by
  Edmond 2026-08-18, analyzed the same day through three lenses and two
  Critic rounds; the analysis is `rfc/model/gc/pure-destructors.md`, with
  the 2026-08-23 amendment that withdraws the collector-side free. The runtime-only step (the specialized P0 dispose and the
  raw-sever drain arm) needs no ruling and no compiler; the hand-off
  drain waits on the residual-duties and tail-bound questions the
  analysis names, its external-child delay accepted by ruling
  2026-08-18; the child-release-order ruling landed the same day —
  specified, P2 keeps its call (`dev/DECISIONS.md`) — so the
  compiler tiers wait only on the compiler. The composition with the
  ownership pair — including the fast class that can block its own memory
  return — was `dev/design/owned-slots-and-the-walk.md`, deleted 2026-08-26
  and readable on `archive/pre-rc-cycle`; it is argued against the walk
  throughout, so it is a source to re-read rather than a conclusion to
  carry.
- [ ] **The horizon's borrow elision** (Edmond's algorithm, 2026-08-18,
  named `proof-horizon` until 2026-08-20) — closed, and no pre-D step can
  change that status: the scan is kill-only, the census is undated, every
  verification artifact needs the compiler. Pre-D work is instrument
  preparation: the graded corpus scan, the census channel list owed to
  `dev/DECISIONS.md` before the census is specified, the summary-language
  question. Three Critic rounds, the granularity ruling of 2026-08-18
  (`dev/DECISIONS.md`), Edmond's corpus names and his
  family-borrow-analysis and summary-language rulings, and the five
  questions the 2026-08-20 case book opened — the weak cell's uncounted
  edge, promotion in the arena and immortal categories, raise sites in the
  placement rule, the COW-unique intersection, and runtime entries read as
  calls — were all written in documents S30.5 deletes. The arguments that
  outlive them move where S30.5 names; the rest is at
  `archive/pre-rc-cycle`.
- [ ] **Strategy 1, the typed vector.** No producer, so the 1 → 2
  transition waits on one — `dev/DECISIONS.md`, 2026-08-13, which also
  says what to confirm against `arrays.md` before opening it.
- [ ] **The rest of the language runtime**, listed in `rfc/BACKLOG.md`:
  exceptions, actors, closures, enums, generators and fibers, resources,
  generics, stdlib, I/O.
- [ ] **Phase D, the vertical slice** — hello-world through the whole
  stack, PHP to IR to executable, on the simplest memory setup. It
  validates the central bet, that the compiler can prove escape,
  monomorphism and ARC-pairing on real PHP, and it unblocks every
  calibration item below. It runs as a parallel track rather than in
  turn, because it waits on the unwritten execution-pipeline decisions
  (`rfc/BACKLOG.md`, "the big one") and on the C++/LLVM front end, both
  outside this crate.

## Residual / carried-over items

What the map design owed by the array table — the per-process key, the
ladder's repair and the key word's tag — was S27, closed 2026-08-18 and
deleted with its steps; the decisions it leaves are in `dev/DECISIONS.md`
(2026-08-17 and 2026-08-18), the traps in `dev/POSTMORTEM.md` and the map
in `dev/INDEX.md`. What it did not do is below.

- [ ] **The ladder's refusal has nowhere to go.**
  `InsertOutcome::RefusedByLadder` is answered inside the crate — a null
  from `ll_cow_separate`, a `false` from `element::set` — because the
  crate has no error channel. `rfc/model/maps.md`, "Rung three,
  refusal", says the runtime raises it as a catchable error, and that
  waits on the exceptions work (`rfc/BACKLOG.md`). Until then a refused
  insert is indistinguishable from memory pressure to the program, which
  is the one thing the two-variant outcome exists to prevent.
- [ ] **The equal-identity trigger's tag test has no test of its own.**
  S27.3 changed the counter from "not an integer key" to "the tag equals
  the incoming string's", which in an array names the same set, so the
  change was verified by reading. `Map` is where the two sets differ —
  an object key is neither — and the test is owed there.

- [ ] **The long-key slot itself.** S27 re-keys `strong_hash`; it does
  not fill the slot `strong_hash`'s doc stands in for, which is
  HighwayHash-64 behind a length threshold `rfc/model/strings.md` says is
  unmeasured. Blocked on that measurement, and it belongs with the
  strings work rather than the table's.
- [ ] **Doc links that point at private items.** Public documentation
  links `pub(crate)` and private names — `Table::empty` to
  `Table::reseed`, `InsertOutcome::RefusedByLadder` to `CHAIN_LIMIT` —
  which `rustdoc` warns about unless private items are documented too.
  Crate-wide practice rather than one site, so it is a ruling and not a
  fix: either the links stay and `--document-private-items` becomes how
  the crate's documentation is built, or they become plain names. Raised
  by S27's Code Reviewer, 2026-08-18.
- [ ] **The per-process key's Windows randomness source.** S27.1 lands
  unix-only, `#[cfg(not(unix))]` a `compile_error!` naming this gap, so
  the Windows build refuses until a session on the Windows box adds the
  source (`BCryptGenRandom` or an equivalent OS draw) and runs the gate
  there. Deferred by Edmond, 2026-08-17.
- [ ] **No ABI entry creates or mounts an arena.** `LLContext` is
  `#[repr(C)]` with one public pointer and a null context is legal, so an
  external caller can build one and reach the store barrier; what it
  cannot obtain is an `*mut Arena`, every arena in the crate being made
  by Rust code inside tests. An embedder needs that door before anything
  outside this crate exercises the arena paths.
- [ ] **The retained arm's per-edge registry lock.** `cycle::mark` resolves
  every child through `cycle::row::resolve_edge_target`, and the retained arm of that
  dispatch reaches `memory::retained::occupant_index`, which takes the
  registry mutex to find the block's index before searching it. One lock per
  retained edge, where a per-block visit holding the index's `Arc` would take
  one per block: `occupant_count` already takes it that way at the block's
  first touch, and the search itself is over an `Arc` slice and needs no lock.
  The step that built the mark expected to build the visit and did not, the
  visit being outside its done clause and a change to `resolve_edge_target`'s interface.
  No measurement of what the lock costs a trace exists.
  **The scan doubled the exposure**: `cycle::scan` resolves a popped
  entity's row a second time, to read the colour it may itself have
  raised, so a retained entity costs one lock per in-edge and one more
  for its own expansion. The recorded alternative for that half is a row
  pointer on the worklist beside the entity, the pointer being stable for
  the collection's life; it doubles a worklist entry and is not weighed
  in `dev/DECISIONS.md`, "the scan re-reads a colour it may have
  written", which weighed the colour alone (found by the Code Reviewer,
  2026-08-29).

Memory manager, still open:

- [ ] **Batch the cross-thread free, once a workload exists** — gated on
  measurement, and the gate comes first. Today `Heap::free_remote` posts
  each foreign slot with its own CAS onto the owning block's
  `remote_free` stack, and `buffer_arena::post_remote` does the same for
  a chunk, so the cost is linear in items freed. snmalloc gathers the
  same work into one message queue per owning allocator and pays one
  atomic operation per batch instead (`dev/RESEARCH.md`, 2026-08-08).

  The shape, if it is ever wanted: stage foreign frees in a bounded
  thread-local buffer with no atomics, group them by block on flush —
  `ptr & !BLOCK_MASK`, one AND, and a 64 KiB block holds thousands of
  slots, so a batch lands in a handful of blocks — link each group into a
  chain through the dead slots themselves, and CAS each chain onto its
  block's head once. No per-object memory: the links live in the freed
  slots, as they do now. The staging buffer is the only new memory, one
  fixed-size array per thread.

  What it costs is not memory but **return latency**: freed memory
  reaches its owner a batch late, so peak RSS rises by the batch, which
  is a real change of behaviour in a runtime whose ordinary free is
  immediate. A thread exiting with a staged batch must flush it or leak
  it; `deferred_free::dispose` is the existing shape for that.

  Removing the atomic entirely means a per-thread-pair SPSC ring
  (`ck_ring`), which costs memory per pair. That is the trade snmalloc
  declines, and so should we.

  **Why not now.** Our CAS is already spread across blocks, which is
  mimalloc's contention argument, so the win would be in the count of
  atomic operations and not in contention. Nothing today drives the path:
  the crate is single-mutator, and the callers are one test group
  (`heap::tests::frees_arriving_from_another_thread`) plus whatever
  reaches the raw C ABI from another thread. Order: a program that frees
  another thread's objects in bulk, then a measurement, then this.

- [ ] Buffer *K* and memory-pressure mode thresholds — **blocked on D**:
  they need real workloads, and designing them further on paper is what
  the block is for (`rfc/model/memory/buffers.md`).
- [ ] Per-block dense/sparse reset threshold calibration — **blocked on
  D** for the same reason (`rfc/model/memory/arena-reset.md`).

Read from rpmalloc 2.0.1 on 2026-08-10 (`dev/RESEARCH.md`). Material to
think with, not decisions: none of it is measured here, and each entry
names what would have to be measured first.

- [ ] **Reallocate in place when the class does not change.**
  `stdapi::ll_realloc` allocates, copies and frees on every call, so 40
  bytes to 48 costs a block, a `memcpy` and a free to move inside one
  48-byte slot. `stdapi::ll_usable_size` already reads the class size out
  of the block header, so the test is one comparison on a path that is
  cold anyway. rpmalloc also declines to move a huge block that shrinks
  by less than half, and overallocates to 1.375x on a small growth so
  that a loop growing a few bytes at a time stops reallocating at every
  step (`rpmalloc.c:2402`, `2413`, `2429`).
  **What comes first:** a harness. `rptest` in `benches/standard.rs`
  frees and allocates rather than reallocating, so this path has no
  measurement at all, and nothing in the runtime calls it either — it
  serves the raw C surface.

- [ ] **Size classes for the band between 8 KiB and one block.** Classes
  stop at `heap::MAX_SMALL` and everything above takes a whole 64 KiB
  block, so a 9 KiB request holds 64 KiB. Five classes divide the
  65280-byte payload without a tail — 10880, 13056, 16320, 21760 and
  32640, at six slots down to two — and hold the worst case to 1.33x at
  the bottom of the band and 1.5x at the top; past 32 KiB one object per
  block is already the two-times ceiling. The fast path need not move:
  `ll_alloc` routes anything past `MAX_SMALL` into a cold function, and
  the class is chosen there by a short comparison chain, so `CLASS_LUT`
  stays 514 entries instead of growing to 4082. Free simplifies, since
  these become ordinary heap blocks that the existing `BLOCK_KIND_HEAP`
  arm serves.
  **What it costs:** five more classes in three per-heap arrays and in
  the abandoned table, about 120 bytes per thread, and a high block
  switch rate on a two-slot class — against today's pool get and put per
  object, which is worse in every case. The routing list at the head of
  `stdapi.rs` and `docs/memory-manager.md` move with the change.
  **What comes first:** a footprint measurement, and there is none:
  `benches/alloc.rs` stops at 8192. The metric is `blocks_out` and RSS
  rather than operations per second.
  **Settle separately:** entities past 8 KiB take the same path and are
  reached by their own block header's row rather than by a stride
  (`cycle::row::resolve_edge_target`);
  a uniform stride would make them walkable, which `rc-walk` decided the
  other way and `rc-cycle` re-decided by dispatching on the block's kind.

- [ ] **A flag saying the block already reads zero.** `Heap::refill`
  writes eight bytes into every slot of an entity block unconditionally —
  up to 4080 stores at the 16-byte class, and at a 16-byte stride that
  dirties every line of the 64 KiB block. The invariant is narrower than
  the pass: the walker reads only slots below `bump` and tests one field.
  Two sources of the same knowledge exist and neither is used. A region
  taken with `alloc_zeroed` instead of `alloc` is untouched kernel
  memory. A block returned empty from an entity heap already satisfies
  the invariant, because `FreeSlot` preserves the dead entity's final
  header and an entity dies at refcount 0. What breaks it is a block that
  served as raw or arena memory in between, or a recommissioning at a
  different stride, so the flag has to name the stride it holds for.
  **What comes first:** the case that shows the cost. Amortised over the
  steady-state benchmarks it is small, refill running about 0.00003 times
  per allocation; the workload to measure is a growing one, where the
  pass is one extra store per object created.

- [ ] **Return memory to the OS, and cache huge mappings** — the
  prerequisite this was blocked on is met since `8208815`: a region is an
  OS mapping (`memory::os::map_aligned`) and `os::unmap` is already used
  by the large-run path. What the item now needs is a workload, not a
  mechanism. rpmalloc lets free pages
  accumulate to 16, 8, 4 or 2 per page type and then decommits down to 4,
  2, 1 or 1, keeping the header prefix committed (`rpmalloc.c:712`,
  `2003`, `1249`), and sends a freed huge mapping to a 32-slot cache
  bounded by committed bytes and evicted by age rather than straight back
  to the OS (`rpmalloc.c:1600`). Ours never come back, and `LARGE_RUN`
  unmaps on every free. Either way the block header line stays committed:
  the walker reads every block's kind across the region.

Object model, deferred by design:

- [ ] General interception Proxy — transparent method interception on an
  existing target without touching its class; prerequisite for
  proxy-mediated movability. Needs a mechanism discussion.
- [ ] Binary-level class interceptors (vtable-slot patching) — check
  whether this is the same mechanism as the deferred CHA-style optimistic
  devirtualization (`rfc/model/classes.md`, Deferred).
- [ ] Allocation telemetry layer 2 / debug mode — full design in
  `dev/design/debug-modes.md`, and the build order is its section 10.
  Item 1 of that order, the event journal, is built; the rest of the
  section is unscheduled.

## Cross-cutting (every phase)

- Correctness tests per the project style (`test_guard`, scenario-per-test)
  and criterion benchmarks per `dev/BENCHMARKS.md` — follow the protocol,
  do not improvise. Benches do not cross the C ABI; ABI-entry work is shown
  by IR/asm.
- `dev/ARCHITECTURE.md` — the crate's knowledge map: layers and their
  sanctioned edges, the per-module "does not know" table, the header-bit
  ledger, the five end-to-end paths. Written; it moves with behaviour
  like any other document (`dev/WORKFLOW.md`).
