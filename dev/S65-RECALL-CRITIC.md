# Critic on the Sage's recall ruling

Date: 2026-09-24. Over `dev/S65-RECALL-SAGE.md`, with Edmond's override in
place: on the critical paths the mutator does not recall, it waits for the
collector's batch and uses its verdicts. The report as the Critic returned
it, from reading the code; nothing was run.

**1. On two of the three critical paths the owner's reason for waiting ("it would do that work itself anyway") fails, so the wait buys nothing there.**
- **Teardown retirement.** It reads slot state, not verdicts: `retire_candidates` → `compact`, which frees on `completed_death(entity)` (queue.rs:1350, compaction.rs:172). A recalled batch posts `Unwalked` into P and advances R, and the retirement then retires the same completed deaths in P in place. The yield is identical, and the wait is spent inside a destructor whose allocation was refused.
- **Exit.** Round 1's close moves every collector `ReadLive` into the deferred lane (compaction.rs:185–190). `registered_by_lane` counts that move as progress (queue.rs:1606–1617, collect.rs:656–668). Round 2 re-offers the lane (collect.rs:663) and traces it again.
- **Pressure.** Only this path uses a verdict, and only `ReadLive`, and only once. Its close defers those roots, and the next refused allocation splices them back (collect.rs:844).
- **Proposed and `Unwalked`.** On all three paths these are batch roots (verdicts.rs:95, collect.rs:504) and are traced again in full.
- **Scenario:** the exit waits out its own batch over a root reaching a vector of 10⁶ scalars, read live. That is milliseconds, and round 2 traces the same root again.

**2. With no take recalling, nothing bounds a critical wait, and the override collides with the owner's own ruling of 2026-09-23.**
- The block budget bounds blocks, not positions; the Sage makes this argument itself in §4.
- DECISIONS.md:117–120 reads "the form is a recall: the collector hands the token back when the mutator asks". Lines 18–20 and 25–27 keep the exit and the explicit fire under the stage's rule until "the recall bounds it". Under the override that condition never arrives.
- So S65.13's parts (686–703 µs against 158–170 µs on the wide rings) and S65.9's retry at `B_max` lengthen exactly these waits and cannot ship.
- Speculative: under a shortage across the whole process, the collector's `grow` draws from the same pool that refused the mutator (`BlockPool::global`, gc_metadata.rs:270). The batch then likely ends all `Unwalked`, and the wait is pure loss.
- Day-one cost: `a_pressure_collection_recalls_the_grant_over` (worker/tests/the_recall.rs:326), which runs over four containers, asserts the opposite and must be rewritten.
- This is a premise change and goes back to Edmond with these figures.

**3. The poll window is real, but the non-blocking take is given to the one arm that can never meet it.**
- **The window, verified:** the poll reads the byte at gc.rs:288. At gc.rs:294 `make_returns_withheld_under_a_foreign_trace` re-enters the free path: deferred_slot_reuse.rs:1027 → stdapi.rs:325 → :513 → deferred_slot_reuse.rs:806 → consent at token.rs:595.
- **Why the Verdicts arm cannot meet it:** `Arming::Verdicts` is set only on a reading of `POSTED` (gc.rs:100), and every collection spends it (collect.rs:308). A request lands only on `FREE` (token.rs:267), and only this thread moves `POSTED`. So the Verdicts take never meets `COLLECTOR`.
- **The arm that can:** `AllRoots` (gc.rs:327 → `ll_gc_collect_cycles`, a blocking take). The ruling files it with the explicit fire, so the poll waits a whole batch, against E9 (gc.rs:300–307).
- **Fix:** give the non-blocking take to the poll's `AllRoots` arm and keep the arming, or re-read the byte after line 294.

**4. The explicit fire's "the wait is repaid in kind" is true only for `ReadLive`.**
- Proposed roots are validated again by an exact trace. An embedder fires expecting garbage, so every garbage batch is a loss case, not only one that met the budget.
- "The collector sizes batches to complete" is false in three ways: K floors at 1 (worker.rs:2159), so a root whose closure exceeds 8 blocks meets the budget on every batch; K doubles after every completed full clamp (worker.rs:2156), so K oscillates; a take of a standing ring never sizes K at all (worker.rs:2019).
- Figure (i) runs only `-live` loads, so it cannot see this. Add a garbage load.

**5. A take's announce can overwrite M's recall.**
- The take's store at token.rs:425 is unconditional.
- **Scenario:** the free path reaches M and stores the recall. Within the next stride (up to 1024 positions), the same thread's allocation is refused and the pressure take stores the announce over it. The collector continues, and the withheld stack stays at M or above for the whole batch, during a shortage.
- **Fix:** a monotone update (`fetch_max`).

**6. S65.14 has two defensible readings and a cost it does not name.**
- **Two readings:** §4 says M acts on an unserved grant "only through S65.14's stride reading", yet names "every announcing take" as the trigger. The free path at M takes nothing. If M does not also write the slot word, a mutator behind a stranger's batch of 10⁶ scalars withholds without bound.
- **Hidden cost:** "one CAS" hides a walk of the standing list, which has no cap (the pattern of worker.rs:1765–1791), at every stride where the slot word is set.
- **Word ordering:** the slot word must be cleared before the walk. Cleared after it, a set that lands mid-walk is lost, since the hint is two relaxed locations.
- **The release:** it must go through `release_claim`, whose notify is made under the mutex (token.rs:320–333). A bare CAS leaves the waiter asleep.
- **`note_released_unserved`:** do not set it on this path. Set, it makes `serve` skip the consent wait (worker.rs:1211) for a mutator that is awake.

**What survived.**
- S65.14 is more justified under the override, not less. The owner's reason does not hold for a stranger's batch, and S65.14 is now the only bound on a critical wait behind one.
- Its release cannot race the checkpoint, a withdrawal, `forget` or `Standing::drop`: all run on the one collector thread, and nobody else writes the byte from `COLLECTOR|s`.
- The poll's non-blocking take cannot leave `POSTED` standing forever or starve P, because a take from `POSTED` always succeeds.
- `retire_at_the_poll`'s re-read after the returns is correct (queue.rs:1236).
