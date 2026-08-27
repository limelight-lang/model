//! Four bytes carry two things, and the split is what every later stage
//! reads: thirty bits of working count under two of colour. The colour's
//! zero is reserved, so the tests here are as much about what a row
//! cannot say as about what it can.

use super::*;

/// The round trip, at the boundaries of both fields. A colour written
/// into the top bits must not disturb the count under it, and a count at
/// its maximum must not spill into the colour.
#[test]
fn a_row_carries_a_colour_and_a_count_without_either_reaching_the_other() {
    // The split itself, which the rest of this test would not notice: the
    // round trip holds for any width, and the design fixes this one
    // (`rfc/model/gc/rc-cycle.md`, "Where the shadow count lives").
    assert_eq!(size_of::<u32>(), 4, "a row is four bytes");
    assert_eq!(
        COUNT_MAX, 0x3FFF_FFFF,
        "thirty bits of working count under two of colour"
    );

    for colour_code in [
        Colour::Untouched,
        Colour::Met,
        Colour::Condemned,
        Colour::Live,
    ] {
        for value in [0, 1, 2, 1023, COUNT_MAX - 1, COUNT_MAX] {
            let word = compose(colour_code, value);
            assert_eq!(colour(word), colour_code, "colour of {word:#x}");
            assert_eq!(count(word), value, "count of {word:#x}");
        }
    }
}

/// The saturation the field's width forces: a count past the bound is
/// held at the bound rather than wrapped into the colour, which would
/// turn a live entity's row into a condemned one.
#[test]
fn a_count_past_the_field_saturates_instead_of_wrapping() {
    for value in [COUNT_MAX + 1, COUNT_MAX + 2, u32::MAX] {
        let word = compose(Colour::Met, value);
        assert_eq!(count(word), COUNT_MAX);
        assert_eq!(colour(word), Colour::Met, "the colour survives the clamp");
        assert!(is_saturated(word));
    }

    assert!(
        !is_saturated(compose(Colour::Met, COUNT_MAX - 1)),
        "a count the field holds exactly is a total, not a floor"
    );
}

/// What the saturated value means, which is the whole of the clause: the
/// count is a floor, so every subtraction leaves it a floor and the
/// entity stays conservatively live. Without this a refcount above the
/// field is condemnable — the row starts at the bound and enough
/// internal edges walk it to zero while the external references it could
/// not count are still there.
#[test]
fn a_saturated_count_absorbs_every_subtraction() {
    let mut word = compose(Colour::Met, u32::MAX);
    let row = &raw mut word;
    assert!(is_saturated(unsafe { *row }));

    assert_eq!(unsafe { subtract(row, 1) }, COUNT_MAX);
    assert_eq!(unsafe { subtract(row, COUNT_MAX) }, COUNT_MAX);
    assert!(
        is_saturated(unsafe { *row }),
        "the entity is externally referenced whatever the trace found"
    );
    assert_eq!(colour(unsafe { *row }), Colour::Met);

    // One below the bound is an ordinary count and answers ordinarily,
    // which is what makes the clause a clause rather than a ceiling.
    let mut ordinary = compose(Colour::Met, COUNT_MAX - 1);
    let row = &raw mut ordinary;
    assert_eq!(unsafe { subtract(row, 1) }, COUNT_MAX - 2);
}

/// The reserved code. A zeroed row is untouched and nothing else, so a
/// group the init has just cleared says "not met in this collection" for
/// every slot in it — and a met row, whatever its count, never reads as
/// one.
#[test]
fn only_a_zero_word_reads_as_untouched() {
    assert_eq!(colour(0), Colour::Untouched);
    for colour_code in [Colour::Met, Colour::Condemned, Colour::Live] {
        assert_ne!(
            colour(compose(colour_code, 0)),
            Colour::Untouched,
            "a met row with a zero count is not an untouched slot"
        );
    }
}

/// The array fits the one block the arena grants for it, at every size
/// class a block can be cut into — the smallest class is the widest
/// array, and it is the one an `alloc` above `BLOCK_PAYLOAD` would
/// refuse outright (`cycle::arena::ShadowArena::alloc`).
#[test]
fn an_array_for_any_size_class_fits_one_block() {
    use crate::memory::block_pool::BLOCK_PAYLOAD;

    let widest = BLOCK_PAYLOAD / crate::memory::heap::SIZE_CLASSES[0];
    assert_eq!(widest, 4080, "the smallest class cuts a block into 4080");
    for &class in crate::memory::heap::SIZE_CLASSES {
        let slots = (BLOCK_PAYLOAD / class) as u32;
        assert!(
            bytes_for(slots) <= BLOCK_PAYLOAD,
            "an array for {slots} rows is past one block"
        );
    }

    assert_eq!(
        bytes_for(widest as u32),
        16_408,
        "the figure the arena's refusal is written against"
    );
}

/// The mark's own operation, and the floor that keeps it from turning a
/// condemned row into a live one. `count - 1` at zero wraps to
/// `u32::MAX`, which [`compose`] clamps to [`COUNT_MAX`] — the value the
/// design reserves for "externally referenced, conservatively live" — so
/// the subtraction has to stop at zero itself.
#[test]
fn a_subtraction_stops_at_zero_and_keeps_the_colour() {
    let mut word = compose(Colour::Met, 3);
    let row = &raw mut word;

    assert_eq!(unsafe { subtract(row, 1) }, 2);
    assert_eq!(unsafe { subtract(row, 2) }, 0);
    assert_eq!(colour(unsafe { *row }), Colour::Met);

    // More in-edges than the refcount held, which a dirty pass may read
    // and the exact test on the owner's thread is what corrects.
    assert_eq!(unsafe { subtract(row, 1) }, 0, "the count stops at zero");
    assert_eq!(
        count(unsafe { *row }),
        0,
        "and does not come back as a saturated count"
    );
    assert_eq!(colour(unsafe { *row }), Colour::Met);

    let mut condemned = compose(Colour::Condemned, 0);
    let row = &raw mut condemned;
    assert_eq!(unsafe { subtract(row, 7) }, 0);
    assert_eq!(
        colour(unsafe { *row }),
        Colour::Condemned,
        "a subtraction carries the colour it found"
    );
}
