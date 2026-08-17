//! The behavioural half draws and compares; the source-reading half is
//! the idiom of `refcount::tests::who_may_read_a_header`, and for the
//! same reason: a build that folds and one that does not behave
//! identically here, so no `cargo test` run separates them and the
//! sources are read instead. Equally cheap to evade, equally aimed at
//! inattention.

use std::fs;
use std::path::Path;

#[test]
fn two_draws_of_the_underlying_key_differ() {
    assert_ne!(
        super::draw_for_tests(),
        super::draw_for_tests(),
        "two independent 256-bit draws agreed, which fresh OS bytes do not do"
    );
}

#[test]
fn the_accessor_answers_the_same_words_across_calls_and_threads() {
    let first = *super::words();
    let again = *super::words();
    assert_eq!(first, again, "two calls read different keys");
    assert_ne!(
        first, [0u64; 4],
        "the memoized key reads all-zero, which a genuine draw does not \
         produce; the accessor is not wired to the draw"
    );

    let handles: Vec<_> = (0..2)
        .map(|_| std::thread::spawn(|| *super::words()))
        .collect();
    for handle in handles {
        assert_eq!(
            first,
            handle.join().expect("a reading thread finished"),
            "a thread read a different key"
        );
    }
}

/// A source file of this crate, read whole.
fn source(name: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(name))
        .expect("a source file is readable")
}

/// The key exists in every build, so the module may not grow a folding
/// arm. The doc prose names the option; only a `cfg` test of it counts.
#[test]
fn the_key_module_carries_no_folding_arm() {
    let text = source("src/hash/process_key.rs");
    assert!(
        !text.contains("feature = \"hash-folding\""),
        "the key module tests the folding feature; the key is per-process \
         random on both sides of that option"
    );
}

/// Lines of `text` naming the key module outside the block of
/// `ll_hash_seed_init`, as (line number, line). Brace counting from the
/// function line holds because `rustfmt` governs this crate.
fn stray_key_mentions(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut inside_init: Option<i32> = None;
    let mut depth: i32 = 0;
    let mut arming = false;

    for (number, line) in text.lines().enumerate() {
        let opens = line.matches('{').count() as i32;
        let closes = line.matches('}').count() as i32;

        if line.contains("fn ll_hash_seed_init") && !line.trim_start().starts_with("//") {
            arming = true;
        }

        if arming && opens > 0 {
            inside_init = Some(depth);
            arming = false;
        }

        if inside_init.is_none() && !arming && line.contains("process_key") {
            found.push((number + 1, line.trim().to_string()));
        }

        depth += opens - closes;
        if let Some(floor) = inside_init
            && depth <= floor
        {
            inside_init = None;
        }
    }

    found
}

/// The stamp covers what a compiled program may depend on, and nothing
/// may be folded against the per-process key — so `seed.rs` may name the
/// key module only to force its draw at startup, never in the constant
/// chain the stamp is built from.
#[test]
fn seed_names_the_key_only_to_force_it_at_startup() {
    let strays = stray_key_mentions(&source("src/hash/seed.rs"));
    assert!(
        strays.is_empty(),
        "seed.rs names the key module outside ll_hash_seed_init, where the \
         stamp's constant chain could pick it up:\n{}",
        strays
            .iter()
            .map(|(number, line)| format!("{number}: {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The guard has to see an offence, or it passes by finding nothing
/// anywhere: a key word mixed into the stamp is one stray, a helper
/// whose doc merely cross-references the init function is another —
/// the comment must not arm the exemption — and the forcing line in
/// the real init function is the one exemption.
#[test]
fn the_guard_tells_the_forcing_line_from_a_stray_one() {
    let planted = "\
/// Forced by fn ll_hash_seed_init at startup.
pub fn key_word() -> u64 {
    super::process_key::words()[0]
}

pub const STAMP: u64 = mix(process_key::words()[0], 1);

pub extern \"C\" fn ll_hash_seed_init() {
    let _ = expanded();
    let _ = super::process_key::words();
}
";
    let found = stray_key_mentions(planted);
    let lines: Vec<usize> = found.iter().map(|(number, _)| *number).collect();
    assert_eq!(
        lines,
        vec![3, 6],
        "the helper body and the stamp line are strays, the forcing line \
         is exempt: {found:?}"
    );
}
