//! The salt's derivation, proved by reading the source — the idiom of
//! `refcount::tests::who_may_read_a_header`, for the same reason: a
//! salt drawn from the foldable seed behaves identically to one drawn
//! from the key in every `cargo test` run, and differs only for an
//! attacker holding a folding artifact, whom no suite simulates.

use std::fs;
use std::path::Path;

/// The body of the function `declaration` names, by brace counting from
/// its first line — which holds because `rustfmt` governs this crate.
/// `declaration` is the `fn <name>` prefix, so a longer name sharing the
/// prefix is not what arms the walk.
fn body_of(text: &str, declaration: &str) -> String {
    let mut body = String::new();
    let mut inside: Option<i32> = None;
    let mut depth: i32 = 0;
    let mut arming = false;

    for line in text.lines() {
        let opens = line.matches('{').count() as i32;
        let closes = line.matches('}').count() as i32;

        if line.contains(declaration) && !line.trim_start().starts_with("//") {
            arming = true;
        }

        if arming && opens > 0 {
            inside = Some(depth);
            arming = false;
        }

        if inside.is_some() {
            // Code only: a comment must not satisfy the positive check
            // nor trip a negative one. Everything from `//` to end of
            // line goes.
            let code = line.split_once("//").map_or(line, |(before, _)| before);
            body.push_str(code);
            body.push('\n');
        }

        depth += opens - closes;
        if let Some(floor) = inside
            && depth <= floor
        {
            break;
        }
    }

    body
}

/// The derivation must reach the per-process key and must not reach the
/// foldable seed: under `hash-folding` the seed travels inside the
/// artifact, and a salt an artifact-holder can compute prices nothing.
#[test]
#[cfg_attr(miri, ignore = "reads a source file, which Miri's isolation forbids")]
fn the_salt_derives_from_the_key_module_and_not_the_foldable_seed() {
    let text = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/array/table.rs"))
        .expect("the table source is readable");
    let body = body_of(&text, "fn salt_from");
    assert!(!body.is_empty(), "the walk did not find salt_from at all");
    assert!(
        body.contains("process_key"),
        "the derivation no longer reaches the per-process key:\n{body}"
    );
    assert!(
        !body.contains("hash_bytes") && !body.contains("seed::"),
        "the derivation reaches the foldable seed:\n{body}"
    );
}

/// Both draws go through that one derivation, which is what makes the
/// guard above cover the copy's redraw as well as the ladder's first
/// draw: a second derivation written inline in either would be a salt
/// no source test reads.
#[test]
#[cfg_attr(miri, ignore = "reads a source file, which Miri's isolation forbids")]
fn every_draw_goes_through_the_one_derivation() {
    let text = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/array/table.rs"))
        .expect("the table source is readable");
    for declaration in ["fn draw_salt", "fn redraw_salt"] {
        let body = body_of(&text, declaration);
        assert!(!body.is_empty(), "the walk did not find {declaration}");
        assert!(
            body.contains("salt_from("),
            "{declaration} derives a salt of its own:\n{body}"
        );
    }
}

/// The guard has to see an offence, or it passes by finding nothing:
/// the old derivation is the planted body, and the extractor must not
/// be armed by a doc line naming the function.
#[test]
fn the_guard_reads_the_old_derivation_as_an_offence() {
    let planted = "\
/// Cited by fn salt_from above.
fn unrelated() {}

fn salt_from(head: &StorageHead) -> u64 {
    crate::hash::hash_bytes(&(head.storage() as u64).to_le_bytes())
}
";
    let body = body_of(planted, "fn salt_from");
    assert!(
        body.contains("hash_bytes"),
        "the planted offence was missed"
    );
    assert!(
        !body.contains("unrelated"),
        "a doc line naming the function armed the extractor"
    );
}
