//! The zeroth rung pays no mix: a fresh table indexes an integer key
//! by its value, as Zend does, so the salt is paid where a flood
//! shows up rather than by every honest table. A long chain draws
//! the salt once and a second one escalates to the keyed hash
//! instead of drawing again, which is what bounds an attacker at one
//! rebuild and one escalation per table. A salt already drawn is
//! never redrawn: redrawing in response to equal-hash keys is what
//! made Perl's REHASH exploitable.

use super::*;

/// Forge the state the backstop exists for: many entries whose *full*
/// 64-bit hash agrees. Real construction of such a set needs a break
/// of the hash; here the stored hash is written directly, which
/// exercises the same code path the attack would reach.
fn force_equal_hashes(m: &mut Owned, n: usize) {
    for i in 0..n {
        let s = mk(format!("collider-{i}").as_bytes());
        unsafe { (*s).hash = 0x0BAD_C0DE_0BAD_C0DE };
        m.insert(Key::Str(s), Value::int(i as i64));
    }
}

/// Forge the other trigger's state: keys whose full hashes *differ*,
/// so the equal-hash trigger stays quiet, but whose low 16 bits agree,
/// so every one lands in the same index slot at any table size up to
/// 65536 and they form one chain.
fn extend_one_chain(m: &mut Owned, from: usize, to: usize) -> Vec<*mut LLString> {
    (from..to)
        .map(|i| {
            let s = mk(format!("chain-{i}").as_bytes());
            unsafe { (*s).hash = ((i as u64 + 1) << 16) | 0xC0DE };
            m.insert(Key::Str(s), Value::int(i as i64));
            s
        })
        .collect()
}

/// The ladder's zeroth rung: a fresh table indexes an integer key by
/// its value, as Zend does, and pays no mix. Three stride keys
/// sharing one bucket is the by-value signature — a salted mix would
/// scatter them. The salt is paid where a flood shows up rather than by
/// every honest table.
#[test]
fn a_fresh_table_indexes_an_integer_key_by_its_value() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    for i in 0..3i64 {
        m.insert(Key::Int(i * 1024), Value::int(i));
    }

    assert!(
        !m.is_reseeded(),
        "three keys are far below the chain trigger"
    );
    assert_eq!(m.salt, 0, "an unsalted table holds no number to mix with");
    let mut chain = 0usize;
    let mut i = unsafe { *m.slots().add(0) };
    while i != NONE {
        chain += 1;
        i = m.entry(i as usize).link();
    }

    assert_eq!(
        chain, 3,
        "stride keys share slot 0 only when indexed by value"
    );
    for i in 0..3i64 {
        assert_eq!(m.get(Key::Int(i * 1024)).unwrap().as_int(), i);
    }
}

/// The flood the zeroth rung admits by design: indexed by value, a
/// power-of-two stride builds exactly one chain — which is the first
/// rung's own trigger, so nobody had to predict where keys come
/// from. The rung draws a salt and rebuilds; the mix scatters the
/// rest of the flood and no key is lost across the rebuild.
#[test]
fn an_integer_flood_fires_the_first_rung_and_the_drawn_salt_scatters_it() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    for i in 0..512i64 {
        m.insert(Key::Int(i * 1024), Value::int(i));
    }

    assert!(m.is_reseeded(), "the flood's own chain is the trigger");
    assert!(
        !m.is_strong(),
        "differing hashes never take the strong rung"
    );
    assert_ne!(m.salt, 0, "the rung drew nothing");
    // Longest chain: with the drawn salt this is a handful; by-value
    // indexing would put all 512 in one bucket.
    let mut longest = 0usize;
    for slot in 0..m.nslots() {
        let mut n = 0usize;
        let mut i = unsafe { *m.slots().add(slot) };
        while i != NONE {
            n += 1;
            i = m.entry(i as usize).link();
        }

        longest = longest.max(n);
    }

    assert!(
        longest < 16,
        "longest chain {longest} — the drawn salt is not being applied"
    );
    for i in 0..512i64 {
        assert_eq!(
            m.get(Key::Int(i * 1024)).unwrap().as_int(),
            i,
            "a key was lost across the rung's rebuild"
        );
    }
}

/// The salt is a secret, not a checksum: derived from the storage
/// address under the per-process key, so holding the artifact — which
/// under `hash-folding` contains the seed — prices no salt. The seed's
/// hash of the bare address is the number an artifact-holder computes,
/// so it is the one comparison that can go red on the defect.
#[test]
fn the_drawn_salt_is_not_the_bare_address_hash() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    // The honest phase pushes the capacity ahead of the flood, so the
    // tripping insert does not also grow: a grow moves the storage in
    // the same call that drew the salt, and the address below would
    // then be one the salt was never derived from.
    // 70, not 64: at exactly 64 the honest phase ends with used == cap,
    // and the first forged insert grows — moving the storage mid-test.
    for i in 0..70i64 {
        m.insert(Key::Int(i), Value::int(i));
    }

    let storage_before = m.storage();
    extend_one_chain(&mut m, 0, CHAIN_LIMIT as usize + 1);
    assert!(m.is_reseeded(), "the forged chain draws the salt");
    assert_eq!(
        m.storage(),
        storage_before,
        "the draw and the assert see one storage, or the test is void"
    );
    assert_ne!(
        m.salt,
        crate::hash::hash_bytes(&(m.storage() as u64).to_le_bytes()),
        "the salt reads as the seed's hash of the bare address, which \
         anyone holding a folding artifact can compute"
    );
}

/// Rung one mixes string slots as well as integer ones: under
/// `hash-folding` a cached string hash is a build constant, so a rung
/// that salts only integers rebuilds an offline-built string chain
/// into the same chain. The probe side and the rebuild side must move
/// together, or the salted kind's entries are present, iterable and
/// unfindable.
#[test]
fn a_reseeded_tables_string_slot_is_salted_on_both_sides() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    let s = mk(b"salted");
    m.insert(Key::Str(s), Value::int(7));
    for i in 0..512i64 {
        m.insert(Key::Int(i * 1024), Value::int(i));
    }

    assert!(m.is_reseeded());
    assert!(!m.is_strong(), "differing hashes stay below strong");
    let cached = unsafe { LLString::hash(s) };
    let probe = {
        let (table, _) = unsafe { crate::array::entity::as_table(m.0) };
        table.slot_hash(Key::Str(s))
    };
    assert_ne!(
        probe, cached,
        "a reseeded table still slots a string by its cached hash, \
         which no salt enters"
    );

    let (table, head) = unsafe { crate::array::entity::as_table(m.0) };
    let of_entry = (0..head.used())
        .map(|i| table.entry(head, i))
        .find(|e| e.string_key() == s)
        .map(|e| table.entry_slot_hash(e))
        .expect("the string's entry is present");
    assert_eq!(
        probe, of_entry,
        "the probe side and the rebuild side disagree on a string slot"
    );
    assert_eq!(m.get(Key::Str(s)).unwrap().as_int(), 7);
}

/// Forge a chain against a drawn salt: hashes whose *salted mixes*
/// agree in their low 13 bits, so the family shares one slot at any
/// table size up to 8192 — the post-draw counterpart of
/// [`extend_one_chain`], which forges against the cached hash an
/// unsalted table slots by.
fn extend_one_chain_salted(m: &mut Owned, from: usize, to: usize) -> Vec<*mut LLString> {
    let salt = m.salt;
    assert_ne!(salt, 0, "forging against no salt forges the wrong rung");
    assert!(
        m.head().nslots() <= 1 << AGREEING_BITS,
        "the table outgrew the family's agreement, so it no longer shares a slot"
    );
    let mut forged = Vec::new();
    let mut h: u64 = 1;
    while forged.len() < to - from {
        if mix_word(h, salt) & AGREEING_MASK == AGREEING_TARGET {
            forged.push(h);
        }

        h += 1;
    }

    forged
        .into_iter()
        .enumerate()
        .map(|(i, hash)| {
            let s = mk(format!("salted-chain-{}", from + i).as_bytes());
            unsafe { (*s).hash = hash };
            m.insert(Key::Str(s), Value::int((from + i) as i64));
            s
        })
        .collect()
}

/// The ladder's rungs above the zeroth, in order and each once. A
/// long chain of keys whose hashes differ draws the salt a fresh
/// table does not have; a second chain forged against the *drawn*
/// salt — read through the test window, as an attacker with a timing
/// oracle would recover it — escalates instead of drawing again,
/// which is what bounds the attacker at one rebuild and one
/// escalation per table.
///
/// Seen failing at the escalation: without the reseed counter the
/// chain trigger redraws forever. The second family is forged under
/// the salted mix because the mix covers string slots: an unsalted
/// forge scatters at the draw's own rebuild and never trips again —
/// the rung doing its work, not a way to reach the next one.
#[test]
fn a_long_chain_draws_the_salt_once_and_then_escalates() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    assert_eq!(m.salt, 0, "a fresh table is the zeroth rung");

    let first = extend_one_chain(&mut m, 0, CHAIN_LIMIT as usize + 1);
    assert!(m.is_reseeded(), "the first long chain draws the salt");
    assert_ne!(m.salt, 0, "and the drawn salt is a real one");
    assert!(!m.is_strong(), "and does not escalate on the first firing");
    let redrawn = m.salt;

    let second = extend_one_chain_salted(
        &mut m,
        CHAIN_LIMIT as usize + 1,
        2 * (CHAIN_LIMIT as usize + 1),
    );
    assert!(m.is_strong(), "the second firing escalates");
    assert_eq!(
        m.salt, redrawn,
        "escalation redraws nothing: that is the Perl REHASH defect"
    );
    assert!(
        m.nslots() <= 1 << 13,
        "the forge's low-13 agreement no longer covers this table"
    );

    for (i, s) in first.iter().chain(second.iter()).enumerate() {
        assert_eq!(
            m.get(Key::Str(*s)).unwrap().as_int(),
            i as i64,
            "a key was lost across the ladder"
        );
    }
}

/// Equal full hashes take the strong rung directly — and firing from
/// an unsalted table draws the salt on the way, because the keyed
/// hash's key *is* the salt and zero is a key every attacker knows.
#[test]
fn equal_full_hashes_escalate_the_table_to_the_keyed_hash() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    assert!(!m.is_strong());
    force_equal_hashes(&mut m, EQUAL_HASH_LIMIT as usize + 4);
    assert!(
        m.is_strong(),
        "a set of equal full hashes must escalate, not reseed"
    );
    assert!(
        m.is_reseeded(),
        "strong implies a drawn salt: the two bits never separate"
    );
    assert_ne!(
        m.salt, 0,
        "escalation from the zeroth rung left the keyed hash keyed by zero"
    );
}

#[test]
fn every_key_still_resolves_after_escalation() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();

    let honest: Vec<*mut LLString> = (0..50).map(|i| mk(format!("h{i}").as_bytes())).collect();
    for (i, s) in honest.iter().enumerate() {
        m.insert(Key::Str(*s), Value::int(1000 + i as i64));
    }

    let mut colliders = Vec::new();
    for i in 0..(EQUAL_HASH_LIMIT as usize + 4) {
        let s = mk(format!("collider-{i}").as_bytes());
        unsafe { (*s).hash = 0x0BAD_C0DE_0BAD_C0DE };
        m.insert(Key::Str(s), Value::int(i as i64));
        colliders.push(s);
    }

    assert!(m.is_strong());

    for (i, s) in honest.iter().enumerate() {
        assert_eq!(
            m.get(Key::Str(*s)).unwrap().as_int(),
            1000 + i as i64,
            "escalation must not lose an honest key"
        );
    }

    for (i, s) in colliders.iter().enumerate() {
        assert_eq!(m.get(Key::Str(*s)).unwrap().as_int(), i as i64);
    }

    assert_eq!(m.len(), 50 + EQUAL_HASH_LIMIT as usize + 4);
}

#[test]
fn escalation_scatters_a_colliding_set_instead_of_chaining_it() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    force_equal_hashes(&mut m, 64);
    assert!(m.is_strong());

    let mut longest = 0usize;
    for slot in 0..m.nslots() {
        let mut n = 0usize;
        let mut i = unsafe { *m.slots().add(slot) };
        while i != NONE {
            n += 1;
            i = m.entry(i as usize).link();
        }

        longest = longest.max(n);
    }

    assert!(
        longest < 16,
        "longest chain {longest} after escalation — the keyed hash is not separating them"
    );
}

/// A salt that is already drawn stays exactly as it was across
/// escalation: redrawing in response to equal-hash keys is what made
/// Perl's REHASH exploitable. The *draw* an unsalted escalation
/// makes is pinned by the test above; this pins that it never
/// becomes a redraw.
#[test]
fn escalation_happens_once_and_a_drawn_salt_is_not_redrawn_on_equal_hashes() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    extend_one_chain(&mut m, 0, CHAIN_LIMIT as usize + 1);
    assert!(m.is_reseeded(), "the chain draws the salt first");
    let drawn = m.salt;
    force_equal_hashes(&mut m, 64);
    assert!(m.is_strong());
    assert_eq!(
        m.salt, drawn,
        "redrawing the salt on equal hashes is the Perl REHASH defect"
    );
}

/// Names whose *strong* slots agree in their low [`AGREEING_BITS`] bits,
/// so the family shares one index slot at any table size up to twice
/// what these fixtures reach — the escalated counterpart of
/// [`extend_one_chain_salted`]. The search stands in for the break of
/// the keyed PRF the design's residual assumption prices
/// (`Table::draw_salt`): the test window reads the key, an attacker
/// would have to recover it.
///
/// **The width is the table's and not a round number.** One bit more
/// doubles the candidates the search reads, and it reads them under
/// Miri too, where a thirteen-bit family put this module past an hour;
/// one bit less and the family scatters, so the caller's slot count is
/// asserted rather than assumed.
///
/// Names only, no entities: the caller decides which are inserted and
/// which one springs the trigger.
/// How far a forged strong family agrees, and so the widest index that
/// still holds the whole of it in one slot: these fixtures fill a table
/// of 64 entries over 128 slots, and eight bits carry that with a
/// doubling to spare.
const AGREEING_BITS: u32 = 8;
const AGREEING_MASK: u64 = (1 << AGREEING_BITS) - 1;
/// Which of the agreeing slots the family lands in — any value under
/// the mask does, and a fixed one keeps the search reproducible.
const AGREEING_TARGET: u64 = 0xB5;

fn strong_slot_family(m: &Owned, n: usize, tag: &str) -> Vec<String> {
    assert!(
        m.head().nslots() <= 1 << AGREEING_BITS,
        "the table outgrew the family's agreement, so it no longer shares a slot"
    );
    let strong_key = {
        let (table, _) = unsafe { crate::array::entity::as_table(m.0) };
        table.strong_key()
    };
    let mut found = Vec::new();
    let mut i = 0usize;
    while found.len() < n {
        let name = format!("{tag}-{i}");
        if strong_hash(name.as_bytes(), strong_key) & AGREEING_MASK == AGREEING_TARGET {
            found.push(name);
        }

        i += 1;
    }

    found
}

/// The shared raw insert (`array::testing::raw_insert`) over the
/// wrapper these tests hold, so a call reads like every other one here.
fn raw_insert(m: &mut Owned, kind: InsertKind, key: Key, value: Value) -> InsertOutcome {
    unsafe { crate::array::testing::raw_insert(m.0, kind, key, value) }
}

/// A table with both rebuilds spent and one forged chain standing one
/// short of the trigger, plus the name that would spring it: the state
/// every terminal-rung test starts from.
fn spent_ladder_with_a_chain(m: &mut Owned) -> (Vec<*mut LLString>, String) {
    force_equal_hashes(m, EQUAL_HASH_LIMIT as usize + 1);
    assert!(m.is_strong(), "the ladder must be spent before the trip");
    let mut names = strong_slot_family(m, CHAIN_LIMIT as usize + 1, "strong-chain");
    let tripper = names.pop().expect("the family holds the tripper");
    let chain = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let s = mk(name.as_bytes());
            // As a replay, so the build cannot trip the rung itself: a
            // stray collider sharing the family's slot would otherwise
            // put the last additions over the trigger — whether it does
            // depends on the drawn salt, which varies with the storage
            // address, and a fixture must not.
            let outcome = raw_insert(m, InsertKind::Replay, Key::Str(s), Value::int(i as i64));
            assert!(matches!(outcome, InsertOutcome::Added));
            s
        })
        .collect();
    (chain, tripper)
}

/// Rung three: a chain trip on a table whose ladder is spent refuses
/// the admission, with every entry, count and key exactly as it was —
/// including the rung state, which a refusal spends nothing of.
///
/// Without the terminal rung `reseed` and `escalate` both return early
/// on a strong table, the admission is taken, and the chain grows
/// without bound.
#[test]
fn a_spent_ladders_chain_trip_refuses_the_admission_and_changes_nothing() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    let (chain, tripper) = spent_ladder_with_a_chain(&mut m);
    let len_before = m.len();
    let used_before = m.used();
    let salt_before = m.salt;

    let s = mk(tripper.as_bytes());
    let outcome = raw_insert(&mut m, InsertKind::Admission, Key::Str(s), Value::int(999));
    assert!(
        matches!(outcome, InsertOutcome::RefusedByLadder),
        "a trigger tripped with no rebuild left must refuse the admission"
    );
    assert_eq!(
        m.len(),
        len_before,
        "a refused insert changed the live count"
    );
    assert_eq!(m.used(), used_before, "a refused insert wrote an entry");
    assert_eq!(m.salt, salt_before, "a refusal spends nothing");
    assert!(m.is_strong(), "a refusal clears no rung state");
    assert!(
        m.get(Key::Str(s)).is_none(),
        "the refused key entered the table anyway"
    );
    for (i, c) in chain.iter().enumerate() {
        assert_eq!(
            m.get(Key::Str(*c)).unwrap().as_int(),
            i as i64,
            "a refusal disturbed a stored key"
        );
    }

    // The refusal consumed nothing, so the key reference is still the
    // test's to give back.
    unsafe {
        assert!(crate::refcount::ll_release(s as *mut RcHeader));
        crate::object::ll_entity_die(s as *mut RcHeader);
    }
}

/// The fourth terminal case: the equal-identity trigger over string
/// entries on a table that is already escalated. Equal full hashes are
/// what escalation answers; met again past it, there is no rebuild
/// left and the admission is refused.
#[test]
fn equal_hashes_on_an_escalated_table_refuse_instead_of_rebuilding() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    force_equal_hashes(&mut m, EQUAL_HASH_LIMIT as usize + 1);
    assert!(m.is_strong());

    // A second forged family: one strong slot, so the walk meets them
    // as one chain, and one forged full hash, so the equal-identity
    // counter counts every one of them.
    let names = strong_slot_family(&m, EQUAL_HASH_LIMIT as usize + 1, "equal-on-strong");
    let (tripper_name, stored) = names.split_last().expect("the family holds the tripper");
    for (i, name) in stored.iter().enumerate() {
        let s = mk(name.as_bytes());
        unsafe { (*s).hash = 0x0E0_0E0_0E0 };
        m.insert(Key::Str(s), Value::int(i as i64));
    }

    let len_before = m.len();
    let s = mk(tripper_name.as_bytes());
    unsafe { (*s).hash = 0x0E0_0E0_0E0 };
    let outcome = raw_insert(&mut m, InsertKind::Admission, Key::Str(s), Value::int(999));
    assert!(
        matches!(outcome, InsertOutcome::RefusedByLadder),
        "equal identities on an escalated table have no rebuild to take"
    );
    assert_eq!(m.len(), len_before);
    unsafe {
        assert!(crate::refcount::ll_release(s as *mut RcHeader));
        crate::object::ll_entity_die(s as *mut RcHeader);
    }
}

/// The exemption: the same trip that refuses an admission admits a
/// replay, because a key admitted once cannot be refused on
/// re-admission — the chain grows past the trigger's limit, which is
/// the price of honouring the earlier admission.
#[test]
fn a_replay_is_admitted_past_a_spent_ladder() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    let (_, tripper) = spent_ladder_with_a_chain(&mut m);
    let len_before = m.len();

    let s = mk(tripper.as_bytes());
    let outcome = raw_insert(&mut m, InsertKind::Replay, Key::Str(s), Value::int(999));
    assert!(
        matches!(outcome, InsertOutcome::Added),
        "a replay may not be refused by the ladder"
    );
    assert_eq!(m.len(), len_before + 1);
    assert_eq!(m.get(Key::Str(s)).unwrap().as_int(), 999);
}

/// Only the refusal is exempt: a replay tripping the chain trigger on
/// a fresh table fires rung one exactly as an admission does, so a
/// replayed flood is scattered rather than rebuilt verbatim.
#[test]
fn a_replay_still_fires_the_rungs_below_the_terminal_one() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    for i in 0..(CHAIN_LIMIT as i64 + 1) {
        let outcome = raw_insert(
            &mut m,
            InsertKind::Replay,
            Key::Int(i * 1024),
            Value::int(i),
        );
        assert!(matches!(outcome, InsertOutcome::Added));
    }

    assert!(
        m.is_reseeded(),
        "a replayed flood left rung one unfired: the exemption leaked \
         below the terminal rung"
    );
    for i in 0..(CHAIN_LIMIT as i64 + 1) {
        assert_eq!(m.get(Key::Int(i * 1024)).unwrap().as_int(), i);
    }
}

#[test]
fn the_cached_string_hash_is_not_touched_by_escalation() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    let s = mk(b"shared-with-other-tables");
    let h = unsafe { LLString::hash(s) };
    m.insert(Key::Str(s), Value::int(1));
    force_equal_hashes(&mut m, 64);
    assert!(m.is_strong());
    assert_eq!(
        unsafe { (*s).hash },
        h,
        "the +16 hash is shared across tables and must survive escalation"
    );
    assert_eq!(m.get(Key::Str(s)).unwrap().as_int(), 1);
}
