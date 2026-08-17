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
//! | consumer | use |
//! |---|---|
//! | `array/table.rs`, `draw_salt` | the salt: a keyed hash of the storage address under this key |
//! | `array/table.rs`, `strong_hash` | the escalated hash: keyed by this key together with the table's salt |
//! | the long-key function, when it arrives | all 32 bytes as its 256-bit key (`rfc/model/strings.md`, "Seeding") |
//!
//! The keyed hash is the only sanctioned derivation because an avalanche
//! of `value ^ word` is a bijection: one recovered output beside one
//! known input hands the word back (`array/table.rs`, `draw_salt`'s
//! doc). Splitting words would also leave the long function less than
//! the 256 bits `rfc/model/strings.md` names for it.
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
