//! A box is a heap entity whatever the array's category, so boxing
//! an element of an arena array crosses the boundary twice: the
//! element enters a longer-lived holder and is counted as an escape,
//! and the box enters the arena entry, which logs its release
//! against the reset. After a promotion the write reads the owner's
//! category at the call rather than a cached answer, the header
//! having been rewritten a moment before.

use super::*;

/// The other crossing the heap box forces: the element enters a
/// longer-lived holder, so an arena element becomes an escapee and
/// outlives the request that made it. Without the gain the reset
/// frees the object while the box still names it, which the
/// destructor count sees as a death one reset too early.
#[test]
fn boxing_an_arena_element_counts_its_escape() {
    let _g = crate::memory::block_pool::test_guard();
    use std::sync::atomic::AtomicUsize;
    static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn counting(_o: *mut Object) {
        DESTRUCTS.fetch_add(1, Ordering::Relaxed);
    }

    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("Boxed")
        .destructor(counting as *const ())
        .build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    let a = unsafe { ll_array_new(MemoryCategory::RequestArena) };
    let mut named = Value::entity(Tag::Array, a as *mut RcHeader);
    let slot: *mut Value = &raw mut named;
    let (keeper, keeper_slot, boxed) = unsafe {
        let target = new_constructed(context_ptr, cls, MemoryCategory::RequestArena);
        assert!(set(
            context_ptr,
            MemoryCategory::RequestArena,
            slot,
            Key::Int(0),
            Value::entity(Tag::Object, target as *mut RcHeader),
        ));
        let boxed = make_ref(context_ptr, MemoryCategory::RequestArena, slot, Key::Int(0));
        assert!(!boxed.is_null());

        // A heap holder for the box, so the box is what outlives the
        // request and the object's survival is the box's doing.
        let holder_cls = ClassBuilder::new("BoxKeeper").prop("r", true).build();
        let keeper = new_constructed(context_ptr, holder_cls, MemoryCategory::GcHeap);
        let keeper_slot = Object::prop_at(keeper, 16);
        assert!(crate::memory::barrier::ref_store(
            arena_ptr,
            keeper as *mut RcHeader,
            keeper_slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Reference, boxed as *mut RcHeader),
        ));
        (keeper, keeper_slot, boxed)
    };

    named = Value::null();
    let _ = named;
    unsafe { crate::promote::arena_reset_full(arena_ptr) };
    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        0,
        "the reset freed an arena object a heap box still named"
    );

    unsafe {
        let survivor = (*boxed).value;
        assert_eq!(survivor.tag(), Tag::Object, "the box lost its element");
        assert_eq!(
            crate::object::header_category(survivor.entity_ptr()),
            MemoryCategory::GcHeap,
            "the survivor was not promoted out of the arena"
        );
        crate::memory::barrier::write_value_slot(keeper_slot, Value::null());
        crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, boxed as *mut RcHeader);
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            1,
            "the object outlived the last holder of its box"
        );
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
}

/// A whole request that takes a reference
/// ends with the box freed and no arena block retained. The box is a
/// heap entity inside an arena array, so the only thing that can free
/// it is the release the entry logged — the mechanism the ruling
/// leans on, exercised through `arena_reset_full` rather than by
/// draining the log by hand.
#[test]
fn a_request_that_takes_a_reference_ends_holding_nothing() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);
    let retained_before = crate::memory::retained::snapshot().len();

    let cls = ClassBuilder::new("Plain").build();
    let a = unsafe { ll_array_new(MemoryCategory::RequestArena) };
    let mut holder = Value::entity(Tag::Array, a as *mut RcHeader);
    let slot: *mut Value = &raw mut holder;
    let boxed = unsafe {
        let target = new_constructed(context_ptr, cls, MemoryCategory::RequestArena);
        assert!(set(
            context_ptr,
            MemoryCategory::RequestArena,
            slot,
            Key::Int(0),
            Value::entity(Tag::Object, target as *mut RcHeader),
        ));
        make_ref(context_ptr, MemoryCategory::RequestArena, slot, Key::Int(0))
    };

    assert!(!boxed.is_null());
    assert_eq!(
        unsafe { crate::object::header_category(boxed as *const RcHeader) },
        MemoryCategory::GcHeap
    );

    // The request ends: no live stack, so the local names go first.
    holder = Value::null();
    let _ = holder;
    unsafe { crate::promote::arena_reset_full(arena_ptr) };

    let mut alive = Vec::new();
    unsafe { crate::memory::heap::for_each_entity_slot(|e| alive.push(e as usize)) };
    assert!(
        !alive.contains(&(boxed as usize)),
        "the reference box outlived the request that made it"
    );
    assert_eq!(
        crate::memory::retained::snapshot().len(),
        retained_before,
        "the request retained a block on the way out"
    );
    crate::memory::context::set_current_context(std::ptr::null_mut());
}

/// A promoted array takes its next storage from the heap, and what
/// makes it so is that the write reads the owner's category at the
/// call. Promotion rewrites the header and nothing else: the array
/// answered `RequestArena` a moment ago, and a caller still holding
/// that answer would allocate out of whatever arena is mounted, whose
/// reset then hands the chunk back with a live heap array pointing
/// into it — a use-after-free rather than the leak a refusal looks
/// like (`dev/DECISIONS.md`, "the `RcHeader` is the only authority
/// on which memory an entity lives in").
///
/// The table cannot make this test itself since S10: it is handed a
/// category and routes by it, so what is under test is the write
/// above it. The array is left empty before the header changes, so
/// the first storage is the one measured and no old storage has to be
/// freed out of an arena block the reset never stamped.
#[test]
fn a_promoted_array_takes_its_next_storage_from_the_heap() {
    use crate::memory::block_pool::{BLOCK_KIND_BUFFER, BLOCK_MASK};
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    let a = unsafe { ll_array_new(MemoryCategory::RequestArena) };
    // What promotion does to a survivor, and the whole of what the
    // write needs from it: clear the category bits, which leaves 00 —
    // the GC heap (`promote.rs`).
    unsafe { (*a).rc.flags &= !crate::refcount::MEMORY_CATEGORY_MASK };

    let class = ClassBuilder::new("PromotedHolder").prop("a", true).build();
    let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
    let slot = unsafe { Object::prop_at(h, 16) };
    unsafe {
        assert!(crate::memory::barrier::ref_store(
            arena_ptr,
            h as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, a as *mut RcHeader),
        ));
        ll_release(a as *mut RcHeader);

        assert!(set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot,
            Key::Int(1),
            Value::int(1)
        ));
        assert_eq!(
            (*slot).entity_ptr() as *mut LLArray,
            a,
            "the write separated, so the storage below is a copy's"
        );

        let storage = crate::array::entity::storage_address(a);
        assert!(!storage.is_null(), "the write allocated no storage");
        let kind = *(((storage as usize) & !BLOCK_MASK) as *const u32);
        assert_eq!(
            kind, BLOCK_KIND_BUFFER,
            "the storage came from the arena the array was promoted out of"
        );

        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}
