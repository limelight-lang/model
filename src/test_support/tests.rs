//! A test that reads a file or spawns a process carries
//! `#[cfg_attr(miri, ignore = "…")]`, and this reads the crate's sources to
//! say so.
//!
//! Miri stops at the first error, so such a test does not skip itself under
//! the interpreter — it ends the run, and every test after it in the slice is
//! never executed while the report names the subject rather than the harness.
//! The convention has been broken twice, `cycle::` on 2026-09-01 and
//! `memory::` on 2026-09-02, and each time a whole slice went unrun for days
//! while `cargo test` stayed green (`dev/POSTMORTEM.md`, 2026-09-02;
//! `dev/WORKFLOW.md`, "Tests").
//!
//! **How a function's extent is decided, the crate having no parser.** Every
//! `#[test]` in this crate stands at column zero, so rustfmt closes its
//! function with a `}` at column zero and the lines between are the body. The
//! premise is asserted rather than assumed: a `#[test]` indented inside a
//! `mod` fails this test instead of being skipped by it, which is the failure
//! that can be read.
//!
//! **What the walk reaches.** A call in the test's own body, and a call in any
//! function of the same file the test reaches through other functions of that
//! file. That covers the shape the guard files have — one `sources` or
//! `source` helper beside the tests that call it — and not a read two modules
//! away, which no reading of one file can see. A name inside a string literal
//! counts as a call: the crate has no such literal today, and the cost of one
//! appearing is a test asked for an attribute it does not need.

use std::fs;
use std::path::{Path, PathBuf};

/// The calls whose test Miri's isolation refuses: the four file readings this
/// crate uses, `fs::read` beside them, and the two halves of running a child
/// process.
const REACHES_OUTSIDE: [&str; 7] = [
    "read_dir",
    "read_to_string",
    "File::open",
    "fs::read(",
    "current_exe",
    "Command::new",
    "process::Command",
];

/// What an ignored test's attributes carry, in both the one-line spelling and
/// the wrapped one rustfmt produces for a long reason.
const IGNORED_UNDER_MIRI: [&str; 3] = ["cfg_attr(", "miri", "ignore"];

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

/// One function declared at the top level of a file.
struct Function {
    name: String,
    /// The attribute and doc lines directly above the declaration, up to the
    /// nearest blank line or the end of the item before it.
    attributes: String,
    /// The declaration line and everything under it to the closing brace at
    /// column zero.
    body: String,
}

/// The words that may stand before `fn` in a declaration at column zero.
/// Anything else there — `type`, a field, a `where` clause — is not one.
fn is_declaration_word(word: &str) -> bool {
    matches!(
        word,
        "pub" | "unsafe" | "const" | "async" | "extern" | "\"C\""
    ) || word.starts_with("pub(")
}

/// The name of the function `line` declares, or **None** where it declares
/// none. Indented lines answer **None**: what this file reads is the column-
/// zero population, and an `impl` body's methods are outside it.
fn declared_name(line: &str) -> Option<&str> {
    if line.starts_with(' ') || line.starts_with('\t') {
        return None;
    }

    let start = line.find("fn ")?;
    if !line[..start].split_whitespace().all(is_declaration_word) {
        return None;
    }

    let tail = &line[start + "fn ".len()..];
    let end = tail
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(tail.len());
    (end > 0).then(|| &tail[..end])
}

/// `text` with every line comment cut, so a comment naming one of the calls
/// above does not read as the call. A doc comment is in a function's
/// attributes rather than in its body and is cut by the same rule.
fn code_of(text: &str) -> String {
    text.lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<&str>>()
        .join("\n")
}

/// Whether `code` calls `name`, by the name standing before a `(` with no
/// identifier character before it — so `source(` is not found inside
/// `my_source(`.
fn calls(code: &str, name: &str) -> bool {
    let mut rest = code;
    while let Some(at) = rest.find(name) {
        let before = rest[..at].chars().next_back();
        let after = rest[at + name.len()..].trim_start();
        let starts_a_word = before.is_some_and(|c| c.is_alphanumeric() || c == '_');
        if !starts_a_word && after.starts_with('(') {
            return true;
        }

        rest = &rest[at + name.len()..];
    }

    false
}

/// The top-level functions of one source file.
fn functions(text: &str) -> Vec<Function> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        let Some(name) = declared_name(line) else {
            continue;
        };

        let mut first = index;
        while first > 0 && !lines[first - 1].trim().is_empty() && lines[first - 1] != "}" {
            first -= 1;
        }

        let mut last = index + 1;
        while last < lines.len() && lines[last] != "}" {
            last += 1;
        }

        found.push(Function {
            name: name.to_string(),
            attributes: lines[first..index].join("\n"),
            body: code_of(&lines[index..last].join("\n")),
        });
    }

    found
}

/// The names of the functions in `file` that reach outside the process, by
/// their own calls or through one another.
fn reach_outside(file: &[Function]) -> Vec<&str> {
    let mut reaching: Vec<&str> = file
        .iter()
        .filter(|f| REACHES_OUTSIDE.iter().any(|call| f.body.contains(call)))
        .map(|f| f.name.as_str())
        .collect();

    // To a fixpoint: a test calls a helper that calls the helper that reads,
    // which is one hop more than the guard files happen to need today.
    let mut grew = true;
    while grew {
        grew = false;
        for function in file {
            if reaching.contains(&function.name.as_str()) {
                continue;
            }

            if reaching.iter().any(|name| calls(&function.body, name)) {
                reaching.push(function.name.as_str());
                grew = true;
            }
        }
    }

    reaching
}

#[test]
#[cfg_attr(
    miri,
    ignore = "reads the crate's sources; `opendir` is unavailable under Miri's isolation, \
              and the abort takes the whole slice with it"
)]
fn a_test_that_reaches_outside_the_process_is_ignored_under_miri() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(root.as_path(), &mut files);
    assert!(files.len() > 50, "the source walk found almost nothing");

    let mut tests = 0;
    let mut reaching = 0;
    let mut unguarded = Vec::new();

    for path in &files {
        let text = fs::read_to_string(path).expect("a source file is readable");
        let named = path
            .strip_prefix(&root)
            .expect("every path came out of the walk over src/")
            .to_string_lossy()
            .replace('\\', "/");

        for (number, line) in text.lines().enumerate() {
            if line.trim() == "#[test]" && line != "#[test]" {
                panic!(
                    "{named}:{} is an indented `#[test]`, which this walk cannot \
                     read the extent of",
                    number + 1
                );
            }
        }

        let file = functions(&text);
        let outside = reach_outside(&file);
        for function in &file {
            if !function.attributes.contains("#[test]") {
                continue;
            }

            tests += 1;
            if !outside.contains(&function.name.as_str()) {
                continue;
            }

            reaching += 1;
            let ignored = IGNORED_UNDER_MIRI
                .iter()
                .all(|part| function.attributes.contains(part));
            if !ignored {
                unguarded.push(format!("{named}::{}", function.name));
            }
        }
    }

    // The population is what says the walk still reads what it used to: a
    // parse that matched nothing would satisfy the emptiness below without it.
    assert!(tests > 500, "the walk found {tests} tests");
    assert!(
        reaching >= 10,
        "the walk found {reaching} tests reaching outside the process"
    );
    assert!(
        unguarded.is_empty(),
        "these tests read a file or spawn a process and carry no \
         `#[cfg_attr(miri, ignore = \"…\")]`, so Miri stops the whole slice at \
         the first of them (`dev/WORKFLOW.md`, \"Tests\"): {unguarded:?}"
    );
}
