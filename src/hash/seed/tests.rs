use super::*;

/// One build option decides both halves: drawn from the OS per
/// process, or fixed from the environment so the compiler can fold
/// with it. Either way it does not move while the process runs.
mod where_the_seed_comes_from {
    use super::*;

    /// Without folding the seed comes from the operating system.
    ///
    /// **Across threads, not within one**, and the distinction is the whole
    /// test. `RandomState::new()` bumps a thread-local counter between
    /// calls, so two draws on one thread differ whatever the entropy source
    /// did — a platform whose randomness returned a fixed value, which is
    /// the failure this arm exists to prevent, would pass that. A second
    /// thread initializes its own keys from the source, so a constant
    /// source shows up as two threads agreeing.
    #[cfg(not(feature = "hash-folding"))]
    #[test]
    fn the_process_seed_comes_from_the_operating_system() {
        use std::hash::{BuildHasher, Hasher, RandomState};

        fn draw() -> u64 {
            let mut hasher = RandomState::new().build_hasher();
            hasher.write_u64(0x9e37_79b9_7f4a_7c15);
            hasher.finish()
        }

        let here = draw();
        let there = std::thread::spawn(draw).join().unwrap();

        assert_ne!(here, there, "the source of the seed is not random");
    }

    /// A folding build hashes under its own seed rather than under the
    /// reference's default of zero.
    ///
    /// That a seed was supplied at all is a build error, not a test failure
    /// (the `const _: () = assert!` beside `BUILD_SEED`), because
    /// `cargo build --features hash-folding` runs no tests. What is left
    /// for a test is the consequence: the seed reaches the expansion the
    /// hash consumes. Deleting the const assertion makes this fail.
    #[cfg(feature = "hash-folding")]
    #[test]
    fn a_folding_build_hashes_under_its_own_seed() {
        assert_ne!(
            expanded(),
            rapidhash::expand_seed(0, &rapidhash::DEFAULT_SECRET),
            "this build hashes exactly as an unseeded one does"
        );
    }

    /// The seed holds still within a process however it was obtained,
    /// which is what lets a string cache its hash at all.
    #[test]
    fn the_seed_does_not_move_under_a_running_process() {
        assert_eq!(raw(), raw());
        assert_eq!(expanded(), expanded());
        assert_eq!(
            expanded(),
            rapidhash::expand_seed(raw(), &rapidhash::DEFAULT_SECRET),
            "the expanded seed is the expansion of the raw one"
        );
    }
}

/// A program folded under one seed must not miss silently against a
/// runtime holding another, so the stamp has to separate the seeds
/// and the two arms, and to cover the constants that define the
/// function itself.
mod the_stamp {
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
}
