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
/// Only words the audit retires *as metaphors* are here. The ordinary-English
/// half of the mapping — `live`, `held`, `drain`, `top` — is the sibling
/// guard's, scoped by subtree, because a substring of those is English far
/// more often than it is a leftover.
///
/// `door` joins this list at `PLAN.md` S41.7, which is the step that
/// classifies its sites; before that classification the word is not yet a
/// defect wherever it stands.
const METAPHORS: [&str; 10] = [
    "condemn", "acquit", "corpse", "judge", "park", "escrow", "floor", "climb", "enrol", "discount",
];

/// A name that carries a metaphor and keeps it, with the reason.
///
/// The reason is the point: an exemption without one is indistinguishable
/// from an oversight, and this list is read by whoever finds the next
/// offence.
const EXEMPT: [(&str, &str); 4] = [
    (
        "the_arena_keeps_a_ratified_name_of_its_own_for_enrol",
        "names the token it is about, in the sibling guard's own test",
    ),
    (
        "CORPSE_WALKS",
        "`memory::reset_window`'s own vocabulary. The glossary names no          outcome for its window yet (`PLAN.md`, Fog), and a name invented          here would be a third one",
    ),
    (
        "park_large",
        "`memory::reset_window`'s, as `CORPSE_WALKS` above",
    ),
    (
        "a_copy_of_an_unsalted_table_is_unsalted_and_climbs_its_own_ladder",
        "the hash table's collision defence, which `PLAN.md` S41.8 renames",
    ),
];

/// This file, which writes every metaphor on purpose.
const SELF: &str = "cycle/tests/the_metaphors_the_names_still_carry.rs";

/// The declaration keywords whose name is an item name.
const DECLARATIONS: [&str; 8] = [
    "fn ", "struct ", "enum ", "const ", "static ", "mod ", "type ", "trait ",
];

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

/// The metaphor `name` carries, if it carries one and is not exempt.
fn metaphor_in(name: &str) -> Option<&'static str> {
    let lowered = name.to_ascii_lowercase();
    if EXEMPT.iter().any(|(exempt, _)| *exempt == name) {
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

                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    found.push((number + 1, name));
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

        if let Some(metaphor) = metaphor_in(&name) {
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
            if let Some(metaphor) = metaphor_in(&declared) {
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
    assert_eq!(metaphor_in("what_the_corpse_rule_drops.rs"), Some("corpse"));
    assert_eq!(metaphor_in("condemned_from"), Some("condemn"));
    assert_eq!(metaphor_in("TraceWindowEscrow"), Some("escrow"));
    assert_eq!(metaphor_in("register_candidate"), None);
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
}
