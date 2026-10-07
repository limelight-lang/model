# The cycle collector's kinds

The runtime carries one cycle collector, chosen at build time by exactly one
`gc-*` Cargo feature. Both kinds count references the same way and differ in
who decides that a garbage ring may be freed. Ruled by Edmond on 2026-10-07
(`dev/DECISIONS.md`, "two collector kinds, named").

| Kind | Feature | Who decides and frees | What the program pays |
|---|---|---|---|
| A, checkpoint | `gc-checkpoint` (default) | The collector thread finds garbage rings; the owning thread checks them and frees them at its poll | No tag on any write; the frees and their pauses fall on the owner |
| B, window | `gc-window` | The collector judges a garbage set alone and frees it while the program runs | A window tag stored into header byte 7 by every count write and every tagging slot store |

```
cargo build --release                                            # gc-checkpoint
cargo build --release --no-default-features --features gc-window # gc-window
```

A build with none or two kinds stops at a `compile_error!` in `src/gc.rs`.
A program asks which kind it was linked with through `ll_gc_kind()`: 1 for
`gc-checkpoint`, 2 for `gc-window` (`crate::gc::GcKind`). The selection is
built for a third kind: a tracing collector with stack-map roots is on the
far list (`PLAN.md`), and would be added as one more `gc-*` feature and one
more `GcKind` value, not as an edit of these two.

`ll_gc_checkpoint` and `ll_gc_checkpoint_ack` are unrelated to the
`gc-checkpoint` kind: they are the empty bracket generated code emits around
batched releases in every build.

## Where they come from

Both descend from Bacon and Rajan's cycle collection (2001), from its two
forms (`dev/HOW-THE-CYCLE-COLLECTOR-EVOLVED.md`, §7):

- `gc-checkpoint` is closer to the synchronous form PHP and Firefox ship:
  trial deletion over candidates, with the search moved to a collector
  thread and the owner's check at the poll keeping it exact.
- `gc-window` descends from their concurrent Recycler, where the collector
  frees alone. Its check that nothing changed under the collector is the
  window tag: an entity touched while the window is open counts as live
  (`dev/design/recycler-over-counts.md`, §2 and §5f).

## What each costs

`web-heap`, two mutators, three repeats, CPU seconds
(`dev/BENCHMARKS.md`, 2026-10-07, "A with the same higher offer against B
with R50"; `gc-checkpoint` is its row A, the build on main, and
`gc-window` its row R50):

| | mutator | collector | total | garbage mean, MB |
|---|---|---|---|---|
| `gc-checkpoint` | 69.4–71.4 | 49.5–51.7 | 119–123 | 25.8–31.3 |
| `gc-window` | 45.6–46.6 | 56.4–58.5 | 102–105 | 23.1–26.8 |

- The owner's longest pause on `web-heap`: 117–198 ms for `gc-checkpoint`,
  10–34 ms for `gc-window` (`dev/BENCHMARKS.md`, 2026-10-05, S68.11).
- Request latency p99 on `live-churn`: 786–983 µs against 164–262 µs (the
  2026-10-07 entry above).
- The tag itself, built for an executable: `ll_retain` 16 instructions
  against 13, `ll_release` 25 against 22
  (`dev/design/the-general-algorithm.md`, the write-order revision).

## Choosing

- `gc-window` where the program's own time and its pauses matter: about a
  third less mutator CPU, pauses of tens of milliseconds rather than
  hundreds, lower tail latency, about 15 % less CPU in total.
- `gc-checkpoint` where the collector's core is the scarce one: its
  collector spends about an eighth less, and no write carries a tag.
