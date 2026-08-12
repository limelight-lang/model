//! A port is defined by the vendored header, so the arm a length
//! selects is a property to pin rather than a detail to rediscover.

use super::*;

/// The bulk loop is entered on `> 112`, not `>= 112`, so an input of
/// exactly one stride never enters it and one byte more enters it once.
///
/// **This names the boundary; the vector table is what defends it.**
/// Each of the two off-by-ones available here is narrow, not broad:
/// `>=` at the entry changes the hash of exactly one length, 112, and
/// `<` at the exit changes only lengths whose residue is exactly a
/// stride — 224, 336, 448. `filled_lengths` in the generator covers
/// 112, 224 and 336 for that reason.
#[test]
fn the_bulk_loop_boundary_is_where_the_reference_puts_it() {
    let input = vec![0x5au8; BULK_STRIDE + 1];

    let at_stride = hash(&input[..BULK_STRIDE], 0, &DEFAULT_SECRET);
    let over_stride = hash(&input, 0, &DEFAULT_SECRET);

    assert_ne!(at_stride, over_stride);
}
