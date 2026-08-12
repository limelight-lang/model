//! A hash is never zero, since zero is the string field's "not
//! computed" sentinel; equal for equal bytes whatever their source;
//! and under the seed this build installed.

use super::*;

/// The sentinel stays unambiguous because the hash never returns zero.
/// No input is known that hashes to zero — finding one is the point of a
/// 64-bit hash — so the mapping is checked on the boundary directly
/// rather than through a search.
#[test]
fn a_genuine_zero_hash_is_mapped_away() {
    assert_eq!(remap_zero(0), ZERO_REPLACEMENT);
    assert_ne!(ZERO_REPLACEMENT, 0);
    assert_eq!(remap_zero(ZERO_REPLACEMENT), ZERO_REPLACEMENT);
    assert_ne!(hash_bytes(b"anything"), 0);
    assert_ne!(hash_bytes(&[]), 0);
}

/// Hashing is a pure function of the bytes: the same content hashes
/// alike whether it arrived as a literal, a slice of a longer buffer, or
/// a vector.
#[test]
fn the_same_bytes_hash_alike_from_any_source() {
    let owned = b"limelight".to_vec();
    let embedded = b"xxlimelightxx";

    assert_eq!(hash_bytes(b"limelight"), hash_bytes(&owned));
    assert_eq!(hash_bytes(b"limelight"), hash_bytes(&embedded[2..11]));
}

/// The seed reaches the function the runtime actually calls.
///
/// Every other test here would pass on a `hash_bytes` that ignored the
/// seed entirely: the vector table passes its seeds explicitly, and the
/// rest compare `hash_bytes` against itself. This one compares it
/// against the same input hashed under the reference's default seed of
/// zero, which is what an unseeded build produces. It holds in both
/// arms because neither can install a zero seed: the folding arm is
/// refused at compile time by the `const` assertion beside
/// `seed::BUILD_SEED`, and the drawn seed is remapped away from zero.
#[test]
fn the_installed_seed_reaches_hash_bytes() {
    let unseeded = rapidhash::hash(b"limelight", 0, &rapidhash::DEFAULT_SECRET);

    assert_ne!(hash_bytes(b"limelight"), unseeded);
}
