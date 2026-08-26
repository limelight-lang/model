//! The mutator's accesses to a published header are two bytes for the
//! flags and four for the counter, and the one eight-byte access is the
//! publication.
//!
//! **Behaviour cannot separate the widths**, which is why this reads the
//! sources. A four-byte flags access that preserved bytes 6-7 — load the
//! word, put the top half back — answers every question a running test
//! can ask, and it is exactly the mixed-size access the rule forbids.
//! Miri does not separate them either: it permits a width change on a
//! byte whose previous atomic accesses the accessing thread is ordered
//! after, which on one thread is always.
//!
//! The same instrument closes the second hole. A `const` battery over a
//! list of constants proves nothing about a constant nobody added to the
//! list, so the list is checked against the declarations instead.

use std::fs;

/// `refcount.rs` itself.
fn source() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("refcount.rs");
    fs::read_to_string(path).expect("refcount.rs is readable")
}

/// The body of `fn name`, from its signature to the closing brace at
/// column zero. Enough for a helper written at module level.
fn body_of<'a>(text: &'a str, name: &str) -> &'a str {
    let signature = format!("fn {name}(");
    let start = text
        .find(&signature)
        .unwrap_or_else(|| panic!("{name} is declared in refcount.rs"));
    let rest = &text[start..];
    let end = rest
        .find("\n}\n")
        .unwrap_or_else(|| panic!("{name}'s body ends at column zero"));
    &rest[..end]
}

/// Bytes 4-5 for the flags, bytes 0-3 for the counter. A wider flags
/// access would overlap the collector's byte at +6 without covering it.
#[test]
#[cfg_attr(
    miri,
    ignore = "reads the crate's sources; `opendir` is unavailable under Miri's isolation"
)]
fn the_mutators_header_helpers_are_narrow() {
    let text = source();

    for name in ["flags_load", "flags_store"] {
        let body = body_of(&text, name);
        assert!(
            body.contains("AtomicU16"),
            "{name} accesses the flags half; two bytes is the contract"
        );
        for wider in ["AtomicU32", "AtomicU64"] {
            assert!(
                !body.contains(wider),
                "{name} uses {wider}, which overlaps the collector's byte at +6 \
                 without covering it"
            );
        }
    }

    for name in ["refcount_load", "refcount_store"] {
        let body = body_of(&text, name);
        assert!(
            body.contains("AtomicU32"),
            "{name} covers exactly the counter's four bytes"
        );
        assert!(
            !body.contains("AtomicU64"),
            "{name} must not span the flags"
        );
    }

    assert_eq!(
        text.matches("AtomicU64").count(),
        1,
        "the one eight-byte access is `publish_header`, made before the \
         entity is published and therefore overlapping nothing"
    );
}

/// Constants that are not mutator flags, and why. Everything else
/// declared `pub const … : u32` in `refcount.rs` has to appear in the
/// `const` battery that holds the flags below bit 16 — a new constant
/// lands on the accounted-for side by default, which is the whole point
/// of checking the declarations rather than trusting a list.
const NOT_A_FLAG: [&str; 2] = [
    // A shift, not a mask: its value is a position.
    "ENTITY_KIND_SHIFT",
    // Composed from constants the battery already covers.
    "ENROLMENT_GATE_MASK",
];

#[test]
#[cfg_attr(
    miri,
    ignore = "reads the crate's sources; `opendir` is unavailable under Miri's isolation"
)]
fn the_battery_names_every_flag_constant_that_exists() {
    let text = source();
    let battery = {
        let start = text
            .find("a mutator-visible flag above bit 15")
            .expect("the battery's message is in refcount.rs");
        let head = text[..start]
            .rfind("const _: () = assert!(")
            .expect("the battery is a const assertion");
        text[head..start].to_string()
    };

    let mut missing = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix("pub const ") else {
            continue;
        };
        let Some((name, tail)) = rest.split_once(':') else {
            continue;
        };
        if !tail.trim_start().starts_with("u32") || NOT_A_FLAG.contains(&name) {
            continue;
        }

        if !battery.contains(name) {
            missing.push(name.to_string());
        }
    }

    assert!(
        missing.is_empty(),
        "declared in refcount.rs and absent from the battery that holds the \
         mutator's flags below bit 16, so one of them at bit 16 would be \
         written as nothing and read back as false: {}",
        missing.join(", ")
    );
}
