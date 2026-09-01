//! A copy takes the source's defense state before its first insert, the mode
//! deciding how a key is hashed and a table that adopts it later
//! having already indexed its entries the other way. The two defense-state
//! bits carry; the salt does not, because the copy draws its own from
//! its own storage address, and a set forged against the source
//! scatters instead of arriving slot for slot. The terminal admission denial is
//! here too: a source that refuses an admission is still copyable,
//! the replay being exempt from that refusal, and no chain in a copy
//! reaches the threshold.

use super::*;

/// Spend both rebuilds: string keys whose *full* hashes agree fire the
/// equal-hash threshold, and escalation draws the salt on its way, so
/// the table comes back strong and salted. Forged rather than found —
/// a real such set needs a break of the hash — and the same path the
/// attack would reach.
///
/// The keys stay in the table, which holds one reference to each; the
/// caller keeps the creation reference and owes each key a release.
fn escalate_with_equal_hashes(a: *mut LLArray) -> Vec<*mut LLString> {
    use crate::array::table::EQUAL_HASH_LIMIT;
    let keys: Vec<*mut LLString> = (0..EQUAL_HASH_LIMIT as usize + 1)
        .map(|i| {
            let s = mk(format!("collider-{i}").as_bytes());
            unsafe { (*s).hash = 0x0BAD_C0DE_0BAD_C0DE };
            unsafe {
                crate::refcount::ll_retain(s as *mut RcHeader);
                crate::array::testing::insert(a, Key::Str(s), Value::int(i as i64));
            }

            s
        })
        .collect();
    assert!(
        unsafe { crate::array::testing::table(a).is_strong() },
        "the forged set did not escalate the table, so nothing below proves anything"
    );
    keys
}

/// How far a forged family agrees, and therefore the widest mask that
/// still puts the whole of it in one slot: a table of more slots than
/// this scatters it, and a test whose subject is one chain has to say so
/// where its table can reach that width.
const AGREEING_BITS: u64 = 8;

/// `n` integer keys whose slots agree in the low [`AGREEING_BITS`] bits
/// of the table's own derivation, found by search because an escalated
/// table's integer slot is a keyed mix and cannot be forged.
///
/// The bits are fixed rather than taken from the current mask: the keys
/// have to share a slot at every slot count the table passes through as
/// it grows, and agreement in eight bits is agreement at every count up
/// to 256. `from` starts the search above whatever integer keys the
/// table already holds.
fn family_sharing_a_slot(a: *mut LLArray, from: i64, n: usize) -> Vec<i64> {
    let low = (1u64 << AGREEING_BITS) - 1;
    let table = unsafe { crate::array::testing::table(a) };
    let target = table.slot_hash_of(Key::Int(from)) & low;
    let mut keys = vec![from];
    let mut k = from;
    while keys.len() < n {
        k += 1;
        if table.slot_hash_of(Key::Int(k)) & low == target {
            keys.push(k);
        }
    }

    keys
}

/// A copy of an attacked table is attacked. The mode is one-way on
/// the source and `$b = $a` is the ordinary thing the language does,
/// so a copy that starts weak hands the attacker an unescalated table
/// whenever they want one.
///
/// **The colliding set is removed before the copy, and that is the
/// point.** While the whole set is still in the table the copy
/// re-escalates on its own — the equal-hash threshold fires again on
/// the ninth collider it re-inserts — so a copy made then proves
/// nothing about carrying the state. `unset` is what makes the loss
/// permanent: below the threshold nothing is met again, and the
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

    // Leave one collider behind: far below the threshold, so nothing in
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

/// A copy of a table whose salt the salted rebuild drew indexes under a
/// salt of its own, drawn from the copy's own storage address. The
/// number is what an attacker's set is built against, and the replay
/// puts every one of its keys back by hand, so a copy keeping the
/// source's number would reproduce the source's chains slot for slot
/// — and reproduce them past the point where the collision defense can answer,
/// both defense-state bits being spent by inheritance.
///
/// The collision defense's bound is carried by those bits rather than by the
/// number: the copy's next long chain escalates because it arrives
/// already reseeded. What the bit without a number would mean is
/// `mix_int(k, 0)`, a mix every attacker computes offline, so the
/// redraw is what the inherited bit stands on.
///
/// Seen failing on the salt inequality.
#[test]
fn a_copy_of_a_reseeded_table_redraws_its_salt() {
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
        "the stride flood drew no salt, so this proves nothing"
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
    assert!(
        unsafe { crate::array::testing::table(copy).is_reseeded() },
        "the defense-state bit is inherited: the copy's next long chain escalates"
    );
    assert_ne!(
        unsafe { crate::array::testing::table(copy).salt() },
        drawn,
        "the copy indexes under the source's number, so a forged set arrives intact"
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
/// mix — and stays a full citizen of the collision defense: its own flood fires
/// its own salted rebuild, drawing a salt of its own.
#[test]
fn a_copy_of_an_unsalted_table_is_unsalted_and_runs_its_own_defense() {
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
        "the copy's own flood must draw the copy's own salt"
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

/// A source whose collision defense refuses an admission is still copyable. The
/// terminal admission denial answers a threshold met past both rebuilds, and
/// only an admission: the copy replays keys the source admitted once, so every
/// entry arrives and the copy holds what the source held.
///
/// Green before the copy's own change, and pinned here rather than
/// left to the table's tests: the exemption is `Table::insert`'s, and
/// what this says is that the copy path reaches it — `fill_table_from`
/// replays.
#[test]
fn a_copy_of_a_table_that_refuses_an_admission_still_copies() {
    use crate::array::table::CHAIN_LIMIT;
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;

    let src = hash_arr();
    let colliders = escalate_with_equal_hashes(src);
    let family = family_sharing_a_slot(src, 1, CHAIN_LIMIT as usize + 1);
    let (chain, tripper) = family.split_at(CHAIN_LIMIT as usize);
    for (i, k) in chain.iter().enumerate() {
        // As replays: the build must not spring the defense it is here to
        // observe, and a stray collider sharing the slot would put the
        // last of them over the threshold.
        let outcome = unsafe {
            crate::array::testing::raw_insert(
                src,
                InsertKind::Replay,
                Key::Int(*k),
                Value::int(i as i64),
            )
        };

        assert!(matches!(outcome, InsertOutcome::Added));
    }

    assert!(
        matches!(
            unsafe {
                crate::array::testing::raw_insert(
                    src,
                    InsertKind::Admission,
                    Key::Int(tripper[0]),
                    Value::int(-1),
                )
            },
            InsertOutcome::AdmissionDenied
        ),
        "a chain threshold met past both rebuilds refuses an admission"
    );
    assert!(
        !unsafe { crate::array::testing::contains(src, Key::Int(tripper[0])) },
        "a refused key is not in the table"
    );

    let copy = unsafe {
        separate(
            src,
            MemoryCategory::GcHeap,
            arena_ptr,
            CopyReason::Duplication,
        )
    };

    assert!(!copy.is_null(), "the copy's replay was refused");
    for (i, k) in chain.iter().enumerate() {
        assert_eq!(
            unsafe { crate::array::testing::get(copy, Key::Int(*k)) }
                .unwrap()
                .as_int(),
            i as i64,
            "the forged chain did not survive the copy"
        );
    }

    for (i, s) in colliders.iter().enumerate() {
        assert_eq!(
            unsafe { crate::array::testing::get(copy, Key::Str(*s)) }
                .unwrap()
                .as_int(),
            i as i64,
            "the escalating set did not survive the copy"
        );
    }

    unsafe {
        assert!(ll_release(copy as *mut RcHeader));
        crate::object::ll_entity_die(copy as *mut RcHeader);
        assert!(ll_release(src as *mut RcHeader));
        crate::object::ll_entity_die(src as *mut RcHeader);
        for s in &colliders {
            assert!(ll_release(*s as *mut RcHeader), "the test's own reference");
            crate::object::ll_entity_die(*s as *mut RcHeader);
        }
    }
}

/// No chain in a copy reaches the threshold, even where the source's own
/// slot array is wide enough to hide the set that would make one.
///
/// The source here holds 64 forged keys agreeing in eight bits of
/// their slot, scattered across the 131072 slots a 40000-entry table
/// grew, and those 40000 are then unset. The copy is sized by what it
/// replays, so those eight bits cover its whole mask: under the
/// source's number the 64 would arrive as one chain, with both defense-statee
/// bits spent by inheritance and the replay exempt from the refusal,
/// so nothing rebuilds it and the chain is heritable one copy to the
/// next. The redraw is what answers that, the copy's salt coming from
/// its own storage address and re-deriving every slot. The assertion
/// on the mask below is what keeps a scatter from being the mask's own
/// doing.
///
/// Seen failing on a chain of 64.
#[test]
#[cfg_attr(miri, ignore = "40000 inserts and their removal")]
fn no_chain_in_a_copy_of_a_dense_then_unset_table_reaches_a_threshold() {
    use crate::array::table::CHAIN_LIMIT;
    const FILLER: i64 = 40_000;
    const FORGED: usize = 64;
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;

    let src = hash_arr();
    for i in 0..FILLER {
        unsafe {
            crate::array::testing::insert(src, Key::Int(i), Value::int(i));
        }
    }

    assert!(
        unsafe { !crate::array::testing::table(src).is_reseeded() },
        "dense by-value keys form no chain, so the defense is untouched here"
    );
    assert_eq!(
        unsafe { as_table(src) }.1.nslots(),
        131072,
        "the scenario is a wide slot array, and the copy's cost is stated in it"
    );

    let colliders = escalate_with_equal_hashes(src);
    let family = family_sharing_a_slot(src, FILLER, FORGED);
    for (i, k) in family.iter().enumerate() {
        let outcome = unsafe {
            crate::array::testing::raw_insert(
                src,
                InsertKind::Replay,
                Key::Int(*k),
                Value::int(i as i64),
            )
        };

        assert!(matches!(outcome, InsertOutcome::Added));
    }

    for i in 0..FILLER {
        let (_, key) = unsafe { crate::array::testing::remove(src, Key::Int(i)) }.unwrap();
        assert!(key.is_null(), "an integer key owes the caller no reference");
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
    let (copy_table, copy_head) = unsafe { as_table(copy) };
    assert!(
        copy_head.nslots() <= 1 << AGREEING_BITS,
        "the copy's mask is wider than the family agrees, so the family \
         would scatter under any salt and the chain below proves nothing"
    );
    let longest = copy_table.longest_chain(copy_head);
    assert!(
        longest <= CHAIN_LIMIT as usize,
        "a copy holds a chain of {longest}: the forged set arrived slot for slot, \
         and the replay is exempt from the denial that would answer it"
    );
    for (i, k) in family.iter().enumerate() {
        assert_eq!(
            unsafe { crate::array::testing::get(copy, Key::Int(*k)) }
                .unwrap()
                .as_int(),
            i as i64
        );
    }

    unsafe {
        assert!(ll_release(copy as *mut RcHeader));
        crate::object::ll_entity_die(copy as *mut RcHeader);
        assert!(ll_release(src as *mut RcHeader));
        crate::object::ll_entity_die(src as *mut RcHeader);
        for s in &colliders {
            assert!(ll_release(*s as *mut RcHeader), "the test's own reference");
            crate::object::ll_entity_die(*s as *mut RcHeader);
        }
    }
}

/// A copy of a reseeded source that holds no entries still owns a chunk and a
/// salt of its own. The defense-state bit travels whatever the entry count is —
/// the source is one `unset` away from being flooded again, and its copy is the
/// same table — while the draw takes a storage address, so the copy takes the
/// growth schedule's first chunk to be drawn from rather than starting with
/// none.
///
/// Seen failing against a copy that presizes nothing: the draw meets a
/// null chunk and the debug assertion in `redraw_salt` fires.
#[test]
fn a_copy_of_a_reseeded_but_emptied_source_owns_a_chunk_and_a_salt() {
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
        "the stride flood drew no salt, so this proves nothing"
    );
    let drawn = unsafe { crate::array::testing::table(src).salt() };
    for i in 0..(CHAIN_LIMIT as i64 + 1) {
        let (_, key) = unsafe { crate::array::testing::remove(src, Key::Int(i * 1024)) }.unwrap();
        assert!(key.is_null(), "an integer key owes the caller no reference");
    }

    assert_eq!(
        unsafe { crate::array::testing::table(src) }.len(),
        0,
        "the source must be empty for the copy to have nothing to replay"
    );

    let copy = unsafe {
        separate(
            src,
            MemoryCategory::GcHeap,
            arena_ptr,
            CopyReason::Duplication,
        )
    };

    assert!(!copy.is_null());
    let (copy_table, copy_head) = unsafe { as_table(copy) };
    assert!(
        copy_table.is_reseeded(),
        "the defense-state bit did not travel"
    );
    assert!(
        !copy_head.storage().is_null(),
        "the salt was drawn from an address the copy does not own"
    );
    assert_eq!(
        copy_head.nslots(),
        16,
        "the chunk is the schedule's first, taken for the draw alone"
    );
    assert_ne!(
        copy_table.salt(),
        drawn,
        "the copy indexes under the source's number"
    );

    unsafe {
        crate::array::testing::insert(copy, Key::Int(1), Value::int(7));
    }

    assert_eq!(
        unsafe { crate::array::testing::get(copy, Key::Int(1)) }
            .unwrap()
            .as_int(),
        7,
        "the presized chunk does not take an entry"
    );

    unsafe {
        assert!(ll_release(copy as *mut RcHeader));
        crate::object::ll_entity_die(copy as *mut RcHeader);
        assert!(ll_release(src as *mut RcHeader));
        crate::object::ll_entity_die(src as *mut RcHeader);
    }
}
