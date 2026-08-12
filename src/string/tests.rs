use super::*;
use crate::memory::arena::Arena;
use crate::refcount::{ENTITY_KIND_MASK, ll_release};

mod the_cached_hash;
mod the_cow_rule_and_the_order_it_reads_in;
mod the_inline_layout;
mod the_layout_size_chooses;
mod the_length_gate;
mod the_out_of_line_layout;
mod the_payload_and_who_frees_it;
