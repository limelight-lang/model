//! A refused child loses its edge at the smallest unit its holder's
//! layout leaves consistent: a hash entry's element is nulled with the
//! entry and its collision link standing, a hash entry's string key takes
//! the whole entry as a hole, the element beside it going too, a vector
//! element is nulled in place, and a cell outside the body goes through
//! the class's own group
//! (`dev/DECISIONS.md`, "a sever takes the smallest unit its holder's
//! layout leaves consistent, and never lands on a counted edge").
//!
//! Every case here arms `RefusedSurvivorSegments`, which refuses the
//! draws the survivor chain makes after the count it is given. The root is
//! admitted by compacting its escapee record and needs no cell, so only a
//! child is ever refused; an array is reached as a child of an escaping
//! object, so the three array cases let that one child in and refuse what
//! it names. A child that is not an arena entity is never offered to the
//! chain at all, which is how a case chooses which of an entry's two
//! occupants the refusal falls on.

use super::*;
use crate::array::table::Key;
use crate::memory::arena::RefusedSurvivorSegments;

/// Bytes whose slot agrees with `with`'s under `mask`, found by search
/// because a string's slot is its own hash under a per-process seed and
/// cannot be chosen — the same search `array::entity`'s copy tests make
/// for the integer families they need. A fresh table is neither reseeded
/// nor strong, so the derivation is `hash::hash_bytes` itself and a
/// candidate costs no entity.
fn bytes_sharing_a_slot_with(with: &[u8], mask: u64) -> Vec<u8> {
    let target = crate::hash::hash_bytes(with) & mask;
    for i in 0..100_000u32 {
        let candidate = format!("key{i}").into_bytes();
        if crate::hash::hash_bytes(&candidate) & mask == target {
            return candidate;
        }
    }

    panic!(
        "no candidate of 100000 shares a slot of {} with the key",
        mask + 1
    );
}

/// A refused element leaves the entry live with a null value, and leaves
/// the collision link the entry carries in its reserved bytes. A whole
/// `Value` store over that element would publish zeros over the link,
/// where zero is a legal entry index rather than an end of chain — the
/// lookup that follows would then walk a self-referencing entry.
///
/// The keys are 1 and 17, which share a slot at `nslots = 16`, so the
/// lookup of the second key reads the link the sever had to keep. The
/// chain length is asserted before the reset: two keys that fell into two
/// slots would leave this test measuring nothing.
#[test]
fn a_refused_element_keeps_the_entry_and_its_link() {
    let _g = crate::memory::block_pool::test_guard();
    let holder_cls = ClassBuilder::new("ElementCache").prop("last", true).build();
    let owner_cls = ClassBuilder::new("ElementOwner")
        .prop("items", true)
        .build();
    let child_cls = ClassBuilder::new("ElementChild").build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let holder = unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let owner =
        unsafe { new_constructed(&mut *context_ptr, owner_cls, MemoryCategory::RequestArena) };
    let child =
        unsafe { new_constructed(&mut *context_ptr, child_cls, MemoryCategory::RequestArena) };
    let array = unsafe { crate::array::testing::hash_array(MemoryCategory::RequestArena) };

    let link_before = unsafe {
        // `insert` stores the word and counts nothing, so the element's
        // reference is retained here, as the vector's `push` needs too.
        crate::refcount::ll_retain(child as *mut RcHeader);
        crate::array::testing::insert(
            array,
            Key::Int(1),
            Value::entity(Tag::Object, child as *mut RcHeader),
        )
        .expect("the table refused the first entry");
        crate::array::testing::insert(array, Key::Int(17), Value::int(99))
            .expect("the table refused the second entry");

        let (table, head) = crate::array::entity::as_table(array);
        assert_eq!(
            table.longest_chain(head),
            2,
            "the two keys fell into two slots, so no link is under test"
        );

        // An array never escapes on its own: the barrier copies a COW
        // value out of the arena instead. It reaches the reset as the
        // child of an arena object that did escape, which is also why the
        // refusal below admits one survivor first — that one is the array.
        let slot = Object::prop_at(owner, 16);
        assert!(crate::memory::barrier::ref_store(
            arena_ptr,
            owner as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, array as *mut RcHeader),
        ));
        store_prop(arena_ptr, holder, 16, owner);

        table.entry(head, 0).link()
    };

    let severed = {
        let _refused = RefusedSurvivorSegments::after(1);
        unsafe { arena_reset_full(&mut *arena_ptr) }
    };

    set_current_context(std::ptr::null_mut());
    unsafe {
        assert_eq!(severed, 1, "the one edge the arena could not record");
        let (table, head) = crate::array::entity::as_table(array);
        assert_eq!(table.len(), 2, "the entry died with its element");
        assert!(
            !crate::array::testing::get(array, Key::Int(1))
                .expect("the entry is gone")
                .is_refcounted(),
            "the refused child is still the element"
        );
        assert_eq!(
            crate::array::testing::get(array, Key::Int(17))
                .expect("the chain no longer reaches the second key")
                .as_int(),
            99
        );
        assert_eq!(
            table.entry(head, 0).link(),
            link_before,
            "the sever published zeros over the collision link"
        );

        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
}

/// A refused string key takes the whole entry, because a key word has no
/// null: below the sentinel limit it reads as an integer key rather than
/// as a hole. The entry becomes a hole, the live count drops by one, and
/// the entries around it keep their chains and their lookups.
///
/// The two surviving keys are heap strings, which the chain never offers
/// to the arena and so never refuses; the refused one is an arena string.
/// It shares a bucket with one of them, because a hole that keeps its
/// collision link is the whole difference between this and
/// `Table::remove`: a lookup of the key behind the hole is what reads that
/// link. The bucket is forged by search, a string's slot coming from a
/// per-process seed, and the chain is asserted before the reset.
#[test]
fn a_refused_string_key_holes_the_whole_entry() {
    let _g = crate::memory::block_pool::test_guard();
    let holder_cls = ClassBuilder::new("KeyCache").prop("last", true).build();
    let owner_cls = ClassBuilder::new("KeyOwner").prop("items", true).build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let holder = unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let owner =
        unsafe { new_constructed(&mut *context_ptr, owner_cls, MemoryCategory::RequestArena) };
    let array = unsafe { crate::array::testing::hash_array(MemoryCategory::RequestArena) };
    let alpha =
        unsafe { crate::string::ll_string_new(context_ptr, MemoryCategory::GcHeap, b"alpha") };
    let beta =
        unsafe { crate::string::ll_string_new(context_ptr, MemoryCategory::GcHeap, b"beta") };

    let gone = unsafe {
        // The first insert is what gives the table its storage, so the
        // slot count the search agrees on is read after it.
        crate::array::testing::insert(array, Key::Str(alpha), Value::int(1))
            .expect("the table refused a key");
        let mask = crate::array::entity::as_table(array).1.nslots() as u64 - 1;
        let colliding = bytes_sharing_a_slot_with(b"alpha", mask);
        crate::string::ll_string_new(context_ptr, MemoryCategory::RequestArena, &colliding)
    };

    unsafe {
        // The insert consumes the caller's key reference; every lookup
        // below reaches its key through the one the table now holds.
        crate::array::testing::insert(array, Key::Str(gone), Value::int(2))
            .expect("the table refused a key");
        crate::array::testing::insert(array, Key::Str(beta), Value::int(3))
            .expect("the table refused a key");
        assert_eq!(
            crate::array::testing::table(array)
                .longest_chain(crate::array::entity::as_table(array).1),
            2,
            "the forged pair fell into two slots, so no link is under test"
        );

        // An array never escapes on its own: the barrier copies a COW
        // value out of the arena instead. It reaches the reset as the
        // child of an arena object that did escape, which is also why the
        // refusal below admits one survivor first — that one is the array.
        let slot = Object::prop_at(owner, 16);
        assert!(crate::memory::barrier::ref_store(
            arena_ptr,
            owner as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, array as *mut RcHeader),
        ));
        store_prop(arena_ptr, holder, 16, owner);
    }

    let severed = {
        let _refused = RefusedSurvivorSegments::after(1);
        unsafe { arena_reset_full(&mut *arena_ptr) }
    };

    set_current_context(std::ptr::null_mut());
    unsafe {
        assert_eq!(severed, 1, "the one edge the arena could not record");
        let (table, head) = crate::array::entity::as_table(array);
        assert!(
            table.entry(head, 1).is_hole(),
            "the key word was emptied in place rather than holed"
        );
        assert_eq!(table.len(), 2, "the live count still counts the hole");
        assert_eq!(
            crate::array::testing::get(array, Key::Str(alpha))
                .expect("the entry before the hole is unreachable")
                .as_int(),
            1
        );
        assert_eq!(
            crate::array::testing::get(array, Key::Str(beta))
                .expect("the entry after the hole is unreachable")
                .as_int(),
            3
        );

        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
}

/// A vector keeps nothing in an element's reserved bytes, so a refused
/// element is nulled where it stands and the elements around it are
/// untouched.
#[test]
fn a_refused_vector_element_is_nulled_in_place() {
    let _g = crate::memory::block_pool::test_guard();
    let holder_cls = ClassBuilder::new("VectorCache").prop("last", true).build();
    let owner_cls = ClassBuilder::new("VectorOwner").prop("items", true).build();
    let child_cls = ClassBuilder::new("VectorChild").build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let holder = unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let owner =
        unsafe { new_constructed(&mut *context_ptr, owner_cls, MemoryCategory::RequestArena) };
    let child =
        unsafe { new_constructed(&mut *context_ptr, child_cls, MemoryCategory::RequestArena) };
    let array = unsafe { crate::array::entity::ll_array_new(MemoryCategory::RequestArena) };

    unsafe {
        crate::refcount::ll_retain(child as *mut RcHeader);
        assert!(crate::array::testing::push(
            array,
            Value::entity(Tag::Object, child as *mut RcHeader)
        ));
        assert!(crate::array::testing::push(array, Value::int(7)));

        // An array never escapes on its own: the barrier copies a COW
        // value out of the arena instead. It reaches the reset as the
        // child of an arena object that did escape, which is also why the
        // refusal below admits one survivor first — that one is the array.
        let slot = Object::prop_at(owner, 16);
        assert!(crate::memory::barrier::ref_store(
            arena_ptr,
            owner as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, array as *mut RcHeader),
        ));
        store_prop(arena_ptr, holder, 16, owner);
    }

    let severed = {
        let _refused = RefusedSurvivorSegments::after(1);
        unsafe { arena_reset_full(&mut *arena_ptr) }
    };

    set_current_context(std::ptr::null_mut());
    unsafe {
        assert_eq!(severed, 1, "the one edge the arena could not record");
        assert!(
            !crate::array::testing::at(array, 0)
                .expect("the element is gone rather than empty")
                .is_refcounted(),
            "the refused child is still the element"
        );
        assert_eq!(crate::array::testing::at(array, 1).unwrap().as_int(), 7);

        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
}

/// A cell outside the body is the group's to empty, and the group empties
/// the cell it is given: the block's other cell keeps the child it held,
/// which is what a walk after the sever reads.
///
/// The refused child is in the **second** cell and the kept one in the
/// first. A `sever_one` that ignored its `Cell` and emptied the block from
/// the front would pass with them the other way round, and acting on the
/// named cell is the whole reason the member exists.
#[test]
fn a_refused_outside_cell_goes_through_the_group() {
    use crate::test_support::outside_block;
    let _g = crate::memory::block_pool::test_guard();
    let holder_cls = ClassBuilder::new("WakerSeveredHolder")
        .prop("waker", true)
        .build();
    let waker_cls = outside_block::class("WakerSevered");
    let child_cls = ClassBuilder::new("WakerSeveredChild").build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let holder = unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let waker =
        unsafe { new_constructed(&mut *context_ptr, waker_cls, MemoryCategory::RequestArena) };
    let refused =
        unsafe { new_constructed(&mut *context_ptr, child_cls, MemoryCategory::RequestArena) };
    let kept = unsafe { new_constructed(&mut *context_ptr, child_cls, MemoryCategory::GcHeap) };

    unsafe {
        outside_block::install_block(context_ptr, waker);
        assert!(outside_block::store_cell(
            arena_ptr,
            waker,
            0,
            std::ptr::null_mut(),
            Value::entity(Tag::Object, kept as *mut RcHeader),
        ));
        assert!(outside_block::store_cell(
            arena_ptr,
            waker,
            1,
            std::ptr::null_mut(),
            Value::entity(Tag::Object, refused as *mut RcHeader),
        ));
        store_prop(arena_ptr, holder, 16, waker);
        assert!(!crate::refcount::ll_release(kept as *mut RcHeader));
    }

    let severed = {
        let _refused = RefusedSurvivorSegments::arm();
        unsafe { arena_reset_full(&mut *arena_ptr) }
    };

    set_current_context(std::ptr::null_mut());
    unsafe {
        assert_eq!(severed, 1, "the one edge the arena could not record");
        let mut seen = Vec::new();
        crate::cells::trace_entity(waker as *mut RcHeader, |c| seen.push(c));
        assert_eq!(
            seen,
            vec![kept as *mut RcHeader],
            "the group severed the wrong cell, or more than one"
        );

        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
}

/// The element beside a refused key loses its edge with the entry, and
/// that is what `displaced` is for: the reset is handed every child the
/// unit emptied, not only the one it refused.
///
/// The element here is an arena object the chain admitted a moment
/// earlier, through a second property of the same owner, so its count is
/// rebuilt from the edges that remain — one — and the lost edge costs
/// nothing. A key sever with the element branch deleted leaves this test
/// green in the heap and red in the journal, which is why the count is
/// read rather than the memory.
#[test]
fn a_refused_key_takes_the_element_beside_it() {
    let _g = crate::memory::block_pool::test_guard();
    let holder_cls = ClassBuilder::new("PairCache").prop("last", true).build();
    let owner_cls = ClassBuilder::new("PairOwner")
        .prop("items", true)
        .prop("also", true)
        .build();
    let child_cls = ClassBuilder::new("PairChild").build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let holder = unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let owner =
        unsafe { new_constructed(&mut *context_ptr, owner_cls, MemoryCategory::RequestArena) };
    let child =
        unsafe { new_constructed(&mut *context_ptr, child_cls, MemoryCategory::RequestArena) };
    let array = unsafe { crate::array::testing::hash_array(MemoryCategory::RequestArena) };
    let key =
        unsafe { crate::string::ll_string_new(context_ptr, MemoryCategory::RequestArena, b"gone") };

    unsafe {
        crate::refcount::ll_retain(child as *mut RcHeader);
        crate::array::testing::insert(
            array,
            Key::Str(key),
            Value::entity(Tag::Object, child as *mut RcHeader),
        )
        .expect("the table refused the entry");

        let slot = Object::prop_at(owner, 16);
        assert!(crate::memory::barrier::ref_store(
            arena_ptr,
            owner as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, array as *mut RcHeader),
        ));
        // The second edge to the element, and the reason the refusal here
        // admits two survivors: the array and the element behind it.
        store_prop(arena_ptr, owner, crate::test_support::prop_offset(1), child);
        store_prop(arena_ptr, holder, 16, owner);
    }

    let severed = {
        let _refused = RefusedSurvivorSegments::after(2);
        unsafe { arena_reset_full(&mut *arena_ptr) }
    };

    set_current_context(std::ptr::null_mut());
    unsafe {
        assert_eq!(severed, 1, "the one edge the arena could not record");
        let (table, head) = crate::array::entity::as_table(array);
        assert!(table.entry(head, 0).is_hole(), "the entry outlived its key");
        assert_eq!(table.len(), 0);

        let mut seen = Vec::new();
        crate::cells::trace_entity(array as *mut RcHeader, |c| seen.push(c));
        assert!(seen.is_empty(), "the element kept the edge the entry lost");
        assert_eq!(
            crate::refcount::entity_refcount(child as *mut RcHeader),
            1,
            "the count was rebuilt from an edge the sever had emptied"
        );

        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
}

/// The element beside a refused key may carry `IS_ESCAPEE`, and the entry
/// is severed all the same: the edge the sever empties was never counted.
/// A store into a holder still `RequestArena` counts nothing into an arena
/// child, so the escapee's count is the one `escape_gain` wrote for its
/// heap holder, `mark_root` keeps it, and the child dies of that holder's
/// release and of nothing else (Sage, 2026-09-13, `dev/plans/S47.md`).
///
/// A weak cell is the observer, because the count cannot be read from a
/// freed header: it resolves to the element until the keeper dies and to
/// null afterwards.
#[test]
fn a_refused_key_severs_an_entry_whose_element_escaped() {
    let _g = crate::memory::block_pool::test_guard();
    let holder_cls = ClassBuilder::new("EscapeeCache").prop("last", true).build();
    let owner_cls = ClassBuilder::new("EscapeeOwner")
        .prop("items", true)
        .build();
    let child_cls = ClassBuilder::new("EscapeeChild").build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let holder = unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let keeper = unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let owner =
        unsafe { new_constructed(&mut *context_ptr, owner_cls, MemoryCategory::RequestArena) };
    let child =
        unsafe { new_constructed(&mut *context_ptr, child_cls, MemoryCategory::RequestArena) };
    let array = unsafe { crate::array::testing::hash_array(MemoryCategory::RequestArena) };
    let key =
        unsafe { crate::string::ll_string_new(context_ptr, MemoryCategory::RequestArena, b"gone") };

    let cell = unsafe {
        crate::refcount::ll_retain(child as *mut RcHeader);
        crate::array::testing::insert(
            array,
            Key::Str(key),
            Value::entity(Tag::Object, child as *mut RcHeader),
        )
        .expect("the table refused the entry");

        let slot = Object::prop_at(owner, 16);
        assert!(crate::memory::barrier::ref_store(
            arena_ptr,
            owner as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, array as *mut RcHeader),
        ));
        // The escape that gives the element its record and its count, and
        // the reason it is admitted as a root rather than pushed.
        store_prop(arena_ptr, keeper, 16, child);
        store_prop(arena_ptr, holder, 16, owner);
        crate::weak::ll_weakref_create(context_ptr, child as *mut RcHeader)
    };

    let severed = {
        let _refused = RefusedSurvivorSegments::after(1);
        unsafe { arena_reset_full(&mut *arena_ptr) }
    };

    set_current_context(std::ptr::null_mut());
    unsafe {
        assert_eq!(severed, 1, "the one edge the arena could not record");
        let (table, head) = crate::array::entity::as_table(array);
        assert!(table.entry(head, 0).is_hole(), "the entry outlived its key");
        assert_eq!(
            crate::refcount::entity_category(child),
            MemoryCategory::GcHeap,
            "the escapee was not promoted"
        );
        assert_eq!(
            crate::refcount::entity_refcount(child as *mut RcHeader),
            1,
            "the count is the keeper's edge and nothing the sever left behind"
        );
        // The get hands back a retained reference, so it is given up
        // again: kept, it would be the holder this case says does not
        // exist.
        let seen = crate::weak::ll_weakref_get(cell);
        assert_eq!(seen, child as *mut RcHeader, "the element died early");
        assert!(
            !crate::refcount::ll_release(seen),
            "the keeper still holds it"
        );

        assert!(crate::refcount::ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
        assert!(
            crate::weak::ll_weakref_get(cell).is_null(),
            "the element outlived the one holder that counted it"
        );

        assert!(crate::refcount::ll_release(cell as *mut RcHeader));
        crate::weak::weakref_die(cell);
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
}
