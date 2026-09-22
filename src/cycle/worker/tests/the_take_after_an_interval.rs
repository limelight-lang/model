//! The take of a candidate ring that stands below the round's threshold:
//! the interval it must stand for, and whose figure the round counts it
//! against — the crate's, the embedder's through the ABI, or a case's
//! override, each outranking the one before it.

use super::*;

/// Take at `interval` for the case, and at the module's own again when the
/// guard drops.
struct StandingInterval;

impl StandingInterval {
    fn of(interval: Duration) -> Self {
        testing::take_standing_after(Some(interval));
        Self
    }
}

impl Drop for StandingInterval {
    fn drop(&mut self) {
        testing::take_standing_after(None);
    }
}

/// Zero for the embedder's interval when the guard drops: a case that
/// failed between the setter and its own zero would otherwise leave every
/// later case's rounds taking at its figure.
struct EmbeddersInterval;

impl Drop for EmbeddersInterval {
    fn drop(&mut self) {
        set_standing_interval(Duration::ZERO);
    }
}

/// The embedder's interval replaces the crate's, and zero restores it.
#[test]
fn the_embedders_interval_replaces_the_crates_and_zero_restores_it() {
    let _g = test_guard();
    let _embedders = EmbeddersInterval;
    testing::take_standing_after(None);
    assert_eq!(standing_interval(), STANDING_INTERVAL);

    crate::gc::ll_gc_set_standing_interval(3);
    assert_eq!(standing_interval(), Duration::from_millis(3));

    crate::gc::ll_gc_set_standing_interval(0);
    assert_eq!(
        standing_interval(),
        STANDING_INTERVAL,
        "zero restores the crate's"
    );
}

/// A case's override outranks the embedder's figure, and the embedder's
/// stands again when the override goes: a case reaching the take by running
/// rounds against a millisecond leaves an embedded runtime's dial where it
/// was.
#[test]
fn a_cases_override_outranks_the_embedders_interval() {
    let _g = test_guard();
    let _embedders = EmbeddersInterval;
    crate::gc::ll_gc_set_standing_interval(3);

    let case = StandingInterval::of(Duration::from_millis(1));
    assert_eq!(standing_interval(), Duration::from_millis(1));

    drop(case);
    assert_eq!(
        standing_interval(),
        Duration::from_millis(3),
        "the embedder's figure stands again"
    );
}
