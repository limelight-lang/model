# Plan

Implementation plan for `ll-model`, re-sorted 2026-07-24 against the RFC
after the 2026-07-22 object-layout redesign landed in the `rfc` repo.

Design lives in `rfc` and is authoritative — read before coding, do not
re-derive: `model/classes.md`, `model/values.md`, `model/lowering.md`,
`model/gc/rc-cycle.md`, `model/gc/cycle/questions.md`, `model/memory/ffi.md`,
`runtime/object-lifecycle.md`.

The `rfc` repository carries its own plan at `dev/PLAN.md` for work that lands
in the specification rather than in this crate.

Updated: 2026-08-26 · Active: S31 — the sections after S40 are the backlog

**Closed stages are deleted whole** (rule 23.1.3), and what outlived each
of them is in the journals rather than here: `dev/DECISIONS.md` for a
decision and its reason, `dev/POSTMORTEM.md` for a trap,
`dev/BENCHMARKS.md` for a measurement, `dev/INDEX.md` and
`dev/ARCHITECTURE.md` for the map. Deleted so far: S4 through S30. A number is never reissued, so a
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
is retained and acyclic garbage dies by counting. S31 through S40 build
`rc-cycle`; what the deletion took, what it kept and why is `dev/DECISIONS.md`
under that date, and the old code is on `archive/pre-rc-cycle`. S28 was
abandoned rather than closed by the same ruling, and S29 was split — its
second half is carried as S39.

**S31 through S40 went through a Critic round and four Sage rulings on
2026-08-26**, on Edmond's instruction, and the stages below are the amended
form. The rulings and their reasons are in `dev/DECISIONS.md`.

**Verification is one configuration** since 2026-08-26: the GC axis went with
the collectors, `hash-folding` and `debug-journal` are what remains, and
`cargo bench --no-run` is part of the gate because `cargo test --lib` builds
no bench target while `benches/lifecycle.rs` imports the GC ABI
(`dev/WORKFLOW.md`).

## Fog

Empty.

---

## S31 — The header's new flag layout

Goal: one collector claims the flags word, so the layout is chosen for the
paths that read it rather than for a truce between two strategies.

Done when: the layout below is in `refcount.rs` with every constant named, the
enrolment gate is one mask, and the kind field is four bits wide.

| bits | field | | bits | field |
|---|---|---|---|---|
| 0–1 | memory category | | 12 | has weak references |
| 2–5 | entity kind (4 bits) | | 13 | destructor pending |
| 6 | copy-on-write | | 14 | destructor ran |
| 7 | arena reset mark | | 15 | free |
| 8 | acyclic gate | | 16–17 | epoch |
| 9 | ownership mark | | 18–19 | maturation age |
| 10 | enrolled | | 20–23 | collector reserve |
| 11 | live escapee | | 24–31 | free |

- [x] S31.0 Write the new layout into `classes.md`   *(before S31.1)*
      done: `rfc/model/classes.md`'s "Flags layout" table carries the layout
        below rather than the two-collector one — no GC-state field, no colour
        bits, no buffered bit, no candidate index — and the prose under it that
        names bit positions moves with it: the kind field is bits 2–5, the
        eight codes are named with 8–15 free and 0–3 reserved for ring-closing
        kinds, and the parenthesis about the candidate buffer's membership test
        goes; `rfc/model/lowering.md`'s C mirror of the flags word is rewritten
        to match, being what a consumer transcribes; `linkcheck.php` clean
      tier: T1 · role: —
      handoff: `refcount.rs` names that table **authoritative** and
        `lowering.md` names it the same, so writing the code first would put
        the crate in contradiction with the document it cites — which is the
        failure S30 spent a day repairing. The decision itself is already
        recorded (`rfc/dev/DECISIONS.md`, 2026-08-26, "the flags word is
        re-laid for one collector"); what is missing is the normative table.
      handoff: closed 2026-08-26 by `rfc` `1000e9d`. Four more documents named
        positions that moved and were taken with it — `lowering.md`'s C mirror,
        `layouts.md`'s diagram, `arena-reset.md`'s promotion step and
        `weak-references.md`'s eight mentions of bit 7. `cycle/questions.md`
        Y7 got a superseding note instead: its bit accounting reasoned about
        the old positions, and what it settles is which bits have customers.
- [x] S31.1 Renumber the entity kinds so the predicates become masks
      done: `Object 0, Lazy 1, Array 2, Reference 3, String 8, StringDynamic 9,
        Box 10, WeakRef 11`, codes 4–7 held free for ring-closing kinds and
        12–15 free for the rest; the category keeps bits 0–1 because
        more surviving sites read its value than the kind's, and a mask test is
        position-free; "closes a cycle" is
        `flags & 0b100000 == 0`, "carries a class at +8" is
        `flags & 0b111000 == 0`, "is a string" is `flags & 0b111000 == 0b100000`;
        `CANDIDATE_KINDS`' bitset is replaced by the range test, the codes below
        eight are declared reserved for ring-closing kinds so a later kind
        is not silently excluded by a permanent refusal (Y6), and the decision
        that refused renumbering is superseded in `dev/DECISIONS.md` with its
        reason; `kind_may_close_a_cycle` gains the caller S31.3 gives it or
        goes too
      tier: T2 · role: Critic
      Critic 2026-08-26: the reserve was 0–3 and its four codes were all
        assigned, so a fifth ring-closing kind would take code 8 and be refused
        by the mask forever — Y6's own failure inside the clause written against
        it. Also: a member list is the bitset again, since a kind never added to
        the list passes every assertion; three numeric citations missed
        (`Lazy (6)`, `Kind-5`, `bit-7`); `carries_a_class_word` conflates the
        weak-referent question with the class-word one, and they diverge at FFI.
      Sage 2026-08-26: the reserve widens to codes 0–7 in the Critic's
        assignment; the `rfc` amendment is a precondition and lands first; the
        exhaustive `match` is necessary and not sufficient, so a `const`
        assertion ties each kind's classification to its code and a
        `debug_assert!` in `to_flags` catches a kind the battery never named.
        `lowering.md` needs no edit — its C mirror names positions, not codes.
        Final.
      handoff: closed 2026-08-26. The reserve is four free codes, and that is
        the whole content of the clause: a full range refuses the next kind for
        ever and reports nothing. `EntityKind::closes_a_ring` is the
        classification and `kind_may_close_a_cycle` the mask; the `const`
        battery is what makes them agree, verified by a standalone `rustc` run
        in which a ring-closing kind coded at 8 fails the build.
      handoff: the criterion's clause "the decision that refused renumbering is
        superseded" was already met by `dev/DECISIONS.md`, 2026-08-26, "the
        flags word is re-laid for one collector", written in S31.0's session —
        checked against the criterion rather than carried over from the handoff.
- [x] S31.2 Fold the string's layout into the kind field
      done: `STRING_OUT_OF_LINE` is gone, `LLStringDynamic` is selected by kind
        code 9 — whose meaning is **bytes outside the body, whatever the
        reason**, not "growable" — and every "is a string" site accepts both
        codes; the inventory of sites that name one representation is taken by
        flipping the factory's stamp so every string is out-of-line and reading
        the failures (`dev/WORKFLOW.md`, Tests), not by grep; a red-first test
        proves an out-of-line string is still read through `data`
      tier: T2 · role: —
      handoff: `promote.rs:461` tests `k == EntityKind::String.to_flags() &&
        flags & STRING_OUT_OF_LINE != 0`, which is exactly the shape the stamp
        flip finds and static reading does not.
      handoff: four sites reach code 9 through a catch-all and the stamp flip
        finds each only if the suite drives that path, so they are named here
        rather than left to it: `object.rs`'s `ll_cow_separate`, whose `_` arm
        returns the original and so writes a **shared** string in place;
        `escape_copy`'s `unreachable!`; `promote::external_memory`'s
        `_ => External::None`, which loses a survivor's out-of-line bytes at a
        reset; and `promote::traceable_in_full`. S31.1 named the code in
        `cells::sever_cells` already, that arm's answer being mechanical.
      handoff: closed 2026-08-26. `rfc` `d5ea1b1` went first — `strings.md`
        still selected the layout with the flag while `classes.md` had already
        given it a code, and the crate cites both. The stamp flip ran **twice**,
        which is what the method is worth here: before the fold it finds only
        sites that name a *representation*, because the kind does not yet
        discriminate; after it, every string carries code 9 and the kind
        dispatch is exercised. The second run added exactly one failure over the
        first, and no production site among them — the four named above were
        widened before it ran, which is what makes their silence evidence.
      handoff: the flip's own casualty is worth knowing before re-running it.
        `the_hash_is_computed_once_on_demand_and_never_zero` poisons bytes by
        writing at `size_of::<LLString>()`, which in the other layout is the
        `data` pointer, so the run **aborts** on a misaligned free and hides
        every later failure. It now asserts the layout it assumes; a future flip
        still has to skip it, since the assertion is what fires.
- [x] S31.3 The enrolment gate is one mask
      done: the release path decides with `flags & 0x723 == 0` — category zero,
        kind below eight, class not acyclic, ownership not proven, not already
        enrolled — and a `#[cfg(test)]` counter past the gate proves each of the
        five conditions rejects on its own; the mask is composed from the named
        constants rather than written as a literal, and
        `EntityKind::closes_a_ring` is what its kind term is checked against
      tier: T2 · role: —
      handoff: a scenario test covers a pair, never one half — the counter is
        what sees a condition that never fires.
      handoff: closed 2026-08-26. The gate decides and counts and stores
        nothing: `ENROLLED`, `ACYCLIC_GATE` and `OWNERSHIP_MARK` have no writer
        until S34.1, S37.2 and S37.3, so the mask reads three bits that are
        always zero today and the clause tests set them by hand. The counter is
        thread-local, because the harness runs tests in parallel and a global
        one charges another test's releases to this one.
      handoff: the clause test was shown non-vacuous by dropping `ACYCLIC_GATE`
        from the composed mask: both `the_enrolment_gate` tests turn red, one on
        the admission and one on the mask's own coverage. A `const` assertion
        ties the composition to the `0x723` the RFC names, so the two cannot
        part silently.
- [ ] S31.4 Narrow the mutator's header writes, and rule the read side
      done: the mutator writes the refcount with a 32-bit store and the flags
        half with stores that stop below byte 2, so no mutator write spans it;
        the whole-word `mutator_update_flags` is gone, and so is the flags half
        of `mutator_guard_retain` and `mutator_unguard_release`, which write it
        in a 64-bit store on the teardown path; the release path's
        `flags & 0x723` read is narrowed with it, because a 32-bit load at +4
        against the collector's byte store at +6 is a mixed-size atomic access
        that Rust's memory model does not define and Miri refuses; a test
        asserts the collector's byte survives a concurrent flags update, and it
        is written so the mutator's load precedes the collector's store, since
        the sequential order passes on today's defective code
      tier: T2 · role: Critic
      handoff: today's comment promises the opposite — "may bury a concurrent
        collector byte store". The Critic round of 2026-08-26 found the clause
        guarding writes while the day-one defect is a read, and naming one of
        three writers.

## S32 — The block header's collector triple

Goal: the collector reaches a block's shadow array without touching the cache
line the owner writes.

Done when: the triple sits in the free tail of the block's 256-byte header
line, and the slot index derived from an address is proven exact.

- [ ] S32.0 Dispatch on the block's kind before any row
      done: the trace reads the block header first and branches — ordinary
        entity block by arithmetic, retained block by binary search over the
        occupancy index, large entity to a row in its own block header, and any
        other kind stops the descent with the child read as an external live
        reference; the dispatch runs in the collector's per-child visit, above
        the enumerator, so `cells.rs` keeps no knowledge of rows; a test drives
        one entity of each population and asserts that the row resolved for an
        entity is the row that entity's own address derives, and that two
        differently sized occupants of one retained block resolve to distinct
        rows
      tier: T2 · role: Critic
      handoff: the arithmetic covers one population of three. A retained block
        was filled by an arena's bump — mixed sizes, no stride — and this is
        what `memory/retained.rs` was built for. The row-identity assertion is
        owed because four smoke calls that only prove the descent terminated
        pass while the arithmetic returns another entity's row.
- [ ] S32.1 Prove the slot index derivation
      done: `((p & BLOCK_MASK) - LINE_SIZE) * recip >> 32` returns the slot's
        own index for every size class and every slot of a block, proven by an
        exhaustive test against the division already at `heap.rs:2127` rather
        than against an address recomputed from the index, which is a tautology
      tier: T1 · role: —
- [ ] S32.2 Put the triple in the header's free tail
      done: `HeapBlockHeader` occupies 192 bytes of the 256-byte line and the
        triple — shadow pointer, `recip`, the collector's own copy of the size
        class — sits past it on its own cache line; the layout test that pins
        the header's halves is extended rather than replaced, and a `const`
        assertion ties the triple's offset to `size_of::<HeapBlockHeader>()`,
        because 192 is today's number by `BlockRemote`'s 64-byte alignment and
        the existing test only asserts the header fits the line
      tier: T2 · role: Code Reviewer
      handoff: the size class is duplicated on purpose — it is written once at
        commissioning, and the copy is what keeps the lookup off the owner's
        line.

## S33 — The shadow arena and the per-block rows

Goal: the collector's working state lives entirely off the heap, and an
aborted collection costs nothing.

Done when: a collection allocates rows, uses them, and returns everything in
one reset, with no write into any entity.

- [ ] S33.1 The arena
      done: a bump arena over 64 KB blocks, taken by the collector and returned
        whole at the end of a collection **including on the abort path**, so a
        refusal to grow aborts the collection rather than failing the process
        and rather than leaking the blocks it already holds — the path that runs
        when memory is short; the abort also nulls the shadow pointer of every
        block on the touched list, because a stale pointer left in a block whose
        arena has been recommissioned makes the next collection decrement live
        payload; where the blocks come from — the ordinary pool or the critical
        reserve Y14 says the in-line form must draw through, since the ordinary
        path has already refused — is settled in this step and recorded
      tier: T2 · role: Critic
      handoff: the abort path is the one the Critic round found unexercised and
        leak-prone, and the touched-list sweep is what closes the staleness the
        arithmetic form has no tag for.
- [ ] S33.2 The per-block row array
      done: `slots × 4` bytes reserved at a block's first touch **without being
        zeroed**, the pointer stamped into the block's triple, the block pushed
        onto the touched list; the met flag lives in a bitmap of one bit per
        group of eight slots, only the bitmap and a touched group are
        initialised, and the row is colour 2 plus working count 30; the colour
        assignment names its reserved code, so a met, condemned, zero-count row
        is distinguishable from an untouched slot and a second reach cannot
        re-initialise it from the refcount; what `slots` means for a retained
        block, which has mixed sizes and no stride, and what the bitmap's groups
        group there are settled in this step; a large entity, which gets one row
        in its own block header and no group, carries its met flag in that row
      tier: T2 · role: —
      handoff: three holes the Critic round opened. Without the reserved code a
        condemned zero row reads as unmet; without a met bit the large entity is
        condemned live on the first ring it joins; and `slots × 4` has no
        subject in a retained block.
- [ ] S33.3 Name the saturation clause
      done: a working count that would exceed the field saturates, saturation
        reads as "external references exist, conservatively live", and a test
        drives an entity past the bound
      tier: T1 · role: —
- [ ] S33.4 Hold the row at four bytes
      done: no captured count is stored anywhere — not in the row and not in a
        parallel array — because the commit stage judges again rather than
        comparing with a captured value; a probe on the collector's own path
        counts bytes written per touched block at first touch and shows the
        figure proportional to the bitmap and not to `slots × 4`, which a
        standalone memset benchmark cannot show, since it reports the same two
        numbers whether or not the array was zeroed (1.4 ms against 41–76 ms
        measured for the 717 MiB case)
      tier: T1 · role: —
      handoff: decided 2026-08-26 by the ruling that phase 2 is a second
        judgement. Storing a captured value would have doubled the row and the
        design's memory with it.

## S34 — The root queue, enrolment and parking

Goal: candidates reach the collector without the mutator paying for a data
structure, and an entity that dies while enrolled leaves no dangling pointer.

- [ ] S34.1 The queue against Y12's contract
      done: **all seven** clauses hold — `questions.md` says "Six clauses" and
        numbers seven — so a failed growth never drops a root, no allocation
        happens on the enrolling thread's hot path, proven by a `#[cfg(test)]`
        allocation counter bracketing the enrolment call rather than by defining
        the growth path as not hot, and a second reader is either supported or
        refused by construction rather than by a `debug_assert!`; clause 4's
        second half is superseded by S34.2 and the step says so; the one arm the
        runtime keeps is rebuilt here — a growth refusal or a reserve draw
        during enrolment sets the pending flag, and the poll fires at the next
        clean point, returning 0 until S36.7 wires the collection
      tier: T2 · role: Critic
- [ ] S34.2 The law: only the owner reduces state
      done: no dirty pass clears an enrolment bit, drops a queue entry or
        returns a slot; a reader may mark an entry a corpse and pass it on; the
        bit is cleared only at death — the drain's corpse rule, or commit's
        free — and **never at acquittal**, which supersedes Y12 clause 4's
        "cleared after the root is walked"; a test proves the acquittal case —
        ring A↔B with an external X→B that is released after the trace read the
        count — does not lose the ring, and the assertion is that a later
        collection reclaims it, not merely that the bit is still set
      tier: T2 · role: Critic
      handoff: clause 4 and the law of 2026-08-26 contradicted each other, and
        both were in the plan. The Sage ruled for the law: clearing on acquittal
        is the permanent miss, because enrolment is edge-triggered.
- [ ] S34.3 Parking a slot that dies enrolled
      done: death runs in full — weak cells cleared first, then `__destruct`,
        then children released — and the slot is withheld from the allocator
        while a queue entry names it; the drain reads the refcount, retires a
        zero-count entry, clears the bit and returns the slot without touching
        the body; the return is the crate's single slot-return path and the
        block's `used` falls **there and never at the parking**, proven by a test
        that empties a block around a parked corpse and shows the block reaching
        the pool only at the return
      tier: T2 · role: —
- [ ] S34.4 Prove the corpse rule against arena reuse
      done: a red-first test enrols, kills, resets the arena and drains, and the
        category-zero clause is what makes it pass
      tier: T1 · role: —

## S35 — Mark and scan

Goal: trial deletion runs entirely in the shadow rows.

- [ ] S35.1 Mark
      done: the trace decrements children's working counts in their rows and
        writes nothing into any entity; children are enumerated through
        `cells::trace_cells` — the tracer moved at S30.2, not a second stride —
        and S32.0's block-kind dispatch runs per yielded child in the
        collector's visit; an aborted mark leaves the heap byte-identical,
        proven by hashing every block on the touched list before and after, with
        the abort forced at a depth past the first descent rather than at the
        first instruction
      tier: T2 · role: —
      handoff: this clause stands verbatim against S37: the maturation stamp is
        written by commit and only read by the trace, so no write into an entity
        happens during a mark. The "retired on contact" clause that contradicted
        it was withdrawn 2026-08-26.
- [ ] S35.2 Scan
      done: a non-zero working count marks its reachable set live, a zero one
        leaves it white, and the pair is proven on two graphs — a ring with an
        external reference into its middle, which must survive, and the same
        ring without that reference, which must go white — because a scan that
        marks everything live passes the first alone
      tier: T2 · role: —

## S36 — Commit

Goal: only the owning thread frees, and it frees what the judge condemned and
the exact test confirmed. The teardown is here in full: the Critic round of
2026-08-26 found the stage claiming the frees while building none of them.

- [ ] S36.1 The exact test on the owner's thread
      done: current fields are re-read on the owning thread before any free, and
        the test opens with the corpse rule — a member read at count zero drops
        the component whole before any guard or field write — exercised by a
        test in which tearing down one component releases into a second already
        judged white; the refusal path is exercised by a mutation racing the
        verdict, and by a positive control in which the same scenario without
        the mutation does free
      tier: T2 · role: Critic
- [ ] S36.2 The epoch parking
      done: a slot freed inside a collection waits for its end, releases into
        S34.3's single return path, and that path refuses while **either**
        window is open — a queue entry naming the slot, or a collection in
        flight; a red-first test shows the defect it prevents, a reused slot
        inheriting the dead occupant's row, and overlaps the two windows in both
        orders
      tier: T2 · role: —
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
        owner's by the law of S34.2 — and because S35.1's zero-write mark is
        what makes S33.1's abort free.
- [ ] S36.7 Wire the collection into the ABI
      done: `ll_gc_collect_cycles` runs a collection and reports what it
        reclaimed, and `ll_gc_maybe_collect` fires on the armed pending flag and
        nowhere earlier; a test arms the flag, shows nothing collected before
        the next poll, and shows the collection at it — restaging the
        deferred-fire contract the dying `gc/tests/where_a_collection_may_fire.rs`
        carried
      tier: T2 · role: —

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
- [ ] S37.4 The suspects buffer and the turnover re-offer
      done: acquittal never clears the enrolment bit; a proven-live root parks
        in the suspects buffer with its bit set, and every suspect is re-offered
        at the first collection after the heap's epoch advances; red tests prove
        that a matured ring losing its last external reference mid-epoch is
        collected at that re-offer and not before, and that a ring whose mates
        carry unequal ages is likewise collected, so maturing apart costs recall
        rather than a permanent miss
      tier: T2 · role: Critic
      handoff: this is the backstop the withdrawn "retired on contact" clause
        was supposed to be and never was — eager clearing fires only when a
        trace touches the entity, and the stamp that wraps is exactly the one no
        trace touched for four epochs. It also collects YRC's 56 % saving on
        re-registration.
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
      handoff: S31.3's mask already tests bit 9 and no compiler stamps it, so
        this step's remaining content is the factory-side and FFI-side write.

## S38 — The claim and concurrency

Goal: a collection runs either in a collector thread or in the mutator, never
both, and the losing side never deadlocks.

- [ ] S38.0 The collector's reader
      done: a second `CellReader` impl reads `word` and `ptr` as relaxed atomics
        and answers nothing else — no storage version and no give-up, because a
        torn read costs at most a phantom edge or a missed one, and a child
        mapping to no GC-heap block already ends the descent as an external live
        reference (S32.0); the collector-thread trace instantiates `trace_cells`
        with it, and a Miri slice drives it over an object with outside cells
        and over an array mid-move
      tier: T2 · role: Critic
      handoff: `RelaxedCells` and its re-check plumbing died at S30.2 because
        they existed for `rc-walk`'s precision. What the accelerator needs is
        strictly smaller, and the `CellReader` trait is the socket it plugs into.
- [ ] S38.1 The claim
      done: one word for the process, three states, CAS from free; it covers the
        **trace** — the arena, the block triples and the touched list — while
        each owner's exact judgement runs at its own checkpoint; the third
        state's only entrant in this plan is a `#[cfg(test)]` seizure, named in
        this criterion, because no collector thread exists here; the claim
        carries a thread-local held flag so self-re-entry is distinguishable
        from contention, and a test proves a held claim blocks collection entry
        alone while enrolment, release and allocation on other threads proceed
      tier: T2 · role: Critic
- [ ] S38.4 The entry gate and the slow-path fire   *(before S38.2)*
      done: the GC-heap slot allocation slow path, on a forced refusal that
        names which allocation refused, waits on a held claim by any holder but
        itself, takes the claim, runs the in-line collection, retries once and
        reports null only after; a shortage at teardown depth ≥ 1, or under a
        claim this thread already holds, collects nothing and reports; a heap of
        one size class full of cyclic garbage serves the allocation with no
        explicit collect call
      tier: T2 · role: Critic
      handoff: Y14's clause "a thread that finds the token taken does not wait"
        was argued from the handshake deadlock, and the amendment of 2026-08-26
        deleted the handshake, so the Sage retired the clause with its reason
        and generalised the wait to any non-self holder. That generalisation is
        recorded in `rfc` (Y14 and `rc-cycle.md`, Concurrency) as well as in
        `dev/DECISIONS.md`; it is a decision of the round, not of the design of
        record as it stood.
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
      done: while a collection is in flight over a thread's blocks, that
        thread's frees park until it ends; the cost is measured as the churn
        held across one collection
      tier: T2 · role: —

## S39 — Thread exit  (carried from S29.2)

- [ ] S39.1 Exit drains its own queue
      done: `ll_thread_exit` retires its queue before handing the heap over, and
        the fate of a live enrolled entity at exit is **chosen** — which of
        collect, hand over or leak, and why — rather than described in a
        comment, with the test that observes the chosen fate named; a red-first
        test kills a thread between enrolment and collection
      tier: T2 · role: —
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
  `Lazy` nevertheless answers yes to `EntityKind::closes_a_ring`, on the argument recorded
  in `dev/DECISIONS.md`, 2026-08-07. `StringDynamic` (code 9) has no
  producer either and is S31.2's, so it is not carried here.
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
- [ ] **Pure destructors, and the hand-off drain** — proposed by
  Edmond 2026-08-18, analyzed the same day in
  `dev/design/pure-destructors.md` through three lenses and two Critic
  rounds. The runtime-only step (the specialized P0 dispose and the
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
- [ ] **The per-process key's Windows door.** S27.1 lands unix-only,
  `#[cfg(not(unix))]` a `compile_error!` naming this gap, so the
  Windows build refuses until a session on the Windows box adds the
  door (`BCryptGenRandom` or an equivalent OS draw) and runs the gate
  there. Deferred by Edmond, 2026-08-17.
- [ ] **No ABI entry creates or mounts an arena.** `LLContext` is
  `#[repr(C)]` with one public pointer and a null context is legal, so an
  external caller can build one and reach the store barrier; what it
  cannot obtain is an `*mut Arena`, every arena in the crate being made
  by Rust code inside tests. An embedder needs that door before anything
  outside this crate exercises the arena paths.

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
  reached by their own block header's row rather than by a stride (S32.0);
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

- [ ] **Return memory to the OS, and cache huge mappings** — blocked on
  the prerequisite the head of `stdapi.rs` already names: regions come
  from `std::alloc::alloc`, not from mmap. rpmalloc lets free pages
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
