//! The hash of a byte string, and the choice of which function computes it.
//!
//! One function serves every hashed thing in the runtime: a string's cached
//! hash today, an array's keys next. It is **fixed when the runtime is
//! built** rather than selected at run time — the same axis the GC strategy
//! uses — so the constant reaches every call site inlined and nothing
//! dispatches through a pointer on the path that matters
//! (`rfc/model/strings.md`, "The hash function is a build-time choice").
//!
//! That build-time choice exists because the hash is a contract between two
//! programs, not one: the runtime computes a string's hash, and the compiler
//! is meant to fold the same value for a literal key. The two agree only if
//! they were built from the same definition, which is why the reference
//! implementation is vendored and pinned rather than depended on by version
//! range (`vendor/rapidhash/README.md`).
//!
//! Zero is not a hash here. [`LLString`](crate::string::LLString) stores a
//! zero in its `hash` field to mean "not computed yet", so this module maps a
//! genuine zero to [`ZERO_REPLACEMENT`] before returning. The remap belongs to
//! the definition of the hash and not to the caller: the compiler folding a
//! literal's hash has to produce the same value, and it will not know which
//! caller it is folding for.

pub mod rapidhash;

#[cfg(test)]
mod vectors;

/// What a genuine zero hash is reported as, so that zero stays available as
/// the "not computed" sentinel.
///
/// The value is arbitrary and only has to be non-zero; it is frozen because
/// compiler-folded hashes have to match runtime-computed ones.
pub const ZERO_REPLACEMENT: u64 = 1;

pub mod seed;

/// The hash of `bytes`, never zero.
///
/// Equal for equal byte sequences for as long as the seed holds still, and
/// no longer: with `hash-folding` off the seed is drawn per process, so a
/// hash may not be persisted, cached on disk, or sent to a peer. See
/// [`seed`] for which it is in this build.
///
/// The empty input is valid and hashes like any other.
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    remap_zero(rapidhash::hash_expanded(
        bytes,
        seed::expanded(),
        &rapidhash::DEFAULT_SECRET,
    ))
}

/// Zero out of the hash function, [`ZERO_REPLACEMENT`] out of here.
#[inline(always)]
fn remap_zero(hash: u64) -> u64 {
    if hash == 0 { ZERO_REPLACEMENT } else { hash }
}

#[cfg(test)]
mod tests;
