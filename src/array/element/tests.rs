use super::*;
use crate::array::entity::ll_array_new;
use crate::array::table::Key;
use crate::class::ClassBuilder;
use crate::memory::arena::Arena;
use crate::memory::block_pool::FORCE_OOM;
use crate::memory::stdapi::ll_free;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, ll_release};
use crate::string::{LLString, ll_string_new};
use crate::value::Value;
use std::sync::atomic::Ordering;

fn mk(bytes: &[u8]) -> *mut LLString {
    let s = unsafe { ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, bytes) };
    assert!(!s.is_null());
    s
}

/// A table's first storage: 8 index slots and 8 entries.
const FIRST_STORAGE_BYTES: usize = 288;

/// What the first growth asks for: 16 index slots and 16 entries.
const DOUBLED_STORAGE_BYTES: usize = 576;

/// Eat every buffer-arena source that could serve `size` — warm
/// block tails and recycled holes left by earlier tests on this
/// thread — so the next such allocation must draw a pool block,
/// which the already-raised `FORCE_OOM` refuses. Without this a
/// forced refusal is a coin flip on test order: the source array's
/// own storage warms a 64 KiB block and the copy's fits its tail.
/// The fillers go back through the caller, after the assertion.
unsafe fn exhaust_buffer_sources(size: usize) -> Vec<(*mut u8, usize)> {
    let mut fillers = Vec::new();
    loop {
        let (p, granted) = crate::memory::buffer_arena::buffer_alloc_longlived_payload(size);
        if p.is_null() {
            break;
        }

        fillers.push((p, granted));
    }

    fillers
}

fn free_fillers(fillers: Vec<(*mut u8, usize)>) {
    for (p, granted) in fillers {
        unsafe { crate::memory::buffer_arena::buffer_free_longlived_payload(p, granted) };
    }
}

/// Eat every free entity slot that could serve an inline string of
/// `len` bytes, so the next such allocation must draw a pool block,
/// which the already-raised `FORCE_OOM` refuses. The buffer-arena
/// helper above cannot stand in for this: an entity comes from the
/// object heap, which that one never touches.
unsafe fn exhaust_string_entities(len: usize) -> Vec<*mut LLString> {
    let bytes = vec![b'x'; len];
    let mut fillers = Vec::new();
    loop {
        let s = unsafe { ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, &bytes) };
        if s.is_null() {
            break;
        }

        fillers.push(s);
    }

    fillers
}

fn free_string_fillers(fillers: Vec<*mut LLString>) {
    for s in fillers {
        free(s);
    }
}

/// A heap holder object with two array-slot props, both naming
/// `src`, which therefore reads as shared: the two-`$var` setup of
/// every criterion below, built through the real barrier.
unsafe fn two_holders(
    ctx: *mut crate::memory::context::LLContext,
    arena: *mut Arena,
    src: *mut LLArray,
) -> (*mut crate::object::Object, *mut Value, *mut Value) {
    let class = ClassBuilder::new("ElementHolder")
        .prop("a", true)
        .prop("b", true)
        .build();
    let h = unsafe { new_constructed(ctx, class, MemoryCategory::GcHeap) };
    let slot_a = unsafe { Object::prop_at(h, 16) };
    let slot_b = unsafe { Object::prop_at(h, 32) };
    unsafe {
        assert!(crate::memory::barrier::ref_store(
            arena,
            h as *mut RcHeader,
            slot_a,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, src as *mut RcHeader),
        ));
        assert!(crate::memory::barrier::ref_store(
            arena,
            h as *mut RcHeader,
            slot_b,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, src as *mut RcHeader),
        ));
        // The creation reference goes: the two slots are the holders.
        ll_release(src as *mut RcHeader);
    }

    (h, slot_a, slot_b)
}

fn free(s: *mut LLString) {
    unsafe {
        (*s).rc.refcount = 0;
        ll_free(s as *mut u8);
    }
}

/// `$a` with one integer element, `&$a[0]` taken or not and the
/// binding kept or dropped, then `$b = $a; $b[0] = 3;` — and what the
/// two names read afterwards. The whole of S3's criterion runs
/// through this, in both memory categories.
///
/// The holder is one object with two properties, so both arrays are
/// named by real slots and every write goes through the layer rather
/// than through the table.
unsafe fn reference_then_copy(
    ctx: *mut crate::memory::context::LLContext,
    arena: *mut Arena,
    category: MemoryCategory,
    take_reference: bool,
    keep_binding: bool,
) -> (i64, i64) {
    let class = ClassBuilder::new("RefCopyHolder")
        .prop("a", true)
        .prop("b", true)
        .build();
    let holder = unsafe { new_constructed(ctx, class, category) };
    let slot_a = unsafe { Object::prop_at(holder, 16) };
    let slot_b = unsafe { Object::prop_at(holder, 32) };
    let a = unsafe { ll_array_new(category) };
    unsafe {
        assert!(crate::memory::barrier::ref_store(
            arena,
            holder as *mut RcHeader,
            slot_a,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, a as *mut RcHeader),
        ));
        ll_release(a as *mut RcHeader);
        assert!(set(ctx, category, slot_a, Key::Int(0), Value::int(1)));

        if take_reference {
            let r = make_ref(ctx, category, slot_a, Key::Int(0));
            assert!(!r.is_null(), "the reference was refused");
            // The `$r` binding, taken and — unless it is kept — given
            // straight back, which is `unset($r)` and leaves the
            // element a reference with one holder (measured on php
            // 8.3.6: `unset` does not collapse the element).
            //
            // `GcHeap` is the binding's category whatever the array's
            // is, because `$r` is a frame slot rather than a container
            // in the arena: its reference is counted and given back
            // inside the request. Through an arena owner the release
            // would belong to the reset log, and `unset($r)` would not
            // take effect until the request ended.
            crate::refcount::ll_retain(r as *mut RcHeader);
            if !keep_binding {
                crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, r as *mut RcHeader);
            }
        }

        // `$b = $a`, then `$b[0] = 3`.
        assert!(crate::memory::barrier::ref_store(
            arena,
            holder as *mut RcHeader,
            slot_b,
            std::ptr::null_mut(),
            *slot_a,
        ));
        assert!(set(ctx, category, slot_b, Key::Int(0), Value::int(3)));

        let read_a = get(slot_a, Key::Int(0)).expect("the key is there").as_int();
        let read_b = get(slot_b, Key::Int(0)).expect("the key is there").as_int();
        if take_reference && keep_binding {
            let boxed = match get_element(slot_a) {
                Some(v) => v.entity_ptr(),
                None => std::ptr::null_mut(),
            };

            if !boxed.is_null() {
                crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, boxed);
            }
        }

        // The holder goes, and both arrays with it. Left standing,
        // their storage keeps buffer-arena chunks that the arena's
        // own tests then find in a shape they did not put it in.
        if category == MemoryCategory::GcHeap {
            assert!(ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }

        (read_a, read_b)
    }
}

/// The element as the entry holds it, box included — what [`get`]
/// deliberately looks through.
unsafe fn get_element(slot: *const Value) -> Option<Value> {
    let a = unsafe { (*slot).entity_ptr() } as *mut LLArray;
    unsafe { crate::array::testing::get(a, Key::Int(0)) }
}

/// `canonical_key` turns the numeric strings PHP means as integers
/// into integer keys and leaves every other spelling a string key —
/// a leading zero, a plus sign, a leading space, a value past what
/// `i64` holds.
mod the_key_a_spelling_means {
    use super::*;

    /// The three canonical spellings of the done criterion, each finding
    /// what the integer key stored — one table, one lookup per pair.
    #[test]
    fn a_canonical_numeric_string_finds_what_the_integer_key_stored() {
        let _g = crate::memory::block_pool::test_guard();
        let a = unsafe { crate::array::entity::ll_array_new(MemoryCategory::GcHeap) };
        for (i, k) in [1i64, -1, i64::MAX, i64::MIN].into_iter().enumerate() {
            unsafe {
                crate::array::testing::insert(a, Key::Int(k), Value::int(i as i64));
            }
        }

        for (i, spelling) in [
            &b"1"[..],
            b"-1",
            b"9223372036854775807",
            b"-9223372036854775808",
        ]
        .into_iter()
        .enumerate()
        {
            let s = mk(spelling);
            let key = unsafe { canonical_key(s) };
            assert!(
                matches!(key, Key::Int(_)),
                "{:?} must canonicalise",
                std::str::from_utf8(spelling).unwrap()
            );
            unsafe {
                assert_eq!(
                    crate::array::testing::get(a, key).unwrap().as_int(),
                    i as i64,
                    "{:?} missed the integer key's entry",
                    std::str::from_utf8(spelling).unwrap()
                );
            }

            free(s);
        }

        unsafe {
            crate::array::entity::dispose_storage(a, category_of(a));
            (*a).rc.refcount = 0;
            ll_free(a as *mut u8);
        }
    }

    /// The five non-canonical spellings of the done criterion stay
    /// string keys, plus the two cheap boundaries beside them.
    #[test]
    fn a_non_canonical_spelling_stays_a_string_key() {
        let _g = crate::memory::block_pool::test_guard();
        for spelling in [&b"011"[..], b"1.0", b" 1", b"-0", b"9223372036854775808"] {
            let s = mk(spelling);
            let key = unsafe { canonical_key(s) };
            assert!(
                matches!(key, Key::Str(p) if p == s),
                "{:?} must stay a string key",
                std::str::from_utf8(spelling).unwrap()
            );
            free(s);
        }

        for spelling in [&b""[..], b"+1", b"-"] {
            let s = mk(spelling);
            assert!(
                matches!(unsafe { canonical_key(s) }, Key::Str(_)),
                "{:?} must stay a string key",
                std::str::from_utf8(spelling).unwrap()
            );
            free(s);
        }
    }
}

/// Every write separates a shared array before it touches anything,
/// so a second holder sees none of it, and then publishes the copy,
/// spends its creation reference and drops the displaced original.
/// The order of those last two is `write_through`'s and is argued
/// there; what these tests read is the end state. An exclusively
/// owned array is written in place and hands the displaced element
/// back, and an arena holder's copy is an arena array too.
mod the_writes_and_the_separation_they_share {
    use super::*;

    /// The store's whole composition, measured from both holders: a
    /// store through one leaves the other's entries alone, the displaced
    /// original ends at one holder so the next store to it writes in
    /// place, the copy is held once, and the array takes a value
    /// reference of its own without consuming the caller's.
    #[test]
    fn a_store_through_one_holder_leaves_the_other_holders_entries_alone() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        unsafe {
            crate::array::testing::insert(src, Key::Int(0), Value::int(10));
        }

        let (h, slot_a, slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };
        let val = mk(b"forty-one");

        assert!(unsafe {
            set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot_a,
                Key::Int(1),
                Value::entity(Tag::String, val as *mut RcHeader),
            )
        });

        unsafe {
            let copy = (*slot_a).entity_ptr() as *mut LLArray;
            assert_ne!(copy, src, "the shared table separated");
            assert_eq!(
                crate::array::testing::get(copy, Key::Int(1))
                    .unwrap()
                    .entity_ptr(),
                val as *mut RcHeader
            );
            assert_eq!(
                crate::array::testing::get(copy, Key::Int(0))
                    .unwrap()
                    .as_int(),
                10,
                "the copy replayed the source"
            );
            assert!(
                crate::array::testing::get(src, Key::Int(1)).is_none(),
                "the other holder's entries changed"
            );
            assert_eq!(
                (*src).rc.refcount,
                1,
                "the displaced original keeps exactly its other holder"
            );
            assert_eq!((*copy).rc.refcount, 1, "the copy is held once, by the slot");
            assert_eq!(
                (*val).rc.refcount,
                2,
                "the array takes its own reference and leaves the caller's"
            );

            // The second store goes through the other holder, whose array
            // is now at count one: in place, no second separation.
            assert!(set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot_b,
                Key::Int(1),
                Value::int(7),
            ));
            assert_eq!(
                (*slot_b).entity_ptr() as *mut LLArray,
                src,
                "a store to the displaced original separated again"
            );
            assert_eq!(
                crate::array::testing::get(src, Key::Int(1))
                    .unwrap()
                    .as_int(),
                7
            );

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            assert_eq!(
                (*val).rc.refcount,
                1,
                "the dying array did not give the value back"
            );
            assert!(ll_release(val as *mut RcHeader));
            crate::object::ll_entity_die(val as *mut RcHeader);
        }
    }

    /// The in-place arm, which the shared-array tests above never take:
    /// an exclusively owned array takes a value reference of its own and
    /// gives the displaced element back. One refcounted element
    /// overwrites another, so both halves are measured on one entity
    /// each.
    #[test]
    fn a_store_in_place_gives_the_displaced_element_back() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let class = ClassBuilder::new("InPlaceHolder").prop("a", true).build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
        let slot = unsafe { Object::prop_at(h, 16) };
        unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            ll_release(src as *mut RcHeader);
        }

        let first = mk(b"first");
        let second = mk(b"second");
        let first_start = unsafe { (*first).rc.refcount };
        let second_start = unsafe { (*second).rc.refcount };

        unsafe {
            assert!(set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Int(0),
                Value::entity(Tag::String, first as *mut RcHeader),
            ));
            assert_eq!(
                (*slot).entity_ptr() as *mut LLArray,
                src,
                "an exclusively owned array separated"
            );
            assert_eq!(
                (*first).rc.refcount,
                first_start + 1,
                "the array takes its own reference and leaves the caller's"
            );

            assert!(set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Int(0),
                Value::entity(Tag::String, second as *mut RcHeader),
            ));
            assert_eq!(
                (*first).rc.refcount,
                first_start,
                "the displaced element kept the array's reference"
            );
            assert_eq!((*second).rc.refcount, second_start + 1);
            assert_eq!(
                crate::array::testing::get(src, Key::Int(0))
                    .unwrap()
                    .entity_ptr(),
                second as *mut RcHeader
            );

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            assert_eq!((*second).rc.refcount, second_start);
            for s in [first, second] {
                assert!(ll_release(s as *mut RcHeader));
                crate::object::ll_entity_die(s as *mut RcHeader);
            }
        }
    }

    /// The append's three clauses: it writes under the cursor's key, a
    /// shared array separates so the other holder's length stays put,
    /// and an exhausted cursor refuses instead of wrapping onto a live
    /// entry.
    #[test]
    fn an_append_through_one_holder_leaves_the_other_holders_length_alone() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        unsafe {
            for i in 0..2i64 {
                crate::array::testing::insert(src, Key::Int(i), Value::int(10 + i));
            }
        }

        let (h, slot_a, slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };

        assert!(unsafe { append(context_ptr, MemoryCategory::GcHeap, slot_a, Value::int(99)) });

        unsafe {
            let copy = (*slot_a).entity_ptr() as *mut LLArray;
            assert_ne!(copy, src, "the shared table separated");
            assert_eq!(
                crate::array::testing::get(copy, Key::Int(2))
                    .unwrap()
                    .as_int(),
                99,
                "the append took the cursor's key"
            );
            assert_eq!(crate::array::testing::table(copy).len(), 3);
            assert_eq!(
                crate::array::testing::table(src).len(),
                2,
                "the other holder's length followed the append"
            );
            assert!(crate::array::testing::get(src, Key::Int(2)).is_none());

            // The original is exclusively `slot_b`'s now, so the highest
            // integer key goes straight in: the cursor has no successor
            // and the next append must refuse.
            crate::array::testing::insert(src, Key::Int(i64::MAX), Value::int(1));
            assert!(
                !append(context_ptr, MemoryCategory::GcHeap, slot_b, Value::int(0)),
                "an exhausted cursor appended anyway"
            );
            assert_eq!(
                crate::array::testing::table(src).len(),
                3,
                "a refused append wrote an entry"
            );
            assert_eq!(
                (*slot_b).entity_ptr() as *mut LLArray,
                src,
                "a refused append separated"
            );

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
        }
    }

    /// `unset` through one holder of a shared array: the copy loses the
    /// entry, the other holder keeps it, and both of the table's
    /// references come back — the key's by the table's ownership rule, the value's by
    /// the barrier. The separation replays the entry and the removal
    /// gives it back, so the measurement is a net zero on each entity.
    #[test]
    fn an_unset_gives_the_key_back_and_leaves_the_other_holder_standing() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let key = mk(b"gone");
        let value = mk(b"payload");
        unsafe {
            crate::refcount::ll_retain(key as *mut RcHeader);
            crate::refcount::ll_retain(value as *mut RcHeader);
            crate::array::testing::insert(
                src,
                Key::Str(key),
                Value::entity(Tag::String, value as *mut RcHeader),
            );
        }

        let key_shared = unsafe { (*key).rc.refcount };
        let value_shared = unsafe { (*value).rc.refcount };
        let (h, slot_a, _slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };

        assert!(unsafe { unset(context_ptr, MemoryCategory::GcHeap, slot_a, Key::Str(key)) });

        unsafe {
            let copy = (*slot_a).entity_ptr() as *mut LLArray;
            assert_ne!(copy, src, "the shared table separated");
            assert!(
                crate::array::testing::get(copy, Key::Str(key)).is_none(),
                "the copy kept the unset entry"
            );
            assert!(
                crate::array::testing::get(src, Key::Str(key)).is_some(),
                "the other holder lost its entry"
            );
            assert_eq!(
                (*key).rc.refcount,
                key_shared,
                "the removed key did not come back"
            );
            assert_eq!(
                (*value).rc.refcount,
                value_shared,
                "the removed element did not come back"
            );

            // An absent key is not an error, and it still separates:
            // `slot_a`'s array is exclusively its own by now, so the
            // observable part is only the report.
            assert!(unset(
                context_ptr,
                MemoryCategory::GcHeap,
                slot_a,
                Key::Int(7)
            ));

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            assert_eq!(
                (*key).rc.refcount,
                key_shared - 1,
                "the dying array kept it"
            );
            assert_eq!((*value).rc.refcount, value_shared - 1);
            for s in [key, value] {
                assert!(ll_release(s as *mut RcHeader));
                crate::object::ll_entity_die(s as *mut RcHeader);
            }
        }
    }

    /// The arena half of the operation, which every test above leaves
    /// out: `separation_category` keeps an arena holder's copy in the
    /// arena, so the store neither counts an escape nor logs a release,
    /// and the reset reclaims both arrays.
    #[test]
    fn a_store_through_an_arena_holder_separates_into_the_arena() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        let src = unsafe { ll_array_new(MemoryCategory::RequestArena) };
        unsafe {
            crate::array::testing::insert(src, Key::Int(0), Value::int(10));
        }

        let class = ClassBuilder::new("ArenaHolder")
            .prop("a", true)
            .prop("b", true)
            .build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::RequestArena) };
        let slot_a = unsafe { Object::prop_at(h, 16) };
        let slot_b = unsafe { Object::prop_at(h, 32) };
        unsafe {
            for s in [slot_a, slot_b] {
                assert!(crate::memory::barrier::ref_store(
                    arena_ptr,
                    h as *mut RcHeader,
                    s,
                    std::ptr::null_mut(),
                    Value::entity(Tag::Array, src as *mut RcHeader),
                ));
            }

            ll_release(src as *mut RcHeader);
        }

        assert!(unsafe {
            set(
                context_ptr,
                MemoryCategory::RequestArena,
                slot_a,
                Key::Int(1),
                Value::int(7),
            )
        });

        unsafe {
            let copy = (*slot_a).entity_ptr() as *mut LLArray;
            assert_ne!(copy, src, "the shared table separated");
            assert_eq!(
                crate::object::header_category(copy as *const RcHeader),
                MemoryCategory::RequestArena,
                "an arena holder's copy left the arena"
            );
            assert_eq!(
                crate::array::testing::get(copy, Key::Int(0))
                    .unwrap()
                    .as_int(),
                10
            );
            assert_eq!(
                crate::array::testing::get(copy, Key::Int(1))
                    .unwrap()
                    .as_int(),
                7
            );
            assert!(
                crate::array::testing::get(src, Key::Int(1)).is_none(),
                "the other holder's entries changed"
            );
            assert_eq!(
                (*src).rc.refcount,
                1,
                "the displaced original keeps exactly its other holder"
            );
            assert_eq!(
                (*copy).rc.flags & crate::refcount::IS_ESCAPEE,
                0,
                "an arena copy in an arena slot crossed no boundary"
            );
        }

        crate::memory::context::set_current_context(std::ptr::null_mut());

        // Nothing was logged for the reset to release: the log exists for
        // a longer-lived entity entering an arena slot, and every entity
        // this store touched is the arena's own. Draining is what says
        // so — a spurious record would be freed by the reset below and
        // read as a clean run.
        let mut logged = 0usize;
        arena.drain_release_log(|_| logged += 1);
        assert_eq!(logged, 0, "an arena-to-arena store logged a release");

        arena.reset(|_| {});
    }

    /// The key-ownership half through `set` itself: a fresh string key is
    /// consumed, an equal-bytes overwrite hands the operation's own
    /// reference back, and a refused growth hands the published key
    /// back. Each arm seen failing under a targeted revert of its
    /// giveback.
    #[test]
    fn a_string_key_through_the_store_obeys_the_ownership_rule() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let class = ClassBuilder::new("KeyHolder").prop("a", true).build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
        let slot = unsafe { Object::prop_at(h, 16) };
        unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            ll_release(src as *mut RcHeader);
        }

        let k1 = mk(b"key");
        let k2 = mk(b"key");
        assert_ne!(k1, k2, "two distinct entities, or the arms collapse");
        let k1_start = unsafe { (*k1).rc.refcount };
        let k2_start = unsafe { (*k2).rc.refcount };

        unsafe {
            assert!(set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Str(k1),
                Value::int(1),
            ));
            assert_eq!(
                (*k1).rc.refcount,
                k1_start + 1,
                "a stored new key is consumed into the table's reference"
            );

            assert!(set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Str(k2),
                Value::int(2),
            ));
            assert_eq!(
                (*k2).rc.refcount,
                k2_start,
                "the overwrite arm kept its published reference"
            );
            assert_eq!(
                crate::array::testing::get(src, Key::Str(k1))
                    .unwrap()
                    .as_int(),
                2
            );

            // Fill to capacity, so the next new key must grow — and the
            // growth is refused, so the published key must come back.
            for i in 0..7i64 {
                crate::array::testing::insert(src, Key::Int(i), Value::int(i));
            }

            let k3 = mk(b"other");
            let k3_start = (*k3).rc.refcount;
            FORCE_OOM.store(true, Ordering::Relaxed);
            let fillers = exhaust_buffer_sources(DOUBLED_STORAGE_BYTES);
            let stored = set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Str(k3),
                Value::int(9),
            );
            FORCE_OOM.store(false, Ordering::Relaxed);
            free_fillers(fillers);
            assert!(!stored, "growth was meant to be refused");
            assert_eq!(
                (*k3).rc.refcount,
                k3_start,
                "the refused insert kept the published key"
            );

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            assert_eq!(
                (*k1).rc.refcount,
                k1_start,
                "the dying array did not give its key back"
            );
            for s in [k1, k2, k3] {
                assert!(ll_release(s as *mut RcHeader));
                crate::object::ll_entity_die(s as *mut RcHeader);
            }
        }
    }
}

/// Four allocations on a write can be refused: the separation's
/// copy, the table's growth, the escape copy of a value crossing
/// into a longer-lived array, and the box. Each reports its refusal
/// with every array reading as it did before the call — `false` from
/// the three writes, and a null box from `make_ref`, whose result is
/// a pointer. A copy
/// destroyed mid-write gives its children back at once rather than
/// waiting for `ll_release`'s verdict, which on an arena copy never
/// reports a death.
mod what_a_refusal_leaves_behind {
    use super::*;

    /// The separation's refusal: `false`, and nothing observable moved —
    /// the slot, the original's count, its entries and the caller's value
    /// reference all read as before the call.
    #[test]
    fn a_refused_separation_reports_and_changes_nothing() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        unsafe {
            crate::array::testing::insert(src, Key::Int(0), Value::int(10));
        }

        let (h, slot_a, _slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };
        let val = mk(b"unstored");

        FORCE_OOM.store(true, Ordering::Relaxed);
        let fillers = unsafe { exhaust_buffer_sources(FIRST_STORAGE_BYTES) };
        let stored = unsafe {
            set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot_a,
                Key::Int(1),
                Value::entity(Tag::String, val as *mut RcHeader),
            )
        };

        FORCE_OOM.store(false, Ordering::Relaxed);
        free_fillers(fillers);
        assert!(!stored, "the copy's storage was meant to be refused");

        unsafe {
            assert_eq!(
                (*slot_a).entity_ptr() as *mut LLArray,
                src,
                "a refused store moved the slot"
            );
            assert_eq!((*src).rc.refcount, 2, "a refused store moved a count");
            assert_eq!(crate::array::testing::table(src).len(), 1);
            assert!(crate::array::testing::get(src, Key::Int(1)).is_none());
            assert_eq!(
                (*val).rc.refcount,
                1,
                "a refused store kept the caller's value reference"
            );
            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            assert!(ll_release(val as *mut RcHeader));
            crate::object::ll_entity_die(val as *mut RcHeader);
        }
    }

    /// The table's own refusal, on an exclusively owned array: growth
    /// cannot allocate, the store reports, and every entry reads as
    /// before.
    #[test]
    fn a_refused_growth_reports_with_the_table_unchanged() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let class = ClassBuilder::new("OneHolder").prop("a", true).build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
        let slot = unsafe { Object::prop_at(h, 16) };
        unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            ll_release(src as *mut RcHeader);
            // Fill to capacity, so the next insert must grow.
            for i in 0..8i64 {
                crate::array::testing::insert(src, Key::Int(i), Value::int(i));
            }
        }

        FORCE_OOM.store(true, Ordering::Relaxed);
        let fillers = unsafe { exhaust_buffer_sources(DOUBLED_STORAGE_BYTES) };
        let stored = unsafe {
            set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Int(100),
                Value::int(1),
            )
        };

        FORCE_OOM.store(false, Ordering::Relaxed);
        free_fillers(fillers);
        assert!(!stored, "growth was meant to be refused");

        unsafe {
            assert_eq!(
                crate::array::testing::table(src).len(),
                8,
                "a refused growth moved an entry"
            );
            assert!(crate::array::testing::get(src, Key::Int(100)).is_none());
            for i in 0..8i64 {
                assert_eq!(
                    crate::array::testing::get(src, Key::Int(i))
                        .unwrap()
                        .as_int(),
                    i
                );
            }

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
        }
    }

    /// The table's refusal inside a private copy: the copy dies whole,
    /// and the slot, the original and the caller's value all read as
    /// before — the second refusal of the criterion, one array further
    /// in than the separation's.
    #[test]
    fn a_growth_refusal_inside_the_copy_destroys_the_copy_alone() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        // Empty and shared: the separation's replay allocates nothing,
        // so the forced refusal lands on the copy's own first storage.
        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let (h, slot_a, _slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };
        let val = mk(b"unstored");

        FORCE_OOM.store(true, Ordering::Relaxed);
        let fillers = unsafe { exhaust_buffer_sources(FIRST_STORAGE_BYTES) };
        // The separation must not be the refusal, or this measures
        // `a_refused_separation_reports_and_changes_nothing` a second
        // time — its assertions are these. The copy's entity comes from
        // the object heap, which the exhaustion above does not reach, so
        // prove a slot is there and hand it straight back.
        unsafe {
            let probe = ll_array_new(MemoryCategory::GcHeap);
            assert!(!probe.is_null(), "the copy's entity slot was refused");
            assert!(ll_release(probe as *mut RcHeader));
            crate::object::ll_entity_die(probe as *mut RcHeader);
        }

        let stored = unsafe {
            set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot_a,
                Key::Int(0),
                Value::entity(Tag::String, val as *mut RcHeader),
            )
        };

        FORCE_OOM.store(false, Ordering::Relaxed);
        free_fillers(fillers);
        assert!(!stored, "the copy's storage was meant to be refused");

        unsafe {
            assert_eq!((*slot_a).entity_ptr() as *mut LLArray, src);
            assert_eq!((*src).rc.refcount, 2);
            assert!(crate::array::testing::table(src).is_empty());
            assert_eq!(
                (*val).rc.refcount,
                1,
                "the giveback did not balance the copy's publication"
            );
            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            assert!(ll_release(val as *mut RcHeader));
            crate::object::ll_entity_die(val as *mut RcHeader);
        }
    }

    /// The refusal teardown cannot wait for `ll_release`'s verdict: on
    /// an arena copy the release reports no death, and a verdict-gated
    /// branch leaves every reference the replay published sitting on a
    /// corpse until the reset — a shared COW child then reads an extra
    /// holder and separates on every write for the rest of the request.
    /// Seen failing exactly there with the gated teardown.
    #[test]
    fn a_destroyed_arena_copy_gives_its_children_back() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        let src = unsafe { ll_array_new(MemoryCategory::RequestArena) };
        let child = unsafe { ll_string_new(context_ptr, MemoryCategory::RequestArena, b"cow") };
        unsafe {
            crate::refcount::ll_retain(child as *mut RcHeader);
            crate::array::testing::insert(
                src,
                Key::Int(0),
                Value::entity(Tag::String, child as *mut RcHeader),
            );
            crate::refcount::ll_retain(src as *mut RcHeader);
        }

        let before = unsafe { (*child).rc.refcount };

        let copy = unsafe {
            crate::array::entity::separate(
                src,
                MemoryCategory::RequestArena,
                arena_ptr,
                crate::array::entity::CopyReason::Duplication,
            )
        };

        assert!(!copy.is_null());
        unsafe {
            assert_eq!(
                (*child).rc.refcount,
                before + 1,
                "the replay was meant to take a reference of its own"
            );
            destroy_unpublished(copy as *mut RcHeader);
            assert_eq!(
                (*child).rc.refcount,
                before,
                "the corpse kept the replay's reference"
            );
        }

        crate::memory::context::set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    /// The third refusal, which is neither the separation's nor the
    /// table's: publishing an arena COW value into a longer-lived array
    /// copies it out through `escape_copy`, and that copy is an
    /// allocation no reserve funds. The array is exclusively owned, so
    /// no separation runs, and its storage already exists, so no growth
    /// runs.
    #[test]
    fn a_refused_escape_copy_of_the_value_reports_and_changes_nothing() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let class = ClassBuilder::new("EscapeHolder").prop("a", true).build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
        let slot = unsafe { Object::prop_at(h, 16) };
        unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            ll_release(src as *mut RcHeader);
            crate::array::testing::insert(src, Key::Int(0), Value::int(0));
        }

        let value = unsafe { ll_string_new(context_ptr, MemoryCategory::RequestArena, b"arena") };
        let before = unsafe { (*value).rc.refcount };

        FORCE_OOM.store(true, Ordering::Relaxed);
        let fillers = unsafe { exhaust_string_entities(b"arena".len()) };
        let stored = unsafe {
            set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Int(1),
                Value::entity(Tag::String, value as *mut RcHeader),
            )
        };

        FORCE_OOM.store(false, Ordering::Relaxed);
        free_string_fillers(fillers);
        assert!(!stored, "the value's escape copy was meant to be refused");

        unsafe {
            assert_eq!((*slot).entity_ptr() as *mut LLArray, src);
            assert_eq!(crate::array::testing::table(src).len(), 1);
            assert!(crate::array::testing::get(src, Key::Int(1)).is_none());
            assert_eq!(
                (*value).rc.refcount,
                before,
                "a refused store kept the caller's value reference"
            );
            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
        }

        crate::memory::context::set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    /// A refused box leaves the array exactly as it was, the key the
    /// reference would have created included. The exclusively-owned path
    /// has no private copy to throw away, so that rollback is explicit
    /// and this is what holds it.
    #[test]
    fn a_refused_box_takes_the_vivified_element_back_out() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let class = ClassBuilder::new("RefusedRefHolder")
            .prop("a", true)
            .build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
        let slot = unsafe { Object::prop_at(h, 16) };
        unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            ll_release(src as *mut RcHeader);
            // One entry buys the storage, so the vivified insert below
            // needs no growth and the refusal lands on the box alone.
            crate::array::testing::insert(src, Key::Int(0), Value::int(1));
        }

        FORCE_OOM.store(true, Ordering::Relaxed);
        // A reference box is 24 bytes, the size class an empty inline
        // string takes.
        let fillers = unsafe { exhaust_string_entities(0) };
        let r = unsafe { make_ref(context_ptr, MemoryCategory::GcHeap, slot, Key::Int(9)) };
        FORCE_OOM.store(false, Ordering::Relaxed);
        free_string_fillers(fillers);
        assert!(r.is_null(), "the box was meant to be refused");

        unsafe {
            assert!(
                !crate::array::testing::contains(src, Key::Int(9)),
                "the refusal left the vivified element behind"
            );
            assert_eq!(crate::array::testing::table(src).len(), 1);
            assert_eq!(
                (*slot).entity_ptr() as *mut LLArray,
                src,
                "a refused reference separated"
            );
            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
        }
    }
}

/// A read yields what the box holds and a store goes through it, so
/// the entry still names the box afterwards and a refused store
/// leaves it holding what it held. `&$a[k]` separates a shared table
/// before it boxes, and an absent key is created as null first: the
/// null `box_element` reports means absent, and the layer above must
/// not forward it.
mod an_element_in_a_reference_state {
    use super::*;

    /// The by-value read of an element in a reference state yields what
    /// the box holds rather than the box, and reading separates nothing:
    /// both holders still name the one array afterwards.
    #[test]
    fn a_read_goes_through_a_reference_box_and_separates_nothing() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        unsafe {
            crate::array::testing::insert(src, Key::Int(0), Value::int(5));
            let boxed = box_element(src, arena_ptr, Key::Int(0));
            assert!(!boxed.is_null(), "the element was meant to be boxed");
            assert_eq!(
                crate::array::testing::get(src, Key::Int(0)).unwrap().tag(),
                Tag::Reference,
                "the entry does not hold a box, so the read proves nothing"
            );
        }

        let (h, slot_a, slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };

        unsafe {
            let read = get(slot_a, Key::Int(0)).expect("the key is there");
            assert_eq!(read.tag(), Tag::Int, "the read handed the box back");
            assert_eq!(read.as_int(), 5);
            assert!(get(slot_a, Key::Int(1)).is_none(), "an absent key answered");
            assert_eq!(
                (*slot_a).entity_ptr() as *mut LLArray,
                src,
                "the read separated"
            );
            assert_eq!((*slot_b).entity_ptr() as *mut LLArray, src);

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
        }
    }

    /// A store into an element in a reference state goes **through** the
    /// box: the entry still names the box afterwards, the box holds the
    /// new value, and the value it displaced came back.
    #[test]
    fn a_store_into_a_boxed_element_goes_through_the_box() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let class = ClassBuilder::new("BoxHolder").prop("a", true).build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
        let slot = unsafe { Object::prop_at(h, 16) };
        let first = mk(b"first");
        let boxed = unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            ll_release(src as *mut RcHeader);
            crate::refcount::ll_retain(first as *mut RcHeader);
            crate::array::testing::insert(
                src,
                Key::Int(0),
                Value::entity(Tag::String, first as *mut RcHeader),
            );
            let boxed = box_element(src, arena_ptr, Key::Int(0));
            assert!(!boxed.is_null(), "the element was meant to be boxed");
            boxed
        };

        let first_held = unsafe { (*first).rc.refcount };

        let second = mk(b"second");
        let second_start = unsafe { (*second).rc.refcount };
        assert!(unsafe {
            set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Int(0),
                Value::entity(Tag::String, second as *mut RcHeader),
            )
        });

        unsafe {
            assert_eq!(
                crate::array::testing::get(src, Key::Int(0))
                    .unwrap()
                    .entity_ptr(),
                boxed as *mut RcHeader,
                "the store replaced the box instead of writing through it"
            );
            assert_eq!((*boxed).value.entity_ptr(), second as *mut RcHeader);
            assert_eq!(
                get(slot, Key::Int(0)).unwrap().entity_ptr(),
                second as *mut RcHeader
            );
            assert_eq!(
                (*first).rc.refcount,
                first_held - 1,
                "the value the box displaced did not come back"
            );
            assert_eq!(
                (*second).rc.refcount,
                second_start + 1,
                "the box took no reference of its own"
            );

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            assert_eq!((*second).rc.refcount, second_start);
            for s in [first, second] {
                assert!(ll_release(s as *mut RcHeader));
                crate::object::ll_entity_die(s as *mut RcHeader);
            }
        }
    }

    /// The barrier publishes before it releases, so a store through the
    /// box that the barrier refuses leaves the box holding exactly what
    /// it held — the displaced value keeps its reference rather than
    /// being dropped for a store that never happened.
    #[test]
    fn a_refused_store_through_the_box_keeps_the_displaced_value() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let class = ClassBuilder::new("RefusedBoxHolder")
            .prop("a", true)
            .build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
        let slot = unsafe { Object::prop_at(h, 16) };
        let held = mk(b"held");
        let boxed = unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            ll_release(src as *mut RcHeader);
            crate::refcount::ll_retain(held as *mut RcHeader);
            crate::array::testing::insert(
                src,
                Key::Int(0),
                Value::entity(Tag::String, held as *mut RcHeader),
            );
            let boxed = box_element(src, arena_ptr, Key::Int(0));
            assert!(!boxed.is_null());
            boxed
        };

        let held_start = unsafe { (*held).rc.refcount };

        // An arena COW value crossing into the heap box is copied out,
        // and that copy is the allocation the refusal lands on.
        let crossing =
            unsafe { ll_string_new(context_ptr, MemoryCategory::RequestArena, b"crossing") };
        let crossing_start = unsafe { (*crossing).rc.refcount };

        FORCE_OOM.store(true, Ordering::Relaxed);
        let fillers = unsafe { exhaust_string_entities(b"crossing".len()) };
        let stored = unsafe {
            set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Int(0),
                Value::entity(Tag::String, crossing as *mut RcHeader),
            )
        };

        FORCE_OOM.store(false, Ordering::Relaxed);
        free_string_fillers(fillers);
        assert!(!stored, "the crossing value's copy was meant to be refused");

        unsafe {
            assert_eq!(
                (*boxed).value.entity_ptr(),
                held as *mut RcHeader,
                "a refused store moved the box"
            );
            assert_eq!(
                (*held).rc.refcount,
                held_start,
                "the displaced value was released for a store that never happened"
            );
            assert_eq!((*crossing).rc.refcount, crossing_start);

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            assert!(ll_release(held as *mut RcHeader));
            crate::object::ll_entity_die(held as *mut RcHeader);
        }

        crate::memory::context::set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    /// An absent key is null here, and the layer above is what turns that
    /// into a vivified element ([`make_ref`]): the two nulls mean
    /// different things and only one of them is a refusal.
    #[test]
    fn box_element_reports_on_an_absent_key() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;

        let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        unsafe {
            crate::array::testing::insert(a, Key::Int(1), Value::int(1));
            assert!(box_element(a, arena_ptr, Key::Int(2)).is_null());

            let absent = mk(b"nope");
            assert!(box_element(a, arena_ptr, Key::Str(absent)).is_null());
            assert!(ll_release(absent as *mut RcHeader));
            crate::object::ll_entity_die(absent as *mut RcHeader);

            assert!(ll_release(a as *mut RcHeader));
            crate::object::ll_entity_die(a as *mut RcHeader);
        }
    }

    /// `$r = &$a['nope']` creates the element as null and references it,
    /// which is PHP's rule and the reason the layer cannot forward the
    /// boxing step's null: that one means "absent".
    ///
    /// Read through the array rather than out of the entry: what the
    /// caller is owed is that `$a[5]` is null afterwards and that a write
    /// through `$r` is visible there. Which entity the entry holds to
    /// achieve it is this layer's to change.
    #[test]
    fn a_reference_to_an_absent_key_creates_it_as_null() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let class = ClassBuilder::new("VivifyHolder").prop("a", true).build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
        let slot = unsafe { Object::prop_at(h, 16) };
        unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            ll_release(src as *mut RcHeader);
        }

        let r = unsafe { make_ref(context_ptr, MemoryCategory::GcHeap, slot, Key::Int(5)) };

        unsafe {
            assert!(!r.is_null(), "an absent key was reported as a refusal");
            assert_eq!(
                get(slot, Key::Int(5))
                    .expect("the absent key was not created")
                    .tag(),
                Tag::Null,
                "the vivified element reads as something other than null"
            );

            // The write `$r = 7` makes goes into the box's own slot,
            // which is where a reference-state element is written.
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                r as *mut RcHeader,
                &raw mut (*r).value,
                std::ptr::null_mut(),
                Value::int(7),
            ));
            let read = get(slot, Key::Int(5)).expect("the key stopped existing");
            assert_eq!(
                read.tag(),
                Tag::Int,
                "the write through the reference is not visible at the key"
            );
            assert_eq!(read.as_int(), 7);

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
        }
    }

    /// The whole of the shared-box rule: `$a=['x'=>1]; $b=$a; $r=&$b['x']; $r=2`
    /// leaves `$a['x']` at 1 and `$b['x']` at 2. The shared table is
    /// separated before the box is written, so `$a` never names the box
    /// and the reference is not refused.
    #[test]
    fn taking_a_reference_separates_the_shared_table_first() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let x = mk(b"x");
        unsafe {
            crate::refcount::ll_retain(x as *mut RcHeader);
            crate::array::testing::insert(src, Key::Str(x), Value::int(1));
        }

        let x_shared = unsafe { (*x).rc.refcount };
        // `slot_a` is `$a`, `slot_b` is `$b`.
        let (h, slot_a, slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };

        let r = unsafe { make_ref(context_ptr, MemoryCategory::GcHeap, slot_b, Key::Str(x)) };
        assert!(!r.is_null(), "the reference was refused");

        unsafe {
            assert_ne!(
                (*slot_b).entity_ptr() as *mut LLArray,
                src,
                "the shared table was boxed without separating"
            );
            assert_eq!(
                (*slot_a).entity_ptr() as *mut LLArray,
                src,
                "the other holder followed the separation"
            );
            assert!(
                crate::array::testing::get(src, Key::Str(x)).unwrap().tag() != Tag::Reference,
                "the original's element was boxed too"
            );

            // `$r = 2` through the public door: `$b['x']` is in a
            // reference state, so the store finds the box and writes
            // into it.
            assert!(set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot_b,
                Key::Str(x),
                Value::int(2)
            ));
            assert_eq!((*r).value.as_int(), 2, "the store missed the box");
            assert_eq!(
                get(slot_a, Key::Str(x)).unwrap().as_int(),
                1,
                "the write through the reference reached the other holder"
            );
            assert_eq!(get(slot_b, Key::Str(x)).unwrap().as_int(), 2);

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            assert_eq!((*x).rc.refcount, x_shared - 1);
            assert!(ll_release(x as *mut RcHeader));
            crate::object::ll_entity_die(x as *mut RcHeader);
        }
    }
}

/// Growth moves the entries into another chunk and compaction slides
/// them inside the one they are in, which is why an element
/// reference is a `ReferenceBox` and not a pointer to the slot.
mod a_box_outliving_what_moves_the_entry {
    use super::*;

    // ---- a reference into an element ---------------------------------

    /// The box outlives the storage the element lived in. A slot pointer
    /// would be dangling after the growth below; the box is not, which is
    /// the whole reason an element reference is boxed
    /// ([`box_element`]).
    #[test]
    fn a_reference_into_an_element_survives_growth() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;

        let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        unsafe {
            crate::array::testing::insert(a, Key::Int(1), Value::int(41));

            let r = box_element(a, arena_ptr, Key::Int(1));
            assert!(!r.is_null());
            assert_eq!((*r).value.as_int(), 41);

            // Enough inserts to reallocate the storage several times.
            for i in 2..5000i64 {
                crate::array::testing::insert(a, Key::Int(i), Value::int(i));
            }

            (*r).value = Value::int(99);
            assert_eq!((*r).value.as_int(), 99);

            // The element still holds the same box.
            let again = box_element(a, arena_ptr, Key::Int(1));
            assert_eq!(again, r, "asking twice must not build a second box");

            // Released to zero before the kill: `ll_free` asserts that
            // a slot reaching the free list carries a dead header.
            assert!(ll_release(a as *mut RcHeader));
            crate::object::ll_entity_die(a as *mut RcHeader);
        }
    }

    /// Compaction slides live entries down inside the same chunk, which
    /// moves the element without moving the storage — the case a double
    /// read of the storage pointer cannot see, and the box does not care
    /// about either.
    #[test]
    fn a_reference_into_an_element_survives_compaction() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;

        let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        unsafe {
            for i in 0..200i64 {
                crate::array::testing::insert(a, Key::Int(i), Value::int(i));
            }

            let r = box_element(a, arena_ptr, Key::Int(150));
            assert!(!r.is_null());
            for i in 0..150i64 {
                let _ = crate::array::testing::remove(a, Key::Int(i));
            }

            crate::array::testing::compact(a);

            assert_eq!(
                box_element(a, arena_ptr, Key::Int(150)),
                r,
                "compaction moved the element, not the box"
            );
            (*r).value = Value::int(-1);
            assert_eq!(
                crate::array::testing::get(a, Key::Int(150)).unwrap().tag(),
                Tag::Reference,
                "the element holds the box, not the value"
            );

            assert!(ll_release(a as *mut RcHeader));
            crate::object::ll_entity_die(a as *mut RcHeader);
        }
    }
}

/// A copy unwraps a box nobody else names and shares one a live `&`
/// still binds, which is where PHP collapses a reference and the
/// only place it does — so a write through a shared box reaches both
/// holders. In the arena the count is an upper bound, so the copy
/// errs toward sharing: a count above the holders can only share,
/// never unwrap a box a live name still reaches.
mod what_a_copy_does_with_a_box {
    use super::*;

    /// Four cases against php 8.3.6, in both memory
    /// categories. The copy unwraps a box nobody else names and shares
    /// one a live `&` still binds — which is where PHP collapses a
    /// reference, and the only place it does.
    #[test]
    fn a_copy_unwraps_a_box_with_a_single_holder() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        for category in [MemoryCategory::GcHeap, MemoryCategory::RequestArena] {
            assert_eq!(
                unsafe { reference_then_copy(context_ptr, arena_ptr, category, false, false) },
                (1, 3),
                "no reference: {category:?}"
            );
            assert_eq!(
                unsafe { reference_then_copy(context_ptr, arena_ptr, category, true, false) },
                (1, 3),
                "a dead reference must not alias the copy: {category:?}"
            );
            assert_eq!(
                unsafe { reference_then_copy(context_ptr, arena_ptr, category, true, true) },
                (3, 3),
                "a live reference must alias the copy: {category:?}"
            );
        }

        crate::memory::context::set_current_context(std::ptr::null_mut());
        unsafe { crate::promote::arena_reset_full(arena_ptr) };
    }

    /// A box is identity, so the deep copy out of the arena **shares**
    /// it rather than boxing a second one, and the escape hold-count is
    /// what keeps the arena box alive for the longer-lived copy.
    #[test]
    fn a_copy_over_an_arena_source_shares_the_box() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        let src = unsafe { ll_array_new(MemoryCategory::RequestArena) };
        let boxed = unsafe {
            crate::array::testing::insert(src, Key::Int(0), Value::int(7));
            let boxed = box_element(src, arena_ptr, Key::Int(0));
            assert!(!boxed.is_null());
            boxed
        };

        assert_eq!(
            unsafe { crate::object::header_category(boxed as *const RcHeader) },
            MemoryCategory::GcHeap,
            "an arena array's box is still a heap entity"
        );
        assert_eq!(
            unsafe { (*boxed).rc.flags } & crate::refcount::IS_ESCAPEE,
            0,
            "a heap box is never an escapee"
        );
        assert_eq!(
            unsafe { (*boxed).rc.refcount },
            1,
            "the source's entry is the box's one holder"
        );

        let copy = unsafe {
            crate::object::escape_copy(arena_ptr, MemoryCategory::GcHeap, src as *mut RcHeader)
        } as *mut LLArray;
        assert!(!copy.is_null());

        unsafe {
            assert_eq!(
                crate::array::testing::get(copy, Key::Int(0))
                    .unwrap()
                    .entity_ptr(),
                boxed as *mut RcHeader,
                "the copy boxed a second reference instead of sharing this one"
            );
            // A heap box is counted like any other heap entity, which is
            // the whole reason the box lives there: that count is what
            // the copy reads to decide between sharing and unwrapping. It
            // stood at one before this copy, and the copy shared anyway,
            // because an escape copy is a store crossing a lifetime
            // boundary rather than a duplication and collapses nothing
            // (`entity::CopyReason`).
            assert_eq!(
                (*boxed).rc.refcount,
                2,
                "the copy took no hold of its own on the shared box"
            );

            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
            assert_eq!(
                (*boxed).rc.refcount,
                1,
                "the dying copy kept its hold on the box"
            );

            // The source's own reference is the reset's to give back, and
            // the record is what makes that happen. Draining it here is
            // the reset's release, done by hand so the box's death is
            // visible to this test.
            let mut logged = Vec::new();
            arena.drain_release_log(|e| logged.push(e));
            assert!(
                logged.contains(&(boxed as *mut RcHeader)),
                "the arena entry holding a heap box logged no release"
            );
            for e in logged {
                if ll_release(e) {
                    crate::object::ll_entity_die(e);
                }
            }
        }

        crate::memory::context::set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    /// The sequence that separates an exact holder count from an upper
    /// bound, pinned in both categories because the two answers differ
    /// and the difference is decided rather than accidental
    /// (`dev/DECISIONS.md`, 2026-08-08, the arena's upper bound).
    ///
    /// `$a=[1]; $r=&$a[0]; $b=$a; $b[0]=3; unset($b); unset($r);
    /// $c=$a; $c[0]=9;` then `($a[0], $c[0])`. php 8.3.6 answers
    /// `(3, 9)`: by the third copy the box has one holder and is
    /// collapsed. The heap agrees. The arena answers `(9, 9)`, because
    /// `unset($b)` gives nothing back there — an arena container is not
    /// counted, so it dies at the reset and its hold on the box stands
    /// until then. The copy therefore errs toward sharing, which is the
    /// safe direction: a count above the holders can only share, never
    /// unwrap a box a live name still reaches.
    #[test]
    fn the_arena_reads_a_box_count_as_an_upper_bound() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        for (category, expected) in [
            (MemoryCategory::GcHeap, (3, 9)),
            (MemoryCategory::RequestArena, (9, 9)),
        ] {
            let class = ClassBuilder::new("UpperBoundHolder")
                .prop("a", true)
                .prop("b", true)
                .prop("c", true)
                .build();
            let holder = unsafe { new_constructed(context_ptr, class, category) };
            let slot_a = unsafe { Object::prop_at(holder, 16) };
            let slot_b = unsafe { Object::prop_at(holder, 32) };
            let slot_c = unsafe { Object::prop_at(holder, 48) };
            let a = unsafe { ll_array_new(category) };
            let answer = unsafe {
                assert!(crate::memory::barrier::ref_store(
                    arena_ptr,
                    holder as *mut RcHeader,
                    slot_a,
                    std::ptr::null_mut(),
                    Value::entity(Tag::Array, a as *mut RcHeader),
                ));
                ll_release(a as *mut RcHeader);
                assert!(set(
                    context_ptr,
                    category,
                    slot_a,
                    Key::Int(0),
                    Value::int(1)
                ));

                // `$r = &$a[0]`, then a copy taken while it is alive.
                let r = make_ref(context_ptr, category, slot_a, Key::Int(0));
                assert!(!r.is_null());
                crate::refcount::ll_retain(r as *mut RcHeader);
                assert!(crate::memory::barrier::ref_store(
                    arena_ptr,
                    holder as *mut RcHeader,
                    slot_b,
                    std::ptr::null_mut(),
                    *slot_a,
                ));
                assert!(set(
                    context_ptr,
                    category,
                    slot_b,
                    Key::Int(0),
                    Value::int(3)
                ));

                // `unset($b)` through the holder's own category, which is
                // the step the arena defers, and `unset($r)` through the
                // frame's.
                let held_b = (*slot_b).entity_ptr();
                assert!(crate::memory::barrier::ref_store(
                    arena_ptr,
                    holder as *mut RcHeader,
                    slot_b,
                    held_b,
                    Value::null(),
                ));
                crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, r as *mut RcHeader);

                // `$c = $a; $c[0] = 9;`
                assert!(crate::memory::barrier::ref_store(
                    arena_ptr,
                    holder as *mut RcHeader,
                    slot_c,
                    std::ptr::null_mut(),
                    *slot_a,
                ));
                assert!(set(
                    context_ptr,
                    category,
                    slot_c,
                    Key::Int(0),
                    Value::int(9)
                ));

                let read_a = get(slot_a, Key::Int(0)).expect("the key is there").as_int();
                let read_c = get(slot_c, Key::Int(0)).expect("the key is there").as_int();
                if category == MemoryCategory::GcHeap {
                    assert!(ll_release(holder as *mut RcHeader));
                    ll_object_die(holder);
                }

                (read_a, read_c)
            };

            assert_eq!(answer, expected, "{category:?}");
        }

        crate::memory::context::set_current_context(std::ptr::null_mut());
        unsafe { crate::promote::arena_reset_full(arena_ptr) };
    }

    /// A copy shares the box, so a write through one holder of a
    /// once-shared array reaches the other: `$a=['x'=>1]; $r=&$a['x'];
    /// $b=$a; $b['x']=3;` leaves both at 3, which is PHP's rule and the
    /// reason its manual warns about copying an array holding a
    /// reference.
    #[test]
    fn a_write_through_a_shared_box_reaches_both_holders() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let class = ClassBuilder::new("SharedBoxHolder")
            .prop("a", true)
            .prop("b", true)
            .build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
        let slot_a = unsafe { Object::prop_at(h, 16) };
        let slot_b = unsafe { Object::prop_at(h, 32) };
        unsafe {
            crate::array::testing::insert(src, Key::Int(0), Value::int(1));
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot_a,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            ll_release(src as *mut RcHeader);
        }

        // `$r = &$a['x']` while `$a` is the only holder: nothing to
        // separate, so the box goes into the array both will share.
        let r = unsafe { make_ref(context_ptr, MemoryCategory::GcHeap, slot_a, Key::Int(0)) };
        assert!(!r.is_null());
        // `$r` is a name, and a name is a holder. The layer hands the box
        // back at the element's count and leaves the caller's reference to
        // the caller; without it the box has one holder and the copy below
        // would unwrap it rather than share it.
        unsafe { crate::refcount::ll_retain(r as *mut RcHeader) };
        assert_eq!(
            unsafe { (*slot_a).entity_ptr() } as *mut LLArray,
            src,
            "an exclusively owned array separated"
        );

        unsafe {
            // `$b = $a`, then `$b['x'] = 3`.
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot_b,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            assert!(set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot_b,
                Key::Int(0),
                Value::int(3)
            ));

            let copy = (*slot_b).entity_ptr() as *mut LLArray;
            assert_ne!(copy, src, "the shared table separated");
            assert_eq!(
                crate::array::testing::get(copy, Key::Int(0))
                    .unwrap()
                    .entity_ptr(),
                r as *mut RcHeader,
                "the copy boxed a second reference instead of sharing this one"
            );
            assert_eq!((*r).value.as_int(), 3);
            assert_eq!(
                get(slot_a, Key::Int(0)).unwrap().as_int(),
                3,
                "the shared box did not carry the write to the other holder"
            );
            assert_eq!(get(slot_b, Key::Int(0)).unwrap().as_int(), 3);

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            // `$r` goes out of scope last, and it is the box's final
            // holder once both arrays are gone.
            assert!(ll_release(r as *mut RcHeader));
            crate::object::ll_entity_die(r as *mut RcHeader);
        }
    }
}

/// A box is a heap entity whatever the array's category, so boxing
/// an element of an arena array crosses the boundary twice: the
/// element enters a longer-lived holder and is counted as an escape,
/// and the box enters the arena entry, which logs its release
/// against the reset. After a promotion the write reads the owner's
/// category at the call rather than a cached answer, the header
/// having been rewritten a moment before.
mod crossing_out_of_the_arena {
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
    /// like (`dev/DECISIONS.md`, 2026-08-07).
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
}
