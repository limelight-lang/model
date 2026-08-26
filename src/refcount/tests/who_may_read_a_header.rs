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
//! **Both spellings of a header count**: the `rc` field of an entity struct,
//! and the header behind a raw pointer of its own. The second was added on
//! 2026-08-26: twenty-four sites in `promote.rs`, `memory/heap.rs`,
//! `cells.rs` and one fixture spelled a header `(*p).flags` and walked
//! through the guard on that alone.
//!
//! It is still cheap to evade: a rename, or a reference binding taken first
//! (`let e = &mut *entity; e.flags`), passes it. That spelling is not
//! hypothetical — it stood in `memory/barrier.rs` until the same day, where
//! it was worse than a plain read, the `&mut` asserting uniqueness over a
//! word the collector writes; reading found it, and this test cannot. So the
//! guard is aimed at inattention — four such reads stood in `object.rs` and
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

/// What a direct read looks like, in both spellings a header has: the
/// `RcHeader` field of an entity struct, and a header reached through a
/// raw pointer of its own. Both go past
/// [`crate::refcount::mutator_flags`] and its neighbours.
///
/// The pointer spellings anchor on the closing parenthesis of `(*p)`, and
/// that is what keeps them off the other `flags` words in this crate — a
/// class descriptor's and an array table's. A descriptor's is read at its
/// offset by `Class::flags_of`, a table's through `&self`, so neither
/// spells a pointer deref of a field named `flags`.
const READS: [&str; 8] = [
    ".rc.flags",
    ".rc.refcount",
    ".rc.memory_category()",
    ".rc.lifetime_counted()",
    ").flags",
    ").refcount",
    ").memory_category()",
    ").lifetime_counted()",
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

/// The helpers themselves, and the tests.
///
/// **The test exemption is wider than it reads.** 187 header accesses stand
/// in 37 test files outside `refcount`'s own (counted 2026-08-26 by these
/// same patterns), and most are on entities a factory allocated and published
/// rather than on a header built in a local. So this
/// guard is silent about exactly the population a ThreadSanitizer run reaches
/// first, which is the instrument the module doc above defers to. Narrowing
/// the exemption means converting those sites, which is the same job as
/// taking `RcHeader`'s fields private — one open decision, not two.
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
         `mutator_flags`, `header_refcount` or `header_pair`; there is no \
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

/// The pointer spelling is the one every site converted on 2026-08-26
/// used, and a class descriptor's own `flags` word is what the guard must
/// not answer to:
/// `Class` carries a field of that name, is immortal, and is read through
/// a shared reference for that reason.
#[test]
fn the_guard_sees_the_pointer_spelling_and_spares_a_descriptor() {
    let source = "fn count(child: *mut RcHeader, cls: *const Class) -> u32 {
    if unsafe { Class::flags_of(cls) } & CLASS_TEMPLATE != 0 {
        return 0;
    }

    let cow = unsafe { (*child).flags } & COW != 0;
    match unsafe { (*child).memory_category() } {
        MemoryCategory::RequestArena => unsafe { (*child).refcount += 1 },
        _ => {}
    }

    cow as u32
}
";
    let found = direct_reads(source);
    assert_eq!(found.len(), 3, "found: {found:?}");
    assert_eq!((found[0].0, found[1].0, found[2].0), (6, 7, 8));
    assert!(found[0].1.contains("(*child).flags"));
    assert!(found[1].1.contains("memory_category"));
    assert!(found[2].1.contains("(*child).refcount"));
}
