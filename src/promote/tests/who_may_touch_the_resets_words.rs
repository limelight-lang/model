//! Three words of a block's collector line are the reset's own, and this
//! reads the sources to say so: `reset_pins` and `emptied_chain`, the two
//! chains a reset walks, and `placed_list`, which carries a survivor list
//! between the two passes that place and publish it.
//!
//! **A reader elsewhere is what the design forbids**, and no compiler
//! error would report one: the words are private to `memory::heap` and
//! reached through four `pub(crate)` accessors, which any module of the
//! crate may call with the suite staying green. The cost of a second
//! caller is not a race but a meaning: `reset_chain` says different things
//! in the two halves of one reset, and `placed_list` holds an address
//! nothing may publish yet.
//!
//! The same reading also guards the budget. Three words and the header's
//! own six fill the line's sixty-four bytes exactly, so the next step that
//! wants one reuses a word of these — and reuse is sound only while the
//! set of callers is this small.

use std::fs;
use std::path::{Path, PathBuf};

/// The accessors, and the two files allowed to call them: `memory/heap.rs`
/// declares them and `promote.rs` is the reset.
const ACCESSORS: [&str; 6] = [
    "set_block_reset_pin",
    "block_reset_pin",
    "set_block_emptied_chain",
    "block_emptied_chain",
    "set_block_placed_list",
    "take_block_placed_list",
];

/// The line's own clearing writes all three words at once, so it belongs
/// here too — with a third file, because a test commissions a block of the
/// pool the way the reset commissions one of the arena
/// (`memory::retained::commission_retained_block`). What it must never
/// reach is a block the reset has already chained: the clearing would take
/// it off both chains with the chains still naming it, which the walks
/// report as a block that left while it was on. Today's callers are the
/// reset itself and a test helper over a block fresh from the pool.
const CLEARING: &str = "clear_collector_line";

/// Paths relative to `src/`, in the crate's own spelling.
const CALLERS: [&str; 3] = [
    "memory/heap.rs",
    "promote.rs",
    "promote/tests/who_may_touch_the_resets_words.rs",
];

/// The files allowed to clear a line, which are the callers above and the
/// two that commission a retained block outside a reset.
const CLEARERS: [&str; 5] = [
    "memory/heap.rs",
    "promote.rs",
    "memory/retained.rs",
    "memory/retained/tests/when_a_retained_block_goes_home.rs",
    "promote/tests/who_may_touch_the_resets_words.rs",
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

#[test]
#[cfg_attr(miri, ignore = "reads the crate's sources rather than running it")]
fn the_resets_own_words_are_named_where_the_reset_is() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(root.as_path(), &mut files);
    assert!(files.len() > 50, "the source walk found almost nothing");

    let mut strangers = Vec::new();
    for path in &files {
        let relative = path
            .strip_prefix(&root)
            .expect("a path under src/")
            .to_string_lossy()
            .replace('\\', "/");
        let text = fs::read_to_string(path).expect("a source file is readable");
        let touches = !CALLERS.contains(&relative.as_str());
        let clears = !CLEARERS.contains(&relative.as_str());
        for (number, line) in text.lines().enumerate() {
            // One report per line, whichever name it carries: the shorter
            // accessors are substrings of the longer ones, and a line that
            // calls one would otherwise read as two offences.
            let named = ACCESSORS
                .iter()
                .find(|accessor| touches && line.contains(*accessor))
                .or(Some(&CLEARING).filter(|name| clears && line.contains(**name)));
            if let Some(name) = named {
                strangers.push(format!("{relative}:{}: {name}", number + 1));
            }
        }
    }

    assert!(
        strangers.is_empty(),
        "the reset's own words are reached from outside the reset:\n{}",
        strangers.join("\n")
    );
}
