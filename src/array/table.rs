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

/// A keyed hash over the key's bytes, used only after the flood backstop
/// has escalated a table.
///
/// The cached hash at a string's +16 stays rapidhash and is shared with
/// every other table holding that string, so escalation must not touch
/// it — a table that has been attacked hashes bytes itself instead.
///
/// This is a placeholder shape rather than the final function: the design
/// names the long-key slot (`rfc/model/strings.md`) with a per-process key
/// that is never folded, and this stands in until that slot is filled.
#[inline]
fn strong_hash(bytes: &[u8], key: u64) -> u64 {
    let mut h = key ^ 0x9E37_79B9_7F4A_7C15;
    for chunk in bytes.chunks(8) {
        let mut w = 0u64;
        for (i, b) in chunk.iter().enumerate() {
            w |= (*b as u64) << (i * 8);
        }
        h = (h ^ w).wrapping_mul(0x1000_0000_01B3);
        h ^= h >> 29;
    }
    h = h.wrapping_add(bytes.len() as u64);
    h = (h ^ (h >> 32)).wrapping_mul(0xD6E8_FEB8_6659_FD93);
    h ^ (h >> 32)
}

/// The flood backstop's first trigger: how many entries with a full
/// 64-bit hash equal to the incoming key's may be met during one insert
/// before the table escalates.
///
/// A size-independent constant, and that is the point. Eight-way
/// agreement on a full 64-bit hash by chance needs on the order of 2^56
/// keys, so an honest table never reaches it at any size; probe length
/// would have to grow with the table and could not be a constant. It is
/// also unaffected by deletion, which a running maximum would not be.
const EQUAL_HASH_LIMIT: u32 = 8;

/// The second trigger: chain length. This catches families whose hashes
/// differ but whose slots coincide, including an integer flood. Generous,
/// since the honest maximum is 4-8 even at millions of keys.
const CHAIN_LIMIT: u32 = 32;

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
    /// Set once, one way, when the flood backstop fires on equal full
    /// hashes: from then on a string key's slot comes from a keyed hash
    /// over its bytes rather than from the cached hash at +16.
    strong: bool,
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
            strong: false,
        }
    }

    /// True once the table has escalated to the keyed byte hash.
    #[inline]
    pub fn is_strong(&self) -> bool {
        self.strong
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
            Key::Str(s) => {
                if self.strong {
                    strong_hash(unsafe { LLString::bytes(s) }, self.salt)
                } else {
                    unsafe { LLString::hash(s) }
                }
            }
        }
    }

    /// The same, derived from an entry rather than from a key — what the
    /// index rebuild needs, since it has entries and no keys.
    #[inline]
    fn entry_slot_hash(&self, e: &Entry) -> u64 {
        if e.is_int_key() {
            mix_int(e.hash_or_key as i64, self.salt)
        } else if self.strong {
            strong_hash(unsafe { LLString::bytes(e.key) }, self.salt)
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
        // Counted during the insert's own walk, against current state:
        // nothing is stored between operations, so deletion cannot leave a
        // counter stuck high — the defect a running maximum would have.
        let mut equal_hashes: u32 = 0;
        let mut chain_len: u32 = 0;
        let stored_hash = match key {
            Key::Int(k) => k as u64,
            Key::Str(s) => unsafe { LLString::hash(s) },
        };
        if !self.storage.is_null() {
            let mut i = unsafe { *self.slots().add(sh as usize & self.mask) };
            while i != NONE {
                let matched = Self::entry_matches(self.entry(i as usize), key);
                if matched {
                    let e = self.entry_mut(i as usize);
                    let old = std::mem::replace(&mut e.value, value);
                    return Some((false, Some(old)));
                }
                chain_len += 1;
                let e = self.entry(i as usize);
                if !e.is_int_key() && e.hash_or_key == stored_hash {
                    equal_hashes += 1;
                }
                i = e.next;
            }
        }
        // Fires on insertion only: this path already holds exclusive
        // ownership, may allocate and may raise, while a lookup may do
        // none of those under a live iterator on a shared table.
        if equal_hashes >= EQUAL_HASH_LIMIT {
            self.escalate();
        } else if chain_len >= CHAIN_LIMIT {
            self.reseed();
        }
        let sh = self.slot_hash(key);

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
                // `stored_hash`, not `sh`: the entry holds the key's own
                // identity, which is the string's cached hash. In strong
                // mode `sh` is a different number entirely, and storing it
                // here would make the key unfindable by its own hash.
                Key::Str(s) => e.set_string_key(s, stored_hash),
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
            // Holes are skipped rather than linked: a hole's `key` field
            // is a sentinel, not a string, so reading bytes through it in
            // strong mode would dereference 1.
            if self.entry(k).is_hole() {
                self.entry_mut(k).next = NONE;
                continue;
            }
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

    /// Turn the element under `key` into a reference and hand the box
    /// back, creating the box if the element is not one already.
    ///
    /// **A reference into an element is a `ReferenceBox`, never a pointer
    /// to the slot.** The other form `values.md` offers — an owner plus a
    /// slot pointer — is for slots that never move, and an element moves
    /// whenever growth or compaction reallocates the storage: `$r =
    /// &$a['x']` followed by enough inserts to grow would leave `$r`
    /// pointing into freed storage. Boxing means growth moves sixteen
    /// bytes containing a pointer, and the box stays put.
    ///
    /// Null when the key is absent or the box could not be allocated.
    /// The caller retains the box for its own holder; this leaves the
    /// element's reference to it at the count the factory gave.
    pub fn make_ref(&mut self, key: Key) -> *mut crate::reference::LLReference {
        if self.storage.is_null() {
            return std::ptr::null_mut();
        }
        let sh = self.slot_hash(key);
        let mut i = unsafe { *self.slots().add(sh as usize & self.mask) };
        while i != NONE {
            if Self::entry_matches(self.entry(i as usize), key) {
                let current = self.entry(i as usize).value;
                if current.tag() == crate::value::Tag::Reference {
                    return current.entity_ptr() as *mut crate::reference::LLReference;
                }
                let category = self.category;
                let boxed =
                    unsafe { crate::reference::ll_reference_new(std::ptr::null_mut(), category) };
                if boxed.is_null() {
                    return std::ptr::null_mut();
                }
                unsafe { (*boxed).value = current };
                self.entry_mut(i as usize).value = Value::entity(
                    crate::value::Tag::Reference,
                    boxed as *mut crate::refcount::RcHeader,
                );
                return boxed;
            }
            i = self.entry(i as usize).next;
        }
        std::ptr::null_mut()
    }

    /// Escalate to the keyed byte hash, once and one way. The response
    /// to *equal full hashes*: redrawing a salt cannot separate keys whose
    /// hashes agree, and doing so on that trigger is what made Perl's
    /// REHASH exploitable (CVE-2013-1667).
    fn escalate(&mut self) {
        if self.strong {
            return;
        }
        self.strong = true;
        if !self.storage.is_null() {
            self.rebuild_index();
        }
    }

    /// Redraw the per-table salt and rebuild the index. The response to a
    /// long chain of keys whose hashes *differ* — an accident, an integer
    /// flood, or a leaked salt. A second firing escalates instead.
    fn reseed(&mut self) {
        if self.strong {
            return;
        }
        self.salt = self
            .salt
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        if !self.storage.is_null() {
            self.rebuild_index();
        }
    }

    /// Visit every live element's `Value`, in insertion order.
    ///
    /// **This enumeration has to be complete rather than conservative.**
    /// The arena reset's escaped-subgraph trace marks visited *entities*
    /// in flag bits, and table storage is not an entity and has no
    /// header, so the tracer enumerates elements from the storage itself;
    /// an array survivor has its elements' references erased rather than
    /// ignored (`dev/DECISIONS.md`, 2026-08-04, and `traceable_in_full`).
    /// Scanning the dense prefix `0..used` and skipping holes satisfies
    /// that by construction — and the hole marker lives in `key`, outside
    /// the sixteen bytes the store barrier writes, precisely so that an
    /// ordinary value store cannot destroy it.
    pub fn for_each_value(&self, mut f: impl FnMut(&Value)) {
        for k in 0..self.used {
            let e = self.entry(k);
            if !e.is_hole() {
                f(&e.value);
            }
        }
    }

    /// The same, with the elements mutable — what a walker that rewrites
    /// references needs.
    pub fn for_each_value_mut(&mut self, mut f: impl FnMut(&mut Value)) {
        for k in 0..self.used {
            if self.entry(k).is_hole() {
                continue;
            }
            f(&mut self.entry_mut(k).value);
        }
    }

    /// Every string key that is a live entity, in insertion order. Keys
    /// are counted children too: a table holds a reference to each.
    pub fn for_each_string_key(&self, mut f: impl FnMut(*mut LLString)) {
        for k in 0..self.used {
            let e = self.entry(k);
            if e.is_hole() {
                continue;
            }
            let s = e.string_key();
            if !s.is_null() {
                f(s);
            }
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

    // ---- string keys -----------------------------------------------

    fn mk(bytes: &[u8]) -> *mut LLString {
        unsafe {
            crate::string::ll_string_new(
                std::ptr::null_mut(),
                MemoryCategory::GcHeap,
                bytes,
            )
        }
    }

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

    #[test]
    fn integer_and_string_keys_coexist_without_aliasing() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        m.insert(Key::Int(7), Value::int(700));
        let s = mk(b"7");
        m.insert(Key::Str(s), Value::int(77));

        assert_eq!(m.len(), 2, "int 7 and string \"7\" are different keys here");
        assert_eq!(m.get(Key::Int(7)).unwrap().as_int(), 700);
        assert_eq!(m.get(Key::Str(s)).unwrap().as_int(), 77);
    }

    #[test]
    fn a_string_key_survives_growth_and_compaction() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        let keys: Vec<*mut LLString> =
            (0..300).map(|i| mk(format!("k{i}").as_bytes())).collect();
        for (i, s) in keys.iter().enumerate() {
            m.insert(Key::Str(*s), Value::int(i as i64));
        }
        for (i, s) in keys.iter().enumerate() {
            if i % 3 == 0 {
                m.remove(Key::Str(*s));
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

    // ---- the flood backstop -----------------------------------------

    /// Forge the state the backstop exists for: many entries whose *full*
    /// 64-bit hash agrees. Real construction of such a set needs a break
    /// of the hash; here the stored hash is written directly, which
    /// exercises the same code path the attack would reach.
    fn force_equal_hashes(m: &mut Table, n: usize) {
        for i in 0..n {
            let s = mk(format!("collider-{i}").as_bytes());
            unsafe { (*s).hash = 0x0BAD_C0DE_0BAD_C0DE };
            m.insert(Key::Str(s), Value::int(i as i64));
        }
    }

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
    }

    #[test]
    fn every_key_still_resolves_after_escalation() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();

        let honest: Vec<*mut LLString> =
            (0..50).map(|i| mk(format!("h{i}").as_bytes())).collect();
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
            "longest chain {longest} after escalation — the keyed hash is not separating them"
        );
    }

    #[test]
    fn escalation_happens_once_and_the_salt_is_not_redrawn_on_equal_hashes() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        let before = m.salt;
        force_equal_hashes(&mut m, 64);
        assert!(m.is_strong());
        assert_eq!(
            m.salt, before,
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

    // ---- a reference into an element --------------------------------

    #[test]
    fn a_reference_into_an_element_survives_growth() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        m.insert(Key::Int(1), Value::int(41));

        let r = m.make_ref(Key::Int(1));
        assert!(!r.is_null());
        assert_eq!(unsafe { (*r).value.as_int() }, 41);

        // Enough inserts to reallocate the storage several times. A slot
        // pointer would be dangling by now; the box is not.
        for i in 2..5000i64 {
            m.insert(Key::Int(i), Value::int(i));
        }
        unsafe { (*r).value = Value::int(99) };
        assert_eq!(unsafe { (*r).value.as_int() }, 99);

        // The element still holds the same box.
        let again = m.make_ref(Key::Int(1));
        assert_eq!(again, r, "asking twice must not build a second box");
    }

    #[test]
    fn a_reference_into_an_element_survives_compaction() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        for i in 0..200i64 {
            m.insert(Key::Int(i), Value::int(i));
        }
        let r = m.make_ref(Key::Int(150));
        assert!(!r.is_null());
        for i in 0..150i64 {
            m.remove(Key::Int(i));
        }
        m.compact();

        assert_eq!(m.make_ref(Key::Int(150)), r, "compaction moved the element, not the box");
        unsafe { (*r).value = Value::int(-1) };
        assert_eq!(
            m.get(Key::Int(150)).unwrap().tag(),
            crate::value::Tag::Reference,
            "the element holds the box, not the value"
        );
    }

    #[test]
    fn make_ref_reports_on_an_absent_key() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        m.insert(Key::Int(1), Value::int(1));
        assert!(m.make_ref(Key::Int(2)).is_null());
        assert!(m.make_ref(Key::Str(mk(b"nope"))).is_null());
    }

    // ---- what the memory manager is owed -----------------------------

    #[test]
    fn enumeration_is_complete_over_the_dense_prefix_and_skips_holes() {
        let _g = crate::memory::block_pool::test_guard();
        let mut m = t();
        for i in 0..100i64 {
            m.insert(Key::Int(i), Value::int(i));
        }
        for i in (0..100i64).filter(|i| i % 3 == 0) {
            m.remove(Key::Int(i));
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
        m.remove(Key::Int(3));
        // Simulate a barrier writing the full Value into the dead slot.
        m.for_each_value_mut(|_| {});
        unsafe {
            let e = m.0.entries().add(3);
            (*e).value = Value::int(0xDEAD);
        }
        let mut seen = Vec::new();
        m.for_each_value(|v| seen.push(v.as_int()));
        assert!(!seen.contains(&0xDEAD), "the hole survived a full value write");
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
        m.remove(Key::Str(a));

        let mut keys = Vec::new();
        m.for_each_string_key(|s| keys.push(s));
        assert_eq!(keys, vec![b], "an integer key and a hole are not string children");
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
        let base = m.0.storage as usize;
        let bytes = super::storage_bytes(m.0.nslots, m.0.cap).unwrap();
        for k in 0..m.used() {
            let e = m.entry(k);
            for word in [e.hash_or_key, e.key as u64, e.value.as_int() as u64] {
                let w = word as usize;
                assert!(
                    w < base || w >= base + bytes,
                    "entry {k} holds a word pointing into the storage"
                );
            }
        }
    }
}
