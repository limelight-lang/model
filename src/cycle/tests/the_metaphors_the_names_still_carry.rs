//! No file name and no item name in this crate still reads as one of the
//! metaphors `dev/CYCLE-TERMINOLOGY-AUDIT.md` retires.
//!
//! The sibling guard, `the_words_the_crate_retired`, reads whole identifiers:
//! `condemned_from` is not `Condemned` and `what_the_corpse_rule_drops.rs` is
//! not a token at all, so both stand past it. This one reads the other axis —
//! a metaphor as a **substring**, without case — over the two surfaces the
//! sibling cannot express: the name a source file carries, and the name a
//! declaration carries.
//!
//! **A comment is not read here either.** Prose is `PLAN.md` S41.6's, which
//! classifies every survivor as a citation, unrelated English or a defect.

use std::fs;
use std::path::{Path, PathBuf};

/// The metaphors, lowercase, matched as substrings of a lowercased name.
///
/// `judg` rather than `judge`, so that `judging` and the US `judgment` are
/// read too; the others inflect without losing their stem.
///
/// The last three are the hash table's collision defense, retired by S41.8.
/// `trigger` costs exemptions — it is ordinary English in `cycle`, `hash`,
/// `memory`, `promote` and `refcount` — and it is here anyway: the audit
/// retires it as the name of a threshold, four in five of its `src/array/`
/// occurrences were prose, and prose is the surface only this guard and its
/// comment sibling read.
///
/// Only words the audit retires *as metaphors* are here. The ordinary-English
/// half of the mapping — `live`, `held`, `drain`, `top` — is the sibling
/// guard's, scoped by subtree, because a substring of those is English far
/// more often than it is a leftover.
///
/// `door` is the glossary's context-sensitive word, retired site by site
/// rather than by one ratified name: the replacement depends on the
/// operation, so no single name stands beside it in the sibling guard's
/// table (`dev/CYCLE-TERMINOLOGY-AUDIT.md`, "Glossary check").
pub(super) const METAPHORS: [&str; 14] = [
    "condemn", "acquit", "corpse", "judg", "park", "escrow", "floor", "climb", "enrol", "discount",
    "ladder", "rung", "trigger", "door",
];

/// A subtree, a name in it that carries a metaphor and keeps it, and the
/// reason.
///
/// The reason is the point: an exemption without one is indistinguishable
/// from an oversight, and this list is read by whoever finds the next
/// offence. An empty subtree means the whole crate.
const EXEMPT: [(&str, &str, &str); 8] = [
    (
        "",
        "the_arena_keeps_a_ratified_name_of_its_own_for_enrol",
        "names the token it is about, in the sibling guard's own test",
    ),
    (
        "memory/reset_window",
        "CORPSE_WALKS",
        "`memory::reset_window`'s own vocabulary. The glossary names the \
         window's words since `rfc` `9ca669c` — a corpse of this window is a \
         *torn-down entity* — and no step owns the rename yet",
    ),
    (
        "memory/reset_window",
        "park_large",
        "`memory::reset_window`'s, as `CORPSE_WALKS` above",
    ),
    (
        "promote",
        "corpse",
        "the reset's own vocabulary, and the glossary's *torn-down entity*: a \
         survivor whose refcount reached zero inside the reset is not the \
         collector's zero-count member, and no step owns the rename",
    ),
    (
        "promote",
        "corpse_cls",
        "the fixture class of those bindings",
    ),
    (
        "promote",
        "trigger",
        "a fixture class named `Trigger` and the bindings that hold it, which \
         are a test's own furniture rather than the hash table's threshold",
    ),
    ("promote", "trigger_cls", "the same fixture"),
    (
        "promote",
        "corpse_walks",
        "the binding that reads `memory::reset_window`'s `CORPSE_WALKS`, and \
         it takes that constant's name",
    ),
];

/// This file, which writes every metaphor on purpose.
const SELF: &str = "cycle/tests/the_metaphors_the_names_still_carry.rs";

/// The declaration keywords whose name this guard reads.
///
/// `let` is here with the items: a binding is a name a reader meets as often
/// as a function's, and `let corpse` under a comment that says *zero-count
/// member* is the same defect one level down.
const DECLARATIONS: [&str; 9] = [
    "fn ", "struct ", "enum ", "const ", "static ", "mod ", "type ", "trait ", "let ",
];

pub(super) fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("src/ is readable") {
        let path = entry.expect("a readable directory entry").path();
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
}

/// The metaphor `name` carries, if it carries one and is not exempt where it
/// stands. `path` is relative to `src/`.
fn metaphor_in(path: &str, name: &str) -> Option<&'static str> {
    let lowered = name.to_ascii_lowercase();
    if EXEMPT
        .iter()
        .any(|(prefix, exempt, _)| *exempt == name && path.starts_with(prefix))
    {
        return None;
    }

    METAPHORS
        .into_iter()
        .find(|metaphor| lowered.contains(metaphor))
}

/// The item names `text` declares, as (line number, name).
///
/// A declaration is found by its keyword, so a name in a call or a type
/// position is not read: this guard is about what the crate *declares*, and a
/// caller of a name declared elsewhere is the sibling guard's business.
///
/// **What a keyword cannot find is not read**: a closure parameter, a `match`
/// binding, a struct field and a function parameter carry no keyword of their
/// own. Two closure parameters named `parked` stood in
/// `cycle::deferred_slot_reuse` past this guard until a reading found them,
/// which is the size of the hole.
fn declared_in(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();

    for (number, line) in text.lines().enumerate() {
        let code = line.split("//").next().unwrap_or(line);
        for keyword in DECLARATIONS {
            let mut rest = code;
            while let Some(at) = rest.find(keyword) {
                let before = rest[..at].chars().last();
                rest = &rest[at + keyword.len()..];
                if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                    continue;
                }

                // `let mut x` names `x`: the binding mode is not a name.
                let rest = rest.strip_prefix("mut ").unwrap_or(rest);
                // `let (a, b) = …` names both, and a tuple of results is how a
                // test carries two answers out of a thread.
                let names: Vec<String> = if let Some(tuple) = rest.strip_prefix('(') {
                    tuple
                        .split([',', ')'])
                        .map(|part| {
                            let part = part.trim();
                            part.strip_prefix("mut ")
                                .unwrap_or(part)
                                .chars()
                                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                                .collect()
                        })
                        .collect()
                } else {
                    vec![
                        rest.chars()
                            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                            .collect(),
                    ]
                };

                for name in names {
                    // `let _ = …` names nothing, and neither does `let _x` as
                    // far as a reader is concerned.
                    if !name.is_empty() && !name.starts_with('_') {
                        found.push((number + 1, name));
                    }
                }
            }
        }
    }

    found
}

#[test]
#[cfg_attr(
    miri,
    ignore = "reads the crate's sources; `opendir` is unavailable under Miri's isolation, \
              and the abort takes the whole slice with it"
)]
fn no_source_file_is_named_after_a_metaphor() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(root.as_path(), &mut files);
    assert!(files.len() > 50, "the source walk found almost nothing");

    let mut kept = Vec::new();
    for path in &files {
        let relative = path.strip_prefix(&root).expect("a path under src/");
        let name = relative.to_string_lossy().replace('\\', "/");
        if name == SELF {
            continue;
        }

        if let Some(metaphor) = metaphor_in(&name, &name) {
            kept.push(format!("{name}: `{metaphor}`"));
        }
    }

    assert!(
        kept.is_empty(),
        "{} source files are named after a metaphor the audit retires \
         (`dev/CYCLE-TERMINOLOGY-AUDIT.md`):\n{}",
        kept.len(),
        kept.join("\n")
    );
}

#[test]
#[cfg_attr(
    miri,
    ignore = "reads the crate's sources; `opendir` is unavailable under Miri's isolation, \
              and the abort takes the whole slice with it"
)]
fn no_declaration_is_named_after_a_metaphor() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(root.as_path(), &mut files);

    let mut kept = Vec::new();
    for path in &files {
        let relative = path.strip_prefix(&root).expect("a path under src/");
        let name = relative.to_string_lossy().replace('\\', "/");
        if name == SELF {
            continue;
        }

        let text = fs::read_to_string(path).expect("a source file is readable");
        for (number, declared) in declared_in(&text) {
            if let Some(metaphor) = metaphor_in(&name, &declared) {
                kept.push(format!(
                    "{name}:{number}: `{declared}` carries `{metaphor}`"
                ));
            }
        }
    }

    assert!(
        kept.is_empty(),
        "{} declarations are named after a metaphor the audit retires \
         (`dev/CYCLE-TERMINOLOGY-AUDIT.md`):\n{}",
        kept.len(),
        kept.join("\n")
    );
}

/// The guard has to see an offence, or it passes by finding nothing anywhere.
#[test]
fn the_guard_reads_a_metaphor_without_its_case_or_its_boundary() {
    let path = "cycle/queue.rs";
    assert_eq!(
        metaphor_in(path, "what_the_corpse_rule_drops.rs"),
        Some("corpse")
    );
    assert_eq!(metaphor_in(path, "condemned_from"), Some("condemn"));
    assert_eq!(metaphor_in(path, "TraceWindowEscrow"), Some("escrow"));
    assert_eq!(metaphor_in(path, "register_candidate"), None);
    assert_eq!(
        metaphor_in("promote/tests/a.rs", "corpse"),
        None,
        "the reset's own vocabulary, where the exemption stands"
    );
    assert_eq!(
        metaphor_in(path, "corpse"),
        Some("corpse"),
        "and nowhere else"
    );
}

/// A declaration is read by its keyword, and a keyword inside a longer word
/// declares nothing.
#[test]
fn the_guard_reads_declarations_and_not_calls() {
    let source = "\
struct Escrow;
fn park_it() {}
    let _ = self.judge(members);
const FLOOR_BYTES: usize = 8;
";
    let found = declared_in(source);
    let names: Vec<&str> = found.iter().map(|(_, name)| name.as_str()).collect();
    assert_eq!(names, ["Escrow", "park_it", "FLOOR_BYTES"], "{found:?}");

    let bindings = declared_in(
        "    let mut corpse = 0;\n let judged = 1;\n let (started, floorless) = f();\n",
    );
    let names: Vec<&str> = bindings.iter().map(|(_, name)| name.as_str()).collect();
    assert_eq!(
        names,
        ["corpse", "judged", "started", "floorless"],
        "{bindings:?}"
    );
}
