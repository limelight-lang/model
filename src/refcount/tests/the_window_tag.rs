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
/// array.
#[test]
fn a_slot_write_tags_its_holder() {
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

    unsafe {
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
