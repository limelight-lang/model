//! The per-process key: 32 bytes of keying material for the hash table's
//! flood ladder, drawn from the operating system once per process.
//!
//! This is the second keying value `rfc/model/strings.md` ("Seeding")
//! requires to be read independently rather than derived from a common
//! master: the short hash function is the weak one, and material shared
//! with it hands an attacker the long one along with it. Independence is
//! why the draw below reads `/dev/urandom` directly instead of going
//! through `RandomState`, which caches one `(k0, k1)` pair per thread —
//! any number of words drawn from it carries 128 bits and shares the
//! master the string seed comes from.
//!
//! **The key exists in every build.** The `hash-folding` axis fixes the
//! rapidhash seed, because folded constants depend on it; nothing may be
//! folded against this key, so it stays per-process random on both sides
//! of that option and this module carries no `cfg` arm for it. [`STAMP`]
//! covers what a compiled program may depend on, which is why it must
//! not cover this.
//!
//! **How a consumer takes the key is fixed here, once — whole, as the
//! key of a keyed hash, never split into words and never mixed
//! bijectively** (`rfc/model/maps.md`, "What the flood ladder becomes":
//! every secret the ladder draws comes from this key):
//!
//! - `array/table.rs`, `salt_from` — the salt: a keyed hash of the
//!   storage address under this key, taken by both draws;
//! - `array/table.rs`, `strong_hash` — the escalated hash: keyed by
//!   this key together with the table's salt;
//! - the long-key function, when it arrives — all 32 bytes as its
//!   256-bit key (`rfc/model/strings.md`, "Seeding").
//!
//! Consumers take the key through [`folded`] — the whole key compressed
//! to the 64-bit width their keyed primitive takes — rather than picking
//! a word, so no single word is exposed through a derived value and a
//! recovered `folded` does not hand back the 256-bit key the long
//! function will own (folding is a 256→64 compression). This is not a
//! claim of one-wayness: the short function's keyed primitive
//! (`array/table.rs`, `strong_hash`) is a placeholder invertible in
//! its key, so a timing oracle recovers the salt and through it
//! `folded`. `rfc/model/strings.md` ("Neither position is a defence")
//! concedes exactly that — a per-process key raises hash flooding from
//! reading a constant to mounting a timing attack and no further, and
//! the structural backstop is the flood ladder's bounded rebuilds, not
//! the secrecy of this key.
//!
//! A zero word stays as drawn: unlike the seed, no field stores a key
//! word to mean "not installed", so zero carries no sentinel meaning and
//! remapping it would only narrow the key.
//!
//! "Per process" is per-deployment under a pre-forking master, exactly as
//! for the seed: the fork inherits the drawn words (`dev/DECISIONS.md`,
//! "folding a literal key's hash is a build option, and it is off by
//! default").
//!
//! [`STAMP`]: super::seed::STAMP

use std::sync::LazyLock;

#[cfg(not(unix))]
compile_error!(
    "the per-process key is drawn from /dev/urandom, and this target has \
     no door: add an OS randomness read for it (Windows: BCryptGenRandom \
     or an equivalent) before building here"
);

/// The drawn key, memoized for the life of the process.
///
/// [`crate::hash::seed::ll_hash_seed_init`] is the intended trigger and
/// this lazy path the backstop, for the seed's own reasons: a consumer
/// must never observe two different keys, and the read of the operating
/// system belongs at startup rather than at whichever draw happens to be
/// first.
static WORDS: LazyLock<[u64; 4]> = LazyLock::new(draw);

/// The key's four words. Equal across calls and threads for the life of
/// the process; which word a caller may take is fixed in the module doc.
#[inline]
pub(crate) fn words() -> &'static [u64; 4] {
    &WORDS
}

/// The whole key compressed to 64 bits, for the short function's
/// consumers in the module-doc table — their keyed primitive takes a
/// 64-bit key until the long-key function arrives. A mix chain over all
/// four words rather than a word pick, so no single word is exposed
/// through any derived value.
#[inline]
pub(crate) fn folded() -> u64 {
    let words = words();
    let mut folded = words[0];
    for word in &words[1..] {
        // The splitmix64 finalizer: full avalanche per absorbed word.
        let mut x = folded ^ *word;
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        folded = x ^ (x >> 31);
    }

    folded
}

/// 32 fresh bytes from `/dev/urandom`, as four little-endian words.
///
/// A refused read aborts the process here, at the draw: on a host with
/// no `/dev/urandom` it is better to stop at startup —
/// `ll_hash_seed_init` forces this — than on the first flooded insert.
/// An explicit abort rather than a panic, because a panic during
/// [`LazyLock`] initialization poisons the lock and turns a transient
/// refusal — a full descriptor table, say — into every later `words`
/// call failing forever.
fn draw() -> [u64; 4] {
    // Miri's isolation forbids `open`, so under it the key is a
    // non-zero constant: every property but freshness holds, and the
    // freshness test is ignored there for exactly this reason
    // (`dev/WORKFLOW.md`, the sanctioned narrow exception).
    #[cfg(miri)]
    return [0xA5A5_A5A5_A5A5_A5A5; 4];

    #[cfg(not(miri))]
    {
        draw_from_os()
    }
}

/// The real draw, behind [`draw`]'s Miri exemption.
#[cfg(not(miri))]
fn draw_from_os() -> [u64; 4] {
    use std::io::Read;

    let mut bytes = [0u8; 32];
    let read = std::fs::File::open("/dev/urandom").and_then(|mut door| door.read_exact(&mut bytes));
    if let Err(refusal) = read {
        eprintln!("ll-model: reading 32 bytes from /dev/urandom failed: {refusal}");
        std::process::abort();
    }

    let mut words = [0u64; 4];
    for (word, chunk) in words.iter_mut().zip(bytes.chunks_exact(8)) {
        *word = u64::from_le_bytes(chunk.try_into().expect("an 8-byte chunk"));
    }

    words
}

/// The test window on the underlying draw: fresh bytes on every call,
/// where [`words`] memoizes.
#[cfg(test)]
pub(crate) fn draw_for_tests() -> [u64; 4] {
    draw()
}

#[cfg(test)]
mod tests;
