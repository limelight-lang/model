use super::*;
use crate::memory::context::LLContext;

fn ctx() -> *mut LLContext {
    std::ptr::null_mut()
}

/// A table whose storage is released when the binding is dropped, so
/// a test cannot leak blocks into the pool's free-list order and
/// disturb an unrelated test (which is exactly what happened once).
/// A table **inside its array**, which is the only place a table
/// lives: the memory its storage comes from is the owning entity's
/// header to say (`dev/DECISIONS.md` 2026-08-07), so a test needs the
/// entity to have a category to pass. Derefs to the table, so a test
/// reads as if it held one.
struct Owned(*mut crate::array::entity::LLArray);

/// The operations that need the category or the head, wrapped so that a
/// test writes them as if the table answered for itself. It cannot,
/// and for two reasons that both come from outside the table: the
/// category is read from the array through
/// `array::entity::category_of`, and a reference to the body carries
/// provenance over the body alone, so the entity pointer has to
/// arrive from outside; and the words a walker reads live in the
/// entity's head, which every operation over them takes as a
/// parameter (`array::head`). Supplying both here is what keeps a
/// test about the ordered hash from being a test about how to reach
/// one.
impl Owned {
    fn category(&self) -> MemoryCategory {
        unsafe { crate::array::entity::category_of(self.0) }
    }

    fn head(&self) -> &StorageHead {
        unsafe { &*crate::array::entity::storage_head(self.0) }
    }

    fn insert(&mut self, key: Key, value: Value) -> Option<(bool, Option<Value>)> {
        let category = self.category();
        let (table, head) = unsafe { crate::array::entity::as_table_mut(self.0) };
        table.insert(head, category, key, value)
    }

    fn get(&self, key: Key) -> Option<Value> {
        let (table, head) = unsafe { crate::array::entity::as_table(self.0) };
        table.get(head, key)
    }

    fn contains(&self, key: Key) -> bool {
        let (table, head) = unsafe { crate::array::entity::as_table(self.0) };
        table.contains(head, key)
    }

    #[must_use = "the pair carries the table's key reference; dropping it leaks the key"]
    fn remove(&mut self, key: Key) -> Option<(Value, *mut LLString)> {
        let (table, head) = unsafe { crate::array::entity::as_table_mut(self.0) };
        table.remove(head, key)
    }

    fn compact(&mut self) -> Option<usize> {
        let category = self.category();
        let (table, head) = unsafe { crate::array::entity::as_table_mut(self.0) };
        table.compact(head, category)
    }

    fn entry(&self, i: usize) -> &Entry {
        let (table, head) = unsafe { crate::array::entity::as_table(self.0) };
        table.entry(head, i)
    }

    fn iter(&self) -> impl Iterator<Item = &Entry> {
        let (table, head) = unsafe { crate::array::entity::as_table(self.0) };
        table.iter(head)
    }

    fn for_each_value(&self, f: impl FnMut(Value)) {
        let (table, head) = unsafe { crate::array::entity::as_table(self.0) };
        table.for_each_value(head, f);
    }

    fn for_each_string_key(&self, f: impl FnMut(*mut LLString)) {
        let (table, head) = unsafe { crate::array::entity::as_table(self.0) };
        table.for_each_string_key(head, f);
    }

    fn used(&self) -> usize {
        self.head().used()
    }

    fn nslots(&self) -> usize {
        self.head().nslots()
    }

    fn storage(&self) -> *mut u8 {
        self.head().storage()
    }

    fn version(&self) -> usize {
        self.head().version()
    }

    fn slots(&self) -> *mut u32 {
        Table::slots(self.head())
    }

    fn entries(&self) -> *mut Entry {
        Table::entries(self.head())
    }

    fn dispose(&mut self) {
        let category = self.category();
        unsafe { crate::array::entity::dispose_storage(self.0, category) };
    }
}

impl std::ops::Deref for Owned {
    type Target = Table;
    fn deref(&self) -> &Table {
        unsafe { crate::array::entity::as_table(self.0).0 }
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            crate::array::entity::dispose_storage(
                self.0,
                crate::array::entity::category_of(self.0),
            );
            // The entity's own slot, by hand rather than through
            // `ll_entity_die`: these tests own the children and give
            // them back themselves, and teardown would release them a
            // second time. The count goes to zero first because that
            // is what a slot reaching the free list must read.
            (*self.0).rc.refcount = 0;
            crate::memory::stdapi::ll_free(self.0 as *mut u8);
        }
    }
}

fn t() -> Owned {
    Owned(unsafe { crate::array::entity::ll_array_new(MemoryCategory::GcHeap) })
}

// ---- string keys -----------------------------------------------

fn mk(bytes: &[u8]) -> *mut LLString {
    unsafe { crate::string::ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, bytes) }
}

/// The entries on one slot's chain, in the order the lookup walks them.
///
/// A hole reached from a chain fails here rather than being skipped:
/// a removal that left its entry linked is what this exists to catch,
/// and `get` would answer correctly through such a chain anyway.
fn chain(m: &Owned, slot: usize) -> Vec<usize> {
    let mut walked = Vec::new();
    let mut i = unsafe { *m.slots().add(slot) };
    while i != NONE {
        let e = m.entry(i as usize);
        assert!(
            !e.is_hole(),
            "entry {i} is a hole and is still on the chain"
        );
        walked.push(i as usize);
        i = e.link();
    }

    walked
}

/// The same chain read as integer keys, which is what a test that built
/// it from a stride can compare against.
fn chain_keys(m: &Owned, slot: usize) -> Vec<i64> {
    chain(m, slot)
        .into_iter()
        .map(|i| {
            let e = m.entry(i);
            assert!(e.is_int_key(), "entry {i} is not an integer key");
            e.hash_or_key as i64
        })
        .collect()
}

// ---- the flood backstop -----------------------------------------

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

/// Insertion order is the enumeration order, so an overwrite keeps a
/// key's position while a delete and a reinsert put it at the end. A
/// removed key stays removed and the table goes on answering for
/// every other, and a removal of a key that was never there changes
/// nothing.
mod the_ordered_hash_itself {
    use super::*;

    #[test]
    fn an_empty_table_finds_nothing_and_does_not_allocate() {
        let m = t();
        assert!(m.is_empty());
        assert!(m.get(Key::Int(0)).is_none());
        assert!(!m.contains(Key::Int(1)));
        assert_eq!(m.used(), 0);
        let _ = ctx();
    }

    #[test]
    fn insert_then_get_round_trips_integer_keys() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        for i in 0..500i64 {
            let (added, old) = m.insert(Key::Int(i), Value::int(i * 3)).unwrap();
            assert!(added && old.is_none(), "a fresh key is an addition");
        }

        assert_eq!(m.len(), 500);
        for i in 0..500i64 {
            assert_eq!(m.get(Key::Int(i)).unwrap().as_int(), i * 3);
        }

        assert!(m.get(Key::Int(500)).is_none());
        assert!(m.get(Key::Int(-1)).is_none());
    }

    #[test]
    fn overwriting_a_key_keeps_its_position_and_returns_the_old_value() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        m.insert(Key::Int(1), Value::int(10));
        m.insert(Key::Int(2), Value::int(20));

        let (added, old) = m.insert(Key::Int(1), Value::int(11)).unwrap();
        assert!(!added, "an overwrite is not a new key");
        assert_eq!(old.unwrap().as_int(), 10);
        assert_eq!(m.len(), 2);

        let order: Vec<i64> = m.iter().map(|e| e.hash_or_key as i64).collect();
        assert_eq!(order, vec![1, 2], "an overwrite must not move the key");
    }

    #[test]
    fn iteration_is_insertion_order_and_skips_holes() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        for i in 0..10i64 {
            m.insert(Key::Int(i), Value::int(i));
        }

        for i in [1i64, 4, 7] {
            assert!(m.remove(Key::Int(i)).is_some());
        }

        let order: Vec<i64> = m.iter().map(|e| e.hash_or_key as i64).collect();
        assert_eq!(order, vec![0, 2, 3, 5, 6, 8, 9]);
        assert_eq!(m.len(), 7);
    }

    #[test]
    fn a_deleted_key_reinserts_at_the_end() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        for i in 0..3i64 {
            m.insert(Key::Int(i), Value::int(i));
        }

        let _ = m.remove(Key::Int(1));
        m.insert(Key::Int(1), Value::int(99));

        let order: Vec<i64> = m.iter().map(|e| e.hash_or_key as i64).collect();
        assert_eq!(order, vec![0, 2, 1], "PHP appends a re-inserted key");
        assert_eq!(m.get(Key::Int(1)).unwrap().as_int(), 99);
    }

    #[test]
    fn removing_shortens_the_chain_rather_than_leaving_a_marker() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();

        // Three keys a multiple of the table's sixteen slots apart share
        // slot 0, a fresh table indexing an integer key by its value.
        // The stride is what builds a chain at all: a dense set puts
        // every key in a slot of its own, and a removal there repairs
        // nothing.
        let stride = 1024i64;
        for i in 0..3i64 {
            m.insert(Key::Int(i * stride), Value::int(i));
        }

        assert_eq!(
            chain_keys(&m, 0),
            vec![2 * stride, stride, 0],
            "insertion is at the head, so the newest key comes first"
        );

        assert_eq!(m.remove(Key::Int(stride)).unwrap().0.as_int(), 1);
        assert_eq!(
            chain_keys(&m, 0),
            vec![2 * stride, 0],
            "the removed entry left the chain instead of staying on it"
        );
        assert!(m.entry(1).is_hole(), "and its entry is a hole");
        assert!(m.get(Key::Int(stride)).is_none());
        for i in [0i64, 2] {
            assert_eq!(m.get(Key::Int(i * stride)).unwrap().as_int(), i);
        }

        // The same over a dense set, kept as a sweep and not as the
        // instrument: every key there is alone in its slot, so every
        // removal takes the head-of-chain arm and a tombstoning table
        // satisfies these assertions too.
        let mut dense = t();
        for i in 0..64i64 {
            dense.insert(Key::Int(i), Value::int(i));
        }

        for i in 0..64i64 {
            assert_eq!(dense.remove(Key::Int(i)).unwrap().0.as_int(), i);
            assert!(
                dense.get(Key::Int(i)).is_none(),
                "a removed key stays removed"
            );
        }

        assert_eq!(dense.len(), 0);
        for i in 0..64i64 {
            assert!(!dense.contains(Key::Int(i)));
        }
    }

    #[test]
    fn removing_a_missing_key_reports_rather_than_disturbing_the_table() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        m.insert(Key::Int(1), Value::int(1));
        assert!(m.remove(Key::Int(2)).is_none());
        assert_eq!(m.len(), 1);
        assert_eq!(m.get(Key::Int(1)).unwrap().as_int(), 1);
    }

    #[test]
    fn negative_and_extreme_integer_keys_round_trip() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        let keys = [0i64, -1, 1, i64::MIN, i64::MAX, -1024, 1024];
        for (n, k) in keys.iter().enumerate() {
            m.insert(Key::Int(*k), Value::int(n as i64));
        }

        for (n, k) in keys.iter().enumerate() {
            assert_eq!(m.get(Key::Int(*k)).unwrap().as_int(), n as i64);
        }

        assert_eq!(m.len(), keys.len());
    }
}

/// PHP's rule in its three table-side clauses: a removal never
/// rewinds the cursor, an explicit key moves it past itself, and a
/// fresh table appends at 0. `i64::MAX` is the one key with no
/// successor, so the next append is refused rather than wrapped.
mod the_append_cursor {
    use super::*;

    /// PHP's append rule, its three table-side clauses: removal never
    /// rewinds the cursor, an explicit key moves it past itself, and a
    /// fresh table appends at 0. The PHP 8.3 arm — a negative key moves
    /// the cursor too, `$a[-5] = 1; $a[] = 2;` appending at −4 — is the
    /// assumption `PLAN.md` S2.4 records for Edmond to overturn.
    #[test]
    fn the_append_cursor_never_rewinds_and_an_explicit_key_moves_it() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        assert_eq!(m.append_key(), Some(0), "a fresh table appends at 0");
        for i in 0..3i64 {
            m.insert(Key::Int(i), Value::int(i));
        }

        let _ = m.remove(Key::Int(1));
        assert_eq!(m.append_key(), Some(3), "removal rewinds nothing");

        m.insert(Key::Int(9), Value::int(9));
        assert_eq!(
            m.append_key(),
            Some(10),
            "an explicit key moves the cursor past itself"
        );

        let mut n = t();
        n.insert(Key::Int(-5), Value::int(1));
        assert_eq!(
            n.append_key(),
            Some(-4),
            "PHP 8.3: a negative key moves the cursor"
        );
    }

    /// `i64::MAX` is the one integer key with no successor, so the next
    /// append is refused rather than wrapped — the same posture as
    /// `storage_bytes`' checked arithmetic.
    #[test]
    fn append_after_the_maximum_key_is_refused_rather_than_wrapping() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        m.insert(Key::Int(i64::MAX), Value::int(1));
        assert_eq!(m.append_key(), None, "i64::MAX + 1 must refuse, not wrap");
        m.insert(Key::Int(5), Value::int(2));
        assert_eq!(m.append_key(), None, "no later key un-exhausts the cursor");
    }
}

/// Growth and compaction both move entries, so both are bracketed by
/// the version counter — odd while the move runs — which is how a
/// walker tells that its reading of `storage`, `nslots` and `used`
/// came from one arrangement. Order and every key survive either,
/// and no link inside the storage is a pointer into it, which is
/// what lets promotion copy the whole block flat.
mod what_moves_the_entries {
    use super::*;

    /// Every operation that moves entries has to be visible to a walker
    /// that is reading them, and the version is how: odd while the move
    /// runs, changed afterwards. A walker validates its reading of
    /// `storage`, `nslots` and `used` against two readings of this
    /// (`PLAN.md`, item 12).
    ///
    /// **What forces a counter rather than a double read of `storage`
    /// has changed, and the counter stays.** The original argument was
    /// compaction sliding entries inside one chunk, which no reading of
    /// the pointer can see; since S13.1 compaction allocates, so that
    /// case is gone. What remains is the 2 → 3 migration, which changes
    /// what the bytes *mean* at an address that may not move, and the
    /// three words themselves: `storage` and `used` are published
    /// separately, and pairing them is exactly what a validated reading
    /// is for.
    #[test]
    fn moving_the_entries_shows_up_as_a_version_change() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        let at_rest = m.version();
        assert_eq!(at_rest % 2, 0, "a table nobody is rearranging reads even");

        // Enough inserts to cross several growths.
        for i in 0..200i64 {
            assert!(m.insert(Key::Int(i), Value::int(i)).is_some());
        }

        let after_growth = m.version();
        assert!(after_growth > at_rest, "growth moved entries silently");
        assert_eq!(after_growth % 2, 0, "the window was left open");

        let before_compaction = m.storage();
        assert!(m.compact().is_some(), "the compaction was refused");
        let after_compaction = m.version();
        assert!(
            after_compaction > after_growth,
            "compaction moved entries silently"
        );
        assert_ne!(
            m.storage(),
            before_compaction,
            "compaction is a move into a fresh chunk since S13.1"
        );
        assert_eq!(after_compaction % 2, 0, "the window was left open");
    }

    /// Compacting a table that has no chunk allocates nothing.
    ///
    /// A chunk for `cap == 0` holds no entry, and the state it leaves —
    /// `storage` non-null with `mask` set — is what the next insert reads
    /// as room for one: it wrote a 32-byte entry into sixteen granted
    /// bytes, and published the count for the walker to stride
    /// (Critic, S13.1). The insert below is half the test.
    #[test]
    fn compacting_a_table_with_no_chunk_allocates_nothing() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        assert_eq!(m.compact(), Some(0), "there was nothing to reclaim");
        assert!(
            m.storage().is_null(),
            "a compaction of nothing produced a chunk"
        );
        assert!(m.insert(Key::Int(1), Value::int(1)).is_some());
        assert_eq!(m.get(Key::Int(1)).unwrap().as_int(), 1);
        assert_eq!(m.used(), 1);
    }

    #[test]
    fn growth_preserves_every_key_and_the_order() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        // Enough to cross several doublings from the initial capacity.
        for i in 0..5000i64 {
            assert!(m.insert(Key::Int(i), Value::int(i)).is_some());
        }

        assert_eq!(m.len(), 5000);
        let order: Vec<i64> = m.iter().map(|e| e.hash_or_key as i64).collect();
        assert_eq!(order, (0..5000).collect::<Vec<i64>>());
        for i in 0..5000i64 {
            assert_eq!(m.get(Key::Int(i)).unwrap().as_int(), i);
        }
    }

    #[test]
    fn compaction_reclaims_holes_and_preserves_order() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        for i in 0..100i64 {
            m.insert(Key::Int(i), Value::int(i));
        }

        for i in (0..100i64).filter(|i| i % 2 == 0) {
            let _ = m.remove(Key::Int(i));
        }

        let before: Vec<i64> = m.iter().map(|e| e.hash_or_key as i64).collect();
        assert_eq!(m.used(), 100, "holes still occupy their slots");

        let moved = m.compact().expect("the compaction was refused");
        assert!(moved > 0);
        assert_eq!(m.used(), 50, "compaction reclaimed the holes");

        let after: Vec<i64> = m.iter().map(|e| e.hash_or_key as i64).collect();
        assert_eq!(before, after, "compaction preserves insertion order");
        for i in (0..100i64).filter(|i| i % 2 == 1) {
            assert_eq!(m.get(Key::Int(i)).unwrap().as_int(), i);
        }
    }

    #[test]
    fn a_string_key_survives_growth_and_compaction() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        let keys: Vec<*mut LLString> = (0..300).map(|i| mk(format!("k{i}").as_bytes())).collect();
        for (i, s) in keys.iter().enumerate() {
            m.insert(Key::Str(*s), Value::int(i as i64));
        }

        for (i, s) in keys.iter().enumerate() {
            if i % 3 == 0 {
                // Table tests leave key ownership unmodelled; the pair is
                // waived here and measured in `array::entity`'s tests.
                let _ = m.remove(Key::Str(*s));
            }
        }

        m.compact();
        for (i, s) in keys.iter().enumerate() {
            if i % 3 == 0 {
                assert!(m.get(Key::Str(*s)).is_none());
            } else {
                assert_eq!(m.get(Key::Str(*s)).unwrap().as_int(), i as i64);
            }
        }
    }

    /// Promotion copies the storage as one contiguous block, so nothing
    /// inside it may point into it. Every link is an index; this pins it.
    #[test]
    fn the_storage_holds_no_pointer_into_itself() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        for i in 0..500i64 {
            m.insert(Key::Int(i), Value::int(i));
        }

        let base = m.storage() as usize;
        let bytes = super::storage_bytes(m.nslots(), m.cap).unwrap();
        for k in 0..m.used() {
            let e = m.entry(k);
            for word in [e.hash_or_key, e.key as u64, e.value().as_int() as u64] {
                let w = word as usize;
                assert!(
                    w < base || w >= base + bytes,
                    "entry {k} holds a word pointing into the storage"
                );
            }
        }
    }
}

/// A string key is matched by content rather than by address, and
/// through the layout-agnostic accessor, so a key past what the heap
/// packs in one slot is found too: the inline accessor would build a
/// slice over the entity and compare the payload pointer instead of
/// the bytes. `7` and `"7"` are two keys, told apart by the entry's
/// key kind rather than by where they land.
mod keys_that_are_strings {
    use super::*;

    #[test]
    fn string_keys_round_trip_and_compare_by_content_not_by_pointer() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();

        let names: Vec<*mut LLString> = (0..200)
            .map(|i| mk(format!("key-{i}").as_bytes()))
            .collect();
        for (i, s) in names.iter().enumerate() {
            let (added, _) = m.insert(Key::Str(*s), Value::int(i as i64)).unwrap();
            assert!(added);
        }

        assert_eq!(m.len(), 200);

        // A *different* entity with the same bytes must find the entry:
        // the table compares content, since only interned names have
        // pointer identity.
        for i in 0..200usize {
            let other = mk(format!("key-{i}").as_bytes());
            assert_eq!(
                m.get(Key::Str(other)).unwrap().as_int(),
                i as i64,
                "a string key is matched by content"
            );
        }

        assert!(m.get(Key::Str(mk(b"absent"))).is_none());
    }

    /// The same, with a key past what the heap packs in one slot. Such a
    /// key is out of line, so every comparison has to reach its bytes
    /// through the layout-agnostic accessor: the inline one would build a
    /// 16 KiB slice over a 32-byte entity, comparing the payload pointer
    /// and whatever follows it, and a key that is present would read as
    /// absent.
    #[test]
    fn an_oversize_string_key_is_matched_by_content() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        let content = vec![b'k'; crate::memory::heap::MAX_SMALL * 2];
        let stored = mk(&content);
        assert_ne!(
            unsafe { crate::refcount::header_flags(stored as *const crate::refcount::RcHeader) }
                & crate::refcount::STRING_OUT_OF_LINE,
            0,
            "the key is out of line, or this test proves nothing"
        );
        m.insert(Key::Str(stored), Value::int(9));

        let other = mk(&content);
        assert_ne!(other, stored, "a second entity with the same bytes");
        assert_eq!(
            m.get(Key::Str(other)).unwrap().as_int(),
            9,
            "an equal oversize key finds the element"
        );

        let mut different = content.clone();
        *different.last_mut().unwrap() = b'x';
        let absent = mk(&different);
        assert!(
            m.get(Key::Str(absent)).is_none(),
            "and one that differs in its last byte does not"
        );

        // Unlike the small-key tests above, these three own payloads in
        // the buffer arena, and a leaked payload is not local to this
        // test: the block-adoption tests read the same thread's arena and
        // assume nothing is holding a block open.
        unsafe {
            for s in [stored, other, absent] {
                // Down to zero before the kill, whatever the table's
                // insert took: the free path asserts a dead header.
                while !crate::refcount::ll_release(s as *mut crate::refcount::RcHeader) {}
                crate::string::string_die(s);
            }
        }
    }

    #[test]
    fn integer_and_string_keys_coexist_without_aliasing() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        m.insert(Key::Int(7), Value::int(700));

        // The string's hash is written rather than hashed, so that the
        // one thing separating the two keys is the entry's key kind:
        // below `strong` a string's slot is its cached hash, so 7 puts
        // it on the integer's chain, and 7 is also what the entry stores
        // as its identity. Left to the real hash, the two share a slot
        // on about one run in sixteen — the seed is drawn per process —
        // and never share an identity at all, so the arm this is named
        // for went untested either way.
        let s = mk(b"7");
        unsafe { (*s).hash = 7 };
        m.insert(Key::Str(s), Value::int(77));

        assert_eq!(
            chain(&m, 7).len(),
            2,
            "the two keys are on one chain, which is where aliasing would show"
        );
        assert_eq!(m.len(), 2, "an integer key and a string key are two keys");
        assert_eq!(m.get(Key::Int(7)).unwrap().as_int(), 700);
        assert_eq!(m.get(Key::Str(s)).unwrap().as_int(), 77);
    }
}

/// The zeroth rung pays no mix: a fresh table indexes an integer key
/// by its value, as Zend does, so the salt is paid where a flood
/// shows up rather than by every honest table. A long chain draws
/// the salt once and a second one escalates to the keyed hash
/// instead of drawing again, which is what bounds an attacker at one
/// rebuild and one escalation per table. A salt already drawn is
/// never redrawn: redrawing in response to equal-hash keys is what
/// made Perl's REHASH exploitable.
mod the_flood_ladder {
    use super::*;

    /// The ladder's zeroth rung: a fresh table indexes an integer key by
    /// its value, as Zend does, and pays no mix. Three stride keys
    /// sharing one bucket is the by-value signature — a salted mix would
    /// scatter them (`PLAN.md` S2.1, Edmond 2026-08-07: the salt is paid
    /// where a flood shows up, not by every honest table).
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

    /// The ladder's rungs above the zeroth, in order and each once. A
    /// long chain of keys whose hashes differ draws the salt a fresh
    /// table does not have; the next one escalates instead of drawing
    /// again, which is what bounds the attacker at one rebuild and one
    /// escalation per table.
    ///
    /// Seen failing at the escalation: without the reseed counter the
    /// chain trigger redraws forever, and for string keys it cannot even
    /// separate them — below `strong` a string's slot is its cached hash,
    /// which no salt enters — so every later insert pays another O(used)
    /// rebuild and the chain stays exactly as long.
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

        let second = extend_one_chain(&mut m, CHAIN_LIMIT as usize + 1, CHAIN_LIMIT as usize + 2);
        assert!(m.is_strong(), "the second firing escalates");
        assert_eq!(
            m.salt, redrawn,
            "escalation redraws nothing: that is the Perl REHASH defect"
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
}

/// Enumeration covers the dense prefix and skips holes, and the hole
/// marker lives in the key word rather than in the element: a store
/// barrier writes all sixteen bytes of a `Value`, so a marker inside
/// one would be erased and the tracer would then walk a dead
/// element. A string key is enumerated beside the elements; whether
/// it is counted is `array::entity`'s to measure.
mod what_a_walker_is_shown {
    use super::*;

    // ---- what the memory manager is owed -----------------------------

    #[test]
    fn enumeration_is_complete_over_the_dense_prefix_and_skips_holes() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        for i in 0..100i64 {
            m.insert(Key::Int(i), Value::int(i));
        }

        for i in (0..100i64).filter(|i| i % 3 == 0) {
            let _ = m.remove(Key::Int(i));
        }

        let mut seen = Vec::new();
        m.for_each_value(|v| seen.push(v.as_int()));
        let expected: Vec<i64> = (0..100).filter(|i| i % 3 != 0).collect();
        assert_eq!(seen, expected, "every live element exactly once, in order");
        assert_eq!(seen.len(), m.len());
    }

    /// The reason the hole marker lives in `key`: a store barrier writes
    /// all sixteen bytes of a `Value`, so a marker inside it would be
    /// erased and the tracer would then walk a dead element.
    #[test]
    fn a_value_store_over_a_hole_does_not_resurrect_it() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        for i in 0..8i64 {
            m.insert(Key::Int(i), Value::int(i));
        }

        let _ = m.remove(Key::Int(3));
        // The store the table really performs, and the link it has to
        // carry through: the element field is private, so a whole `Value`
        // written into the slot by hand does not compile.
        let link_before = m.entry(3).link();
        unsafe { Entry::store_element(m.entries().add(3), Value::int(0xDEAD)) };
        assert!(
            m.entry(3).is_hole(),
            "an element store cleared the hole marker"
        );
        assert_eq!(
            m.entry(3).link(),
            link_before,
            "an element store moved the chain link"
        );
        let mut seen = Vec::new();
        m.for_each_value(|v| seen.push(v.as_int()));
        assert!(
            !seen.contains(&0xDEAD),
            "the hole survived a full value write"
        );
        assert_eq!(seen.len(), 7);
    }

    #[test]
    fn string_keys_are_enumerated_as_counted_children() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        let a = mk(b"alpha");
        let b = mk(b"beta");
        m.insert(Key::Str(a), Value::int(1));
        m.insert(Key::Int(9), Value::int(2));
        m.insert(Key::Str(b), Value::int(3));
        // Ownership unmodelled here too; `array::entity` measures it.
        let _ = m.remove(Key::Str(a));

        let mut keys = Vec::new();
        m.for_each_string_key(|s| keys.push(s));
        assert_eq!(
            keys,
            vec![b],
            "an integer key and a hole are not string children"
        );
    }
}

/// A long-lived table's storage is a buffer-arena chunk and a
/// request table's is an arena body; both allocators split at one
/// block payload, past which the storage is a dedicated run — the
/// split the 1025th element of a request array used to abort for. A
/// table dies wherever its last reference is dropped, so the free
/// routinely arrives from a thread that did not allocate it, and a
/// carry the reset refused leaves the category alone for promotion
/// to change.
mod where_the_storage_comes_from {
    use super::*;

    /// Where a long-lived table's storage lives, pinned from both ends:
    /// the block it comes out of is a buffer block, and disposing puts the
    /// chunk back on that block's free list. While storage came from
    /// `ll_alloc` it landed in a heap block, so the first assertion failed
    /// there and the second could not be asked at all.
    ///
    /// The return half is proved the way `string.rs` proves it for a
    /// payload: in critical mode an allocation searches the free lists, so
    /// the same address coming back means the chunk was really returned
    /// rather than merely forgotten.
    #[test]
    fn heap_storage_is_a_buffer_arena_chunk_and_is_returned_to_it() {
        use crate::memory::block_pool::{BLOCK_KIND_BUFFER, BLOCK_MASK, BLOCK_PAYLOAD};
        use crate::memory::buffer::{PressureMode, set_pressure_mode};
        use crate::memory::buffer_arena::with_buffer_arena;
        let _g = crate::memory::block_pool::test_guard();

        let mut m = t();
        m.insert(Key::Int(1), Value::int(1));
        let storage = m.storage();
        let capacity = m.storage_capacity;
        assert!(!storage.is_null());
        assert!(
            capacity <= BLOCK_PAYLOAD,
            "a table of one entry is a chunk, not an OS-direct run"
        );

        let kind = unsafe { *(((storage as usize) & !BLOCK_MASK) as *const u32) };
        assert_eq!(
            kind, BLOCK_KIND_BUFFER,
            "the storage came from somewhere other than the buffer arena"
        );

        m.dispose();

        set_pressure_mode(PressureMode::Critical);
        let (reused, _) = with_buffer_arena(|a| a.alloc(capacity));
        set_pressure_mode(PressureMode::Plenty);
        assert_eq!(reused, storage, "the storage was not returned to the arena");
        with_buffer_arena(|a| unsafe { a.free(reused, capacity) });
    }

    /// Past a block payload the storage is an OS-direct run instead, the
    /// arena's chunks being bounded by one block. The doubling that
    /// crosses the line frees a chunk and allocates a run, and teardown
    /// then frees the run; both are dispatched on the block kind, so a
    /// storage that lands in the wrong half is released by the wrong
    /// allocator. The table also has to still answer for every key it held
    /// before the crossing.
    #[test]
    fn a_storage_over_a_block_payload_is_an_os_direct_run() {
        use crate::memory::block_pool::{BLOCK_KIND_LARGE_RUN, BLOCK_MASK, BLOCK_PAYLOAD};
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        for i in 0..1100i64 {
            m.insert(Key::Int(i), Value::int(i));
        }

        assert!(
            m.storage_capacity > BLOCK_PAYLOAD,
            "the table never grew past one block, so this proves nothing"
        );

        let kind = unsafe { *(((m.storage() as usize) & !BLOCK_MASK) as *const u32) };
        assert_eq!(
            kind, BLOCK_KIND_LARGE_RUN,
            "a storage larger than a block is a run of blocks, which is what \
             decides the free path that releases it"
        );
        for i in 0..1100i64 {
            assert_eq!(m.get(Key::Int(i)).unwrap().as_int(), i);
        }
    }

    /// A request-arena table has to cross the same line, and the arena
    /// splits at it too: `Arena::alloc` asserts on anything larger than a
    /// block payload, and a run that size belongs to `alloc_large`, which
    /// records it so the reset frees it. Without the split the 1025th
    /// element of a request array kills the process, the release profile
    /// aborting rather than unwinding.
    #[test]
    fn a_request_arena_storage_over_a_block_takes_the_large_run_path() {
        use crate::memory::arena::Arena;
        use crate::memory::block_pool::BLOCK_PAYLOAD;
        use crate::memory::context::set_current_context;
        let _g = crate::memory::block_pool::test_guard();

        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut context = LLContext { arena: arena_ptr };
        let context_ptr: *mut LLContext = &mut context;
        set_current_context(context_ptr);

        // An arena array, because an arena table's storage is routed by
        // the header in front of it like every other table's.
        let a = unsafe { crate::array::entity::ll_array_new(MemoryCategory::RequestArena) };
        let (m, head) = unsafe { crate::array::entity::as_table_mut(a) };
        for i in 0..1100i64 {
            m.insert(
                head,
                unsafe { crate::array::entity::category_of(a) },
                Key::Int(i),
                Value::int(i),
            );
        }

        assert!(
            m.storage_capacity > BLOCK_PAYLOAD,
            "the table never grew past one block, so this proves nothing"
        );
        for i in 0..1100i64 {
            assert_eq!(m.get(head, Key::Int(i)).unwrap().as_int(), i);
        }

        m.dispose(head, unsafe { crate::array::entity::category_of(a) });
        set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    /// The reason the storage moved here at all: a table dies wherever its
    /// last reference is dropped, so the thread that frees a storage is
    /// routinely not the one that allocated it. What this pins is that the
    /// foreign free reaches the owner's block and leaves it alive — the
    /// posting stack itself is the arena's own contract, tested there.
    /// Under Miri it is also the only exercise of that path in this
    /// module.
    #[test]
    fn a_table_disposed_on_another_thread_leaves_the_owners_block_alive() {
        use crate::memory::block_pool::{BLOCK_KIND_BUFFER, BLOCK_MASK};
        let _g = crate::memory::block_pool::test_guard();

        let mut m = t();
        m.insert(Key::Int(1), Value::int(1));
        let storage = m.storage() as usize;

        // The **array** crosses, not the table: a table is embedded in
        // its entity and reads its category from the header in front of
        // it, so handing one over on its own would hand over a header
        // that is not there. An array pointer is a raw pointer and not
        // `Send` by inference; a dying entity crossing threads is the
        // case the buffer arena's ownership protocol exists for, not a
        // violation of it.
        struct HandOver(*mut crate::array::entity::LLArray);
        unsafe impl Send for HandOver {}
        let handed = std::mem::replace(&mut m, t());
        let carried = HandOver(handed.0);
        // The other thread disposes it; this one must not.
        std::mem::forget(handed);

        std::thread::spawn(move || {
            let carried = carried;
            unsafe {
                crate::array::entity::dispose_storage(
                    carried.0,
                    crate::array::entity::category_of(carried.0),
                );
                (*carried.0).rc.refcount = 0;
                crate::memory::stdapi::ll_free(carried.0 as *mut u8);
            }
        })
        .join()
        .unwrap();

        let kind = unsafe { *((storage & !BLOCK_MASK) as *const u32) };
        assert_eq!(
            kind, BLOCK_KIND_BUFFER,
            "the owner's block went home while the owner still held it"
        );
    }

    /// A refused carry decides no category of its own: it leaves the
    /// storage where it is and the header saying `RequestArena`, so
    /// promotion is what changes the answer a moment later, and the four
    /// rewrites `carry_out_of` used to make are gone
    /// (`dev/DECISIONS.md`, 2026-08-07).
    ///
    /// Where the next storage then comes from is no longer the table's to
    /// decide — since S10 it is handed a category and routes by it — so
    /// what it does with a promoted array is measured one layer up, in
    /// `element::tests::a_promoted_array_takes_its_next_storage_from_the_heap`.
    /// The danger both halves guard is one: an owner still answering
    /// `RequestArena` takes its next storage from whatever arena is
    /// mounted then, and that arena's reset returns the chunk to the pool
    /// with a live heap array pointing at it.
    #[test]
    fn a_refused_carry_leaves_the_category_where_it_was() {
        use crate::memory::arena::Arena;
        use crate::memory::block_pool::{BLOCK_PAYLOAD, FORCE_OOM};
        use crate::memory::context::set_current_context;
        use std::sync::atomic::Ordering;
        let _g = crate::memory::block_pool::test_guard();

        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut context = LLContext { arena: arena_ptr };
        let context_ptr: *mut LLContext = &mut context;
        set_current_context(context_ptr);

        let a = unsafe { crate::array::entity::ll_array_new(MemoryCategory::RequestArena) };
        let (m, head) = unsafe { crate::array::entity::as_table_mut(a) };
        for i in 0..8i64 {
            m.insert(
                head,
                unsafe { crate::array::entity::category_of(a) },
                Key::Int(i),
                Value::int(i),
            );
        }

        assert!(
            m.storage_capacity <= BLOCK_PAYLOAD,
            "an in-block storage is the only one that can be refused"
        );

        FORCE_OOM.store(true, Ordering::Relaxed);
        let carried = unsafe { crate::array::entity::carry_storage_out_of(arena_ptr, a) };
        FORCE_OOM.store(false, Ordering::Relaxed);
        assert!(!carried, "the copy was meant to be refused and was not");
        assert_eq!(
            unsafe { crate::array::entity::category_of(a) },
            MemoryCategory::RequestArena,
            "the carry decided a category of its own instead of leaving it to the header"
        );
        assert!(
            !head.storage().is_null(),
            "a refused carry left the array without the storage it had"
        );

        set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }
}

/// What the collector reads while the mutator rearranges the very
/// chunk it is striding.
///
/// **`cargo test` cannot judge this either** (`array::head`'s group
/// says the same of the head's placement): the walker's loads are
/// relaxed atomics, the mutator's writes are ordinary, and a run
/// reports nothing whichever way the entries are moved. What decides
/// it is Miri's data-race detector, so the test below is the
/// regression for `PLAN.md` S13.1 and its verdict is read from a Miri
/// run rather than from the suite.
///
/// Gated to `rc-walk`, and on the group rather than on the test: both
/// instruments it needs are that collector's — the relaxed reader and the
/// epoch whose flag parks a freed chunk. rc-trace walks nothing
/// concurrently, so the arrangement cannot be built there at all.
#[cfg(feature = "rc-walk")]
mod what_a_walker_reads_during_a_move {
    use super::*;

    /// A relaxed reader striding the entries while the owner compacts.
    ///
    /// The epoch flag is raised for the duration, which is what makes
    /// the test faithful rather than lucky: a compaction that replaces
    /// the chunk frees the old one, and only a live epoch parks that
    /// free instead of recycling the bytes the walker is still reading
    /// (`memory::deferred_free`). The collector never walks outside an
    /// epoch, so the production invariant is the same one.
    ///
    /// Nothing is asserted about what the walker sees. A stale reading
    /// is a missed edge and later phases repair it; what must not
    /// happen is a read of bytes the mutator is writing plainly, and
    /// that is not a value any assertion can name.
    #[test]
    fn a_relaxed_reader_strides_the_entries_while_the_owner_compacts() {
        const ENTRIES: i64 = 24;
        const READINGS: usize = 96;
        let _g = crate::memory::block_pool::test_guard();
        crate::memory::deferred_free::begin_epoch();

        let mut m = t();
        for i in 0..ENTRIES {
            m.insert(Key::Int(i), Value::int(i));
        }

        /// The entity, for the collector's thread. The array outlives
        /// the walk: the join below is what makes that true.
        struct Handed(*mut crate::array::entity::LLArray);
        unsafe impl Send for Handed {}

        let handed = Handed(m.0);
        let walker = std::thread::spawn(move || {
            let handed = handed;
            let mut cells = 0usize;
            for _ in 0..READINGS {
                unsafe {
                    crate::walk::trace_cells::<crate::walk::RelaxedCells>(
                        handed.0 as *mut crate::refcount::RcHeader,
                        crate::refcount::EntityKind::Array as u32,
                        |_| cells += 1,
                    )
                };
                std::thread::yield_now();
            }

            cells
        });

        // Holes, then a compaction to reclaim them, three times over:
        // one compaction is one window, and the walker has to be inside
        // a stride when a window opens for this to prove anything.
        for round in 0..3i64 {
            for i in 0..ENTRIES {
                if i % 2 == round % 2 {
                    let _ = m.remove(Key::Int(i));
                }
            }

            assert!(m.compact().is_some(), "the compaction was refused");
            std::thread::yield_now();
            for i in 0..ENTRIES {
                if i % 2 == round % 2 {
                    m.insert(Key::Int(i), Value::int(i));
                }
            }
        }

        let cells = walker.join().unwrap();
        assert_eq!(cells, 0, "every element here is an integer, so no cell");
        drop(m);
        crate::memory::deferred_free::end_epoch();
        assert!(unsafe { crate::memory::deferred_free::flush() } > 0);
    }
}
