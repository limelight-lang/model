//! Which members of a finalization run a `__destruct`, and how often.
//!
//! One per member that owes one, and a member of a kind carrying no class word
//! is passed over. The gate names the two kinds that carry a class pointer at
//! `+8`, which are the two `object::ll_entity_die` sends to `ll_object_die`;
//! the two sets agree by assignment rather than by a shared predicate, so a
//! kind added to one and not the other is caught by neither
//! (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation", step 4).

use super::*;
use crate::array::entity::ll_array_new;
use crate::array::testing::push;
use crate::memory::barrier::ref_store;
use crate::value::{Tag, Value};

/// Destructor bodies run since a case last cleared it.
static DESTRUCTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
    DESTRUCTOR_RUNS.fetch_add(1, Ordering::Relaxed);
}

/// A ring of two objects of `class`, held by nothing else and read as
/// unreachable by a trace that has released its rows.
///
/// # Safety
/// `arena` is this thread's and `class` carries one Box property at
/// `prop_offset(0)`.
unsafe fn unreachable_ring(
    arena: &mut Arena,
    class: *const crate::class::Class,
) -> [*mut Object; 2] {
    let mut context = LLContext { arena: &mut *arena };
    let first = unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) };
    let second = unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) };
    unsafe {
        store_prop(arena, first, prop_offset(0), second);
        store_prop(arena, second, prop_offset(0), first);
        assert!(!ll_release(first as *mut RcHeader));
        assert!(!ll_release(second as *mut RcHeader));
    }

    let mut shadow_arena = unsafe { traced_unreachable_from(first, &[first, second]) };
    shadow_arena.reset();
    [first, second]
}

#[test]
fn a_pending_destructor_runs_once_over_the_whole_finalization() {
    let _g = test_guard();
    let node = ClassBuilder::new("FinalizationCountingNode")
        .prop("next", true)
        .destructor(counting_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let ring = unsafe { unreachable_ring(&mut arena, node) };
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);

    let mut finalization = Finalization::begin();
    let mut members = [ring[0] as *mut RcHeader, ring[1] as *mut RcHeader];
    assert_eq!(
        unsafe { finalization.confirm(&mut members) },
        ValidationResult::Unreachable
    );

    let mut pass = finalization.seal().destructors();
    unsafe { pass.run(&members) };
    assert_eq!(
        DESTRUCTOR_RUNS.load(Ordering::Relaxed),
        2,
        "each member that owes a destructor runs it, and the pass reaches \
         every member of the component"
    );

    let mut revalidation = pass.close();
    let Revalidated::Unreachable(guarded) = (unsafe { revalidation.revalidate(&mut members) })
    else {
        panic!("a destructor that stores nothing leaves the ring unreachable");
    };

    unsafe { unwind_guarded_ring(&mut arena, ring) };
    unsafe { guarded.guards_released() };
    revalidation.close();

    assert_eq!(
        DESTRUCTOR_RUNS.load(Ordering::Relaxed),
        2,
        "and the ordinary death that follows runs neither of them again: \
         `DESTRUCTOR_RAN` is what makes the count exact rather than the pass"
    );
}

#[test]
fn a_member_carrying_no_class_word_is_passed_over() {
    let _g = test_guard();
    let node = ClassBuilder::new("FinalizationArrayRingNode")
        .prop("next", true)
        .destructor(counting_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let holder = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    let array = unsafe { ll_array_new(MemoryCategory::GcHeap) };

    unsafe {
        let slot = Object::prop_at(holder, prop_offset(0));
        assert!(ref_store(
            &mut arena,
            holder as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, array as *mut RcHeader),
        ));

        // `push` stores the word and counts nothing, so the element's
        // reference is retained here.
        ll_retain(holder as *mut RcHeader);
        assert!(push(
            array,
            Value::entity(Tag::Object, holder as *mut RcHeader)
        ));

        assert!(!ll_release(holder as *mut RcHeader));
        assert!(!ll_release(array as *mut RcHeader));
    }

    let mut shadow_arena = unsafe { traced_unreachable_from(holder, &[holder]) };
    shadow_arena.reset();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);

    let mut finalization = Finalization::begin();
    let mut members = [holder as *mut RcHeader, array as *mut RcHeader];
    assert_eq!(
        unsafe { finalization.confirm(&mut members) },
        ValidationResult::Unreachable,
        "the property and the element are the only two references there are"
    );

    let mut pass = finalization.seal().destructors();
    unsafe { pass.run(&members) };
    assert_eq!(
        DESTRUCTOR_RUNS.load(Ordering::Relaxed),
        1,
        "the object of the ring runs its destructor and the array runs none: \
         an array's `+8` is its storage head's version word rather than a \
         class pointer, and a dispatch that read it as one would form a \
         reference out of it"
    );

    let mut revalidation = pass.close();
    let Revalidated::Unreachable(guarded) = (unsafe { revalidation.revalidate(&mut members) })
    else {
        panic!("nothing outside the ring holds either member");
    };
    assert_eq!(guarded.members(), 2);

    unsafe {
        ll_retain(holder as *mut RcHeader);
        ll_retain(array as *mut RcHeader);
        crate::refcount::mutator_unguard_release(holder as *mut RcHeader);
        crate::refcount::mutator_unguard_release(array as *mut RcHeader);
        let slot = Object::prop_at(holder, prop_offset(0));
        assert!(ref_store(
            &mut arena,
            holder as *mut RcHeader,
            slot,
            array as *mut RcHeader,
            Value::null(),
        ));

        // The array's teardown releases the element, which is what puts
        // the holder back at the fixture's own reference.
        assert!(ll_release(array as *mut RcHeader));
        ll_entity_die(array as *mut RcHeader);
        assert!(ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    unsafe { guarded.guards_released() };
    revalidation.close();
}
