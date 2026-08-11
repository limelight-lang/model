use super::*;

/// A port is defined by the vendored header, so the arm a length
/// selects is a property to pin rather than a detail to rediscover.
mod where_the_reference_puts_its_boundaries {
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
}

/// Length and seed both reach the digest for every input, which is
/// what keeps a shared prefix and a shared tail from colliding.
mod what_separates_two_inputs {
    use super::*;

    /// The short arm folds the length into the seed, so two inputs that
    /// share a prefix and differ in length do not collide through the tail
    /// read — which for both covers overlapping bytes.
    ///
    /// Both inputs here are 16 bytes or under, which is where that fold
    /// happens; above 16 the finalizer receives the bulk-loop residue
    /// rather than the length, and separation comes from the accumulated
    /// seed instead.
    #[test]
    fn length_separates_short_inputs_that_share_a_tail() {
        let short = vec![0u8; 16];

        assert_ne!(
            hash(&short, 0, &DEFAULT_SECRET),
            hash(&short[..8], 0, &DEFAULT_SECRET)
        );
        assert_ne!(
            hash(&short[..7], 0, &DEFAULT_SECRET),
            hash(&short[..4], 0, &DEFAULT_SECRET)
        );
    }

    /// The seed reaches the result for every input length, empty
    /// included: it is mixed before any byte is read.
    #[test]
    fn the_seed_reaches_every_length() {
        for len in [0usize, 1, 3, 4, 8, 16, 17, 48, 113, 240] {
            let input = vec![0xa5u8; len];

            assert_ne!(
                hash(&input, 0, &DEFAULT_SECRET),
                hash(&input, 1, &DEFAULT_SECRET),
                "length {len}"
            );
        }
    }
}
