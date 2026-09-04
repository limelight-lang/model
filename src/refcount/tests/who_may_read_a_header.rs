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
//! **The field half belongs to the type since 2026-08-26.** `refcount` and
//! `flags` carry no visibility modifier, so outside this module a rename, a
//! local and a reference binding are all compile errors, and the four field
//! spellings below cannot be written at all. They stay in the list as the
//! tripwire against the privacy being put back, which costs four array
//! entries (`dev/DECISIONS.md`, "`RcHeader`'s fields go private, and the
//! source grep is re-aimed rather than retired").
//!
//! **Every spelling below is a compile error in the checked build**, since
//! `memory_category` and `lifetime_counted` were deleted the same day for
//! having no caller: a `&RcHeader` now reaches a type with private fields and
//! no methods, so the binding that formed it has nothing to do. What this test
//! is for is the two places the compiler does not stand. It fires when the
//! privacy is **reverted** — restoring `pub` breaks no build, nothing outside
//! this module naming either field any more, so the erosion would be silent
//! until the first new site. And it reads **configurations the checking build
//! does not compile**: a `#[cfg]`-disabled branch parses without resolving a
//! name, so the compiler passes `(*p).flags` there and this walk does not.
//! That evasion is in this guard's own history rather than a hypothesis.
//!
//! **The rule has no exemption but this module's own.** The `#[cfg]`-arm
//! exemption went with `rc-trace` on 2026-08-26, and the whole-test-tree one
//! went the same day, once the 187 accesses it had been sparing were
//! converted — they were the population a ThreadSanitizer run reaches first,
//! so sparing them left the fallback instrument covering nothing.
//!
//! **What neither the type nor the grep reaches**, and it is more than one
//! shape. A `&mut Object` formed over a published entity to touch its other
//! fields asserts uniqueness over the header bytes without spelling a header
//! access at all. `core::ptr::read::<RcHeader>(p)` names no field, needs no
//! privacy, and yields a plain eight-byte read spanning byte 6 — the instinct
//! that produced three of the wide reads repaired the same day. Both are
//! Miri's, ThreadSanitizer's and a reader's.

use std::fs;
use std::path::{Path, PathBuf};

/// What a direct read looks like, in both spellings a header has: the
/// `RcHeader` field of an entity struct, and a header reached through a
/// raw pointer of its own. Both go past
/// [`crate::refcount::mutator_flags`] and its neighbours.
///
/// All eight are refused by the compiler in the checked build. The four field
/// spellings stay against a revert of the privacy; the four method spellings
/// stay against `memory_category` and `lifetime_counted` being reintroduced
/// as `&self` from habit, which is the regression this crate has already
/// had once. If the capability is ever wanted again it comes back as a free
/// function over the flags word, the shape `is_object` and
/// `may_become_a_candidate` have.
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

/// This module and its own fixtures, which are where the helpers and the
/// stack-built headers live. Nothing else: the tests were exempt as a tree
/// until 2026-08-26 and are not any more, their 187 accesses having gone
/// through the helpers that day.
///
/// `path` is relative to `src/`, and that is load-bearing: matched against
/// the absolute path, a checkout under a directory named `refcount` would
/// exempt every file in the crate and the walk's `files.len() > 50` would
/// still pass.
fn exempt_file(path: &Path) -> bool {
    let text = path.to_string_lossy().replace('\\', "/");
    text == "refcount.rs" || text.starts_with("refcount/")
}

/// The lines of `text` that read a header directly, as (line number, line).
///
/// The brace counting the exemption needed went with it on 2026-08-26: every
/// direct read counts now, wherever it stands.
fn direct_reads(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();

    for (number, line) in text.lines().enumerate() {
        // A whole-line comment races nothing, and banning the spelling there
        // would ban the warning a reader most needs beside the code. A
        // trailing comment is not spared: its line carries code as well.
        if line.trim_start().starts_with("//") {
            continue;
        }

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
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(root.as_path(), &mut files);
    assert!(files.len() > 50, "the source walk found almost nothing");

    // Every path is validated by its place inside `src/`, never by the
    // directory the checkout sits in.
    let classified: Vec<&PathBuf> = files
        .iter()
        .filter(|p| !exempt_file(p.strip_prefix(&root).expect("a path under src/")))
        .collect();
    assert!(
        classified.len() > 40,
        "the exemption swallowed the crate: {} of {} files classified",
        classified.len(),
        files.len()
    );

    let mut offences = Vec::new();
    for path in classified {
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
/// not answer to: `Class` carries a field of that name, and it is read at
/// its offset by `Class::flags_of` rather than through a deref here.
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

/// One line per pattern, so a pattern dropped from [`READS`] goes red here.
/// The two fixtures above pin five of the eight between them, and the main
/// guard cannot pin any: it finds no offences in a green tree, so a shortened
/// list leaves it passing exactly as it passed before.
#[test]
fn the_guard_sees_every_spelling_in_the_list() {
    let source = "\
fn every_spelling(e: *mut Entity, p: *mut RcHeader) {
    let _ = e.rc.flags;
    let _ = e.rc.refcount;
    let _ = e.rc.memory_category();
    let _ = e.rc.lifetime_counted();
    let _ = (*p).flags;
    let _ = (*p).refcount;
    let _ = (*p).memory_category();
    let _ = (*p).lifetime_counted();
}
";
    // Both counts are literal on purpose. Against `READS.len()` the check is
    // vacuous: dropping a pattern drops the line that matched it, and the two
    // sides fall together — seen doing exactly that on 2026-08-26.
    assert_eq!(READS.len(), 8, "a pattern was added or dropped");
    let found = direct_reads(source);
    assert_eq!(
        found.len(),
        8,
        "one line per pattern, and each must match its own: {found:?}"
    );
}

/// Files that read a count as a number rather than as an occupancy, each
/// for a reason of its own, and each named rather than swept in by a
/// pattern.
///
/// - `cycle/row.rs` asserts that a child the trace descends into is not at
///   zero, which is a rule about the edge and not about the slot;
/// - `cycle/validation.rs` carries the zero-count-member rule, which drops
///   a whole component and never asks whether a slot is occupied;
/// - `object.rs` reads the count after a destructor to see whether the
///   object was resurrected;
/// - `memory/stdapi.rs` asserts that an entity reaches the free path with
///   its teardown finished, which is a statement about the count and not
///   about what the slot holds.
const COUNTS_AS_A_NUMBER: [&str; 3] = ["cycle/validation.rs", "object.rs", "memory/stdapi.rs"];

/// The lines of `text` that test a slot's occupancy by hand.
///
/// The shape is a count compared against zero, which separates two states
/// where a slot has three: a slot whose occupant died inside a trace window
/// that could not record the return is neither live nor free
/// (`PLAN.md` S43.2), and a walker asking the count alone reads it as free.
///
/// **What it cannot see is the comparison on its own line**, the count
/// having been bound to a local first. The walk is line-oriented, as the
/// direct-read guard above it is, and a reader who splits the two lines is
/// past both.
fn hand_rolled_occupancy(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();

    for (number, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }

        let reads_a_count = line.contains("header_refcount(")
            || line.contains("entity_refcount(")
            || line.contains("header_pair(");
        let against_zero = line.contains("!= 0") || line.contains("== 0") || line.contains("> 0");
        if reads_a_count && against_zero {
            found.push((number + 1, line.trim().to_string()));
        }
    }

    found
}

/// A slot's occupancy is asked of `slot_state` and of nothing else.
#[test]
#[cfg_attr(
    miri,
    ignore = "reads the crate's sources; `opendir` is unavailable under Miri's isolation, \
              and the abort takes the whole slice with it"
)]
fn a_slots_occupancy_is_asked_through_one_predicate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(root.as_path(), &mut files);
    assert!(files.len() > 50, "the source walk found almost nothing");

    let mut offences = Vec::new();
    for path in &files {
        let relative = path.strip_prefix(&root).expect("a path under src/");
        if exempt_file(relative) {
            continue;
        }

        let relative_text = relative.to_string_lossy().replace('\\', "/");
        if COUNTS_AS_A_NUMBER
            .iter()
            .any(|allowed| relative_text == *allowed)
        {
            continue;
        }

        let text = fs::read_to_string(path).expect("a source file is readable");
        for (number, line) in hand_rolled_occupancy(&text) {
            offences.push(format!("{}:{number}: {line}", path.display()));
        }
    }

    assert!(
        offences.is_empty(),
        "a slot's occupancy is tested by hand, which reads a slot marked \
         dead in place as free. Ask `refcount::slot_state`:\n{}",
        offences.join("\n")
    );
}

/// The guard has to see an offence, or it passes by finding nothing.
#[test]
fn the_occupancy_guard_sees_the_two_way_test() {
    let source = "\
fn walk(slot: *mut RcHeader) {
    if unsafe { crate::refcount::header_refcount(slot) } != 0 {
        visit(slot);
    }
}
";
    let found = hand_rolled_occupancy(source);
    assert_eq!(found.len(), 1, "the two-way test was not seen: {found:?}");
    assert!(
        found[0].1.contains("header_refcount"),
        "the line reported is the test itself"
    );

    // The other direction is the dangerous one: a marked slot read as free is
    // what the third state exists to prevent.
    let free_test = "    if unsafe { entity_refcount(slot) } == 0 { reuse(slot) }\n";
    assert_eq!(
        hand_rolled_occupancy(free_test).len(),
        1,
        "the free half of the same test is not seen"
    );
}
