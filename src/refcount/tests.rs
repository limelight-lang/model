use super::*;

fn retain(header: &mut RcHeader) {
    unsafe { ll_retain(header) }
}

fn release(header: &mut RcHeader) -> bool {
    unsafe { ll_release(header) }
}

#[cfg(feature = "rc-walk")]
mod the_half_of_the_header_the_collector_claims;
mod the_header_the_compiler_shares;
mod what_the_category_decides;
