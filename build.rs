//! Tells cargo that `LL_HASH_SEED` is an input to the build.
//!
//! `src/hash/seed.rs` reads it through `option_env!`, which cargo cannot
//! see: without this line a rebuild after changing the variable reuses the
//! artifact compiled under the old seed. That failure is the silent kind —
//! the hash simply differs from what the build was supposed to produce —
//! which is the class of defect the seed machinery exists to make loud.

fn main() {
    println!("cargo::rerun-if-env-changed=LL_HASH_SEED");
}
