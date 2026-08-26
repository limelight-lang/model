use super::*;

fn retain(header: &mut RcHeader) {
    unsafe { ll_retain(header) }
}

fn release(header: &mut RcHeader) -> bool {
    unsafe { ll_release(header) }
}

mod the_flags_half_the_mutator_leaves_alone;
mod the_header_the_compiler_shares;
mod what_the_category_decides;
mod who_may_read_a_header;
