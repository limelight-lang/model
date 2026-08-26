use super::*;
use crate::class::ClassBuilder;
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::new_constructed;
use crate::refcount::{MemoryCategory, ll_release};
use crate::test_support::{POOLED_FILLERS, RUN_FILLERS};
use crate::value::{Tag, Value};
use std::sync::atomic::{AtomicUsize, Ordering};

static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
}

/// Collect the addresses the enumerator currently yields. Tests assert
/// membership, never totals: the registry is process-global, and
/// other tests' leftovers (abandoned blocks with live objects) are
/// legitimately visible here.
fn walked_addresses() -> Vec<usize> {
    let mut seen = Vec::new();
    unsafe { for_each_entity_slot(|e| seen.push(e as usize)) };
    seen
}

/// Tie `a.child = b` the way generated code leaves it after
/// `$a->child = $b; unset($b);` — the slot owns one reference.
unsafe fn tie(a: *mut Object, offset: u32, b: *mut Object) {
    unsafe {
        Object::prop_at(a, offset).write(Value::entity(Tag::Object, b as *mut RcHeader));
    }
}

mod the_children_a_kind_has;
mod what_the_walk_enumerates;
