//! The window tag in header byte 7 (`dev/design/recycler-over-counts.md`,
//! §2): a consent opens the next window and publishes its number beside the
//! token; every count write tags its entity, and every slot write of an
//! object or an array tags its holder; the tag and the collector's stamp in
//! byte 6 stand apart.
//!
//! **One `*mut` per local header, made once and used for the reads as well**,
//! for the reason `the_maturation_stamp_the_commit_writes` gives.

use super::*;
use crate::cycle::token::{RECALL_NONE, REQUESTED, TraceToken, word};

/// A consent opens the window one past the last and stores its number where
/// the collector reads it after the grant; 255 wraps to 1, and 0, which no
/// window carries, is skipped.
#[test]
fn a_consent_opens_the_next_window_and_publishes_it() {
    // A slot no other case of the binary consents on.
    const SLOT: usize = 6;
    let token = TraceToken::new_held();
    token.release();
    set_window(254);

    for expected in [255, 1, 2] {
        token.request_for_test(word(REQUESTED, SLOT));
        assert!(token.consent(word(REQUESTED, SLOT), RECALL_NONE).is_ok());
        assert_eq!(
            token.window(),
            expected,
            "the grant carries the window opened"
        );

        let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
        let header: *mut RcHeader = &raw mut h;
        unsafe { ll_retain(header) };
        assert_eq!(
            unsafe { window_tag(header) },
            expected,
            "this thread tags with the window its consent opened"
        );
        token.release_claim(SLOT, false);
    }

    set_window(0);
}

/// A consent whose swap fails — the request it read is gone — opens no window
/// and spends no number: the thread keeps tagging with the window it had, and
/// the next consent that succeeds opens the number this one would have.
#[test]
fn a_lost_consent_spends_no_number() {
    // A slot no other case of the binary consents on.
    const SLOT: usize = 5;
    let token = TraceToken::new_held();
    token.release();
    set_window(30);

    // No request stands, so the swap from REQUESTED reads the mutator's byte.
    assert!(token.consent(word(REQUESTED, SLOT), RECALL_NONE).is_err());
    let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
    let header: *mut RcHeader = &raw mut h;
    unsafe { ll_retain(header) };
    assert_eq!(
        unsafe { window_tag(header) },
        30,
        "the lost consent opened nothing"
    );

    token.request_for_test(word(REQUESTED, SLOT));
    assert!(token.consent(word(REQUESTED, SLOT), RECALL_NONE).is_ok());
    assert_eq!(
        token.window(),
        31,
        "the number the lost consent did not spend"
    );
    token.release_claim(SLOT, false);

    set_window(0);
}

/// A retain and a release each tag the entity whose count they write with the
/// window open at the time.
#[test]
fn every_count_write_tags_its_entity() {
    let _g = crate::memory::block_pool::test_guard();
    let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);

    set_window(5);
    retain(&mut h);
    assert_eq!(unsafe { window_tag(&raw mut h) }, 5, "the retain tagged");

    set_window(6);
    assert!(!release(&mut h));
    assert_eq!(unsafe { window_tag(&raw mut h) }, 6, "the release tagged");

    set_window(0);
}

/// The count writes outside retain and release — a count set whole, the
/// teardown guard's `+1` — tag through the same primitive.
#[test]
fn the_other_count_writes_tag_too() {
    let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
    let header: *mut RcHeader = &raw mut h;

    set_window(11);
    unsafe { set_header_refcount(header, 3) };
    assert_eq!(unsafe { window_tag(header) }, 11, "the count set whole");

    set_window(12);
    unsafe { mutator_guard_retain(header) };
    assert_eq!(unsafe { window_tag(header) }, 12, "the guard's +1");
    assert_eq!(unsafe { header_refcount(header) }, 4);

    set_window(0);
}

/// The tag shares byte 7 with nothing: it leaves byte 6's stamp and the
/// mutator's flags standing, and the stamp leaves the tag.
#[test]
fn the_tag_and_the_stamp_stand_apart() {
    let mut h = RcHeader::new(MemoryCategory::GcHeap, COW);
    let header: *mut RcHeader = &raw mut h;
    let stamp = MaturationStamp { epoch: 9, age: 1 };

    unsafe { write_maturation_stamp(header, stamp) };
    set_window(77);
    unsafe { tag_with_the_window(header) };
    set_window(0);

    assert_eq!(
        unsafe { read_maturation_stamp(header) },
        stamp,
        "the tag kept the stamp"
    );
    assert_eq!(unsafe { window_tag(header) }, 77);

    unsafe { write_maturation_stamp(header, MaturationStamp { epoch: 3, age: 1 }) };
    assert_eq!(unsafe { window_tag(header) }, 77, "the stamp kept the tag");
    assert_eq!(
        h.flags & 0x0000_FFFF,
        MemoryCategory::GcHeap as u32 | COW,
        "neither touched the mutator's half"
    );
}

/// A store into an object's slot tags the object holding the slot, besides
/// the stored entity's count write; a write into an array's slots tags the
/// array, a vector's and a hash's alike.
#[test]
fn a_slot_write_tags_its_holder() {
    use crate::array::table::Key;
    use crate::class::ClassBuilder;
    use crate::memory::arena::Arena;
    use crate::memory::context::LLContext;
    use crate::object::{Object, ll_object_die, new_constructed};
    use crate::test_support::{prop_offset, store_prop};
    use crate::value::Value;

    let _g = crate::memory::block_pool::test_guard();
    let class = ClassBuilder::new("WindowTagHolder")
        .prop("kept", true)
        .build();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let holder = unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) };
    let kept = unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) };
    let array = unsafe { crate::array::entity::ll_array_new(MemoryCategory::GcHeap) };

    set_window(40);
    unsafe { store_prop(arena_ptr, holder, prop_offset(0), kept) };
    assert!(unsafe { crate::array::testing::push(array, Value::int(7)) });
    set_window(0);

    assert_eq!(
        unsafe { window_tag(holder as *const RcHeader) },
        40,
        "the holder whose slot changed"
    );
    assert_eq!(
        unsafe { window_tag(kept as *const RcHeader) },
        40,
        "the stored entity, by its count write"
    );
    assert_eq!(
        unsafe { window_tag(array as *const RcHeader) },
        40,
        "the array whose slots changed"
    );

    // The ABI form generated code calls under the feature, which names the
    // holder: here a store of null, whose only count write would be the
    // displaced occupant's, were there one.
    let slot = unsafe { Object::prop_at(kept, prop_offset(0)) };
    set_window(43);
    assert!(unsafe {
        crate::memory::barrier::ll_store_box_in(
            &mut context,
            MemoryCategory::GcHeap as u32,
            kept as *mut RcHeader,
            slot,
            Value::null(),
        )
    });
    set_window(0);
    assert_eq!(
        unsafe { window_tag(kept as *const RcHeader) },
        43,
        "the holder the ABI named"
    );

    // A hash array, whose writes come through the table's mutable view: an
    // insert, then a remove, each in a window of its own.
    let table = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
    set_window(41);
    let inserted = unsafe { crate::array::testing::insert(table, Key::Int(1), Value::int(2)) };
    assert!(matches!(inserted, Some((true, None))));
    assert_eq!(
        unsafe { window_tag(table as *const RcHeader) },
        41,
        "the insert"
    );
    set_window(42);
    assert!(unsafe { crate::array::testing::remove(table, Key::Int(1)) }.is_some());
    set_window(0);
    assert_eq!(
        unsafe { window_tag(table as *const RcHeader) },
        42,
        "the remove"
    );

    unsafe {
        assert!(ll_release(table as *mut RcHeader));
        crate::object::ll_entity_die(table as *mut RcHeader);
        store_prop(
            arena_ptr,
            holder,
            prop_offset(0),
            std::ptr::null_mut::<Object>(),
        );
        assert!(ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
        assert!(ll_release(kept as *mut RcHeader));
        ll_object_die(kept);
        assert!(ll_release(array as *mut RcHeader));
        crate::object::ll_entity_die(array as *mut RcHeader);
    }
}
