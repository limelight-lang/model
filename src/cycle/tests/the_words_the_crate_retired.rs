//! The identifiers `dev/CYCLE-TERMINOLOGY-AUDIT.md` retires are gone from the
//! sources, and this reads the sources to say so.
//!
//! A rename is invisible to every other test in this crate: the suite calls
//! whatever the code declares, so a module that keeps `escrow` beside a caller
//! that keeps `escrow` is as green as one that keeps neither. What the compiler
//! does catch is half a rename, and only in the configuration it builds — a
//! `#[cfg]`-disabled arm parses without resolving a name, which is how
//! `refcount::tests::who_may_read_a_header` came to read sources rather than
//! run code.
//!
//! **The unit is the identifier, not the word.** A line's trailing comment is
//! cut before the scan: prose is `PLAN.md` S41.6's, which classifies every
//! remaining occurrence as a citation, unrelated English or a defect, and a
//! guard that also owned prose would refuse `live` in the sense the RFC
//! glossary makes canonical. What stays checked inside a line of code is the
//! string literals, because an assertion message names the state it asserts
//! over and S41.5 renames those with the code.
//!
//! **A document citation is exempt**, and it is the one exemption. Active
//! prose that cites a superseded heading keeps the heading exactly and adds
//! the current name outside the quotation (`dev/CYCLE-TERMINOLOGY-AUDIT.md`,
//! "Documentation boundary"), so a literal naming a `.md` file is dropped from
//! the line before its identifiers are read.

use std::fs;
use std::path::{Path, PathBuf};

/// Where a retired identifier is retired.
///
/// Half this vocabulary is ordinary English somewhere in the crate. `drain`
/// is `Vec::drain` in a dozen modules and the queue's segment release in one
/// file; `replenish` names the queue's spare refill and, separately, the
/// reserve's and the critical cache's own, which this mapping does not touch.
enum Where {
    /// Every source under `src/`. The name is this crate's vocabulary
    /// wherever it stands, so a caller in `refcount` that still says `enrol`
    /// is the same defect as the queue's own declaration.
    Anywhere,
    /// The subtree that owns the name, by a path relative to `src/`. Used for
    /// a name with no caller outside it, where the word is ordinary English
    /// elsewhere.
    Under(&'static str),
    /// Everywhere but the subtrees that own a homonym of their own. Used
    /// where the retired name has callers across the crate and the word is
    /// also somebody else's declaration.
    Except(&'static [&'static str]),
}

/// A retired identifier, the name the ratified mapping gives it, and where it
/// is retired.
///
/// The ratified name is carried for the failure message. A guard that only
/// says "this word is gone" sends its reader back to the audit for the
/// replacement, and the replacement is what the audit decided.
const RETIRED: [(&str, &str, Where); 72] = [
    // Candidate registration, whose callers are spread over four modules.
    ("enrol", "register_candidate", Where::Anywhere),
    ("ENROLLED", "CANDIDATE_BIT", Where::Anywhere),
    ("enrolled_count", "candidate_count", Where::Anywhere),
    // The queue's base block, its overflow buffer and its spare segments.
    ("floor_of", "queue_base_of", Where::Anywhere),
    ("draw_floor", "try_ensure_queue_base", Where::Anywhere),
    (
        "draw_floor_or_abort",
        "ensure_queue_base_or_abort",
        Where::Anywhere,
    ),
    ("take_floor", "initialize_queue_base", Where::Anywhere),
    ("release_floor", "release_queue_base", Where::Anywhere),
    ("grow_and_write", "append_with_new_segment", Where::Anywhere),
    ("is_short", "needs_spares", Where::Anywhere),
    (
        "replenish",
        "refill_spares",
        Where::Except(&["memory/reserve", "memory/critical"]),
    ),
    ("spares_held", "spare_count", Where::Anywhere),
    ("live_segment", "write_segment", Where::Anywhere),
    ("fill_live_segment", "fill_write_segment", Where::Anywhere),
    ("live_entry", "write_segment_entry", Where::Anywhere),
    ("OWNER", "OWNER_STATE", Where::Anywhere),
    ("escrow", "append_to_overflow", Where::Under("cycle")),
    ("escrowed", "overflow_len", Where::Under("cycle")),
    ("escrow_entries", "overflow_entries", Where::Under("cycle")),
    ("escrowed_count", "overflow_len", Where::Under("cycle")),
    ("drain_escrow", "drain_overflow", Where::Under("cycle")),
    ("ESCROW_ENTRIES", "OVERFLOW_CAPACITY", Where::Under("cycle")),
    ("live", "write_segment", Where::Under("cycle/queue")),
    ("filled", "write_len", Where::Under("cycle/queue")),
    ("held", "spare_count", Where::Under("cycle/queue")),
    ("entries", "segment_entries", Where::Under("cycle/queue")),
    (
        "drain",
        "release_queue_segments",
        Where::Under("cycle/queue"),
    ),
    ("owner", "owner_state", Where::Under("cycle/queue")),
    ("owner_ref", "owner_state_ref", Where::Under("cycle/queue")),
    // Row resolution.
    ("SOLE_OCCUPANT", "SINGLE_ENTITY_INDEX", Where::Anywhere),
    ("edge_to", "resolve_edge_target", Where::Anywhere),
    ("Row", "RowKey", Where::Under("cycle")),
    ("Sole", "SingleEntity", Where::Under("cycle")),
    ("Edge", "EdgeTarget", Where::Under("cycle")),
    ("Interior", "Tracked", Where::Under("cycle")),
    // The trace scratch arena and its shadow rows. `Colour` and its verbs go
    // together: the US spelling reaches the functions, not only the type.
    ("ShadowArena", "TraceScratchArena", Where::Anywhere),
    ("Met", "RowLookup", Where::Anywhere),
    ("Unplaced", "Untracked", Where::Anywhere),
    ("first_reach", "first_visit", Where::Anywhere),
    ("met_row", "find_initialized_row", Where::Anywhere),
    (
        "met_row_of",
        "find_initialized_row_for_entity",
        Where::Anywhere,
    ),
    ("sweep_touched", "clear_touched_rows", Where::Anywhere),
    ("Colour", "Color", Where::Anywhere),
    ("colour", "color", Where::Anywhere),
    ("recolour", "recolor", Where::Anywhere),
    ("row_colour", "row_color", Where::Anywhere),
    ("meet_group", "ensure_group_initialized", Where::Anywhere),
    ("group_is_met", "group_is_initialized", Where::Anywhere),
    ("meet", "ensure_row", Where::Under("cycle")),
    ("slots", "row_count", Where::Under("cycle/shadow")),
    // Mark and scan.
    ("Marked", "MarkResult", Where::Anywhere),
    ("Scanned", "ScanResult", Where::Anywhere),
    ("meet_root", "schedule_root_if_unvisited", Where::Anywhere),
    ("from_live", "reached_from_live", Where::Anywhere),
    (
        "decide",
        "classify_and_schedule_entity",
        Where::Under("cycle"),
    ),
    // The trace stack.
    ("segments_held", "segment_count", Where::Anywhere),
    ("climb", "advance_segment", Where::Under("cycle")),
    ("top", "current", Where::Under("cycle/stack")),
    ("used", "current_len", Where::Under("cycle/stack")),
    ("below", "previous", Where::Under("cycle/stack")),
    ("above", "next", Where::Under("cycle/stack")),
    // Deferred slot reuse.
    ("parking", "deferred_slot_reuse", Where::Anywhere),
    ("TraceWindow", "ActiveTrace", Where::Anywhere),
    ("ACTIVE", "TRACE_ACTIVE", Where::Anywhere),
    ("PARKED", "DEFERRED_SLOTS", Where::Anywhere),
    ("park_if_active", "defer_reuse_if_tracing", Where::Anywhere),
    ("parked_count", "deferred_slot_count", Where::Anywhere),
    // Exact validation, whose judicial words are the audit's first rule.
    ("Judged", "ValidationResult", Where::Anywhere),
    ("Condemned", "Unreachable", Where::Anywhere),
    ("Corpse", "ZeroCountMember", Where::Anywhere),
    ("Acquitted", "ExternallyReferenced", Where::Anywhere),
    (
        "every_member_holds_its_own_share",
        "member_counts_cover_internal_edges",
        Where::Anywhere,
    ),
];

/// The files that still carry a retired name, and the whole of the debt S41
/// pays. A file leaves this list in the commit that renames it, and the guard
/// refuses a retired name in every file that is not on it: the vocabulary
/// holds over the part of the crate already migrated, from the first commit
/// of the stage rather than the last.
///
/// The list only shrinks. It is empty when S41.5 closes, and a second test
/// below refuses a file that has stopped offending, so a rename cannot leave
/// its entry behind and quietly exempt the file from then on.
const STILL_TO_MIGRATE: [&str; 50] = [
    "cycle/arena.rs",
    "cycle/arena/tests.rs",
    "cycle/arena/tests/the_rows_a_block_gets_at_its_first_touch.rs",
    "cycle/arena/tests/what_a_met_row_answers.rs",
    "cycle/arena/tests/what_the_arena_gives_back.rs",
    "cycle/exact.rs",
    "cycle/exact/tests.rs",
    "cycle/exact/tests/what_a_mutation_racing_the_verdict_costs.rs",
    "cycle/exact/tests/what_a_ring_through_an_array_reads_as.rs",
    "cycle/exact/tests/what_an_edge_out_of_the_component_counts_for.rs",
    "cycle/exact/tests/what_the_corpse_rule_drops.rs",
    "cycle/exact/tests/what_the_guard_discount_answers.rs",
    "cycle/mark.rs",
    "cycle/mark/tests.rs",
    "cycle/mark/tests/an_aborted_mark_writes_nothing.rs",
    "cycle/mark/tests/what_the_trace_subtracts.rs",
    "cycle/mod.rs",
    "cycle/parking.rs",
    "cycle/parking/tests.rs",
    "cycle/queue.rs",
    "cycle/queue/tests.rs",
    "cycle/queue/tests/an_arena_entity_leaves_no_entry.rs",
    "cycle/queue/tests/the_floor_the_escrow_stands_on.rs",
    "cycle/queue/tests/what_an_enrolment_writes.rs",
    "cycle/queue/tests/what_gc_owns.rs",
    "cycle/queue/tests/what_the_poll_owes_the_queue.rs",
    "cycle/queue/tests/where_a_full_segment_comes_from.rs",
    "cycle/row.rs",
    "cycle/row/tests.rs",
    "cycle/row/tests/the_row_each_population_resolves_to.rs",
    "cycle/scan.rs",
    "cycle/scan/tests.rs",
    "cycle/shadow.rs",
    "cycle/shadow/tests.rs",
    "cycle/shadow/tests/what_a_first_touch_writes.rs",
    "cycle/shadow/tests/what_a_row_word_holds.rs",
    "cycle/stack.rs",
    "cycle/stack/tests.rs",
    "cycle/testing.rs",
    "gc.rs",
    "memory/arena/tests/the_logs_the_reset_reads.rs",
    "memory/critical/tests/where_the_first_touch_happens.rs",
    "memory/gc_metadata/tests.rs",
    "memory/heap.rs",
    "memory/stdapi.rs",
    "memory/stdapi/tests/the_slot_a_queue_entry_names.rs",
    "object/tests/who_owes_the_destructor.rs",
    "refcount.rs",
    "refcount/tests/the_enrolment_gate.rs",
    "refcount/tests/the_header_the_compiler_shares.rs",
];

/// Every `.rs` file under `src/`, in no particular order.
fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("src/ is readable") {
        let path = entry.expect("a readable directory entry").path();
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
}

/// This file, which is the one place every retired identifier is written on
/// purpose. `path` is relative to `src/`: matched against an absolute path, a
/// checkout under a directory of this name would exempt the crate.
fn exempt_file(path: &Path) -> bool {
    path.to_string_lossy().replace('\\', "/") == "cycle/tests/the_words_the_crate_retired.rs"
}

/// The code of `line`: its trailing comment removed, and every string literal
/// that names a `.md` document removed with it.
///
/// The `//` that opens a comment is found outside string literals only, so a
/// path written inside a message survives. Escapes are not tracked: a `\"`
/// inside a literal ends it early here, which can only shorten what is cut and
/// so cannot hide an identifier.
fn code_of(line: &str) -> String {
    let bytes: Vec<char> = line.chars().collect();
    let mut kept = String::with_capacity(line.len());
    let mut literal = String::new();
    let mut in_string = false;
    let mut index = 0;

    while index < bytes.len() {
        let c = bytes[index];

        if in_string {
            literal.push(c);
            if c == '"' {
                in_string = false;
                // A citation names its document; anything else is a message,
                // and a message names state this crate has to rename with the
                // code it asserts over.
                if !literal.contains(".md") {
                    kept.push_str(&literal);
                }

                literal.clear();
            }

            index += 1;
            continue;
        }

        if c == '"' {
            in_string = true;
            literal.push(c);
            index += 1;
            continue;
        }

        if c == '/' && index + 1 < bytes.len() && bytes[index + 1] == '/' {
            break;
        }

        kept.push(c);
        index += 1;
    }

    // An unterminated literal is a line continued into the next one; its text
    // is code and is read as such.
    kept.push_str(&literal);
    kept
}

/// The identifier tokens of `text`, each one whole: `owner_state` is one
/// token and does not contain `owner`.
fn identifiers(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current = String::new();

    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            current.push(c);
        } else if !current.is_empty() {
            found.push(std::mem::take(&mut current));
        }
    }

    if !current.is_empty() {
        found.push(current);
    }

    found
}

/// True where `path`, relative to `src/`, lies in the subtree `prefix` names.
fn under(path: &Path, prefix: &str) -> bool {
    let text = path.to_string_lossy().replace('\\', "/");
    text == format!("{prefix}.rs") || text.starts_with(&format!("{prefix}/"))
}

/// The retired identifiers of `text`, as (line number, identifier, ratified
/// name). `path` is relative to `src/` and selects the subtree-scoped half of
/// the mapping.
fn retired_in(path: &Path, text: &str) -> Vec<(usize, String, &'static str)> {
    let mut found = Vec::new();

    for (number, line) in text.lines().enumerate() {
        for token in identifiers(&code_of(line)) {
            let hit = RETIRED
                .iter()
                .find(|(retired, _, scope)| {
                    *retired == token
                        && match scope {
                            Where::Anywhere => true,
                            Where::Under(prefix) => under(path, prefix),
                            Where::Except(owners) => {
                                !owners.iter().any(|prefix| under(path, prefix))
                            }
                        }
                })
                .map(|(_, ratified, _)| *ratified);

            if let Some(ratified) = hit {
                found.push((number + 1, token, ratified));
            }
        }
    }

    found
}

/// Every source under `src/`, paired with its path relative to `src/`, and
/// the retired identifiers it still holds.
fn offences() -> Vec<(String, Vec<(usize, String, &'static str)>)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(root.as_path(), &mut files);
    assert!(files.len() > 50, "the source walk found almost nothing");

    let mut found = Vec::new();
    for path in &files {
        let relative = path.strip_prefix(&root).expect("a path under src/");
        if exempt_file(relative) {
            continue;
        }

        let text = fs::read_to_string(path).expect("a source file is readable");
        let name = relative.to_string_lossy().replace('\\', "/");
        found.push((name, retired_in(relative, &text)));
    }

    found
}

#[test]
#[cfg_attr(
    miri,
    ignore = "reads the crate's sources; `opendir` is unavailable under Miri's isolation, \
              and the abort takes the whole slice with it"
)]
fn a_migrated_source_keeps_no_retired_name() {
    let mut kept = Vec::new();
    for (name, found) in offences() {
        if STILL_TO_MIGRATE.contains(&name.as_str()) {
            continue;
        }

        for (number, token, ratified) in found {
            kept.push(format!(
                "{name}:{number}: `{token}` is retired; the ratified name is `{ratified}`"
            ));
        }
    }

    assert!(
        kept.is_empty(),
        "{} identifiers the ratified mapping retires stand in files that have \
         already been migrated (`dev/CYCLE-TERMINOLOGY-AUDIT.md`):\n{}",
        kept.len(),
        kept.join("\n")
    );
}

/// A file that has stopped offending leaves the debt list in the same commit.
/// Left behind, its entry would exempt the file from every later run, and the
/// exemption would be invisible: the guard passes either way.
#[test]
#[cfg_attr(
    miri,
    ignore = "reads the crate's sources; `opendir` is unavailable under Miri's isolation, \
              and the abort takes the whole slice with it"
)]
fn the_debt_list_names_only_files_that_still_offend() {
    let found = offences();
    let mut stale = Vec::new();

    for name in STILL_TO_MIGRATE {
        match found.iter().find(|(path, _)| path == name) {
            None => stale.push(format!("{name}: no such source")),
            Some((_, retired)) if retired.is_empty() => {
                stale.push(format!("{name}: migrated, and still on the list"))
            }
            Some(_) => {}
        }
    }

    assert!(
        stale.is_empty(),
        "the debt list of `PLAN.md` S41 is out of date:\n{}",
        stale.join("\n")
    );
}

/// The guard has to see an offence, or it passes by finding nothing anywhere.
#[test]
fn the_guard_sees_a_retired_name_in_code() {
    let source = "\
fn append(state: *mut OwnerCycleState, entity: *mut RcHeader) -> bool {
    if unsafe { (*state).escrowed.get() } == ESCROW_ENTRIES {
        return false;
    }

    unsafe { enrol(entity) }
}
";
    let found = retired_in(Path::new("cycle/queue.rs"), source);
    let names: Vec<&str> = found.iter().map(|(_, token, _)| token.as_str()).collect();
    assert_eq!(names, ["escrowed", "ESCROW_ENTRIES", "enrol"], "{found:?}");
    assert_eq!(found[0].0, 2);
    assert_eq!(found[2].2, "register_candidate");
}

/// A ratified name that carries a retired one as a prefix is not an offence:
/// the unit is the whole identifier.
#[test]
fn the_guard_reads_whole_identifiers() {
    let source = "\
fn append(state: *mut OwnerCycleState) {
    let owner_state = unsafe { (*state).owner_state_ref() };
    owner_state.overflow_len.set(0);
    let _ = ROW_COUNT;
}
";
    let found = retired_in(Path::new("cycle/queue.rs"), source);
    assert!(found.is_empty(), "{found:?}");
}

/// Ordinary English keeps its word outside the subtree that retired it:
/// `Vec::drain` is not the queue's segment release, and `top` is the stack's
/// field and nobody else's.
#[test]
fn the_guard_scopes_ordinary_english_to_its_module() {
    let source = "    let taken: Vec<u8> = buffer.drain(..).collect();\n";
    assert!(
        retired_in(Path::new("memory/arena.rs"), source).is_empty(),
        "drain is Vec::drain outside the queue"
    );
    let found = retired_in(Path::new("cycle/queue.rs"), source);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].2, "release_queue_segments");
}

/// A comment is prose and belongs to the S41.6 pass; a message inside code is
/// an assertion over state and is renamed with the state.
#[test]
fn the_guard_reads_code_and_leaves_prose_alone() {
    let source = "\
    // The escrow holds what no segment could take.
    let held = spares_held(); // and the spares it did not need
    assert_eq!(held, 0, \"the entry is escrowed, not lost\");
";
    let found = retired_in(Path::new("cycle/queue.rs"), source);
    let names: Vec<&str> = found.iter().map(|(_, token, _)| token.as_str()).collect();
    assert_eq!(
        names,
        ["held", "spares_held", "held", "escrowed"],
        "{found:?}"
    );
}

/// A literal that names a document is a citation, and a citation keeps the
/// heading it quotes. A literal that names no document is a message.
#[test]
fn the_guard_spares_a_document_citation() {
    let source = "\
    let cited = \"rfc/model/gc/rc-cycle.md, Death while enrolled\";
    let message = \"death while enrolled\";
";
    let found = retired_in(Path::new("cycle/queue.rs"), source);
    assert!(found.is_empty(), "neither line names a retired identifier");

    let offending = "    let m = \"the entry is escrowed\";\n";
    let found = retired_in(Path::new("cycle/queue.rs"), offending);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].1, "escrowed");
}
