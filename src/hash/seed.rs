//! Where the hash seed comes from, and the stamp that keeps two artifacts
//! from disagreeing about it.
//!
//! **One build option decides both**, because they are not independent: a
//! compiler that folds the hash of a literal key has to know the seed while
//! it compiles, and a seed drawn when the process starts is not knowable
//! then. So `hash-folding` selects a pair.
//!
//! - **Off, the default.** The seed is drawn from the operating system once
//!   per process. The compiler emits no hash constants and the runtime
//!   computes every hash itself. An attacker holding the artifact learns
//!   nothing about which keys share a bucket.
//! - **On.** The seed is fixed at build time from `LL_HASH_SEED`, the same
//!   value is given to the compiler, and it folds the hash of every literal
//!   key. The seed is then inside the artifact, and anyone holding the
//!   artifact can compute a set of colliding keys in advance.
//!
//! What the option buys is small and unmeasured. A literal's hash is
//! already computed once per process at intern time (`crate::intern`), so
//! folding does not remove a hash from the access — it replaces reading one
//! field of a permanently hot immortal entity with an immediate, and
//! generated code needs that entity's pointer regardless, for the identity
//! compare. On the runtime's own side the difference is a compile-time
//! constant against a static read behind [`std::sync::LazyLock`]'s
//! initialization check. Neither has been measured or read in the emitted
//! IR.
//!
//! **Neither arm defends against hash flooding.** A per-process seed raises
//! the cost of the attack from reading a constant out of a binary to
//! mounting a timing attack, and no further: rapidhash claims no resistance
//! to key recovery from observed collisions. Bounding the worst case is the
//! hash table's job — a probe-length counter with an escape hatch
//! (`rfc/model/strings.md`, "Seeding") — and that table is not designed yet.
//!
//! **"Per process" is per address space, and a pre-forking server has one.**
//! A seed established before `fork` is inherited by every worker, so in the
//! deployment shape this language is aimed at — a master process that forks
//! workers, as php-fpm does — the guarantee degrades from per-process to
//! per-deployment: one recovered seed serves every worker for the life of
//! the master. Drawing on first use rather than at startup does not fix it,
//! since the master hashes at least the interned names before it forks.
//! Fixing it means redrawing after `fork` and rehashing everything already
//! cached, which no caller can do today; the honest position is that this
//! is a limit of the arm, not a defect in it.
//!
//! ## The stamp
//!
//! Folded hashes live inside the compiled program while the function that
//! has to agree with them lives in the runtime. Nothing in the linker
//! checks that the two were built from the same hash: a program folded
//! under one seed, run against a runtime holding another, produces lookups
//! that miss — no crash, no failing test, no log line. [`STAMP`] is what
//! makes that loud. It identifies the hash a build computes, generated code
//! carries the value it folded under, and [`ll_hash_stamp_matches`]
//! compares them at startup.
//!
//! The runtime half is here; emitting the program's half is owed by the
//! compiler, which does not exist yet. Until it does, the check is
//! available and nothing calls it.

use super::rapidhash;

/// Bumped by hand for the one change the constants below cannot show: a
/// different vendored version of rapidhash whose published constants
/// happen to be identical to this one's.
///
/// Everything else that decides the bytes-to-hash mapping is folded into
/// [`FUNCTION_IDENTITY`] from the constants themselves, because a version
/// number that has to be remembered is a version number that will not be.
const FUNCTION_REVISION: u64 = 1;

/// The function this build computes, derived from what defines it rather
/// than asserted: the revision above, the whole secret array, the zero
/// remap and the bulk stride.
///
/// A regenerated `vectors.rs` cannot hide a changed constant behind a
/// green suite — the vectors would agree with the port, and this would
/// not agree with the artifact already shipped.
const FUNCTION_IDENTITY: u64 = {
    let mut accumulated = FUNCTION_REVISION;
    let mut index = 0;
    while index < rapidhash::DEFAULT_SECRET.len() {
        accumulated = rapidhash::mix(accumulated, rapidhash::DEFAULT_SECRET[index]);
        index += 1;
    }
    rapidhash::mix(
        accumulated,
        super::ZERO_REPLACEMENT ^ rapidhash::BULK_STRIDE as u64,
    )
};

/// Whether this build folds, as a value the stamp can carry.
///
/// Without it a folding build whose seed is zero and a non-folding build
/// stamp identically, since neither contributes a seed — and those are
/// exactly the two the stamp exists to tell apart.
const FOLDS: u64 = cfg!(feature = "hash-folding") as u64;

/// Identity of the hash this build computes — the function, whether it
/// folds, and when it folds, the seed.
///
/// Two artifacts that must interoperate carry the same value. Under
/// `hash-folding` the seed is part of it, because folded constants depend
/// on the seed; without folding the seed is per process and nothing may be
/// baked against it, so the stamp deliberately does not cover it and a
/// program that folded anything mismatches.
pub const STAMP: u64 = rapidhash::mix(
    FUNCTION_IDENTITY ^ FOLDS ^ rapidhash::DEFAULT_SECRET[7],
    STAMPED_SEED ^ rapidhash::DEFAULT_SECRET[1],
);

/// The part of the seed a compiled program may depend on: the build seed
/// under `hash-folding`, and nothing at all without it.
#[cfg(feature = "hash-folding")]
const STAMPED_SEED: u64 = BUILD_SEED;
#[cfg(not(feature = "hash-folding"))]
const STAMPED_SEED: u64 = 0;

/// Whether a program folded under `stamp` can run against this runtime,
/// as 0 or 1.
///
/// Generated code calls this once at startup with the value its own build
/// recorded, and stops on 0. A mismatch means the two builds hash the same
/// bytes to different values, which is not a condition either side can
/// recover from — every folded constant in the program is wrong.
///
/// **`u32` and not `bool` on purpose.** Rust lowers an `extern "C" -> bool`
/// to `i1 zeroext`, and this crate is merged with compiler-generated IR
/// rather than linked against it (`Cargo.toml`, `[lib]`), so a declaration
/// on the other side that says `i8` would read a wrong answer out of the
/// one function whose whole job is to be right about a mismatch.
#[unsafe(no_mangle)]
pub extern "C" fn ll_hash_stamp_matches(stamp: u64) -> u32 {
    (stamp == STAMP) as u32
}

/// The stamp of this runtime, for a caller that wants to record it.
#[unsafe(no_mangle)]
pub extern "C" fn ll_hash_stamp() -> u64 {
    STAMP
}

/// Draw the seed now rather than at the first hash.
///
/// Runtime startup calls this. Nothing breaks if it does not — [`expanded`]
/// draws on first use either way — but the draw reads the operating
/// system's randomness, and doing that at the first `LLString::hash`
/// puts a `getrandom` syscall at an arbitrary point on the string path:
/// mid-request, or inside `intern` while its table lock is held. On a host
/// that denies the syscall and has no `/dev/urandom`, the standard library
/// aborts, and it is better to abort at startup than on the first request.
///
/// A no-op under `hash-folding`, where the seed is a constant.
#[unsafe(no_mangle)]
pub extern "C" fn ll_hash_seed_init() {
    let _ = expanded();
}

/// The seed, expanded once by [`rapidhash::expand_seed`], which is the form
/// hashing consumes: [`super::hash_bytes`] calls
/// [`rapidhash::hash_expanded`] so the expansion does not run per hash.
///
/// Under `hash-folding` it is a compile-time constant. Without it, the
/// value is drawn on first use and afterwards read from a static behind a
/// one-time initialization check — a load and a predictable branch, not a
/// bare load, and neither arm has been measured or read in the emitted IR.
///
/// Constant for the life of the process either way, which is what lets a
/// string cache the hash it computed.
#[inline(always)]
pub fn expanded() -> u64 {
    #[cfg(feature = "hash-folding")]
    {
        const EXPANDED: u64 = rapidhash::expand_seed(BUILD_SEED, &rapidhash::DEFAULT_SECRET);
        EXPANDED
    }
    #[cfg(not(feature = "hash-folding"))]
    {
        *PROCESS_SEED
    }
}

/// The seed as it was obtained, before expansion.
///
/// For tests and diagnostics. Hashing goes through [`expanded`]; a caller
/// that hands this to [`rapidhash::hash_expanded`] gets a value that is
/// not this build's hash of anything.
pub fn raw() -> u64 {
    #[cfg(feature = "hash-folding")]
    {
        BUILD_SEED
    }
    #[cfg(not(feature = "hash-folding"))]
    {
        *PROCESS_RAW_SEED
    }
}

/// The build seed, parsed from `LL_HASH_SEED` at compile time.
#[cfg(feature = "hash-folding")]
const BUILD_SEED: u64 = parse_seed(option_env!("LL_HASH_SEED"));

/// A folding build with no seed is refused here, at compile time, rather
/// than by a test.
///
/// The value it would take is zero — the reference implementation's own
/// default — which makes every artifact of that build hash alike, and
/// which the stamp cannot report either: a program folded under zero and a
/// runtime that does not fold at all would agree, since neither
/// contributes a seed. [`FOLDS`] closes the second half of that; this
/// closes the first, and it has to be a build error because
/// `cargo build --features hash-folding` with no environment runs no test.
#[cfg(feature = "hash-folding")]
const _: () = assert!(
    BUILD_SEED != 0,
    "hash-folding needs LL_HASH_SEED set to a non-zero value: every artifact \
     of a build without one hashes identically"
);

/// Decimal, or hexadecimal with a `0x` prefix; `_` separators anywhere
/// among the digits.
///
/// Anything else fails the build rather than being taken as zero — a seed
/// silently misread is a seed nobody set. **Including a value with no
/// digits at all** (`_`, `0x_`), which is the shape a typo in a CI file
/// takes, and **a value past 64 bits**, which would otherwise select a
/// different seed than the one written.
#[cfg(feature = "hash-folding")]
const fn parse_seed(text: Option<&str>) -> u64 {
    let text = match text {
        Some(text) => text,
        None => return 0,
    };
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        panic!("LL_HASH_SEED is empty");
    }

    let (radix, start) = if bytes.len() > 2 && bytes[0] == b'0' && (bytes[1] | 0x20) == b'x' {
        (16u64, 2usize)
    } else {
        (10u64, 0usize)
    };

    let mut value: u64 = 0;
    let mut digits = 0usize;
    let mut index = start;
    while index < bytes.len() {
        let digit = match bytes[index] {
            byte @ b'0'..=b'9' => (byte - b'0') as u64,
            byte @ b'a'..=b'f' if radix == 16 => (byte - b'a' + 10) as u64,
            byte @ b'A'..=b'F' if radix == 16 => (byte - b'A' + 10) as u64,
            b'_' => {
                index += 1;
                continue;
            }
            _ => panic!("LL_HASH_SEED is not a decimal or 0x-prefixed hexadecimal number"),
        };
        value = match value.checked_mul(radix) {
            Some(shifted) => shifted,
            None => panic!("LL_HASH_SEED does not fit in 64 bits"),
        };
        value = match value.checked_add(digit) {
            Some(summed) => summed,
            None => panic!("LL_HASH_SEED does not fit in 64 bits"),
        };
        digits += 1;
        index += 1;
    }

    if digits == 0 {
        panic!("LL_HASH_SEED has separators but no digits");
    }
    value
}

#[cfg(not(feature = "hash-folding"))]
mod process {
    use super::rapidhash;
    use std::sync::LazyLock;

    /// The draw, with [`super::ll_hash_seed_init`] as the intended trigger
    /// and this lazy path as the backstop.
    ///
    /// Both exist because they answer different questions. Correctness
    /// wants the lazy path: a *late* seed reports nothing at all — a string
    /// hashed and cached before the seed arrives disagrees forever with the
    /// same content hashed after, and the symptom is a lookup that misses,
    /// so no call site may be able to run first. Everything else wants the
    /// explicit one, because the draw reads the operating system and that
    /// belongs at startup rather than at whichever hash happens to be
    /// first.
    pub(super) static RAW: LazyLock<u64> = LazyLock::new(draw);

    pub(super) static EXPANDED: LazyLock<u64> =
        LazyLock::new(|| rapidhash::expand_seed(*RAW, &rapidhash::DEFAULT_SECRET));

    /// A 64-bit value from the operating system's randomness, by way of
    /// `RandomState` — the same source the standard library's `HashMap`
    /// keys itself from, and the only portable one in the standard library.
    /// The crate has no dependencies and this is not worth acquiring one
    /// for.
    ///
    /// Zero is remapped away: it is the value that means "no seed was
    /// installed" everywhere else here, and one draw in 2^64 should not
    /// make a test flake.
    fn draw() -> u64 {
        use std::hash::{BuildHasher, Hasher, RandomState};

        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(0x9e37_79b9_7f4a_7c15);
        let drawn = hasher.finish();
        if drawn == 0 { 1 } else { drawn }
    }
}

#[cfg(not(feature = "hash-folding"))]
use process::{EXPANDED as PROCESS_SEED, RAW as PROCESS_RAW_SEED};

#[cfg(test)]
mod tests {
    use super::*;

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
                super::super::ZERO_REPLACEMENT ^ rapidhash::BULK_STRIDE as u64,
            )
        };

        assert_ne!(FUNCTION_IDENTITY, with_one_secret_word_changed);
    }
}
