# The `door` sites, classified for S41.7

Every occurrence of `door` in `src/`, with the glossary class it belongs to
and the words that replace it. `PLAN.md` S41.7 renames the word; this
document is the input that rename reads, so that a reader can run it without
re-reading the crate. Counted on 2026-09-01 at commit `019618d`, against the
glossary at `rfc` commit `0075ef3`.

## The count

Occurrences of the substring `door`, in any case, in the contents of `src/`:

```
grep -rnoi 'door' src/ | wc -l          # 140 occurrences
grep -rni  'door' src/ | wc -l          # 138 lines
grep -rli  'door' src/ | wc -l          # 52 files
find src -iname '*door*'                # 3 file names
```

Every occurrence is lower-case; `grep -rno 'door'` and `grep -rnoi 'door'`
both answer 140. Three file names carry the word as well, so the inventory
below has 143 rows: 140 occurrences in file contents and 3 file names.

`PLAN.md` S41.8's handoff quotes 86 sites as of S41.8's commit `27ffbf3`.
That figure is the number of lines carrying the whole word `door` in the
singular, and it reproduces at that commit and at the tree the last handoff
describes:

```
git grep -nw door 27ffbf3 -- src | wc -l     # 86
git grep -nw door 03ac50f -- src | wc -l     # 86
git grep -nw door 019618d -- src | wc -l     # 101
```

The same command answers 101 at `019618d` because `8ccf426` and `bd27cdd`
moved the trace's withheld returns and the weak table into manager memory and
wrote new sites in `src/cycle/deferred_slot_reuse.rs`, its tests and
`src/weak/tests/`. The whole-word count leaves out lines whose only
occurrence is the plural `doors`, a compound test name, or the closure
binding `|mut door|`, which is why this inventory counts occurrences rather
than lines. S41.7's own handoff quotes 76 sites "over those five types", which
is the count of the `Refused` and `InsertOutcome` sites and not of `door`.

## The glossary entry

`rfc/dev/GLOSSARY.md`, "Context-sensitive words":

> *door* becomes *allocation path*, *entry point*, *mailbox*, *channel*, or
> *store-barrier form*, according to the operation.

The same file's "Deprecated terms" table fixes two of the compounds:

> | critical door | reserve allocation path |
> | ordinary door | ordinary allocation path |

And its opening paragraph draws the boundary this inventory uses for the
sites that are none of the five:

> Ordinary English uses of a word are not terms and are outside this
> glossary.

The glossary defines none of the five classes in a row of its own.
*Allocation path* is used by `rfc/model/memory/critical-reserve.md`,
"Allocation paths", which names the *ordinary allocation path* and the
*reserve allocation path*. *Entry point* is used by
`dev/CYCLE-TERMINOLOGY-AUDIT.md`, "Memory-manager terminology", for a site
that is a return API. *Mailbox* appears in the canonical table's definition of
*actor*. *Channel* and *store-barrier form* appear nowhere in the `rfc` tree
outside the glossary line itself.

## How each class is applied

**Allocation path.** The site names a source of blocks or bytes that can
refuse: the block pool, the critical reserve, the queue's spare cells, the
large-entity path. The two compounds take the deprecated-terms rows. A pair
takes the plural: *both allocation paths*, *neither allocation path*. A
refusal is written as *refuses* or *has refused*, and a path that is not
refusing as *serves*, which is what the sites already say beside the word.

**Entry point.** The site names a function through which a caller reaches an
operation: a C ABI symbol, a factory, a free, a teardown body, a store. Where
the sentence needs the function, the replacement names it in backticks
instead of an adjective, because "the ordinary entry point" beside "the
ordinary allocation path" would read as one thing.

**Mailbox, channel, store-barrier form.** No site names a message queue or a
transfer between threads, so mailbox and channel have no rows. One site is a
store and is listed under the rulings rather than classified, because the
glossary's line is the only text defining *store-barrier form* and it does
not say whether a public store function is one.

**None of the five.** An operating-system resource is named exactly:
`/dev/urandom`, `BCryptGenRandom`. Ordinary English keeps a plain word that
is not on the metaphor list: *route* for a second way into a state, *site*
for the one place a check can stand. One site names the token `door` itself.

## Sites outside the five classes

Fifteen of the 143 rows fit none of the glossary's classes. Each is listed
here with the reading that refused the classes; the table below repeats them
in file order with the same replacement.

**Awaiting a ruling.**

- `src/array/element/tests/an_element_in_a_reference_state.rs:353`, "`$r =
  2` through the public door: `$b['x']` is in a reference state, so the
  store finds the box and writes into it." The site is `element::set`, a
  public store into an array element. *Entry point* fits a public function;
  *store-barrier form* fits a store that reaches the barrier; the glossary's
  line is the only text using the second term and does not say which wins.
  The rename waits for the ruling under "Sites that need a ruling".

**Operating-system resource.** The glossary asks for the resource named
exactly; the resource is `/dev/urandom`, and on Windows `BCryptGenRandom`.

- `src/hash/process_key.rs:62`, "the per-process key is drawn from
  /dev/urandom, and this target has no door". The sentence names the device
  one clause earlier; *no door* means *no `/dev/urandom`*.
- `src/hash/process_key.rs:131`, twice: the closure binding `|mut door|` and
  its use `door.read_exact(&mut bytes)`. The binding is the open file of
  `/dev/urandom`; the identifier becomes `urandom`.
- `src/memory/os.rs:135`, "the way the per-process key's own unix-only door
  does". The unix-only thing is the `/dev/urandom` read behind
  `#[cfg(not(unix))] compile_error!`.
- `src/memory/os.rs:136`, the quoted `PLAN.md` lead-in "The per-process key's
  Windows door". The words are the plan's, and the citation moves with them;
  see "Findings beside the inventory".

**Ordinary English.** Each names a second way into a state, or the one
place a check stands, and no allocation, ABI call, message or store is the
subject.

- `src/journal/mod.rs:235`, "the false *none* by the one door that opens
  under memory pressure, which is when the journal is switched on". The
  sentence says how a false *none* arises, and the *door* is the
  circumstance, not an operation; the clause is reworded to "the false
  *none* that only memory pressure produces".
- `src/journal/tests/a_thread_the_journal_could_not_serve.rs:115`, "the
  degradation the per-window difference exists to avoid, through a second
  door". A second way the same degradation could arise: *by a second route*.
- `src/journal/tests/where_the_retirement_sits_inside_the_exit.rs:299`,
  "Both doors into the pending list go through this." Thread exit and
  eviction both take the registry lock the next line acquires: *Both routes
  into the pending list take this lock.*
- `src/memory/retained.rs:215`, "The one door of the three where a miscount
  ends at the block pool rather than at `false`". The three are
  `occupant_freed`, `payload_freed` and `reset_pin_released`, the functions
  that release a count on a retained block; none allocates: *The one of the
  three release functions*.
- `src/object/tests/who_owes_the_destructor.rs:5`, "puts the object in that
  same state by the code's other door", and `:75`, "The other door into that
  state". A second way into the state *owes no destructor*: *route*.
- `src/refcount.rs:278`, "the one door that can catch a kind classified on
  one side of the reserve and coded on the other". `to_flags` is the one
  function every flags word passes at birth; the sentence is about where a
  check can stand: *the one site that can catch*.
- `src/weak/tests/what_the_weak_table_asks_the_allocator.rs:1`, "measured at
  the one door that can answer: the test binary's counting global
  allocator". The subject is where a measurement can be taken, and the
  allocator named is Rust's, outside the crate's allocation paths: *the one
  site that can answer*.

**The token itself.**

- `src/cycle/tests/the_metaphors_the_names_still_carry.rs:34`, "`door` joins
  this list at `PLAN.md` S41.7, which is the step that classifies its
  sites". The word is quoted as a token the guard will read; the rename
  fulfils the sentence and removes it.

## The inventory

Class codes in the table: `AP` allocation path, `EP` entry point, `OS`
operating-system resource, `EN` ordinary English, `TK` the token itself.
A line with two occurrences has one row per occurrence. The replacement is
the words that stand where `door` stood; the rest of the sentence is
unchanged unless the row says otherwise.

| File | Line | Span | Class | Replacement |
|---|---|---|---|---|
| `src/array/element/tests/an_element_in_a_reference_state.rs` | 353 | `$r = 2` through the public door | — | under "Sites that need a ruling" |
| `src/array/element/tests/what_a_key_the_vector_cannot_hold_does.rs` | 6 | neither door here names a representation | EP | neither entry point here (`set` with either key) names a representation |
| `src/array/entity.rs` | 327 | is a separate door on purpose | EP | is a separate entry point on purpose |
| `src/array/entity.rs` | 1371 | those pass no other death door | EP | those pass no other teardown entry point |
| `src/array/entity.rs` | 1379 | so the door is the death | EP | so this teardown entry point is the death |
| `src/array/entity.rs` | 1404 | never passes the bare-pointer door | EP | never passes `ll_entity_die` |
| `src/array/entity.rs` | 1404 | a duty that door carries | EP | a duty that entry point carries |
| `src/array/entity/tests.rs` | 206 | `mod the_two_cow_doors;` | EP | `mod the_two_cow_entry_points;` |
| `src/array/entity/tests/the_two_cow_doors.rs` | name | `the_two_cow_doors.rs` | EP | `the_two_cow_entry_points.rs` |
| `src/array/entity/tests/the_two_cow_doors.rs` | 1 | One body serves both doors | EP | One body serves both entry points, `ll_cow_separate` and the escape copy, |
| `src/array/entity/tests/the_two_cow_doors.rs` | 11 | The COW door. | EP | The separation entry point. |
| `src/array/entity/tests/the_two_cow_doors.rs` | 198 | The escape door. | EP | The escape-copy entry point. |
| `src/array/entity/tests/the_two_cow_doors.rs` | 526 | The deep door over a vector | EP | The escape copy over a vector |
| `src/array/entity/tests/what_a_death_gives_back.rs` | 1 | the only door a bare entity pointer has | EP | the only teardown entry point a bare entity pointer has |
| `src/array/entity/tests/what_a_death_gives_back.rs` | 35 | the only door a bare entity pointer has | EP | the only teardown entry point a bare entity pointer has |
| `src/array/entity/tests/what_a_refused_copy_gives_back.rs` | 441 | The shallow door with an arena holder | EP | The shallow copy with an arena holder |
| `src/array/vector/tests.rs` | 11 | the named door is what keeps | EP | the named factory is what keeps |
| `src/array/vector/tests/the_entity_over_a_vector.rs` | 2 | through the same doors the ordered hash uses | EP | through the same entry points the ordered hash uses |
| `src/array/vector/tests/the_entity_over_a_vector.rs` | 120 | This is the door the tag has to be read at | EP | This is the entry point the tag has to be read at |
| `src/cycle/arena.rs` | 4 | Two doors, in this order | AP | Two allocation paths, in this order |
| `src/cycle/arena.rs` | 9 | the critical door is the fallback | AP | the reserve allocation path is the fallback |
| `src/cycle/arena.rs` | 13 | A refusal at both doors aborts | AP | A refusal on both allocation paths aborts |
| `src/cycle/arena.rs` | 26 | what the critical door lent | AP | what the reserve allocation path lent |
| `src/cycle/arena.rs` | 28 | wants a door that is open | AP | wants an allocation path that serves |
| `src/cycle/arena.rs` | 50 | through the very door that has already refused | AP | through the very allocation path that has already refused |
| `src/cycle/arena.rs` | 105 | Both memory doors refused. | AP | Both allocation paths refused. |
| `src/cycle/arena.rs` | 116 | came through the critical door | AP | came through the reserve allocation path |
| `src/cycle/arena.rs` | 165 | null when both doors have refused | AP | null when both allocation paths have refused |
| `src/cycle/arena.rs` | 304 | null when both memory doors have refused | AP | null when both allocation paths have refused |
| `src/cycle/arena.rs` | 361 | through the reserve's door | AP | through the reserve allocation path |
| `src/cycle/arena.rs` | 431 | when both doors refuse | AP | when both allocation paths refuse |
| `src/cycle/arena.rs` | 449 | after both doors have answered | AP | after both allocation paths have answered |
| `src/cycle/arena/tests/the_rows_a_block_gets_at_its_first_touch.rs` | 360 | shut the door, then spend the block | AP | close the ordinary allocation path, then spend the block |
| `src/cycle/arena/tests/what_the_arena_gives_back.rs` | 32 | closes the ordinary door | AP | closes the ordinary allocation path |
| `src/cycle/arena/tests/what_the_arena_gives_back.rs` | 33 | from both doors having refused | AP | from both allocation paths having refused |
| `src/cycle/arena/tests/what_the_arena_gives_back.rs` | 37 | `a_refusal_at_both_doors_leaves_nothing_behind` | AP | `a_refusal_on_both_allocation_paths_leaves_nothing_behind` |
| `src/cycle/arena/tests/what_the_arena_gives_back.rs` | 43 | "the ordinary door served" | AP | "the ordinary allocation path served" |
| `src/cycle/arena/tests/what_the_arena_gives_back.rs` | 49 | "the ordinary door is refusing" | AP | "the ordinary allocation path is refusing" |
| `src/cycle/arena/tests/what_the_arena_gives_back.rs` | 54 | "and the critical door has nothing to serve" | AP | "and the reserve allocation path has nothing to serve" |
| `src/cycle/arena/tests/what_the_arena_gives_back.rs` | 67 | The ordinary door is asked first | AP | The ordinary allocation path is asked first |
| `src/cycle/arena/tests/what_the_arena_gives_back.rs` | 95 | wants a door that is open | AP | wants an allocation path that serves |
| `src/cycle/arena/tests/what_the_arena_gives_back.rs` | 108 | "the ordinary door is the one refusing" | AP | "the ordinary allocation path is the one refusing" |
| `src/cycle/arena/tests/what_the_arena_gives_back.rs` | 110 | "the critical door served" | AP | "the reserve allocation path served" |
| `src/cycle/arena/tests/what_the_arena_gives_back.rs` | 238 | Both doors refuse | AP | Both allocation paths refuse |
| `src/cycle/arena/tests/what_the_arena_gives_back.rs` | 250 | "the ordinary door is refusing" | AP | "the ordinary allocation path is refusing" |
| `src/cycle/arena/tests/what_the_arena_gives_back.rs` | 255 | "and the critical door has nothing to serve" | AP | "and the reserve allocation path has nothing to serve" |
| `src/cycle/arena/tests/what_the_arena_gives_back.rs` | 291 | "the ordinary door is refusing" | AP | "the ordinary allocation path is refusing" |
| `src/cycle/deferred_slot_reuse.rs` | 18 | That door first refuses the queue window | EP | That entry point first refuses the queue window |
| `src/cycle/deferred_slot_reuse.rs` | 21 | through the same door | EP | through the same entry point |
| `src/cycle/deferred_slot_reuse.rs` | 49 | so both doors refusing is an outcome | AP | so both allocation paths refusing is an outcome |
| `src/cycle/deferred_slot_reuse.rs` | 89 | drawn through the critical door | AP | drawn through the reserve allocation path |
| `src/cycle/deferred_slot_reuse.rs` | 155 | which door answered | AP | which allocation path answered |
| `src/cycle/deferred_slot_reuse.rs` | 181 | when neither door answers | AP | when neither allocation path answers |
| `src/cycle/deferred_slot_reuse.rs` | 233 | through the ordinary door, oldest first | EP | through `ll_free`, oldest first |
| `src/cycle/deferred_slot_reuse.rs` | 259 | Replaying it once through the ordinary door | EP | Replaying it once through `ll_free` |
| `src/cycle/deferred_slot_reuse.rs` | 271 | what the reserve lent through the critical door | AP | what the reserve lent through the reserve allocation path |
| `src/cycle/deferred_slot_reuse.rs` | 274 | wants a door that is open | AP | wants an allocation path that serves |
| `src/cycle/deferred_slot_reuse.rs` | 316 | when neither memory door can fund | AP | when neither allocation path can fund |
| `src/cycle/deferred_slot_reuse.rs` | 377 | both memory doors refuse a single block | AP | both allocation paths refuse a single block |
| `src/cycle/deferred_slot_reuse.rs` | 386 | after both doors have answered | AP | after both allocation paths have answered |
| `src/cycle/deferred_slot_reuse/tests.rs` | 340 | "the withheld return asks no door: | AP | "the withheld return asks no allocation path: |
| `src/cycle/deferred_slot_reuse/tests.rs` | 348 | `an_aborted_window_replays_its_returns_with_both_doors_shut` | AP | `an_aborted_window_replays_its_returns_with_both_allocation_paths_refusing` |
| `src/cycle/deferred_slot_reuse/tests.rs` | 364 | exercised with both doors shut | AP | exercised with both allocation paths refusing |
| `src/cycle/deferred_slot_reuse/tests.rs` | 387 | `a_window_neither_door_can_fund_does_not_open` | AP | `a_window_neither_allocation_path_can_fund_does_not_open` |
| `src/cycle/deferred_slot_reuse/tests.rs` | 395 | "the window opened on memory neither door granted" | AP | "the window opened on memory neither allocation path granted" |
| `src/cycle/deferred_slot_reuse/tests.rs` | 546 | "the reserve is the second door here" | AP | "the reserve is the second allocation path here" |
| `src/cycle/deferred_slot_reuse/tests.rs` | 600 | before the door shuts | AP | before the ordinary allocation path starts refusing |
| `src/cycle/deferred_slot_reuse/tests.rs` | 615 | "the reserve is the second door here" | AP | "the reserve is the second allocation path here" |
| `src/cycle/deferred_slot_reuse/tests.rs` | 623 | "the growth took the second door while the first was shut" | AP | "the growth took the reserve allocation path while the ordinary one refused" |
| `src/cycle/mark.rs` | 58 | Both memory doors refused, so the collection aborts. | AP | Both allocation paths refused, so the collection aborts. |
| `src/cycle/mark.rs` | 125 | False when both memory doors refused. | AP | False when both allocation paths refused. |
| `src/cycle/mark.rs` | 163 | False when both memory doors refused. | AP | False when both allocation paths refused. |
| `src/cycle/mark/tests/an_aborted_mark_writes_nothing.rs` | 86 | finds the arena empty and both doors shut | AP | finds the arena empty and both allocation paths refusing |
| `src/cycle/mark/tests/an_aborted_mark_writes_nothing.rs` | 96 | "the ordinary door is refusing" | AP | "the ordinary allocation path is refusing" |
| `src/cycle/mark/tests/an_aborted_mark_writes_nothing.rs` | 101 | "and the critical door has nothing to serve" | AP | "and the reserve allocation path has nothing to serve" |
| `src/cycle/queue/tests.rs` | 32 | the only door registration has | EP | the only entry point registration has |
| `src/cycle/queue/tests/an_arena_entity_leaves_no_entry.rs` | 6 | An arena slot has no such door | EP | An arena slot has no such entry point |
| `src/cycle/queue/tests/where_a_full_segment_comes_from.rs` | 132 | Every door refused: | AP | Every allocation path refused: |
| `src/cycle/queue/tests/where_a_full_segment_comes_from.rs` | 135 | The registration's doors are the spare cells and the critical reserve | AP | The registration's allocation paths are the spare cells and the critical reserve |
| `src/cycle/queue/tests/where_a_full_segment_comes_from.rs` | 210 | finds every door still spent | AP | finds every allocation path still spent |
| `src/cycle/scan.rs` | 78 | Both memory doors refused the worklist a segment | AP | Both allocation paths refused the worklist a segment |
| `src/cycle/scan.rs` | 146 | False when both memory doors refused. | AP | False when both allocation paths refused. |
| `src/cycle/stack.rs` | 93 | when both memory doors refused | AP | when both allocation paths refused |
| `src/cycle/stack/tests.rs` | 103 | `a_push_with_both_doors_shut_answers_false` | AP | `a_push_with_both_allocation_paths_refusing_answers_false` |
| `src/cycle/stack/tests.rs` | 115 | "the ordinary door is refusing" | AP | "the ordinary allocation path is refusing" |
| `src/cycle/tests/the_metaphors_the_names_still_carry.rs` | 34 | `door` joins this list at `PLAN.md` S41.7 | TK | the paragraph goes, and `"door"` enters `METAPHORS`; see "Edits outside `src/` the rename needs" |
| `src/hash/process_key.rs` | 62 | this target has no door: | OS | this target has no `/dev/urandom`: |
| `src/hash/process_key.rs` | 131 | `\|mut door\|` | OS | `\|mut urandom\|` |
| `src/hash/process_key.rs` | 131 | `door.read_exact(&mut bytes)` | OS | `urandom.read_exact(&mut bytes)` |
| `src/journal/kinds.rs` | 33 | the one door every factory in the crate goes through | EP | the one entry point every factory in the crate goes through, `refcount::publish_header` |
| `src/journal/kinds.rs` | 40 | reaches teardown by two doors | EP | reaches teardown by two entry points |
| `src/journal/kinds/tests/what_the_sites_record.rs` | 172 | One record through the door that ignores the mask | EP | One record through the entry point that ignores the mask, `journal::record` |
| `src/journal/mod.rs` | 235 | the false *none* by the one door that opens under memory pressure | EN | the false *none* that only memory pressure produces |
| `src/journal/tests/a_thread_the_journal_could_not_serve.rs` | 115 | through a second door | EN | by a second route |
| `src/journal/tests/where_the_retirement_sits_inside_the_exit.rs` | 299 | Both doors into the pending list go through this. | EN | Both routes into the pending list take this lock. |
| `src/memory/arena.rs` | 119 | one of the two doors that split at the same bound | EP | one of the two entry points that split at the same bound |
| `src/memory/arena.rs` | 261 | A door of its own rather than a lifted bound | EP | An entry point of its own rather than a lifted bound |
| `src/memory/arena.rs` | 282 | this door reports | EP | this entry point reports |
| `src/memory/arena.rs` | 516 | leaves through the free door | EP | leaves through `ll_free` |
| `src/memory/buffer/tests.rs` | 10 | `mod the_abi_door;` | EP | `mod the_abi_entry_point;` |
| `src/memory/buffer/tests/the_abi_door.rs` | name | `the_abi_door.rs` | EP | `the_abi_entry_point.rs` |
| `src/memory/critical.rs` | 2 | the ordinary door has already refused | AP | the ordinary allocation path has already refused |
| `src/memory/critical.rs` | 5 | "The two doors" | AP | "Allocation paths"; see "Findings beside the inventory" |
| `src/memory/critical.rs` | 141 | both doors have refused | AP | both allocation paths have refused |
| `src/memory/critical.rs` | 165 | whatever door it came through | AP | whatever allocation path it came through |
| `src/memory/critical.rs` | 219 | finds the door shut | AP | finds the reserve allocation path empty |
| `src/memory/critical/tests.rs` | 4 | `mod the_door_that_opens_after_a_refusal;` | AP | `mod the_allocation_path_that_serves_after_a_refusal;` |
| `src/memory/critical/tests/the_door_that_opens_after_a_refusal.rs` | name | `the_door_that_opens_after_a_refusal.rs` | AP | `the_allocation_path_that_serves_after_a_refusal.rs` |
| `src/memory/critical/tests/the_door_that_opens_after_a_refusal.rs` | 1 | where the ordinary door has already said no | AP | where the ordinary allocation path has already refused |
| `src/memory/critical/tests/the_door_that_opens_after_a_refusal.rs` | 21 | names which door said no | AP | names which allocation path refused |
| `src/memory/critical/tests/the_door_that_opens_after_a_refusal.rs` | 32 | "the ordinary door is the one refusing" | AP | "the ordinary allocation path is the one refusing" |
| `src/memory/critical/tests/the_door_that_opens_after_a_refusal.rs` | 35 | "the critical door still serves" | AP | "the reserve allocation path still serves" |
| `src/memory/large_entity/tests/an_entity_that_fills_its_own_block.rs` | 3 | lives and dies through the ordinary doors | EP | is allocated and freed through the ordinary entry points |
| `src/memory/os.rs` | 135 | the per-process key's own unix-only door | OS | the per-process key's own unix-only `/dev/urandom` read |
| `src/memory/os.rs` | 136 | "The per-process key's Windows door" | OS | the `PLAN.md` lead-in, reworded with the plan; see "Findings beside the inventory" |
| `src/memory/retained.rs` | 215 | The one door of the three | EN | The one of the three release functions |
| `src/memory/routing.rs` | 49 | The arena's own entity door | EP | The arena's own entity entry point |
| `src/memory/routing.rs` | 51 | `Arena::alloc` is not that door | EP | `Arena::alloc` is not that entry point |
| `src/memory/routing/tests/where_a_category_gets_its_bytes.rs` | 89 | made by a door that knows which one it is holding | EP | made by an entry point that knows which one it is holding |
| `src/memory/stdapi.rs` | 293 | Every teardown door has to clear it | EP | Every teardown entry point has to clear it |
| `src/memory/stdapi.rs` | 294 | where a door that forgot to says so | EP | where an entry point that forgot to says so |
| `src/memory/stdapi.rs` | 351 | replayed through this same door | EP | replayed through this same entry point |
| `src/memory/stdapi/tests/a_size_the_allocator_must_refuse.rs` | 1 | Refusal is reported as null on every door. | EP | Refusal is reported as null at every entry point. |
| `src/memory/stdapi/tests/a_size_the_allocator_must_refuse.rs` | 32 | The ABI door as well | EP | `ll_malloc` as well |
| `src/memory/stdapi/tests/a_size_the_allocator_must_refuse.rs` | 37 | And the growth door | EP | And `ll_realloc` |
| `src/memory/stdapi/tests/the_slot_a_queue_entry_names.rs` | 123 | the same door takes the slot | EP | the same entry point takes the slot |
| `src/object.rs` | 668 | this door takes both | EP | this entry point takes both |
| `src/object.rs` | 751 | so this door is the array's too | EP | so this entry point is the array's too |
| `src/object.rs` | 755 | until the two doors are one | EP | until the two entry points are one |
| `src/object.rs` | 790 | the candidate-buffer duty this door carries | EP | the candidate-buffer duty this entry point carries |
| `src/object/tests/what_the_factory_stamps.rs` | 5 | The construct-into-a-reserved-cell door shares the stamp | EP | The construct-into-a-reserved-cell entry point shares the stamp |
| `src/object/tests/who_owes_the_destructor.rs` | 5 | by the code's other door | EN | by the code's other route |
| `src/object/tests/who_owes_the_destructor.rs` | 75 | The other door into that state | EN | The other route into that state |
| `src/promote.rs` | 537 | which the arena's entity door gives | EP | which the arena's entity entry point gives |
| `src/promote/tests/the_memory_a_survivor_takes_with_it.rs` | 337 | "the arena's entity door gave it a run of its own" | EP | "the arena's entity entry point gave it a run of its own" |
| `src/promote/tests/the_memory_a_survivor_takes_with_it.rs` | 385 | The other half of the door's contract | EP | The other half of the entry point's contract |
| `src/refcount.rs` | 278 | the one door that can catch a kind | EN | the one site that can catch a kind |
| `src/reference.rs` | 80 | is the only door | EP | is the only entry point |
| `src/test_support.rs` | 78 | "so it exercises no large-entity door" | AP | "so it exercises no large-entity allocation path" |
| `src/test_support/outside_block.rs` | 14 | the same door a table's storage takes | AP | the same allocation path a table's storage takes |
| `src/test_support/outside_block.rs` | 97 | through the same category-routed door | EP | through the same category-routed entry point |
| `src/weak/tests/what_the_weak_table_asks_the_allocator.rs` | 1 | measured at the one door that can answer | EN | measured at the one site that can answer |
| `src/weak/tests/what_the_weak_table_asks_the_allocator.rs` | 272 | until the door lowers them | EP | until `lower_peak_to_current` lowers them |

## The distribution

Counted from the table above, one row per occurrence or file name:

```
grep -c '^| `src/' dev/design/door-sites.md                    # 143 rows
grep '^| `src/' dev/design/door-sites.md | grep -c '| AP |'    # allocation path
grep '^| `src/' dev/design/door-sites.md | grep -c '| EP |'    # entry point
grep '^| `src/' dev/design/door-sites.md | grep -c '| OS |'    # OS resource
grep '^| `src/' dev/design/door-sites.md | grep -c '| EN |'    # ordinary English
grep '^| `src/' dev/design/door-sites.md | grep -c '| TK |'    # the token
grep '^| `src/' dev/design/door-sites.md | grep -c '| — |'     # awaiting a ruling
```

| Class | Rows |
|---|---|
| allocation path | 73 |
| entry point | 55 |
| mailbox | 0 |
| channel | 0 |
| store-barrier form | 0 |
| none: operating-system resource | 5 |
| none: ordinary English | 8 |
| none: the token itself | 1 |
| awaiting a ruling | 1 |
| in `src/` at all | 143 |

Two of the glossary's five classes have no site in the crate. That is a fact
about the crate, not a defect in the list: the crate has no actor mailbox and
no cross-thread channel yet.

## Sites that need a ruling

- `src/array/element/tests/an_element_in_a_reference_state.rs:353`, "`$r =
  2` through the public door". The site is `element::set`, the public store
  into an array element, which resolves a reference-state element to its box
  and writes through the store barrier. Is a public store function an *entry
  point*, or is it a *store-barrier form*? The glossary's line is the only
  text that uses *store-barrier form*, and it does not say.

## Edits outside `src/` the rename needs

- **`src/cycle/tests/the_metaphors_the_names_still_carry.rs`.** The
  paragraph at line 34 says `door` joins `METAPHORS` at S41.7; the rename
  deletes the paragraph and adds `"door"` to the array, which then has
  fourteen entries. The three file names and the four test names in the
  table are what the identifier guard will read; after the rename no
  exemption is needed, because every replacement above is free of the stem.
- **`dev/CYCLE-TERMINOLOGY-AUDIT.md`, "Memory-manager terminology"** already
  gives the rule this table applies; nothing there changes.

## Findings beside the inventory

Found while reading the sites. None is fixed by the inventory commit.

- **`src/memory/os.rs:136` quotes a `PLAN.md` lead-in that carries the
  word.** The bullet "The per-process key's Windows door" stands under
  "Residual / carried-over items", and `dev/WORKFLOW.md`, "Comments: the
  contract in the code, the argument in a document", forbids rewording a
  cited lead-in without moving the citation. The rename of the site and of
  the bullet is one commit, which makes it S41.7's and not a comment fix. A
  wording that names the resource: "The per-process key's Windows randomness
  source".
- **`src/memory/critical.rs:5` cites two headings
  `rfc/model/memory/critical-reserve.md` no longer has.** "The two doors" and
  "The three customers" are cited; the document at `rfc` commit `0075ef3`
  has "Allocation paths" and "Reserve users" in their places. The comment
  guard spares a quoted heading, so this site would survive a rename that
  renames everything else.
- **`src/cycle/arena.rs:6` cites the same document's "The three
  customers"**, with the same repair.
