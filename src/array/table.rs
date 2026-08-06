//! The table itself: one storage allocation of `u32` index slots followed
//! by the dense entry array, and the three operations over it.
//!
//! The index is the hashtable; the entry array is the insertion order.
//! A lookup reads one index slot and then one entry — two dependent
//! memory accesses, and no order-preserving design does better, because
//! the entry read *is* the answer and the index read is what locates it
//! (`rfc/model/arrays-hashtable.md`).
//!
//! Storage layout, one allocation:
//!
//! ```text
//! [ u32 x nslots ][ padding to 8 ][ Entry x cap ]
//! ```
//!
//! `nslots` is twice `cap` rounded up to a power of two, so a full table
//! indexes at load 0.5 while the entry array is full — Zend's ratio, and
//! the one the design's measurements were taken at.
//!
//! **Nothing inside the storage points into the storage.** Chain links
//! are `u32` indices, so promotion can copy the whole block into the heap
//! without fixing anything up (`rfc/model/strings.md`, and the same
//! obligation for arrays). An implementer reaching for a pointer here
//! would break promotion silently.

use crate::array::entry::{Entry, MAX_ENTRIES, NONE};
use crate::memory::context::resolve_arena;
use crate::memory::immortal::immortal_alloc;
use crate::refcount::MemoryCategory;
use crate::string::LLString;
use crate::value::Value;

/// A key as the table sees it: the language's `int|string`, already
/// canonicalized by the caller — PHP turns `"1"` into `1` before the key
/// reaches here, and `"011"` stays a string.
#[derive(Clone, Copy)]
pub enum Key {
    Int(i64),
    /// A live string entity. Its hash is read through
    /// `LLString::hash`, which computes and caches on first use.
    Str(*mut LLString),
}

/// Avalanche mix for an integer key, salted per table.
///
/// Zend indexes an integer key by its value, so `0, 1024, 2048, …` share
/// one bucket at every table size up to 1024 — a flood that needs no
/// knowledge of any seed and no hash function at all. Dense integer
/// arrays are storage strategy 2 and never reach this table, so the
/// multiply is paid only by sparse and mixed ones.
#[inline]
fn mix_int(k: i64, salt: u64) -> u64 {
    let mut x = (k as u64) ^ salt;
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Round up to a power of two, saturating rather than wrapping.
#[inline]
fn pow2ge(n: usize) -> usize {
    let mut p = 1usize;
    while p < n {
        match p.checked_mul(2) {
            Some(q) => p = q,
            None => return p,
        }
    }
    p
}

/// Byte offset of the entry array inside a storage block of `nslots`
/// index slots. The entries are 8-aligned, and `nslots` is a power of two
/// of at least 2, so the slot array is already a multiple of 8.
#[inline]
fn entries_offset(nslots: usize) -> usize {
    nslots * size_of::<u32>()
}

/// Total storage bytes for `nslots` slots and `cap` entries, or `None` on
/// overflow. `cap` reaches here from a program-visible size, so the
/// arithmetic is checked rather than assumed.
#[inline]
fn storage_bytes(nslots: usize, cap: usize) -> Option<usize> {
    cap.checked_mul(size_of::<Entry>())
        .and_then(|e| e.checked_add(entries_offset(nslots)))
}

/// The ordered hash. Holds no header of its own: the entity wrapper that
/// owns it supplies the `RcHeader`, the class pointer and the COW state.
pub struct Table {
    storage: *mut u8,
    nslots: usize,
    mask: usize,
    cap: usize,
    /// Entries written so far, holes included. Iteration and the arena
    /// reset's tracer both scan `0..used`.
    used: usize,
    /// Live entries.
    live: usize,
    holes: usize,
    salt: u64,
    category: MemoryCategory,
}

impl Table {
    /// An empty table with no storage. The first insert allocates.
    pub const fn empty(category: MemoryCategory, salt: u64) -> Self {
        Table {
            storage: std::ptr::null_mut(),
            nslots: 0,
            mask: 0,
            cap: 0,
            used: 0,
            live: 0,
            holes: 0,
            salt,
            category,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.live
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Entries written so far, holes included — the bound the tracer and
    /// iteration scan to.
    #[inline]
    pub fn used(&self) -> usize {
        self.used
    }

    #[inline]
    fn slots(&self) -> *mut u32 {
        self.storage as *mut u32
    }

    #[inline]
    fn entries(&self) -> *mut Entry {
        unsafe { self.storage.add(entries_offset(self.nslots)) as *mut Entry }
    }

    /// The entry at `i`. Callers hold `i < used`.
    #[inline]
    pub fn entry(&self, i: usize) -> &Entry {
        debug_assert!(i < self.used);
        unsafe { &*self.entries().add(i) }
    }

    #[inline]
    fn entry_mut(&mut self, i: usize) -> &mut Entry {
        debug_assert!(i < self.used);
        unsafe { &mut *self.entries().add(i) }
    }

    /// The value an index slot is derived from. **This is not what the
    /// entry stores**, and conflating the two is the first mistake this
    /// code made: an entry holds the raw integer key or the string's full
    /// hash, while the slot comes from a *salted mix* of the integer or
    /// from that same string hash.
    #[inline]
    fn slot_hash(&self, key: Key) -> u64 {
        match key {
            Key::Int(k) => mix_int(k, self.salt),
            Key::Str(s) => unsafe { LLString::hash(s) },
        }
    }

    /// The same, derived from an entry rather than from a key — what the
    /// index rebuild needs, since it has entries and no keys.
    #[inline]
    fn entry_slot_hash(&self, e: &Entry) -> u64 {
        if e.is_int_key() {
            mix_int(e.hash_or_key as i64, self.salt)
        } else {
            e.hash_or_key
        }
    }

    /// Does this entry hold `key`? An integer key is its own identity; a
    /// string key compares the full hash first, so a byte comparison runs
    /// only when 64 bits already agree.
    #[inline]
    fn entry_matches(e: &Entry, key: Key) -> bool {
        if e.is_hole() {
            return false;
        }
        match key {
            Key::Int(k) => e.is_int_key() && e.hash_or_key == k as u64,
            Key::Str(s) => {
                let k = e.string_key();
                if k.is_null() {
                    return false;
                }
                let want = unsafe { LLString::hash(s) };
                e.hash_or_key == want
                    && (k == s || unsafe { LLString::bytes(k) == LLString::bytes(s) })
            }
        }
    }

    /// The value under `key`, or `None`.
    pub fn get(&self, key: Key) -> Option<&Value> {
        if self.storage.is_null() {
            return None;
        }
        let sh = self.slot_hash(key);
        let mut i = unsafe { *self.slots().add(sh as usize & self.mask) };
        while i != NONE {
            let e = self.entry(i as usize);
            if Self::entry_matches(e, key) {
                return Some(&e.value);
            }
            i = e.next;
        }
        None
    }

    #[inline]
    pub fn contains(&self, key: Key) -> bool {
        self.get(key).is_some()
    }

    /// Insert or overwrite. Returns `None` when the storage could not
    /// grow — an allocation refusal reports rather than aborting, and the
    /// table is unchanged. `Some(true)` means a new key was added.
    ///
    /// The old value of an overwritten key is returned to the caller
    /// rather than dropped here: releasing it is the owner's, because the
    /// order matters to the collector.
    pub fn insert(&mut self, key: Key, value: Value) -> Option<(bool, Option<Value>)> {
        let sh = self.slot_hash(key);
        if !self.storage.is_null() {
            let mut i = unsafe { *self.slots().add(sh as usize & self.mask) };
            while i != NONE {
                let matched = Self::entry_matches(self.entry(i as usize), key);
                if matched {
                    let e = self.entry_mut(i as usize);
                    let old = std::mem::replace(&mut e.value, value);
                    return Some((false, Some(old)));
                }
                i = self.entry(i as usize).next;
            }
        }

        if self.used == self.cap && !self.grow() {
            return None;
        }

        let slot = sh as usize & self.mask;
        let k = self.used;
        self.used += 1;
        let head = unsafe { *self.slots().add(slot) };
        {
            let e = self.entry_mut(k);
            match key {
                Key::Int(v) => e.set_int_key(v),
                Key::Str(s) => e.set_string_key(s, sh),
            }
            e.meta = 0;
            e.value = value;
            e.next = head;
        }
        unsafe { *self.slots().add(slot) = k as u32 };
        self.live += 1;
        Some((true, None))
    }

    /// Remove `key`, returning its value for the caller to release.
    /// Unlinking leaves nothing behind: the chain is genuinely shorter,
    /// which is the property an open-addressed index cannot have.
    pub fn remove(&mut self, key: Key) -> Option<Value> {
        if self.storage.is_null() {
            return None;
        }
        let sh = self.slot_hash(key);
        let slot = sh as usize & self.mask;
        let mut i = unsafe { *self.slots().add(slot) };
        let mut prev = NONE;
        while i != NONE {
            let matched = Self::entry_matches(self.entry(i as usize), key);
            let next = self.entry(i as usize).next;
            if matched {
                if prev == NONE {
                    unsafe { *self.slots().add(slot) = next };
                } else {
                    self.entry_mut(prev as usize).next = next;
                }
                let e = self.entry_mut(i as usize);
                let old = std::mem::replace(&mut e.value, Value::undef());
                e.make_hole();
                e.next = NONE;
                self.live -= 1;
                self.holes += 1;
                return Some(old);
            }
            prev = i;
            i = next;
        }
        None
    }

    /// Grow, which is where the design puts the cost: compact in place
    /// when the holes are worth reclaiming, otherwise double. Both paths
    /// rebuild every chain.
    ///
    /// Compaction **moves entries**, so an owner holding live iterator
    /// positions has to repair them; that obligation belongs to the
    /// entity wrapper, and this returns whether a compaction happened so
    /// the wrapper can act on it.
    fn grow(&mut self) -> bool {
        if self.storage.is_null() {
            return self.realloc_storage(8);
        }
        // Zend's rule: reclaim holes rather than doubling when they are
        // more than a thirty-second of the live count.
        if self.holes > self.live / 32 + 1 {
            self.compact();
            return true;
        }
        match self.cap.checked_mul(2) {
            Some(n) if n <= MAX_ENTRIES => self.realloc_storage(n),
            _ => false,
        }
    }

    /// Slide live entries down over the holes and rebuild every chain.
    /// Returns the number of entries that moved, which is what an
    /// iterator repair needs.
    pub fn compact(&mut self) -> usize {
        let mut w = 0usize;
        for r in 0..self.used {
            if self.entry(r).is_hole() {
                continue;
            }
            if w != r {
                unsafe { std::ptr::copy_nonoverlapping(self.entries().add(r), self.entries().add(w), 1) };
            }
            w += 1;
        }
        let moved = self.used - w;
        self.used = w;
        self.holes = 0;
        self.rebuild_index();
        moved
    }

    fn rebuild_index(&mut self) {
        unsafe { std::ptr::write_bytes(self.slots(), 0xFF, self.nslots) };
        for k in 0..self.used {
            // The slot comes from the *mixed* integer or the string hash,
            // never from the stored word as-is.
            let sh = self.entry_slot_hash(self.entry(k));
            let slot = sh as usize & self.mask;
            let head = unsafe { *self.slots().add(slot) };
            self.entry_mut(k).next = head;
            unsafe { *self.slots().add(slot) = k as u32 };
        }
    }

    /// Allocate storage for `cap` entries and move the existing entries
    /// into it. False on refusal, with the table left exactly as it was —
    /// an allocation failure reports to a frame that can raise rather
    /// than aborting.
    fn realloc_storage(&mut self, cap: usize) -> bool {
        if cap > MAX_ENTRIES {
            return false;
        }
        let nslots = pow2ge(cap) * 2;
        let bytes = match storage_bytes(nslots, cap) {
            Some(b) => b,
            None => return false,
        };
        let mem = self.alloc(bytes);
        if mem.is_null() {
            return false;
        }
        let old_storage = self.storage;
        let old_used = self.used;
        let old_entries = if old_storage.is_null() {
            std::ptr::null_mut()
        } else {
            self.entries()
        };

        self.storage = mem;
        self.nslots = nslots;
        self.mask = nslots - 1;
        self.cap = cap;
        if !old_entries.is_null() {
            unsafe { std::ptr::copy_nonoverlapping(old_entries, self.entries(), old_used) };
        }
        self.rebuild_index();
        self.free_storage(old_storage);
        true
    }

    /// Route the allocation by category.
    ///
    /// **Not `entity_alloc`.** Table storage is not an entity: it has no
    /// `RcHeader`, and the cycle collector reads the first eight bytes of
    /// every occupied slot in an entity block as one
    /// (`memory/block_pool.rs`, `BLOCK_KIND_ENTITY`). Storage goes through
    /// the ordinary allocator, which lands in a heap block instead.
    fn alloc(&self, bytes: usize) -> *mut u8 {
        match self.category {
            MemoryCategory::RequestArena => unsafe {
                (*resolve_arena(std::ptr::null_mut())).alloc(bytes)
            },
            MemoryCategory::GcHeap | MemoryCategory::LongLived => unsafe {
                crate::memory::stdapi::ll_alloc(bytes, 8)
            },
            MemoryCategory::Immortal => immortal_alloc(bytes),
        }
    }

    /// Release storage the table has replaced. Only the heap categories
    /// free: arena storage goes at the reset, and immortal never goes.
    fn free_storage(&self, p: *mut u8) {
        if p.is_null() {
            return;
        }
        match self.category {
            MemoryCategory::GcHeap | MemoryCategory::LongLived => unsafe {
                crate::memory::stdapi::ll_free(p)
            },
            MemoryCategory::RequestArena | MemoryCategory::Immortal => {}
        }
    }

    /// Release the storage and return the table to its empty state.
    ///
    /// The values are **not** released here: their order matters to the
    /// collector, so the entity wrapper walks and releases them first and
    /// then calls this. Nothing here reads a value.
    pub fn dispose(&mut self) {
        let p = self.storage;
        self.storage = std::ptr::null_mut();
        self.nslots = 0;
        self.mask = 0;
        self.cap = 0;
        self.used = 0;
        self.live = 0;
        self.holes = 0;
        self.free_storage(p);
    }

    /// Iterate live entries in insertion order. This reads no index at
    /// all, which is why the choice of index layer does not affect
    /// `foreach`.
    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        (0..self.used).map(|i| self.entry(i)).filter(|e| !e.is_hole())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::context::LLContext;

    fn ctx() -> *mut LLContext {
        std::ptr::null_mut()
    }

    /// A table whose storage is released when the binding is dropped, so
    /// a test cannot leak blocks into the pool's free-list order and
    /// disturb an unrelated test (which is exactly what happened once).
    struct Owned(Table);
    impl std::ops::Deref for Owned {
        type Target = Table;
        fn deref(&self) -> &Table { &self.0 }
    }
    impl std::ops::DerefMut for Owned {
        fn deref_mut(&mut self) -> &mut Table { &mut self.0 }
    }
    impl Drop for Owned {
        fn drop(&mut self) { self.0.dispose() }
    }

    fn t() -> Owned {
        Owned(Table::empty(MemoryCategory::GcHeap, 0x243F_6A88_85A3_08D3))
    }

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
        m.remove(Key::Int(1));
        m.insert(Key::Int(1), Value::int(99));

        let order: Vec<i64> = m.iter().map(|e| e.hash_or_key as i64).collect();
        assert_eq!(order, vec![0, 2, 1], "PHP appends a re-inserted key");
        assert_eq!(m.get(Key::Int(1)).unwrap().as_int(), 99);
    }

    #[test]
    fn removing_shortens_the_chain_rather_than_leaving_a_marker() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        for i in 0..64i64 {
            m.insert(Key::Int(i), Value::int(i));
        }
        for i in 0..64i64 {
            assert_eq!(m.remove(Key::Int(i)).unwrap().as_int(), i);
            assert!(m.get(Key::Int(i)).is_none(), "a removed key stays removed");
        }
        assert_eq!(m.len(), 0);
        // Everything still resolves: the chains are empty, not full of
        // markers, so a lookup on an emptied table is one slot read.
        for i in 0..64i64 {
            assert!(!m.contains(Key::Int(i)));
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
            m.remove(Key::Int(i));
        }
        let before: Vec<i64> = m.iter().map(|e| e.hash_or_key as i64).collect();
        assert_eq!(m.used(), 100, "holes still occupy their slots");

        let moved = m.compact();
        assert!(moved > 0);
        assert_eq!(m.used(), 50, "compaction reclaimed the holes");

        let after: Vec<i64> = m.iter().map(|e| e.hash_or_key as i64).collect();
        assert_eq!(before, after, "compaction preserves insertion order");
        for i in (0..100i64).filter(|i| i % 2 == 1) {
            assert_eq!(m.get(Key::Int(i)).unwrap().as_int(), i);
        }
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

    /// The flood the design names: Zend indexes an integer key by its
    /// value, so a stride of the table size collides everywhere. The
    /// salted mix is what stops it, and this pins that the mix is applied.
    #[test]
    fn integer_keys_on_a_power_of_two_stride_do_not_share_one_bucket() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        for i in 0..512i64 {
            m.insert(Key::Int(i * 1024), Value::int(i));
        }
        // Longest chain: with the mix this is a handful; indexing by the
        // key's low bits would put all 512 in one bucket.
        let mut longest = 0usize;
        for slot in 0..m.nslots {
            let mut n = 0usize;
            let mut i = unsafe { *m.slots().add(slot) };
            while i != NONE {
                n += 1;
                i = m.entry(i as usize).next;
            }
            longest = longest.max(n);
        }
        assert!(
            longest < 16,
            "longest chain {longest} — the integer mix is not being applied"
        );
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

    #[test]
    fn removing_a_missing_key_reports_rather_than_disturbing_the_table() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        m.insert(Key::Int(1), Value::int(1));
        assert!(m.remove(Key::Int(2)).is_none());
        assert_eq!(m.len(), 1);
        assert_eq!(m.get(Key::Int(1)).unwrap().as_int(), 1);
    }
}

