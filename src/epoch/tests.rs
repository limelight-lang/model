use super::*;
use crate::class::ClassBuilder;
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::memory::heap::for_each_entity_slot;
use crate::object::{Object, new_constructed};
use crate::refcount::{MemoryCategory, ll_release, ll_retain};
use crate::value::{Tag, Value};
use std::sync::atomic::AtomicUsize;

fn walked_addresses() -> Vec<usize> {
    let mut seen = Vec::new();
    unsafe { for_each_entity_slot(|e| seen.push(e as usize)) };
    seen
}

/// `a.child = b` as generated code leaves it: the slot owns one ref.
unsafe fn tie(a: *mut Object, offset: u32, b: *mut Object) {
    unsafe {
        Object::prop_at(a, offset).write(Value::entity(Tag::Object, b as *mut RcHeader));
    }
}

// No condemn helper: condemnation is collector-private since the
// eager-death amendment — posting the confirmation IS the
// collector's whole footprint on these tests.

static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
}

mod what_the_drain_does_with_a_verdict;
mod where_a_checkpoint_sits;
mod where_a_pickup_is_refused;
