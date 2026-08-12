//! A copy takes the source's rung before its first insert, the mode
//! deciding how a key is hashed and a table that adopts it later
//! having already indexed its entries the other way. All three
//! states carry: escalated stays escalated, a drawn salt is
//! inherited rather than redrawn, and an unsalted copy stays a full
//! citizen of the ladder.

use super::*;

/// A copy of an attacked table is attacked. The mode is one-way on
/// the source and `$b = $a` is the ordinary thing the language does,
/// so a copy that starts weak hands the attacker an unescalated table
/// whenever they want one.
///
/// **The colliding set is removed before the copy, and that is the
/// point.** While the whole set is still in the table the copy
/// re-escalates on its own — the equal-hash trigger fires again on
/// the ninth collider it re-inserts — so a copy made then proves
/// nothing about carrying the state. `unset` is what makes the loss
/// permanent: below the trigger's threshold nothing re-fires, and the
/// table is back to the hash the attacker already knows, ready for
/// the same flood again.
///
/// Seen failing on `is_strong` for the copy.
#[test]
fn a_copy_of_an_escalated_table_is_escalated() {
    use crate::array::table::EQUAL_HASH_LIMIT;
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;

    let src = hash_arr();
    let colliders: Vec<*mut LLString> = (0..EQUAL_HASH_LIMIT as usize + 4)
        .map(|i| {
            let s = mk(format!("collider-{i}").as_bytes());
            // Forged rather than found: constructing a set of equal
            // full hashes needs a break of the hash, and the code
            // path the attack reaches is this one.
            unsafe { (*s).hash = 0x0BAD_C0DE_0BAD_C0DE };
            unsafe {
                crate::refcount::ll_retain(s as *mut RcHeader);
                crate::array::testing::insert(src, Key::Str(s), Value::int(i as i64));
            }

            s
        })
        .collect();
    assert!(
        unsafe { crate::array::testing::table(src).is_strong() },
        "the forged set did not escalate the source, so this proves nothing"
    );

    // Leave one collider behind: far below the trigger, so nothing in
    // the copy can re-fire it.
    for s in &colliders[1..] {
        // `remove` hands the stored key back with the value — the
        // table's one reference per stored key — so the table's
        // reference is released through what came back and the
        // creation reference through the test's own pointer.
        let (_, key) = unsafe { crate::array::testing::remove(src, Key::Str(*s)) }.unwrap();
        assert_eq!(key, *s, "the entry held the inserted key entity");
        unsafe {
            assert!(!ll_release(key as *mut RcHeader), "the table's");
            assert!(ll_release(*s as *mut RcHeader), "and the test's");
            crate::object::ll_entity_die(*s as *mut RcHeader);
        }
    }

    let copy = unsafe {
        separate(
            src,
            MemoryCategory::GcHeap,
            arena_ptr,
            CopyReason::Duplication,
        )
    };

    assert!(!copy.is_null());
    assert!(
        unsafe { crate::array::testing::table(copy).is_strong() },
        "the copy came back to the hash the attacker already knows"
    );
    assert_eq!(
        unsafe { crate::array::testing::get(copy, Key::Str(colliders[0])) }
            .unwrap()
            .as_int(),
        0,
        "a key was lost by the copy's own hashing"
    );

    unsafe {
        assert!(ll_release(copy as *mut RcHeader));
        crate::object::ll_entity_die(copy as *mut RcHeader);
        assert!(ll_release(src as *mut RcHeader));
        crate::object::ll_entity_die(src as *mut RcHeader);
        assert!(ll_release(colliders[0] as *mut RcHeader));
        crate::object::ll_entity_die(colliders[0] as *mut RcHeader);
    }
}

/// A copy of a table whose salt the first rung drew indexes exactly
/// as the source does. The bit without the number would mean
/// `mix_int(k, 0)` — a mix every attacker computes offline — and a
/// fresh draw would break the ladder's bound: a copy's second long
/// chain must escalate, not rebuild again. Seen failing on the salt
/// equality.
#[test]
fn a_copy_of_a_reseeded_table_inherits_the_drawn_salt() {
    use crate::array::table::CHAIN_LIMIT;
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;

    let src = hash_arr();
    for i in 0..(CHAIN_LIMIT as i64 + 1) {
        unsafe {
            crate::array::testing::insert(src, Key::Int(i * 1024), Value::int(i));
        }
    }

    assert!(
        unsafe { crate::array::testing::table(src).is_reseeded() },
        "the stride flood did not fire the rung, so this proves nothing"
    );
    let drawn = unsafe { crate::array::testing::table(src).salt() };

    let copy = unsafe {
        separate(
            src,
            MemoryCategory::GcHeap,
            arena_ptr,
            CopyReason::Duplication,
        )
    };

    assert!(!copy.is_null());
    assert!(unsafe { crate::array::testing::table(copy).is_reseeded() });
    assert_eq!(
        unsafe { crate::array::testing::table(copy).salt() },
        drawn,
        "the copy indexes under a salt of its own"
    );
    for i in 0..(CHAIN_LIMIT as i64 + 1) {
        assert_eq!(
            unsafe { crate::array::testing::get(copy, Key::Int(i * 1024)) }
                .unwrap()
                .as_int(),
            i
        );
    }

    unsafe {
        assert!(ll_release(copy as *mut RcHeader));
        crate::object::ll_entity_die(copy as *mut RcHeader);
        assert!(ll_release(src as *mut RcHeader));
        crate::object::ll_entity_die(src as *mut RcHeader);
    }
}

/// The third state a copy can inherit: nothing. A copy of an
/// unsalted source starts unsalted — by-value integer indexing, no
/// mix — and stays a full citizen of the ladder: its own flood fires
/// its own first rung, drawing a salt of its own.
#[test]
fn a_copy_of_an_unsalted_table_is_unsalted_and_climbs_its_own_ladder() {
    use crate::array::table::CHAIN_LIMIT;
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;

    let src = hash_arr();
    for i in 0..3i64 {
        unsafe {
            crate::array::testing::insert(src, Key::Int(i * 1024), Value::int(i));
        }
    }

    assert!(unsafe { !crate::array::testing::table(src).is_reseeded() });

    let copy = unsafe {
        separate(
            src,
            MemoryCategory::GcHeap,
            arena_ptr,
            CopyReason::Duplication,
        )
    };

    assert!(!copy.is_null());
    assert!(
        unsafe { !crate::array::testing::table(copy).is_reseeded() },
        "a copy of an unsalted table drew a salt from nowhere"
    );

    for i in 3..(CHAIN_LIMIT as i64 + 1) {
        unsafe {
            crate::array::testing::insert(copy, Key::Int(i * 1024), Value::int(i));
        }
    }

    assert!(
        unsafe { crate::array::testing::table(copy).is_reseeded() },
        "the copy's own flood must fire the copy's own rung"
    );
    for i in 0..(CHAIN_LIMIT as i64 + 1) {
        assert_eq!(
            unsafe { crate::array::testing::get(copy, Key::Int(i * 1024)) }
                .unwrap()
                .as_int(),
            i
        );
    }

    unsafe {
        assert!(ll_release(copy as *mut RcHeader));
        crate::object::ll_entity_die(copy as *mut RcHeader);
        assert!(ll_release(src as *mut RcHeader));
        crate::object::ll_entity_die(src as *mut RcHeader);
    }
}
