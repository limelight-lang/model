//! A published header is read through the helpers of this module and
//! nowhere else, because a collector writes a byte of that same word from a
//! thread that did not publish it: a plain field read beside that store is a
//! data race, and one that misbehaves nowhere — the byte read back lies
//! outside every field a caller tests. Behaviour is the same with the plain
//! read and with the helper, so no `cargo test` run separates them and this
//! reads the sources instead; ThreadSanitizer reports the race itself
//! (`dev/WORKFLOW.md`, "ThreadSanitizer"), on the sites a running test
//! reaches.
//!
//! It is cheap to evade: a rename, a read through a local, or a pointer
//! typed `*mut RcHeader` rather than an entity struct all pass it. It is
//! aimed at inattention — four such reads stood in `object.rs` and
//! `array/entity.rs` until a review of 2026-08-15 found them, and each was
//! written by someone who knew the rule.
//!
//! **The rule has no exemption since 2026-08-26.** It used to spare the arm
//! of a `#[cfg]` pair belonging to the build with no concurrent collector,
//! and that build was deleted with `rc-trace`. One arm survives every such
//! pair now, the one that reads through the helpers, so a direct read is an
//! offence wherever it stands.

use std::fs;
use std::path::{Path, PathBuf};

/// What a direct read looks like: the `RcHeader` field of an entity struct,
/// reached past [`crate::refcount::header_flags`] and its neighbours.
const READS: [&str; 4] = [
    ".rc.flags",
    ".rc.refcount",
    ".rc.memory_category()",
    ".rc.lifetime_counted()",
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

/// The helpers themselves, and the tests, which build headers on the stack
/// and assert on them single-threaded.
fn exempt_file(path: &Path) -> bool {
    let text = path.to_string_lossy().replace('\\', "/");
    text.ends_with("/refcount.rs")
        || text.contains("/refcount/")
        || text.contains("/tests/")
        || text.ends_with("/tests.rs")
}

/// The lines of `text` that read a header directly, as (line number, line).
///
/// The brace counting the exemption needed went with it on 2026-08-26: every
/// direct read counts now, wherever it stands.
fn direct_reads(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();

    for (number, line) in text.lines().enumerate() {
        if READS.iter().any(|read| line.contains(read)) {
            found.push((number + 1, line.trim().to_string()));
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
fn a_header_is_read_through_the_helpers_and_nowhere_else() {
    let mut files = Vec::new();
    sources(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src").as_path(),
        &mut files,
    );
    assert!(files.len() > 50, "the source walk found almost nothing");

    let mut offences = Vec::new();
    for path in files.iter().filter(|p| !exempt_file(p)) {
        let text = fs::read_to_string(path).expect("a source file is readable");
        for (number, line) in direct_reads(&text) {
            offences.push(format!("{}:{number}: {line}", path.display()));
        }
    }

    assert!(
        offences.is_empty(),
        "a published header is read past the helpers of `refcount`, which \
         races the collector's byte store into the same word. Use \
         `header_flags`, `header_refcount` or `header_pair`; there is no \
         exemption:\n{}",
        offences.join("\n")
    );
}

/// The guard has to see an offence, or it is a test that passes by finding
/// nothing anywhere. This is the shape of `object_constructed`'s read as it
/// stood before 2026-08-15, beside the write that was exempt until the
/// exemption's build was deleted — both count now.
#[test]
fn the_guard_sees_a_direct_read_wherever_it_stands() {
    let source = "\
fn constructed(obj: *mut Object) -> bool {
    if unsafe { (*obj).rc.memory_category() } == MemoryCategory::RequestArena {
        return false;
    }

    unsafe {
        (*obj).rc.flags |= DESTRUCTOR_PENDING
    };

    true
}
";
    let found = direct_reads(source);
    assert_eq!(found.len(), 2, "found: {found:?}");
    assert_eq!(found[0].0, 2);
    assert!(found[0].1.contains("memory_category"));
    assert!(found[1].1.contains(".rc.flags"));
}
