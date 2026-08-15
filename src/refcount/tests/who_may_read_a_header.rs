//! Under `rc-walk` a published header is read through the helpers of this
//! module and nowhere else, because the collector writes a byte of that
//! same word from its own thread: a plain field read beside that store is a
//! data race, and one that misbehaves nowhere — the byte read back lies
//! outside every field a caller tests. Nothing at runtime can catch it, so
//! this reads the sources instead.
//!
//! What it costs to evade is the measure of what it defends: a rename, a
//! read through a local, or a pointer typed `*mut RcHeader` rather than an
//! entity struct all pass it. It is aimed at inattention — four such reads
//! stood in `object.rs` and `array/entity.rs` until a review of 2026-08-15
//! found them, and each was written by someone who knew the rule.
//!
//! The `rc-trace` arm of a `#[cfg]` pair is exempt and has to be: that build
//! has no concurrent collector, and its half of every dispatching helper
//! reads the field directly on purpose.

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

/// The attribute that opens a block belonging to the build with no
/// concurrent collector.
const RC_TRACE_ONLY: &str = "#[cfg(not(feature = \"rc-walk\"))]";

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

/// The lines of `text` that read a header directly, as (line number, line),
/// skipping any block introduced by [`RC_TRACE_ONLY`].
///
/// The block is found by brace counting from the attribute, which holds
/// because `rustfmt` governs this crate: the attribute sits on its own line
/// and the block opens on the next one or the one after.
fn direct_reads(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut skip_until_depth: Option<i32> = None;
    let mut depth: i32 = 0;
    let mut arming = false;

    for (number, line) in text.lines().enumerate() {
        let opens = line.matches('{').count() as i32;
        let closes = line.matches('}').count() as i32;

        if line.trim() == RC_TRACE_ONLY {
            arming = true;
        } else if arming && opens > 0 {
            skip_until_depth = Some(depth);
            arming = false;
        }

        let inside = skip_until_depth.is_some();
        if !inside && READS.iter().any(|read| line.contains(read)) {
            found.push((number + 1, line.trim().to_string()));
        }

        depth += opens - closes;
        if let Some(floor) = skip_until_depth
            && depth <= floor
        {
            skip_until_depth = None;
        }
    }

    found
}

#[test]
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
         races the collector's epoch-byte store under rc-walk. Use \
         `header_flags`, `header_refcount` or `header_pair`; if this is the \
         rc-trace arm of a `#[cfg]` pair, it belongs inside that block:\n{}",
        offences.join("\n")
    );
}

/// The guard has to see an offence, or it is a test that passes by finding
/// nothing anywhere. This is the shape of `object_constructed`'s read as it
/// stood before 2026-08-15, and of the rc-trace block that must not count.
#[test]
fn the_guard_reads_the_exemption_and_the_offence_apart() {
    let source = "\
fn constructed(obj: *mut Object) -> bool {
    if unsafe { (*obj).rc.memory_category() } == MemoryCategory::RequestArena {
        return false;
    }

    #[cfg(not(feature = \"rc-walk\"))]
    unsafe {
        (*obj).rc.flags |= DESTRUCTOR_PENDING
    };

    true
}
";
    let found = direct_reads(source);
    assert_eq!(found.len(), 1, "found: {found:?}");
    assert_eq!(found[0].0, 2);
    assert!(found[0].1.contains("memory_category"));
}
