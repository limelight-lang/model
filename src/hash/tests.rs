use super::*;

/// Content of the generated inputs in [`vectors::FILLED_VECTORS`], byte
/// by byte. `vendor/rapidhash/generate_vectors.c` computes the same
/// value in C; the two definitions are a pair and neither may move
/// alone.
fn filler_byte(index: usize) -> u8 {
    (index as u8).wrapping_mul(167).wrapping_add(13)
}

mod the_port_against_its_reference;
mod what_the_module_guarantees;
