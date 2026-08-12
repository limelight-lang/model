//! The kinds and the mask are one declaration in two halves, so a
//! constant added without its bit is a site that never fires.

use super::*;

/// The mask and the constants are one thing: a kind whose bit is not
/// in [`DEFAULT_KINDS`] writes nothing, and adding a constant without
/// adding its bit is the way to get a site that silently never fires.
#[test]
fn every_kind_with_a_site_is_in_the_default_set() {
    for kind in 1..=HIGHEST_KIND {
        assert_ne!(
            DEFAULT_KINDS & bit(kind),
            0,
            "kind {kind} has no bit in the default set"
        );
    }

    assert_eq!(
        DEFAULT_KINDS.count_ones(),
        HIGHEST_KIND,
        "the default set has a bit the kinds do not"
    );
}

/// The unset kind is what an unwritten slot reads as, so no site may
/// have it and no mask may enable it.
#[test]
fn the_unset_kind_is_not_a_site() {
    assert_eq!(DEFAULT_KINDS & bit(crate::journal::KIND_UNSET), 0);
}
