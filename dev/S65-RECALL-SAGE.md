# Sage on the recall per take path

Date: 2026-09-24. Asked after S65.6 closed, on the criterion Edmond gave the
same day: outside a critical shortage of memory or the thread's exit, the
mutator's performance decides. The ruling as the Sage returned it, unedited.

Sage ruling on the recall per take path — Final.

**1. Classification, from the code.**

- `collect_under_pressure` (`collect.rs:825`): entered only from `entity_alloc_under_pressure` (`heap.rs:2600`) on a refused allocation. Critical: memory shortage.
- Teardown retirement (`refused_under_pressure`, `collect.rs:725`, Teardown arm): a destructor's allocation refused inside a teardown; traces nothing, returns completed deaths' slots for the retry. Critical: memory shortage.
- `collect_before_exit` (`collect.rs:653`): `heap.rs:1882`, the thread's exit. Critical: exit.
- `ll_gc_collect_cycles` (`gc.rs:185`): the embedder's explicit fire; also the poll's `Arming::AllRoots` arm. Not critical.
- The poll's collection over P (`ll_gc_maybe_collect`, `Arming::Verdicts`) and `retire_at_the_poll` (`queue.rs:1226`): not critical. Neither should ever meet COLLECTOR — the poll reads the byte and defers on `Reading::Collector` (`gc.rs:306`), and only this thread consents. But a window exists: between `read_and_act_on_this_thread` (`gc.rs:288`) and the Verdicts/AllRoots take, `make_returns_withheld_under_a_foreign_trace` re-enters `ll_free`, whose `withhold_under_a_trace_or_make_returns` reads the byte and consents (`deferred_slot_reuse.rs:806`). A request landing there is consented by the return, the poll's stale reading fires, and the take blocks. `retire_at_the_poll` re-reads after the returns, so it has no window (its comment is right).

**2. Non-critical paths under "maximum mutator performance".**

- Poll's collection over P, poll's retirement: **do not take.** State COLLECTOR, input the poll's take, result: no wait, the arming kept for the next poll — exactly what the gate refusal and the Collector-reading branch already do. Mechanism: a non-blocking take (`None` at COLLECTOR, the arming re-armed) for these two callers, which closes the returns window by construction rather than by ordering. No contract breaks: POSTED stays and re-arms at the next reading; the deferred verdicts are the collector's proposals, whose instant nothing promises.
- Explicit fire: **wait, no recall, for its own batch.** It traces R whole under no budget itself; a completed batch of live roots defers them out of that trace, so the wait is repaid in kind — the collector's remaining time is at most the mutator's own trace of the same roots, which a recall hands back whole plus the posts, reset and wake (the measured 15 µs on `overlapping-live`, 49–51 vs 33–36 µs). The loss case (budget met, `disjoint-live`, 30–40 µs) is the one K halves after, so the collector sizes batches to complete. "Not taking" would change `ll_gc_collect_cycles`'s "now" — a premise change, Edmond's, not mine. F3 is not breached: the explicit fire's wait returns to the pre-S65.6 bound, not beyond it.

**3. Critical paths.** Criterion inferred: least time to the first freed slot (pressure, teardown) and to the exit; the memory the collector's arena holds (up to B blocks) is memory the mutator needs, and a recall's reset returns it. Under pressure and at exit the deferred lane is re-offered and R traced whole, so a completed batch spares the mutator nothing and every microsecond waited is pure loss; the teardown retirement traces nothing, so the same holds. **Recall on all three.** The 15 µs figure does not charge this path: the probe's `wait` arm ran a collection over P after the retirement's take, which the real path never runs; the 27 µs of handed-back work is the poll's later, and after S65.10 `Unwalked` returns to R untraced, leaving the recall's total near 21–23 µs against 33–36.

So the hint gains a meaning: every blocked take announces (the collector releases a grant it holds unserved); only a critical take recalls (the collector abandons the batch it is tracing). Two values in the existing byte; `take_unless` takes which.

**4. S65.14 and S65.7.**

S65.14 is needed, and not for the reason it was opened. Under (a) the pressure path and the exit accepted one stranger's batch, and (b) makes that wait the exception — but (a)'s "one batch" was a time claim resting on "a batch in progress ends by its block budget", and B bounds blocks, not positions: a stranger's 10⁶-scalar batch draws no block and holds A's grant for milliseconds. That alone reopens the pressure and exit bound. The explicit fire adds a second reason: behind a stranger's batch its wait buys it nothing, unlike behind its own. What changes is the form: a held-unserved grant is released by one CAS at the stride, the stranger's batch **continuing** — no posts, no reset, no `Unwalked`. The Critic's candidate (recall word on the collector's slot, read beside the traced token at the stride) is the right one; the step's bound shrinks from "N positions, K posts and a reset" to "N positions and one release". The trigger is every announcing take, not only the critical ones.

S65.7 stands as Edmond ruled (waiting stored at M, no block). Two edits: the free path at M stores the recall value (abandon), since its aim is withheld memory; and for a grant held unserved, M is effective only through S65.14's stride reading — S65.7 depends on S65.14, so S65.14 lands first.

**5. Bookkeeping and figures.**

Amend: rfc `rc-cycle.md` "The recall of the token" ("An owner that needs its token while a collector holds it recalls it" → which owners recall, which announce; the stranger-gap sentence replaced by S65.14's bound); handshake ~line 50 ("the mutator's take sets it" → which takes), ~225 ("a batch in progress ends by its block budget — so a woken owner is served … at most one stranger's batch away") and ~825 ("its pressure path and its exit wait the same bound"); `worker.rs` module doc's S65.14 paragraph; `token.rs` "A take that meets COLLECTOR recalls the token first"; PLAN S65 goal ("what bounds the mutator's wait … is a recall") per path; DECISIONS: a new entry citing both 2026-09-23 rulings — (b)'s "an embedder's `ll_gc_collect_cycles` … stay under the rule" now reads: the explicit fire waits out its own batch by choice, never a stranger's.

Figures owed: (i) a `fire` arm of `what_a_take_costs` — `HeldToken::take` then `collect_off_the_poll` (AllRoots), wait vs recall, `overlapping-live` and `disjoint-live` — the ruling on the explicit fire stands only if wait's total is lower where the batch completes; (ii) the `wait` arm re-read after S65.10 (the collection over P with 63 `Unwalked` should fall to the ~4 µs disposal); (iii) S65.14's two-mutator case: A's wait behind B's 10⁶-scalar batch, expected one stride plus a wake, B's batch complete.

Hidden decisions: the hint's two values in one byte (a second byte would do); the non-blocking take chosen over reordering the poll's returns and reading.
