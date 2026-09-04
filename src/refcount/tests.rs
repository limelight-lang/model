use super::*;

fn retain(header: &mut RcHeader) {
    unsafe { ll_retain(header) }
}

fn release(header: &mut RcHeader) -> bool {
    unsafe { ll_release(header) }
}

mod the_candidate_gate;
mod the_flags_half_the_mutator_leaves_alone;
mod the_header_the_compiler_shares;
mod the_three_states_of_a_slot;
mod the_widths_the_mutator_uses;
mod what_the_category_decides;
mod who_may_read_a_header;
