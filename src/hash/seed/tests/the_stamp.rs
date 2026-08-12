//! A program folded under one seed must not miss silently against a
//! runtime holding another, so the stamp has to separate the seeds
//! and the two arms, and to cover the constants that define the
//! function itself.

use super::*;

/// The stamp answers its own value and refuses every other, which is
/// the whole of the check generated code performs at startup.
#[test]
fn the_stamp_admits_this_build_and_no_other() {
    assert_eq!(ll_hash_stamp_matches(ll_hash_stamp()), 1);
    assert_eq!(ll_hash_stamp_matches(STAMP ^ 1), 0);
    assert_ne!(
        STAMP, 0,
        "a zero stamp would be indistinguishable from unset"
    );
}

/// The stamp separates a folding build from a non-folding one even
/// when neither contributes a seed.
///
/// That pair is the one the stamp exists for and the one it used to
/// miss: a program folded under seed zero, run against a runtime that
/// draws per process, agreed on the stamp and disagreed on every hash.
/// The `const` assertion beside `BUILD_SEED` makes the pair
/// unbuildable; [`FOLDS`] makes it distinguishable even so, and this
/// checks the second without needing the first.
#[test]
fn the_stamp_separates_folding_from_not() {
    let other_arm = rapidhash::mix(
        FUNCTION_IDENTITY ^ (FOLDS ^ 1) ^ rapidhash::DEFAULT_SECRET[7],
        STAMPED_SEED ^ rapidhash::DEFAULT_SECRET[1],
    );

    assert_ne!(STAMP, other_arm);
}

/// The function's identity moves when any constant that defines the
/// mapping moves, so a regenerated vector table cannot carry a changed
/// constant past a build that already shipped.
#[test]
fn the_function_identity_covers_the_constants_that_define_it() {
    let with_one_secret_word_changed = {
        let mut secret = rapidhash::DEFAULT_SECRET;
        secret[3] ^= 1;

        let mut accumulated = FUNCTION_REVISION;
        for word in secret {
            accumulated = rapidhash::mix(accumulated, word);
        }

        rapidhash::mix(
            accumulated,
            crate::hash::ZERO_REPLACEMENT ^ rapidhash::BULK_STRIDE as u64,
        )
    };

    assert_ne!(FUNCTION_IDENTITY, with_one_secret_word_changed);
}
