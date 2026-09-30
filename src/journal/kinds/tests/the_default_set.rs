//! The kinds and the masks are one declaration in two halves, so a
//! constant added without its bit is a site that never fires.

use super::*;

/// The masks and the constants are one thing: a kind whose bit is in
/// neither [`DEFAULT_KINDS`] nor the on-demand [`COLLECTOR_KINDS`] has no
/// set that turns it on, and adding a constant without adding its bit is
/// the way to get a site that silently never fires. A kind is in one set
/// alone, and no set has a bit past the kinds.
#[test]
fn every_kind_with_a_site_is_in_one_set() {
    for kind in 1..=HIGHEST_KIND {
        let sets = [DEFAULT_KINDS, COLLECTOR_KINDS]
            .iter()
            .filter(|&&set| set & bit(kind) != 0)
            .count();
        assert_eq!(sets, 1, "kind {kind} is in {sets} sets");
    }

    assert_eq!(
        (DEFAULT_KINDS | COLLECTOR_KINDS).count_ones(),
        HIGHEST_KIND,
        "a set has a bit the kinds do not"
    );
}

/// The unset kind is what an unwritten slot reads as, so no site may
/// have it and no mask may enable it.
#[test]
fn the_unset_kind_is_not_a_site() {
    assert_eq!(DEFAULT_KINDS & bit(crate::journal::KIND_UNSET), 0);
}
