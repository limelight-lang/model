//! Where the hash seed comes from, and the stamp that keeps two artifacts
//! from disagreeing about it.
//!
//! **One build option decides both**, because they are not independent: a
//! compiler that folds the hash of a literal key has to know the seed while
//! it compiles, and a seed drawn when the process starts is not knowable
//! then. So `hash-folding` selects a pair.
//!
//! - **Off, the default.** The seed is drawn from the operating system once
//!   per process, and the compiler emits no hash constants.
//! - **On.** The seed is fixed at build time from `LL_HASH_SEED`, the same
//!   value is given to the compiler, and it folds the hash of every literal
//!   key. The seed then travels inside the artifact.
//!
//! **The stamp is what makes a mismatch loud.** Folded hashes live inside
//! the compiled program while the function that has to agree with them
//! lives here, and nothing in the linker compares the two: a program folded
//! under one seed, run against a runtime holding another, produces lookups
//! that miss — no crash, no failing test, no log line. [`STAMP`] identifies
//! the hash a build computes, generated code carries the value it folded
//! under, and [`ll_hash_stamp_matches`] compares them at startup. The
//! runtime half is here; emitting the program's half is owed by the
//! compiler, which does not exist yet, so the check is available and
//! nothing calls it.
//!
//! What the option buys, what it costs, why neither arm defends against
//! hash flooding, and what a pre-forking server does to "per process":
//! `dev/DECISIONS.md`, 2026-08-04.

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
mod tests;
