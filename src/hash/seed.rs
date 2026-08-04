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

/// Bumped whenever the bytes-to-hash mapping changes for a reason other
/// than the seed: a different function, a different vendored version, a
/// different secret, a different zero remap.
///
/// It exists so that [`STAMP`] separates two builds that differ in the
/// function even when they share a seed.
const FUNCTION_VERSION: u64 = 1;

/// Identity of the hash this build computes — the function and, when it is
/// fixed at build time, the seed.
///
/// Two artifacts that must interoperate carry the same value. Under
/// `hash-folding` the seed is part of it, because folded constants depend
/// on the seed; without folding the seed is per process and nothing may be
/// baked against it, so the stamp deliberately does not cover it and a
/// program that folded anything mismatches.
pub const STAMP: u64 = rapidhash::mix(
    FUNCTION_VERSION ^ rapidhash::DEFAULT_SECRET[7],
    STAMPED_SEED ^ rapidhash::DEFAULT_SECRET[1],
);

/// The part of the seed a compiled program may depend on: the build seed
/// under `hash-folding`, and nothing at all without it.
#[cfg(feature = "hash-folding")]
const STAMPED_SEED: u64 = BUILD_SEED;
#[cfg(not(feature = "hash-folding"))]
const STAMPED_SEED: u64 = 0;

/// Whether a program folded under `stamp` can run against this runtime.
///
/// Generated code calls this once at startup with the value its own build
/// recorded, and stops if it returns false. A mismatch means the two builds
/// hash the same bytes to different values, which is not a condition either
/// side can recover from — every folded constant in the program is wrong.
#[unsafe(no_mangle)]
pub extern "C" fn ll_hash_stamp_matches(stamp: u64) -> bool {
    stamp == STAMP
}

/// The stamp of this runtime, for a caller that wants to record it.
#[unsafe(no_mangle)]
pub extern "C" fn ll_hash_stamp() -> u64 {
    STAMP
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
///
/// Zero when the variable was not set, which is the reference
/// implementation's own default and a defect in a folding build: it makes
/// every such artifact hash alike. `a_folding_build_was_given_a_seed`
/// fails on it.
#[cfg(feature = "hash-folding")]
const BUILD_SEED: u64 = parse_seed(option_env!("LL_HASH_SEED"));

/// Decimal, or hexadecimal with a `0x` prefix. Anything else fails the
/// build rather than being taken as zero — a seed silently misread is a
/// seed nobody set.
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
        // Wrapping rather than checked: a seed is a bit pattern, and
        // refusing one for being large would be refusing half the space.
        value = value.wrapping_mul(radix).wrapping_add(digit);
        index += 1;
    }
    value
}

#[cfg(not(feature = "hash-folding"))]
mod process {
    use super::rapidhash;
    use std::sync::LazyLock;

    /// Drawn on first use rather than in an explicit init call, which is
    /// the opposite of what this crate does elsewhere
    /// (`memory::heap`, on why initialization is a cold explicit call).
    /// The failure modes differ: a heapless thread reports null and every
    /// caller models it, while a *late* seed reports nothing at all — a
    /// string hashed and cached before it arrives disagrees forever with
    /// the same content hashed after, and the symptom is a lookup that
    /// misses. The branch is correct by construction; the explicit call
    /// would have to be proved first on every path.
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

    /// A folding build bakes its seed into every literal key's hash, so a
    /// build that was never given one produces artifacts that all hash
    /// alike — the position a per-build seed exists to avoid. The reference
    /// implementation's default is zero, which is exactly what
    /// `option_env!` yields when nobody set the variable.
    #[cfg(feature = "hash-folding")]
    #[test]
    fn a_folding_build_was_given_a_seed() {
        assert_ne!(
            raw(),
            0,
            "built with hash-folding and no LL_HASH_SEED: every artifact from \
             this build hashes identically, and the seed is the reference's own default"
        );
    }

    /// Without folding the seed comes from the operating system, and the
    /// test that it does is that two independent draws differ. A stub that
    /// returns a constant — or a seed left at the reference's default —
    /// fails here.
    #[cfg(not(feature = "hash-folding"))]
    #[test]
    fn the_process_seed_is_drawn_and_not_a_constant() {
        use std::hash::{BuildHasher, Hasher, RandomState};

        assert_ne!(raw(), 0, "no seed was installed");

        let draw = || {
            let mut hasher = RandomState::new().build_hasher();
            hasher.write_u64(0x9e37_79b9_7f4a_7c15);
            hasher.finish()
        };

        assert_ne!(draw(), draw(), "the source of the seed is not random");
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
        assert!(ll_hash_stamp_matches(ll_hash_stamp()));
        assert!(!ll_hash_stamp_matches(STAMP ^ 1));
        assert_ne!(
            STAMP, 0,
            "a zero stamp would be indistinguishable from unset"
        );
    }
}
