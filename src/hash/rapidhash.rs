//! rapidhash V3, ported from the author's reference header.
//!
//! The reference is vendored at `vendor/rapidhash/rapidhash.h` and pinned
//! to one upstream commit; `vendor/rapidhash/README.md` records which one
//! and why the copy exists. **That header defines this function** — every
//! constant and every branch below is a transcription of it, and a
//! divergence is a defect here rather than a variant. What proves the
//! transcription is [`super::vectors`], a table generated from the header
//! itself, because the author publishes no test vectors of his own.
//!
//! Ported: `rapidhash_internal`, the general-purpose variant, with the
//! header's own default macros — `RAPIDHASH_COMPACT` (the 112-byte loop,
//! not the unrolled 224-byte one) and `RAPIDHASH_FAST` (`rapid_mum`
//! overwrites its operands instead of xoring into them). The `Micro` and
//! `Nano` variants are not ported: they differ only above 16 bytes, and
//! nothing has measured which suits this workload.
//!
//! Every multi-byte read here is little-endian on every target. That is
//! not a portability shortcut but what the reference does — its
//! big-endian arms byte-swap so that one input has one hash everywhere.

/// The reference's `rapid_secret`, in its order.
///
/// The algorithm indexes this by position and gives each slot a different
/// job, so the array is a sequence rather than a set: reordering it
/// produces a different hash, not the same one.
pub(super) const DEFAULT_SECRET: [u64; 8] = [
    0x2d35_8dcc_aa6c_78a5,
    0x8bb8_4b93_962e_acc9,
    0x4b33_a62e_d433_d4a3,
    0x4d5a_2da5_1de1_aa47,
    0xa076_1d64_78bd_642f,
    0xe703_7ed1_a0b4_28db,
    0x90ed_1765_281c_388c,
    0xaaaa_aaaa_aaaa_aaaa,
];

/// Low and high halves of the 128-bit product, in that order.
///
/// The reference calls this `rapid_mum` and returns through its two
/// pointer arguments; a pair says the same thing without the aliasing
/// question.
#[inline(always)]
fn multiply_wide(a: u64, b: u64) -> (u64, u64) {
    let product = (a as u128) * (b as u128);
    (product as u64, (product >> 64) as u64)
}

/// The reference's `rapid_mix`: the two halves of the 128-bit product,
/// folded together.
#[inline(always)]
fn mix(a: u64, b: u64) -> u64 {
    let (low, high) = multiply_wide(a, b);
    low ^ high
}

/// Eight bytes at `offset`, little-endian.
///
/// `offset + 8` must be within `bytes`; every call site below is inside a
/// length branch that has established this.
#[inline(always)]
fn read64(bytes: &[u8], offset: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(word)
}

/// Four bytes at `offset`, little-endian, zero-extended.
#[inline(always)]
fn read32(bytes: &[u8], offset: usize) -> u64 {
    let mut word = [0u8; 4];
    word.copy_from_slice(&bytes[offset..offset + 4]);
    u32::from_le_bytes(word) as u64
}

/// Bytes consumed per iteration of the bulk loop, and the length above
/// which that loop runs at all. Both are the reference's, and the second
/// is the first: the loop is entered only when it can run once.
const BULK_STRIDE: usize = 112;

/// Hash `bytes` under `seed` and `secret`.
///
/// The result is the reference implementation's `rapidhash_internal` for
/// the same three arguments, bit for bit, on any target and any input
/// length including zero.
///
/// `seed` and `secret` alter the result predictably and neither is
/// checked: any 64-bit value and any eight of them are valid. Choosing
/// them is the caller's business — [`super::hash_bytes`] is where this
/// crate's choice is made.
pub fn hash(bytes: &[u8], seed: u64, secret: &[u64; 8]) -> u64 {
    let len = bytes.len();
    let mut seed = seed ^ mix(seed ^ secret[2], secret[1]);

    // The two words the finalizer consumes. Everything before it exists
    // to fill these and to stir `seed`.
    let a: u64;
    let b: u64;

    // Bytes not yet folded into `seed`. It stays equal to `len` on every
    // path except the bulk loop, and the finalizer mixes it in, so two
    // inputs of different lengths cannot finish alike by accident.
    let mut remaining = len;

    if len <= 16 {
        if len >= 4 {
            seed ^= len as u64;
            if len >= 8 {
                a = read64(bytes, 0);
                b = read64(bytes, len - 8);
            } else {
                a = read32(bytes, 0);
                b = read32(bytes, len - 4);
            }
        } else if len > 0 {
            // Three bytes or fewer: first, last and middle, which for
            // len 1 are all the same byte. The shift by 45 is the
            // reference's; it spreads the first byte away from the last.
            a = ((bytes[0] as u64) << 45) | bytes[len - 1] as u64;
            b = bytes[len >> 1] as u64;
        } else {
            a = 0;
            b = 0;
        }
    } else {
        // Offset of the reference's moving `p` from the start of the
        // buffer. `remaining` is its `i`, and the two move together.
        let mut position = 0usize;

        if len > BULK_STRIDE {
            let mut seed1 = seed;
            let mut seed2 = seed;
            let mut seed3 = seed;
            let mut seed4 = seed;
            let mut seed5 = seed;
            let mut seed6 = seed;

            // Seven independent accumulators, each with its own secret
            // slot, so the multiplies do not serialize on one chain.
            loop {
                seed = mix(
                    read64(bytes, position) ^ secret[0],
                    read64(bytes, position + 8) ^ seed,
                );
                seed1 = mix(
                    read64(bytes, position + 16) ^ secret[1],
                    read64(bytes, position + 24) ^ seed1,
                );
                seed2 = mix(
                    read64(bytes, position + 32) ^ secret[2],
                    read64(bytes, position + 40) ^ seed2,
                );
                seed3 = mix(
                    read64(bytes, position + 48) ^ secret[3],
                    read64(bytes, position + 56) ^ seed3,
                );
                seed4 = mix(
                    read64(bytes, position + 64) ^ secret[4],
                    read64(bytes, position + 72) ^ seed4,
                );
                seed5 = mix(
                    read64(bytes, position + 80) ^ secret[5],
                    read64(bytes, position + 88) ^ seed5,
                );
                seed6 = mix(
                    read64(bytes, position + 96) ^ secret[6],
                    read64(bytes, position + 104) ^ seed6,
                );
                position += BULK_STRIDE;
                remaining -= BULK_STRIDE;

                if remaining <= BULK_STRIDE {
                    break;
                }
            }

            // The reference's tree, in its order: xor is associative but
            // these are u64 and the order is observable through nothing —
            // it is kept identical anyway, because a transcription that
            // reorders is a transcription nobody can check by eye.
            seed ^= seed1;
            seed2 ^= seed3;
            seed4 ^= seed5;
            seed ^= seed6;
            seed2 ^= seed4;
            seed ^= seed2;
        }

        // The 16-byte steps between the bulk loop and the final pair,
        // written as a cascade because each depends on the one above.
        if remaining > 16 {
            seed = mix(
                read64(bytes, position) ^ secret[2],
                read64(bytes, position + 8) ^ seed,
            );
            if remaining > 32 {
                seed = mix(
                    read64(bytes, position + 16) ^ secret[2],
                    read64(bytes, position + 24) ^ seed,
                );
                if remaining > 48 {
                    seed = mix(
                        read64(bytes, position + 32) ^ secret[1],
                        read64(bytes, position + 40) ^ seed,
                    );
                    if remaining > 64 {
                        seed = mix(
                            read64(bytes, position + 48) ^ secret[1],
                            read64(bytes, position + 56) ^ seed,
                        );
                        if remaining > 80 {
                            seed = mix(
                                read64(bytes, position + 64) ^ secret[2],
                                read64(bytes, position + 72) ^ seed,
                            );
                            if remaining > 96 {
                                seed = mix(
                                    read64(bytes, position + 80) ^ secret[1],
                                    read64(bytes, position + 88) ^ seed,
                                );
                            }
                        }
                    }
                }
            }
        }

        // The reference reads these at `p + i - 16` and `p + i - 8`.
        // `position` is `len - remaining` on every path that reaches
        // here, so both resolve to the last sixteen bytes of the buffer
        // whatever the loop did — which is also why they are in bounds
        // for `remaining` below 16.
        debug_assert_eq!(position, len - remaining);
        a = read64(bytes, len - 16) ^ remaining as u64;
        b = read64(bytes, len - 8);
    }

    let (low, high) = multiply_wide(a ^ secret[1], b ^ seed);
    mix(low ^ secret[7], high ^ secret[1] ^ remaining as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bulk loop's exit condition is `> 112`, not `>= 112`, so an
    /// input of exactly one stride never enters it and one byte more
    /// enters it once. Both sides are transcribed rather than derived,
    /// and an off-by-one here changes the hash of every long input
    /// without changing any short one — the vector table catches it,
    /// this names it.
    #[test]
    fn the_bulk_loop_boundary_is_where_the_reference_puts_it() {
        let input = vec![0x5au8; BULK_STRIDE + 1];

        let at_stride = hash(&input[..BULK_STRIDE], 0, &DEFAULT_SECRET);
        let over_stride = hash(&input, 0, &DEFAULT_SECRET);

        assert_ne!(at_stride, over_stride);
    }

    /// The length is folded in twice — into `seed` for short inputs, into
    /// the finalizer always — so two inputs that share a prefix and
    /// differ in length do not collide through the tail read, which for
    /// both would cover the same final sixteen bytes.
    #[test]
    fn length_separates_inputs_that_share_a_tail() {
        let long = vec![0u8; 64];

        assert_ne!(
            hash(&long, 0, &DEFAULT_SECRET),
            hash(&long[..48], 0, &DEFAULT_SECRET)
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
