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
mod tests {
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

    /// Content of the generated inputs in [`vectors::FILLED_VECTORS`], byte
    /// by byte. `vendor/rapidhash/generate_vectors.c` computes the same
    /// value in C; the two definitions are a pair and neither may move
    /// alone.
    fn filler_byte(index: usize) -> u8 {
        (index as u8).wrapping_mul(167).wrapping_add(13)
    }

    /// The port agrees with the reference implementation on every input the
    /// generator covers, under every seed.
    ///
    /// This is what makes the port a port rather than a hash of its own. The
    /// expectations come from `vendor/rapidhash/rapidhash.h` compiled and
    /// run (`vendor/rapidhash/generate_vectors.c`), not from this code, so a
    /// constant transcribed wrong fails here and nowhere else: the hash of a
    /// wrong-constant port is still a fine hash — well distributed, stable,
    /// never zero — and every other test in this crate passes on it. What it
    /// is not is the hash the compiler will fold for a literal key, and the
    /// symptom of that is a lookup that misses.
    #[test]
    fn the_port_matches_the_reference_on_every_vector() {
        for (input, expected) in vectors::LITERAL_VECTORS {
            for (seed, expected) in vectors::SEEDS.iter().zip(expected) {
                assert_eq!(
                    rapidhash::hash(input, *seed, &rapidhash::DEFAULT_SECRET),
                    *expected,
                    "input {input:02x?}, seed {seed:#018x}"
                );
            }
        }

        for (len, expected) in vectors::FILLED_VECTORS {
            let input: Vec<u8> = (0..*len).map(filler_byte).collect();

            for (seed, expected) in vectors::SEEDS.iter().zip(expected) {
                assert_eq!(
                    rapidhash::hash(&input, *seed, &rapidhash::DEFAULT_SECRET),
                    *expected,
                    "generated length {len}, seed {seed:#018x}"
                );
            }
        }
    }

    /// The vendored reference is the one the table was generated from.
    ///
    /// Everything else here is circular and cannot see this: the generator
    /// reads `vendor/rapidhash/rapidhash.h`, the port was transcribed from
    /// the same file, so a header that was edited — patched by hand,
    /// swapped for another version, corrupted in transit — yields a
    /// self-consistent table, a self-consistent port and a green suite.
    /// `vendor/rapidhash/README.md` records a sha256 for it, and until now
    /// nothing compared anything against that record.
    ///
    /// The digest below is this crate's own hash of the file rather than
    /// its sha256, because computing sha256 would mean carrying an
    /// implementation of it for one assertion. It answers a weaker
    /// question — "is this the same file" — which is the question being
    /// asked. A port broken badly enough to fake it fails every vector
    /// first.
    ///
    /// **When the header is deliberately updated**, regenerate `vectors.rs`,
    /// update the sha256 in the README, bump `seed::FUNCTION_REVISION`, and
    /// put the new value here.
    #[test]
    fn the_vendored_reference_is_the_one_the_table_came_from() {
        const HEADER: &[u8] = include_bytes!("../../vendor/rapidhash/rapidhash.h");
        const DIGEST: u64 = 0x8b1c_c8ce_ca82_fff7;

        assert_eq!(
            rapidhash::hash(HEADER, 0, &rapidhash::DEFAULT_SECRET),
            DIGEST,
            "vendor/rapidhash/rapidhash.h is not the file src/hash/vectors.rs was \
             generated from; regenerate the table rather than adjust this number"
        );
    }

    /// The vector table covers the branches it exists to cover. A table that
    /// silently lost its long inputs would still pass the comparison above
    /// and would stop proving anything about the bulk loop, which is where a
    /// transcription error is most likely and least visible.
    #[test]
    fn the_vector_table_reaches_past_the_bulk_loop() {
        let longest = vectors::FILLED_VECTORS
            .iter()
            .map(|(len, _)| *len)
            .max()
            .unwrap_or(0);

        assert!(longest > 224, "longest generated input is {longest} bytes");
        assert!(
            vectors::SEEDS.contains(&0),
            "the reference's own default seed is not covered"
        );
        assert!(
            vectors::SEEDS.len() >= 2,
            "one seed cannot show the seed is used"
        );
    }
}
