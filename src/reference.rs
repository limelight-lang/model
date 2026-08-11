//! The reference box (`&`): `RcHeader | Value`, the entity behind PHP
//! references (`rfc/model/values.md`, "Reference Box"). Variables bound
//! by `&` point at the same box — the model's only extra indirection,
//! paid only by code that uses `&` (the `zend_reference` design). The
//! typed slot-reference variant (`&$obj->typedProp`) waits for the type
//! system and is distinguished by a box-header flag when it arrives.
//!
//! The first non-object entity kind the crate produces (A2): no class
//! pointer at `+8` — the kind field in the header is what makes the
//! bare pointer self-describing at teardown
//! (`rfc/model/classes.md`, "Entity kind and non-object teardown").

use crate::journal::kinds::journal_event;
use crate::refcount::{EntityKind, MemoryCategory, RcHeader};
use crate::value::Value;

/// The reference box: header + one Value slot. 24 bytes.
#[repr(C)]
pub struct LLReference {
    pub rc: RcHeader,
    pub value: Value,
}

/// Allocate a reference box, its Value slot `null`. Null is a refusal.
///
/// The box comes back at count one and that reference is the caller's:
/// whatever slot the box is published into either takes it over or takes
/// one of its own, and nothing else holds the box until then.
///
/// **The box is always a GC-heap entity, and the caller has no say in
/// it** (`dev/DECISIONS.md`, 2026-08-08). The rule a copy of an array
/// applies — share a box with two holders, unwrap one with a single
/// holder — needs an exact holder count at the moment of duplication,
/// and the heap is the one place this runtime keeps one: a heap non-COW
/// box is counted by `ll_retain` and `ll_release` with no special case
/// anywhere. Counting a box *in the arena* would break "counted or
/// escaping, never both" and put a kind test on the retain/release fast
/// path of every arena entity. That is why the factory takes neither a
/// category nor a context: it has nothing to choose and nothing to
/// resolve.
///
/// Same commissioning contract as the object factory: body first, the
/// header published LAST as one 8-byte store, so an entity-block slot
/// reads refcount 0 until the box is fully formed
/// (`rfc/model/gc/rc-walk.md`, Phase 1).
pub fn ll_reference_new() -> *mut LLReference {
    let size = size_of::<LLReference>();
    let mem = unsafe {
        crate::memory::routing::entity_alloc_in(std::ptr::null_mut(), MemoryCategory::GcHeap, size)
    };

    if mem.is_null() {
        return std::ptr::null_mut();
    }

    let boxed = mem as *mut LLReference;
    unsafe {
        (*boxed).value = Value::null();
        crate::refcount::publish_header(
            boxed as *mut RcHeader,
            RcHeader::new(MemoryCategory::GcHeap, EntityKind::Reference.to_flags()),
        );
    }

    boxed
}

/// C ABI entry for [`ll_reference_new`].
#[unsafe(export_name = "ll_reference_new")]
pub extern "C" fn ll_reference_new_abi() -> *mut LLReference {
    ll_reference_new()
}

/// Teardown for a reference box whose count reached zero (or that a
/// collector owns): release the one Value through the barrier's drop,
/// then free the slot. No destructor, no resurrection — a box holds a
/// slot, not behavior.
///
/// The free is unconditional because [`ll_reference_new`] is the only
/// door and every box it makes is a GC-heap entity; a box in any other
/// category would be memory this call leaks rather than frees, so the
/// category is asserted rather than branched on.
///
/// # Safety
/// `boxed` must be a live reference box.
pub(crate) unsafe fn reference_die(boxed: *mut LLReference) {
    journal_event!(
        crate::journal::kinds::KIND_ENTITY_DEATH,
        boxed as u64,
        EntityKind::Reference as u64,
        0
    );
    let owner_cat = unsafe { crate::object::header_category(boxed as *const RcHeader) };
    debug_assert_eq!(
        owner_cat,
        MemoryCategory::GcHeap,
        "a reference box is a heap entity in every case"
    );
    let v = unsafe { (*boxed).value };
    if v.is_refcounted() {
        unsafe {
            crate::memory::barrier::write_value_slot(&raw mut (*boxed).value, Value::null());
            crate::memory::barrier::drop_ref(owner_cat, v.entity_ptr());
        }
    }

    unsafe { crate::memory::stdapi::ll_free(boxed as *mut u8) };
}

#[cfg(test)]
mod tests;
