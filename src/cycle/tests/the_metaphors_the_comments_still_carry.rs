//! Every metaphor left in a comment is a citation of a document heading or
//! an exemption with a reason, and this reads the comments to say so.
//!
//! The third surface of `PLAN.md` S41. The first guard reads identifiers with
//! comments cut; the second reads file and declaration names; prose was left
//! to a reading, and a reading is what let `RowLookup` stand for a colour in
//! the scan's own doc through two Critic rounds. This one measures it.
//!
//! **A quoted span is a citation and is spared.** Active prose that cites a
//! superseded heading keeps the heading exactly and adds the current name
//! outside the quotation (`dev/CYCLE-TERMINOLOGY-AUDIT.md`, "Documentation
//! boundary"), so `"an enrolment cannot fail"` stands while the sentence
//! around it says *registration*.

use std::fs;
use std::path::Path;

use super::the_metaphors_the_names_still_carry::{METAPHORS, sources};

/// A subtree, a metaphor it keeps, and why it keeps it.
///
/// The reason is the point: an exemption without one is indistinguishable
/// from an oversight. `path` is matched as a prefix of the path relative to
/// `src/`, so a module and its tests take one entry.
const EXEMPT: [(&str, &str, &str); 12] = [
    (
        "memory/reset_window",
        "escrow",
        "`ResetWindow::escrow` names deferred count corrections, which the \
         glossary calls the *deferred increment* list since `rfc` `9ca669c`; \
         *overflow buffer*, the candidate queue's replacement, is false for \
         that sense and no step owns the rename \
         (`dev/CYCLE-TERMINOLOGY-AUDIT.md`, \"Glossary check\")",
    ),
    (
        "cycle",
        "trigger",
        "what triggered a collection, and an edge-triggered bit — the verb, \
         not the hash table's threshold",
    ),
    ("hash", "trigger", "the intended caller of the seed draw"),
    (
        "memory",
        "trigger",
        "what triggered an arena rotation or an adoption",
    ),
    ("promote", "trigger", "a fixture class named `Trigger`"),
    ("refcount", "trigger", "edge-triggered, the verb"),
    (
        "promote",
        "escrow",
        "the same window's, read from the reset's side",
    ),
    (
        "memory/reset_window",
        "park",
        "the reset's own window over its own frees, a different mechanism from \
         the trace window; the glossary calls this one a *deferred free* and \
         no step owns the rename (`PLAN.md`, Fog)",
    ),
    (
        "memory/reset_window",
        "corpse",
        "the same window: `CORPSE_WALKS` is its own state, and the glossary's \
         word for it is a *torn-down entity*",
    ),
    (
        "promote",
        "corpse",
        "a survivor whose refcount reached zero inside the reset, which is \
         `reset_window`'s vocabulary rather than the collector's",
    ),
    (
        "cycle/arena",
        "enrol",
        "attaching a block to the sweep list, which the glossary calls an \
         *attachment to the touched list* since `rfc` `9ca669c`; *candidate \
         registration* names a different operation, and no step owns the \
         rename (`dev/CYCLE-TERMINOLOGY-AUDIT.md`, \"Glossary check\")",
    ),
    (
        "memory/heap",
        "enrol",
        "the sweep-list sense again, in the contract the arena's prologue \
         reads",
    ),
];

/// This file and the two guards beside it, which write every metaphor on
/// purpose.
const SELF: [&str; 3] = [
    "cycle/tests/the_metaphors_the_comments_still_carry.rs",
    "cycle/tests/the_metaphors_the_names_still_carry.rs",
    "cycle/tests/the_words_the_crate_retired.rs",
];

/// The comment text of `line`, or `None` where the line is code.
///
/// The `//` that opens a comment is found outside string literals, so a path
/// written inside a message is not read as one. This is the complement of the
/// sibling guard's `code_of`: between them they read every byte of a line
/// once.
fn comment_of(line: &str) -> Option<&str> {
    let bytes: Vec<char> = line.chars().collect();
    let mut in_string = false;
    let mut index = 0;

    while index < bytes.len() {
        let c = bytes[index];
        if in_string {
            if c == '\\' {
                index += 2;
                continue;
            }

            if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
        } else if c == '/' && index + 1 < bytes.len() && bytes[index + 1] == '/' {
            let at: usize = line
                .char_indices()
                .nth(index)
                .map(|(offset, _)| offset)
                .expect("the index came from this line");
            return Some(&line[at..]);
        }

        index += 1;
    }

    None
}

/// `text` with every double-quoted span removed, and the quote state the
/// caller carries to the next line.
///
/// A quoted span in a comment is a citation of a heading, and a heading is
/// quoted exactly however the crate has since renamed what it names. **A
/// citation wraps across lines**, and this is where the first draft of this
/// guard was wrong: reading each line from a closed quote made the second half
/// of `"Death while enrolled"` read as prose, so the guard demanded a rename
/// of the one text the audit says to keep exactly.
fn without_citations(text: &str, mut quoted: bool) -> (String, bool) {
    let mut kept = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '"' {
            quoted = !quoted;
            continue;
        }

        if !quoted {
            kept.push(c);
        }
    }

    (kept, quoted)
}

/// Whether `path`, relative to `src/`, keeps `metaphor` by exemption.
fn exempt(path: &str, metaphor: &str) -> bool {
    EXEMPT
        .iter()
        .any(|(prefix, word, _)| *word == metaphor && path.starts_with(prefix))
}

/// Every metaphor a comment of `text` carries outside a citation, as
/// (line number, metaphor, the comment).
fn metaphors_in(path: &str, text: &str) -> Vec<(usize, &'static str, String)> {
    let mut found = Vec::new();

    let mut quoted = false;
    let mut opened_at = 0;
    for (number, line) in text.lines().enumerate() {
        let Some(comment) = comment_of(line) else {
            // A line of code ends whatever a comment above it left open: a
            // citation does not span the code between two comment blocks. An
            // odd quote is reported rather than swallowed — carrying the state
            // to the end of a block is what makes the rest of that block
            // invisible, so the state has to be balanced when the block ends.
            if quoted {
                found.push((opened_at, "an unbalanced quote", String::new()));
            }

            quoted = false;
            continue;
        };

        let (prose, open) = without_citations(comment, quoted);
        if open && !quoted {
            opened_at = number + 1;
        }

        quoted = open;
        let prose = prose.to_ascii_lowercase();
        for metaphor in METAPHORS {
            if prose.contains(metaphor) && !exempt(path, metaphor) {
                found.push((number + 1, metaphor, comment.trim().to_owned()));
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
fn no_comment_carries_a_metaphor_outside_a_citation() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(root.as_path(), &mut files);
    assert!(files.len() > 50, "the source walk found almost nothing");

    let mut kept = Vec::new();
    for path in &files {
        let relative = path.strip_prefix(&root).expect("a path under src/");
        let name = relative.to_string_lossy().replace('\\', "/");
        if SELF.contains(&name.as_str()) {
            continue;
        }

        let text = fs::read_to_string(path).expect("a source file is readable");
        for (number, metaphor, comment) in metaphors_in(&name, &text) {
            kept.push(format!("{name}:{number}: `{metaphor}` in: {comment}"));
        }

        let text_ends_open = metaphors_in(&name, &format!("{text}\nfn end() {{}}\n"));
        for (number, metaphor, _) in text_ends_open {
            if metaphor == "an unbalanced quote" {
                kept.push(format!(
                    "{name}:{number}: a comment block opens a citation and never closes it"
                ));
            }
        }
    }

    assert!(
        kept.is_empty(),
        "{} comments carry a metaphor the audit retires, outside a citation \
         and outside the exemptions above (`dev/CYCLE-TERMINOLOGY-AUDIT.md`, \
         \"Comment rewrite\"):\n{}",
        kept.len(),
        kept.join("\n")
    );
}

/// A citation keeps its heading; the sentence around it does not.
#[test]
fn the_guard_spares_a_quoted_heading_and_reads_the_sentence_around_it() {
    let source = "\
/// The report is the poll's (`dev/DECISIONS.md`, \"an enrolment cannot fail\").
/// The enrolment is the release path's.
";
    let found = metaphors_in("cycle/queue.rs", source);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, 2);
    assert_eq!(found[0].1, "enrol");
}

/// A citation that wraps is one citation, and the half on the second line is
/// as exact as the half on the first.
#[test]
fn the_guard_spares_a_heading_that_wraps_across_two_lines() {
    let source = "\
/// The slot is withheld (`rfc/model/gc/rc-cycle.md`, \"Death while
/// enrolled\"). The registration is the release path's.
";
    assert!(
        metaphors_in("cycle/queue.rs", source).is_empty(),
        "the second line is the same citation"
    );

    let after_code = "\
/// (`rfc/model/gc/rc-cycle.md`, \"Death while
fn f() {}
/// enrolled is what this says
";
    let found = metaphors_in("cycle/queue.rs", after_code);
    let reported: Vec<&str> = found.iter().map(|(_, what, _)| *what).collect();
    assert_eq!(
        reported,
        ["an unbalanced quote", "enrol"],
        "the open citation is reported where it opened, and the code below it \
         closes it so the prose after is read"
    );
    assert_eq!(found[0].0, 1);
    assert_eq!(found[1].0, 3);
}

/// The crate writes no block comments, which is what lets [`comment_of`] read
/// `//` alone.
///
/// Stated as a test rather than as a limitation: a `/* */` that appeared later
/// would be prose no guard of this stage reads, and the reader of that first
/// block comment is the one who has to know.
#[test]
#[cfg_attr(
    miri,
    ignore = "reads the crate's sources; `opendir` is unavailable under Miri's isolation, \
              and the abort takes the whole slice with it"
)]
fn the_crate_writes_no_block_comments() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(root.as_path(), &mut files);

    let mut kept = Vec::new();
    for path in &files {
        let name = path
            .strip_prefix(&root)
            .expect("a path under src/")
            .to_string_lossy()
            .replace('\\', "/");
        // This file writes the opener as a literal, to name it.
        if name == SELF[0] {
            continue;
        }

        let text = fs::read_to_string(path).expect("a source file is readable");
        for (number, line) in text.lines().enumerate() {
            // Before the `//` as well as instead of it: `let x = 1; /* a */ //
            // why` carries both, and only the second is read anywhere.
            let opens_at = line.find("/*");
            let comment_at = comment_of(line).map(|c| line.len() - c.len());
            let outside = match (opens_at, comment_at) {
                (Some(open), Some(comment)) => open < comment,
                (Some(_), None) => true,
                (None, _) => false,
            };
            if outside {
                kept.push(format!("{name}:{}", number + 1));
            }
        }
    }

    assert!(
        kept.is_empty(),
        "a block comment stands where this guard reads only `//`:\n{}",
        kept.join("\n")
    );
}

/// Code is not prose: a metaphor inside a string literal is the sibling
/// guard's, which reads literals and would rename one with the state it
/// asserts over.
#[test]
fn the_guard_reads_comments_and_leaves_code_alone() {
    let source = "    assert_eq!(count, 0, \"the escrow is empty\"); // and the buffer\n";
    assert!(metaphors_in("cycle/queue.rs", source).is_empty());

    let commented = "    let x = 1; // the escrow holds it\n";
    let found = metaphors_in("cycle/queue.rs", commented);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].1, "escrow");
}
