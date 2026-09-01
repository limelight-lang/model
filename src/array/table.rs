//! The table itself: one storage allocation of `u32` index slots followed
//! by the dense entry array, and the three operations over it
//! (`rfc/model/arrays-hashtable.md`, "The shape: an index array over a
//! dense insertion-ordered entry array").
//!
//! Storage layout, one allocation:
//!
//! ```text
//! [ u32 x nslots ][ padding to 8 ][ Entry x cap ]
//! ```

use crate::array::entry::{
    Entry, KEY_SENTINEL_LIMIT, KEY_TAG_MASK, KEY_TAG_STRING, MAX_ENTRIES, NONE,
};
use crate::array::head::{StorageHead, StorageTag};
use crate::refcount::{MemoryCategory, RcHeader};
use crate::string::{LLString, string_bytes};
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

/// Which kind of insert the caller is performing. The table's policy
/// dispatches on it; the caller only states what it is doing.
///
/// The reader is the ladder's terminal rung, the refusal of an
/// admission whose trigger trips with no rebuild left
/// (`rfc/model/maps.md`, "Rung three, refusal"). A replay is exempt
/// from that refusal alone, because a key admitted once cannot be
/// refused on re-admission; rungs one and two stay armed on both. What
/// the exemption costs and what returns it is the copy rule in the same
/// section: the chain a replay carries past the limit is scattered by
/// the copy's own salt.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InsertKind {
    /// A key from outside the table's history: a user store, or a new
    /// association.
    Admission,
    /// An entry admitted once, re-derived: the copy's entry replay,
    /// and the migration re-inserting the vector's positions.
    Replay,
}

/// What [`Table::insert`] did, or the refusal it answered instead.
/// A refusal leaves every entry, count and key unchanged. The ladder's
/// state is outside that promise: the triggers fire on the insert's
/// walk, before the outcome is decided, so a refused insert may have
/// spent a rung.
///
/// The two refusal causes are distinct variants because the runtime
/// owes them different answers: memory pressure is the environment's,
/// while the terminal rung is a property of the keys and is raised as
/// a catchable error once an error channel exists (`rfc/model/maps.md`,
/// "Rung three, refusal").
#[must_use = "a replaced element carries the table's reference; dropping it leaks the entity"]
pub enum InsertOutcome {
    /// A new key entered the table. A string key's reference, published
    /// before the call, is now the table's.
    Added,
    /// The key was present; the old element is handed back for the
    /// owner to release. The entry keeps its original key, so the key
    /// reference published for this call stays the caller's.
    Replaced(Value),
    /// The storage could not grow.
    RefusedForMemory,
    /// The ladder's terminal rung: a trigger tripped with no rebuild
    /// left — the table is escalated, and the walk met either
    /// [`CHAIN_LIMIT`] chained entries or [`EQUAL_HASH_LIMIT`] equal
    /// identities.
    /// Only an admission is answered this; a replay is admitted
    /// ([`InsertKind`]).
    RefusedByLadder,
}

/// Avalanche mix for an integer key, salted per table. Runs only once
/// the flood ladder has drawn the table's salt ([`TABLE_RESEEDED`]);
/// an unsalted table indexes an integer key by its value, as Zend does.
///
/// Indexing by value means `0, 1024, 2048, …` share one bucket at every
/// table size up to 1024 — a flood that needs no knowledge of any seed
/// and no hash function at all. It builds exactly one long chain, which
/// is the first rung's own trigger, so the mix begins where a
/// flood-shaped chain showed up. The trigger reads shape, not intent:
/// honest keys striding by a power of two fire the same rung and pay
/// the mix from then on (Edmond 2026-08-07: the salt is worth paying
/// for where keys can come from outside, and the ladder needs nobody to
/// predict where that is).
#[inline]
fn mix_int(k: i64, salt: u64) -> u64 {
    mix_word(k as u64, salt)
}

/// The salted mix behind every reseeded slot path — the integer's value
/// through [`mix_int`], a string's cached hash directly: splitmix64's
/// finalizer over the identity word XOR the salt, full avalanche, so a
/// family agreeing in its low bits scatters once the salt is drawn.
#[inline]
fn mix_word(word: u64, salt: u64) -> u64 {
    let mut x = word ^ salt;
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
/// The key it takes is the per-process key folded to word width, mixed
/// with the table's salt ([`Table::strong_key`]); the salt draw keys the
/// same way. This is a placeholder shape rather than the final function:
/// the design names the long-key slot (`rfc/model/strings.md`), whose
/// arrival replaces the construction, not the keying.
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
pub(crate) const EQUAL_HASH_LIMIT: u32 = 8;

/// The second trigger: chain length. This catches families whose hashes
/// differ but whose slots coincide, including an integer flood. Generous,
/// since the honest maximum is 4-8 even at millions of keys.
pub(crate) const CHAIN_LIMIT: u32 = 32;

/// What a move of the entries does with the holes among them.
///
/// Two variants rather than a `bool`, because the two are read at their
/// call sites: growth keeps every index it had, and compaction exists to
/// take the holes out.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EntryMove {
    /// Every entry, hole or not, at the index it already has.
    Whole,
    /// Live entries only, closed up from zero.
    DroppingHoles,
}

/// Entries in the chunk a first growth takes, and the minimum of every
/// later size: the growth schedule is this doubled, and a table presized
/// for a replay lands on the same ladder rather than beside it
/// ([`Table::presize_for_replay`]).
const FIRST_CAP: usize = 8;

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

/// The salt a table's storage address yields under the per-process key,
/// remapped away from zero so that a drawn salt cannot read as the
/// unsalted state. [`Table::draw_salt`] and [`Table::redraw_salt`]
/// differ in when they call this, never in what it computes, which is
/// what keeps the derivation one place: the argument for it, and what
/// it does not buy, is on `draw_salt`.
#[inline]
fn salt_from(head: &StorageHead) -> u64 {
    let drawn = strong_hash(
        &(head.storage() as u64).to_le_bytes(),
        crate::hash::process_key::folded(),
    );
    if drawn == 0 { 1 } else { drawn }
}

/// Total storage bytes for `nslots` slots and `cap` entries, or `None` on
/// overflow. `cap` reaches here from a program-visible size, so the
/// arithmetic is checked rather than assumed.
#[inline]
fn storage_bytes(nslots: usize, cap: usize) -> Option<usize> {
    cap.checked_mul(size_of::<Entry>())
        .and_then(|e| e.checked_add(entries_offset(nslots)))
}

/// The ordered hash. Holds no header of its own and reads none: the
/// entity wrapper supplies the `RcHeader`, the COW state and — as a
/// parameter to every allocating call — the memory category its storage
/// comes from (`array::entity::category_of`, and `dev/DECISIONS.md`,
/// "the table is handed its category and reads no header"). Nothing here
/// names an entity kind, which is what leaves the structure usable by a
/// second one.
///
/// **This is the private tail, and the words a walker reads are not in
/// it.** The chunk, the two counts, the tag and the version live in the
/// entity's [`StorageHead`], which every method below that needs one
/// takes as a parameter rather than storing — `category`'s precedent, and
/// for a harder reason than drift: a `&mut Table` claims uniqueness over
/// every byte inside it, so a head kept here would be read by the
/// collector from inside the mutator's borrow (`crate::array::head`).
/// Threading it also leaves this structure usable by a second owner,
/// which is what `Map` needs.
#[repr(C)]
pub struct Table {
    /// Bytes really granted for the storage, which is not always what was
    /// asked for: a reused buffer-arena chunk may be larger, and the free
    /// that returns it carries the size, since a chunk holds no metadata
    /// of its own. Freeing with the requested size would lose the
    /// difference from the block's free list. In the two categories that
    /// never free — the request arena and the immortal region — this holds
    /// the requested size and nothing reads it.
    storage_capacity: usize,
    mask: usize,
    cap: usize,
    /// Live entries.
    live: usize,
    holes: usize,
    /// Zero from birth. Meaningful only while [`TABLE_RESEEDED`] is set:
    /// the ladder's first rung draws it ([`Table::draw_salt`]) and a
    /// copy that took the bit draws its own before its first insert
    /// ([`Table::redraw_salt`]). Nothing else writes it — a salt
    /// rewritten under live entries would leave every one of them
    /// indexed under the old number.
    salt: u64,
    /// The next append key: one past the highest integer key ever
    /// inserted, [`NEXT_FREE_NONE`] while none has been. Removal never
    /// rewinds it — PHP's `nNextFreeElement` — and a COW copy inherits
    /// it ([`Table::adopt_append_state`]), because the replay skips
    /// holes and would otherwise rewind past a removed high key.
    ///
    /// A negative key moves it too: PHP 8.3 semantics, where
    /// `$a[-5] = 1; $a[] = 2;` appends at −4. An assumption, Edmond's to
    /// overturn: the pre-8.3 answer is one comparison away.
    next_free: i64,
    /// The table's one-bit state, [`TABLE_STRONG`], [`TABLE_RESEEDED`]
    /// and [`TABLE_APPEND_EXHAUSTED`]. One byte rather than a `bool`
    /// apiece, and written plainly: the concurrent walker never reads it,
    /// which is why the storage-strategy tag cannot live here and sits in
    /// the head instead ([`crate::array::head`]).
    flags: u8,
}

/// A string key's slot comes from a keyed hash over its bytes rather
/// than from the cached hash at +16. Set once and one way, by the flood
/// backstop's equal-hash trigger.
const TABLE_STRONG: u8 = 1 << 0;

/// This table has a salt: the ladder's first rung drew one, and integer
/// keys index through the salted mix rather than by value. Set once —
/// a second long chain escalates instead of rebuilding again — which
/// bounds the rung at one firing per table.
const TABLE_RESEEDED: u8 = 1 << 1;

/// What a copy of an attacked table inherits — everything the flood
/// backstop has decided, and nothing else in the byte.
const TABLE_FLOOD_STATE: u8 = TABLE_STRONG | TABLE_RESEEDED;

/// `i64::MAX` has been an integer key, so there is no next append key:
/// [`Table::append_key`] refuses rather than wrapping.
const TABLE_APPEND_EXHAUSTED: u8 = 1 << 2;

/// [`Table::next_free`]'s "no integer key yet": the first append is 0.
/// Unreachable as a real cursor — the lowest one a key can set is
/// `i64::MIN + 1`, from the key `i64::MIN`.
const NEXT_FREE_NONE: i64 = i64::MIN;

impl Table {
    /// An empty table with no storage. The first insert allocates.
    ///
    /// **Unsalted** — the flood ladder's zeroth rung: integer keys index
    /// by their value until a long chain fires [`Table::reseed`], which
    /// draws the salt. No caller selects a mode, because the trigger is
    /// the flood itself.
    ///
    /// No category field, and no read of one: which memory this table's
    /// storage comes from is the owning entity's header to say, and every
    /// allocating call is handed the answer (`dev/DECISIONS.md`, "the
    /// table is handed its category and reads no header").
    pub const fn empty() -> Self {
        Table {
            storage_capacity: 0,
            mask: 0,
            cap: 0,
            live: 0,
            holes: 0,
            salt: 0,
            next_free: NEXT_FREE_NONE,
            flags: 0,
        }
    }

    /// The key the next append takes, or `None` once `i64::MAX` has been
    /// a key, which is refused rather than wrapped onto a live entry.
    /// Reading it moves nothing: the append's own insert is what
    /// advances the cursor.
    pub fn append_key(&self) -> Option<i64> {
        if self.flags & TABLE_APPEND_EXHAUSTED != 0 {
            return None;
        }

        Some(if self.next_free == NEXT_FREE_NONE {
            0
        } else {
            self.next_free
        })
    }

    /// Take `source`'s append cursor, which a copy owes for PHP's
    /// append rule: the replay copies live entries only, so a hole under
    /// the highest integer key ever inserted would otherwise rewind the
    /// copy — `[9 => 'x']` minus its 9 appends at 10, not at 0.
    #[inline]
    pub(crate) fn adopt_append_state(&mut self, source: &Table) {
        self.next_free = source.next_free;
        self.flags =
            (self.flags & !TABLE_APPEND_EXHAUSTED) | (source.flags & TABLE_APPEND_EXHAUSTED);
    }

    /// Advance the append cursor past `k`, saturating into the refusal
    /// state at `i64::MAX` — the one integer key with no successor.
    #[inline]
    fn note_int_key(&mut self, k: i64) {
        match k.checked_add(1) {
            Some(next) => {
                if next > self.next_free {
                    self.next_free = next;
                }
            }
            None => self.flags |= TABLE_APPEND_EXHAUSTED,
        }
    }

    /// The per-table salt, drawn from this table's own storage address
    /// under the per-process key. [`TABLE_RESEEDED`] is the authority on
    /// whether one has been drawn; zero happens to mean "not drawn" as
    /// well, because the draw never yields it.
    ///
    /// A test window on purpose: on an escalated table the salt keys
    /// `strong_hash`, and an accessor exported past the crate would hand
    /// that key to anything linking against the runtime.
    #[cfg(test)]
    pub(crate) fn salt(&self) -> u64 {
        self.salt
    }

    /// Whether the ladder's first rung has drawn this table's salt —
    /// the test window for pinning rung state; the code branches on the
    /// flag directly.
    #[cfg(test)]
    pub(crate) fn is_reseeded(&self) -> bool {
        self.flags & TABLE_RESEEDED != 0
    }

    /// The slot derivation, read through a test window: the copy tests
    /// forge colliding families from outside this module, and an
    /// integer key costs the search no entity.
    #[cfg(test)]
    pub(crate) fn slot_hash_of(&self, key: Key) -> u64 {
        self.slot_hash(key)
    }

    /// The longest index chain, walked slot by slot — the copy tests
    /// assert the trigger's bound from outside this module.
    #[cfg(test)]
    pub(crate) fn longest_chain(&self, head: &StorageHead) -> usize {
        let mut longest = 0usize;
        for slot in 0..head.nslots() {
            let mut n = 0usize;
            let mut i = unsafe { *Self::slots(head).add(slot) };
            while i != NONE {
                n += 1;
                i = self.entry(head, i as usize).link();
            }

            longest = longest.max(n);
        }

        longest
    }

    /// True once the table has escalated to the keyed byte hash.
    #[inline]
    pub fn is_strong(&self) -> bool {
        self.flags & TABLE_STRONG != 0
    }

    /// Take `source`'s flood state — both rung bits and, as a number to
    /// stand on until the storage exists, its salt. The bits are what a
    /// copy of an attacked table owes: an escalated table copied through
    /// a fresh [`Table::empty`] would otherwise re-insert the attacker's
    /// whole collision set under the hash it escalated away from, and
    /// copying an array is the ordinary thing the language does.
    ///
    /// **The number does not stay.** [`Table::redraw_salt`] replaces it
    /// once the copy has storage of its own, and the two calls bracket
    /// [`Table::presize_for_replay`] because a salt is drawn from a
    /// storage address — which is also why that call takes a chunk for a
    /// copy holding a rung bit over no entries. What carrying the number
    /// here buys is the state in between: a bit standing over a zero salt
    /// would mean `mix_int(k, 0)`, a mix every attacker computes offline.
    ///
    /// **Call it before the first insert.** The mode decides how a key is
    /// hashed, so a table that adopts it afterwards has already indexed
    /// its entries the other way.
    #[inline]
    pub(crate) fn adopt_flood_state(&mut self, head: &StorageHead, source: &Table) {
        debug_assert_eq!(head.used(), 0, "the mode decides how a key is indexed");
        debug_assert!(
            source.flags & TABLE_STRONG == 0 || source.flags & TABLE_RESEEDED != 0,
            "an escalated table always holds a drawn salt: escalate draws on the way"
        );
        self.salt = source.salt;
        self.flags = (self.flags & !TABLE_FLOOD_STATE) | (source.flags & TABLE_FLOOD_STATE);
    }

    /// Live entries: holes are excluded, unlike in [`StorageHead::used`].
    #[inline]
    pub fn len(&self) -> usize {
        self.live
    }

    /// No live entry left — a table of nothing but holes is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// The dense entry array and how many entries it holds, taken from a
    /// reading [`StorageHead::coherent`] has already validated.
    ///
    /// Where the head answers what every representation has in common,
    /// this turns that answer into the hash's own stride: the entries
    /// begin `nslots * 4` bytes into the chunk. A view whose tag is not
    /// [`StorageTag::Hash`] is not this table's, and saying so is the
    /// caller's error rather than a state to handle.
    ///
    /// # Safety
    /// `view` was read from this table's head.
    pub(crate) unsafe fn entries_of(
        view: &crate::array::head::CoherentView,
    ) -> (*mut Entry, usize) {
        debug_assert_eq!(view.tag, StorageTag::Hash);
        if view.storage.is_null() {
            return (std::ptr::null_mut(), 0);
        }

        (
            unsafe { view.storage.add(entries_offset(view.nslots)) } as *mut Entry,
            view.used,
        )
    }

    /// The storage and the bytes granted for it, or a null pointer when
    /// the table has never grown.
    ///
    /// A test window, like [`Table::salt`]: the address a promotion needs
    /// is the head's (`entity::storage_address`) and the granted size is
    /// read by the one operation that rewrites it
    /// ([`Table::granted_capacity_mut`]), so outside the tests that pin
    /// what was granted this pair has no reader.
    #[cfg(test)]
    #[inline]
    pub(crate) fn storage_and_capacity(&self, head: &StorageHead) -> (*mut u8, usize) {
        (head.storage(), self.storage_capacity)
    }

    /// The bytes the storage was granted, for the one operation that has
    /// to rewrite them: the carry out of a dying arena, which lives in
    /// `array::entity` because a chunk is bytes and both representations
    /// keep their granted size the same way
    /// (`entity::carry_storage_out_of`).
    #[inline]
    pub(crate) fn granted_capacity_mut(&mut self) -> &mut usize {
        &mut self.storage_capacity
    }

    /// The index region, which begins where the storage does.
    ///
    /// A free function rather than a method: everything it needs is in the
    /// head, and a `&self` here would say the answer depends on the tail
    /// when it does not.
    #[inline]
    fn slots(head: &StorageHead) -> *mut u32 {
        head.storage() as *mut u32
    }

    /// The dense entry array, `nslots * 4` bytes into the storage.
    #[inline]
    fn entries(head: &StorageHead) -> *mut Entry {
        unsafe { head.storage().add(entries_offset(head.nslots())) as *mut Entry }
    }

    /// The entry at `i`. Callers hold `i < used`.
    #[inline]
    pub fn entry(&self, head: &StorageHead, i: usize) -> &Entry {
        debug_assert!(i < head.used());
        unsafe { &*Self::entries(head).add(i) }
    }

    /// A raw pointer to the entry at `i`, which is what every write to a
    /// published entry goes through: the element and the key word are
    /// stored atomically ([`Entry::store_element`]), and an atomic store
    /// needs a pointer rather than a reference to reach the bytes.
    #[inline]
    fn entry_ptr(head: &StorageHead, i: usize) -> *mut Entry {
        debug_assert!(i < head.used());
        unsafe { Self::entries(head).add(i) }
    }

    /// The value an index slot is derived from. **This is not what the
    /// entry stores**, and conflating the two is the first mistake this
    /// code made: an entry holds the raw integer key or the string's full
    /// hash, while the slot comes from a *salted mix* of the integer or
    /// from that same string hash.
    #[inline]
    fn slot_hash(&self, key: Key) -> u64 {
        match key {
            Key::Int(k) => {
                if self.flags & TABLE_RESEEDED != 0 {
                    mix_int(k, self.salt)
                } else {
                    k as u64
                }
            }
            Key::Str(s) => {
                if self.flags & TABLE_STRONG != 0 {
                    strong_hash(unsafe { string_bytes(s) }, self.strong_key())
                } else if self.flags & TABLE_RESEEDED != 0 {
                    mix_word(unsafe { LLString::hash(s) }, self.salt)
                } else {
                    unsafe { LLString::hash(s) }
                }
            }
        }
    }

    /// The keyed hash's key: the per-process key folded to word width,
    /// mixed with the table's salt — rung two hashes bytes under the
    /// key *together with* the salt (`rfc/model/maps.md`, "What the
    /// flood ladder becomes"), so two escalated tables scatter one
    /// colliding set differently.
    #[inline]
    fn strong_key(&self) -> u64 {
        mix_word(crate::hash::process_key::folded(), self.salt)
    }

    /// The same, derived from an entry rather than from a key — what the
    /// index rebuild needs, since it has entries and no keys.
    #[inline]
    fn entry_slot_hash(&self, e: &Entry) -> u64 {
        if e.is_int_key() {
            if self.flags & TABLE_RESEEDED != 0 {
                mix_int(e.hash_or_key as i64, self.salt)
            } else {
                e.hash_or_key
            }
        } else if self.flags & TABLE_STRONG != 0 {
            strong_hash(unsafe { string_bytes(e.string_key()) }, self.strong_key())
        } else if self.flags & TABLE_RESEEDED != 0 {
            mix_word(e.hash_or_key, self.salt)
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
                e.hash_or_key == want && (k == s || unsafe { string_bytes(k) == string_bytes(s) })
            }
        }
    }

    /// The value under `key`, or `None`.
    ///
    /// **A copy, not a reference into the entry.** An entry keeps its
    /// chain link inside the element's reserved bytes, so the Box has to
    /// be handed out through `Entry::value`, which clears them
    /// (`array/entry.rs`). The copy also means a caller holds no borrow of
    /// the table, so a value read before a `remove` names an entity the
    /// caller must have its own reference to.
    pub fn get(&self, head: &StorageHead, key: Key) -> Option<Value> {
        if head.storage().is_null() {
            return None;
        }

        let sh = self.slot_hash(key);
        let mut i = unsafe { *Self::slots(head).add(sh as usize & self.mask) };
        while i != NONE {
            let e = self.entry(head, i as usize);
            if Self::entry_matches(e, key) {
                return Some(e.value());
            }

            i = e.link();
        }

        None
    }

    /// Whether `key` is present. A full lookup: the answer costs what
    /// [`get`](Self::get) costs, the element copy included.
    #[inline]
    pub fn contains(&self, head: &StorageHead, key: Key) -> bool {
        self.get(head, key).is_some()
    }

    /// **The caller publishes its references before it inserts.** The
    /// entry is written raw and this counts nothing, so between the write
    /// and a later retain the table would name a child no reference backs,
    /// and the concurrent tracer reads that as a phantom in-edge pushing
    /// the key toward looking unrooted. `barrier::publish_child` is the
    /// operation to publish through: it takes the reference for the entry
    /// and answers with the value the entry must name, which is a
    /// different one when the barrier copied an arena value out
    /// (`array::element::store_into` and `array::entity::fill_table_from`
    /// are the worked examples).
    ///
    /// Insert or overwrite. The outcome says which, or names the cause
    /// of a refusal — [`InsertOutcome`] is the contract, refusal causes
    /// included. `kind` states whether the key is the caller's own
    /// admission or a replay of one admitted before ([`InsertKind`]);
    /// the ladder's terminal rung refuses an admission and admits a
    /// replay, and nothing below it reads the kind.
    ///
    /// **Presence is decided ahead of every refusal**: the walk answers
    /// [`InsertOutcome::Replaced`] the moment it meets the key, so an
    /// overwrite of a present key is never refused.
    /// `array::element::box_element` re-inserts a key it proved present
    /// on that guarantee — a refusal check moved ahead of the walk
    /// breaks a caller, not a convenience.
    ///
    /// **`category` is the owner's, as it stands at this call.** Growth
    /// allocates from it and teardown frees to it, and promotion rewrites
    /// the owner's header, so a value cached across an arena reset frees to
    /// an allocator the storage never came from. Read it at the call site,
    /// which for an array is `array::entity::category_of`, and keep no copy
    /// beside the table (`dev/DECISIONS.md`, "the table is handed its
    /// category and reads no header").
    ///
    /// The old value of an overwritten key is returned to the caller
    /// rather than dropped here: releasing it is the owner's, because the
    /// order matters to the collector.
    ///
    /// **Key ownership.** Storing a *new* string key
    /// consumes the caller's reference — the retain published before this
    /// call becomes the table's one reference per stored key, given back
    /// by [`Table::remove`] or by teardown. The overwrite arm keeps the
    /// entry's original key and never stores the caller's, so
    /// [`InsertOutcome::Replaced`] also says the key reference stays the
    /// caller's: give it back through the barrier's `drop_ref` or reuse
    /// it, but do not count it as stored.
    pub fn insert(
        &mut self,
        head: &StorageHead,
        category: MemoryCategory,
        kind: InsertKind,
        key: Key,
        value: Value,
    ) -> InsertOutcome {
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

        if !head.storage().is_null() {
            let mut i = unsafe { *Self::slots(head).add(sh as usize & self.mask) };
            while i != NONE {
                let matched = Self::entry_matches(self.entry(head, i as usize), key);
                if matched {
                    // The element is published atomically and the chain
                    // link it carries is kept, which is what
                    // `Entry::store_element` exists for: the collector may
                    // be reading this very word.
                    let old = self.entry(head, i as usize).value();
                    unsafe { Entry::store_element(Self::entry_ptr(head, i as usize), value) };
                    return InsertOutcome::Replaced(old);
                }

                chain_len += 1;
                let e = self.entry(head, i as usize);
                // The counter counts an entry only when its tag equals
                // the incoming key's: an integer's identity is its
                // value, so equal identity is an overwrite and never a
                // chain entry (`rfc/model/maps.md`, "What the flood
                // ladder becomes").
                if matches!(key, Key::Str(_))
                    && e.key_word >= KEY_SENTINEL_LIMIT
                    && e.key_word & KEY_TAG_MASK == KEY_TAG_STRING
                    && e.hash_or_key == stored_hash
                {
                    equal_hashes += 1;
                }

                i = e.link();
            }
        }

        // Fires on insertion only: this path already holds exclusive
        // ownership, may allocate and may raise, while a lookup may do
        // none of those under a live iterator on a shared table.
        if equal_hashes >= EQUAL_HASH_LIMIT {
            if self.flags & TABLE_STRONG != 0 {
                // The terminal rung: equal identities met past
                // escalation have no rebuild left to take. A replay is
                // exempt from the refusal alone ([`InsertKind`]), and
                // its chain then grows past the trigger's limit — the
                // price of honouring the earlier admission.
                if kind == InsertKind::Admission {
                    return InsertOutcome::RefusedByLadder;
                }
            } else {
                self.escalate(head);
            }
        } else if chain_len >= CHAIN_LIMIT {
            if self.flags & TABLE_STRONG != 0 {
                // The chain trigger's firing past both rebuilds — the
                // terminal rung again, same exemption.
                if kind == InsertKind::Admission {
                    return InsertOutcome::RefusedByLadder;
                }
            } else {
                self.reseed(head);
            }
        }

        let sh = self.slot_hash(key);

        if head.used() == self.cap && !self.grow(head, category) {
            return InsertOutcome::RefusedForMemory;
        }

        let slot = sh as usize & self.mask;
        let k = head.used();
        let chain = unsafe { *Self::slots(head).add(slot) };
        // **The entry is written before the count that admits it.** A
        // concurrent walker reads `used` to bound its stride, so a count
        // published first offers it an entry nobody has written yet. That
        // is also why this goes through the raw entry pointer rather than
        // `entry_ptr`, which asserts the index is already inside the count.
        unsafe {
            let e = &mut *Self::entries(head).add(k);
            match key {
                Key::Int(v) => e.set_int_key(v),
                // `stored_hash`, not `sh`: the entry holds the key's own
                // identity, which is the string's cached hash. In strong
                // mode `sh` is a different number entirely, and storing it
                // here would make the key unfindable by its own hash.
                Key::Str(s) => e.set_string_key(s, stored_hash),
            }

            Entry::store_element_and_link(&raw mut *e, value, chain);
        }

        head.set_used(k + 1);
        unsafe { *Self::slots(head).add(slot) = k as u32 };
        self.live += 1;
        if let Key::Int(v) = key {
            self.note_int_key(v);
        }

        InsertOutcome::Added
    }

    /// Remove `key`, returning its value **and its key entity** for the
    /// caller to release. Unlinking leaves nothing behind: the chain is
    /// genuinely shorter, which is the property an open-addressed index
    /// cannot have.
    ///
    /// The second half of the pair is the ownership rule above: the table
    /// owes one reference per stored string key, and
    /// dropping a key hands that reference to the caller — `make_hole`
    /// overwrites the key word, so a reference not handed out here is
    /// dropped with nothing left to release it. Null for an integer key,
    /// which owes nothing.
    ///
    /// **Giving either half up goes through the barrier's `drop_ref`
    /// with the owner's category**, the verb `release_children` already
    /// uses, and for the same non-stylistic reasons: a heap key in an
    /// arena table is owed its one release by the reset log, so a bare
    /// `ll_release` double-frees it at the reset, and an arena entity
    /// held by a longer-lived table carries an escape hold-count that
    /// only `escape_lose` settles. `drop_ref` also absorbs the integer
    /// key's null. The halves may go in either order: the entry is a
    /// hole before this returns, so neither release is observable
    /// through the table.
    #[must_use = "the pair carries the table's key reference; dropping it leaks the key"]
    pub fn remove(&mut self, head: &StorageHead, key: Key) -> Option<(Value, *mut LLString)> {
        if head.storage().is_null() {
            return None;
        }

        let sh = self.slot_hash(key);
        let slot = sh as usize & self.mask;
        let mut i = unsafe { *Self::slots(head).add(slot) };
        let mut prev = NONE;
        while i != NONE {
            let matched = Self::entry_matches(self.entry(head, i as usize), key);
            let next = self.entry(head, i as usize).link();
            if matched {
                if prev == NONE {
                    unsafe { *Self::slots(head).add(slot) = next };
                } else {
                    unsafe { Entry::store_link(Self::entry_ptr(head, prev as usize), next) };
                }

                // The element goes first and the marker second: an
                // `undef` element carries no edge, so a collector that
                // reads between the two sees a live key over a value it
                // will not follow, which is a missed edge and not one that
                // never existed.
                let at = Self::entry_ptr(head, i as usize);
                let old = self.entry(head, i as usize).value();
                let removed_key = self.entry(head, i as usize).string_key();
                unsafe {
                    Entry::store_element_and_link(at, Value::undef(), NONE);
                    Entry::make_hole(at);
                }

                self.live -= 1;
                self.holes += 1;
                return Some((old, removed_key));
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
    /// Both arms **move entries**, so an owner holding live iterator
    /// positions has to repair them; that obligation belongs to the entity
    /// wrapper and nothing here reaches it yet. This answers only whether
    /// there is room now, and `insert` forwards even less: the channel is
    /// owed with the iterator rather than before it.
    fn grow(&mut self, head: &StorageHead, category: MemoryCategory) -> bool {
        if head.storage().is_null() {
            return self.realloc_storage(head, category, FIRST_CAP);
        }

        // Zend's rule: reclaim holes rather than doubling when they are
        // more than a thirty-second of the live count. Reclaiming
        // allocates too — a chunk of the same size rather than twice it —
        // so it refuses the same way doubling does.
        if self.holes > self.live / 32 + 1 {
            return self.compact(head, category).is_some();
        }

        match self.cap.checked_mul(2) {
            // `n > 0` is not arithmetic hygiene: doubling zero reports a
            // growth that made no room, and the caller then writes an
            // entry into a chunk that has none.
            Some(n) if n > 0 && n <= MAX_ENTRIES => self.realloc_storage(head, category, n),
            _ => false,
        }
    }

    /// Drop the holes and rebuild every chain, into a chunk of the size
    /// this one already has. `None` when that chunk could not be
    /// allocated, with the table exactly as it was; otherwise **how many
    /// holes were dropped**.
    ///
    /// That is not how many entries changed index, which is what an
    /// iterator repair will want: dropping the last entry's hole moves
    /// nothing, and one hole at the front moves everything behind it. This
    /// loop knows the hole count, so the hole count is what it reports;
    /// whoever builds iterator repair owes the other number, and `insert`
    /// owes it a channel, forwarding nothing about a move today.
    ///
    /// **Into a fresh chunk rather than in place.** Sliding live entries
    /// down inside the published chunk writes plainly under a collector
    /// reading with relaxed atomic loads, and atomic stores would answer
    /// the race without answering the arithmetic: a walker mid-stride
    /// would read one entry at two indices and count its child twice
    /// (`dev/DECISIONS.md`, "compaction moves the entries into a fresh
    /// chunk"). A chunk nothing has published is written by one thread, so
    /// the copy stays plain.
    pub fn compact(&mut self, head: &StorageHead, category: MemoryCategory) -> Option<usize> {
        // A table with no chunk has no holes and no capacity, and asking
        // for a chunk of `cap == 0` would hand back one no entry fits in
        // — with `mask` set and `storage` non-null, which is the state the
        // next insert reads as "room for an entry".
        if head.storage().is_null() {
            debug_assert_eq!(self.cap, 0, "a table with no chunk has no capacity");
            return Some(0);
        }

        self.move_entries(head, category, self.cap, EntryMove::DroppingHoles)
    }

    fn rebuild_index(&mut self, head: &StorageHead) {
        unsafe { std::ptr::write_bytes(Self::slots(head), 0xFF, head.nslots()) };
        for k in 0..head.used() {
            // Holes are skipped rather than linked: a hole's `key_word`
            // is a sentinel, not a string, so reading bytes through it in
            // strong mode would dereference 1.
            if self.entry(head, k).is_hole() {
                unsafe { Entry::store_link(Self::entry_ptr(head, k), NONE) };
                continue;
            }

            // The slot comes from the *mixed* integer or the string hash,
            // never from the stored word as-is.
            let sh = self.entry_slot_hash(self.entry(head, k));
            let slot = sh as usize & self.mask;
            let chain = unsafe { *Self::slots(head).add(slot) };
            // A relaxed atomic store, not a plain one: the link shares its
            // word with the element's tag and flags, which the collector
            // reads while this runs. The composition keeps those bytes, so
            // no version bracket is needed here either.
            unsafe { Entry::store_link(Self::entry_ptr(head, k), chain) };
            unsafe { *Self::slots(head).add(slot) = k as u32 };
        }
    }

    /// Allocate storage for `cap` entries and move the existing entries
    /// into it. False on refusal, with the table left exactly as it was —
    /// an allocation failure reports to a frame that can raise rather
    /// than aborting.
    ///
    /// The entries keep their indices, holes included, so a growth is
    /// invisible to anything holding one. Dropping the holes is
    /// [`Table::compact`], and it is the same move with a different copy.
    fn realloc_storage(
        &mut self,
        head: &StorageHead,
        category: MemoryCategory,
        cap: usize,
    ) -> bool {
        self.move_entries(head, category, cap, EntryMove::Whole)
            .is_some()
    }

    /// Move every entry into a freshly allocated chunk of `cap` entries
    /// and publish it, rebuilding the index there. `None` on refusal, with
    /// the table untouched; otherwise how many entries were left behind —
    /// zero for [`EntryMove::Whole`], the hole count for
    /// [`EntryMove::DroppingHoles`].
    ///
    /// **The entries are copied before anything publishes the
    /// destination**, which is what makes that copy a plain one: until
    /// `set_storage` the fresh chunk is reachable from this thread alone.
    /// The index region is written *after* publication, inside the window,
    /// and is safe for a narrower reason — no walker reads a slot, only
    /// the `nslots` count that offsets past them ([`Table::entries_of`]).
    /// The first reader of a slot has to move that write ahead of
    /// publication or make it atomic; the window hides it from an accepted
    /// reading, not from a stride already under way.
    ///
    /// The chunk being replaced is only read here, so a collector striding
    /// it races nothing.
    ///
    /// The old chunk is freed once the window is closed. A worker trace still
    /// striding it must read intact bytes; S38.3 owns the buffer-chunk
    /// withholding that extends this protection across another thread's trace
    /// (`PLAN.md`).
    fn move_entries(
        &mut self,
        head: &StorageHead,
        category: MemoryCategory,
        cap: usize,
        mode: EntryMove,
    ) -> Option<usize> {
        if cap == 0 || cap > MAX_ENTRIES {
            return None;
        }

        let nslots = pow2ge(cap) * 2;
        let bytes = storage_bytes(nslots, cap)?;
        let (mem, granted) = self.alloc(category, bytes);
        if mem.is_null() {
            return None;
        }

        let old_storage = head.storage();
        let old_capacity = self.storage_capacity;
        let old_used = head.used();
        let fresh_entries = unsafe { mem.add(entries_offset(nslots)) as *mut Entry };
        let mut written = 0usize;
        if !old_storage.is_null() {
            let old_entries = Self::entries(head);
            match mode {
                EntryMove::Whole => {
                    unsafe { std::ptr::copy_nonoverlapping(old_entries, fresh_entries, old_used) };
                    written = old_used;
                }
                EntryMove::DroppingHoles => {
                    for r in 0..old_used {
                        if self.entry(head, r).is_hole() {
                            continue;
                        }

                        unsafe {
                            std::ptr::copy_nonoverlapping(
                                old_entries.add(r),
                                fresh_entries.add(written),
                                1,
                            )
                        };
                        written += 1;
                    }
                }
            }
        }

        debug_assert!(
            mode == EntryMove::Whole || written == self.live,
            "dropping the holes wrote a count the live counter disagrees with"
        );
        head.begin_move();
        head.set_storage(mem);
        head.set_nslots(nslots);
        head.set_used(written);
        self.storage_capacity = granted;
        self.mask = nslots - 1;
        self.cap = cap;
        if mode == EntryMove::DroppingHoles {
            self.holes = 0;
        }

        self.rebuild_index(head);
        head.end_move();
        self.free_storage(category, old_storage, old_capacity);
        Some(old_used - written)
    }

    /// Give an empty table the chunk a replay of `live` entries ends on,
    /// in one allocation instead of the doubling schedule that reaches
    /// the same place. False on refusal, with the table exactly as empty
    /// as it was.
    ///
    /// The copy path is the caller (`array::entity::new_empty_copy`), and
    /// what it saves is the intermediate chunks: in the request arena
    /// nothing is reclaimed until the reset, so every chunk a doubling
    /// replay outgrows is headroom lost for the rest of the request.
    /// The end state is the schedule's own — `cap` a power of two of at
    /// least 8, `nslots` twice it — so a copy grows afterwards exactly
    /// where a filled table does.
    ///
    /// **The slot count is derived and not the source's.** Taking the
    /// source's would keep buckets apart that the copy's narrower mask
    /// merges, which is a defence the mask cannot supply: identities that
    /// differ are scattered by the copy's own salt, and identities that
    /// agree collide at every width and are what the equal-identity
    /// trigger is for (`rfc/model/maps.md`, "Rung three, refusal", and
    /// `dev/DECISIONS.md`, "a copy sizes its storage by its own replay").
    ///
    /// **A copy with nothing to replay takes no chunk, unless it holds a
    /// drawn rung bit**, in which case it takes the schedule's first one
    /// for [`Table::redraw_salt`] to draw an address from.
    pub(crate) fn presize_for_replay(
        &mut self,
        head: &StorageHead,
        category: MemoryCategory,
        live: usize,
    ) -> bool {
        debug_assert!(head.storage().is_null(), "the table already holds a chunk");
        debug_assert_eq!(head.used(), 0, "presizing a table that holds entries");
        debug_assert!(
            self.cap == 0 && self.live == 0 && self.holes == 0,
            "the counters of a table with no chunk are zero"
        );
        if live == 0 && self.flags & TABLE_RESEEDED == 0 {
            return true;
        }

        let cap = pow2ge(live).max(FIRST_CAP);
        if cap > MAX_ENTRIES {
            return false;
        }

        let nslots = cap * 2;
        let Some(bytes) = storage_bytes(nslots, cap) else {
            return false;
        };

        let (mem, granted) = self.alloc(category, bytes);
        if mem.is_null() {
            return false;
        }

        // Before publication rather than inside the window: until
        // `set_storage` the chunk is reachable from this thread alone,
        // which is what keeps this write plain.
        unsafe { std::ptr::write_bytes(mem as *mut u32, 0xFF, nslots) };
        head.begin_move();
        head.set_storage(mem);
        head.set_nslots(nslots);
        head.set_used(0);
        self.storage_capacity = granted;
        self.mask = nslots - 1;
        self.cap = cap;
        head.end_move();
        true
    }

    /// The storage, routed by `category` (`memory::routing::body_alloc`),
    /// with the bytes really granted reported alongside the pointer.
    ///
    /// **A body, not an entity**, which is the one thing a reader has to
    /// know here: storage has no `RcHeader`, and the cycle collector
    /// reads the first eight bytes of every occupied slot in an entity
    /// block as one (`memory/block_pool.rs`, `BLOCK_KIND_ENTITY`). What
    /// the body population buys a table beyond that is the ownership
    /// protocol: a table dies wherever its last reference is dropped, so
    /// a storage chunk is routinely freed by a thread that did not
    /// allocate it, and the buffer block carries the owner and the stack
    /// such a free posts to.
    fn alloc(&self, category: MemoryCategory, bytes: usize) -> (*mut u8, usize) {
        unsafe { crate::memory::routing::body_alloc(std::ptr::null_mut(), category, bytes) }
    }

    /// Sever every live entry: null its element, drop its key, and
    /// collect both into `displaced` — **without releasing them**. The
    /// array's half of a cycle teardown's "sever and free"
    /// (`rfc/model/gc/rc-cycle.md`, "Cycle teardown", step 6); the caller
    /// owes one drop per collected entry.
    ///
    /// An entry becomes a **hole** rather than keeping a nulled element,
    /// because an array's key is a counted child too and there is no
    /// "null key" to write: a hole is the one state in which
    /// `for_each_counted_child` yields neither. That is what makes the
    /// ordinary teardown after the un-guard find nothing left to release,
    /// which is the property the object side gets from writing nulls.
    ///
    /// The counters are settled here rather than left stale: the table
    /// outlives this call by the width of the drain, and a `live` above
    /// zero over an entry array of holes is a contradiction anything
    /// reading it would act on. The index is settled the same way —
    /// every bucket reads `NONE` and every hole's link is cut, so no
    /// chain reaches a hole and an insert after the sever walks nothing.
    pub(crate) fn sever_entries(&mut self, head: &StorageHead, displaced: &mut Vec<*mut RcHeader>) {
        for i in 0..head.used() {
            let e = self.entry(head, i);
            if e.is_hole() {
                continue;
            }

            let value = e.value();
            let key = e.string_key();
            // The table's own store rather than the barrier's: a barrier
            // write publishes a whole Box, zeroed reserved bytes and all,
            // which would set this entry's chain link to 0 — a legal entry
            // index where the unlink needs an end of chain
            // (`array/entry.rs`). One composed store keeps the word whole
            // for the collector.
            let at = Self::entry_ptr(head, i);
            unsafe {
                Entry::store_element_and_link(at, Value::null(), NONE);
                Entry::make_hole(at);
            }

            if value.is_refcounted() {
                displaced.push(value.entity_ptr());
            }

            if !key.is_null() {
                displaced.push(key as *mut RcHeader);
            }
        }

        // The index goes with the entries, for the reason the counters
        // do: a bucket left in place would chain through nothing but
        // holes, and the next insert's walk would read its trigger
        // counters off entries that no longer exist.
        if !head.storage().is_null() {
            unsafe { std::ptr::write_bytes(Self::slots(head), 0xFF, head.nslots()) };
        }

        self.live = 0;
        self.holes = head.used();
    }

    /// Release storage the table has replaced, through
    /// `memory::routing::body_free`: arena storage goes at the reset and
    /// immortal storage never goes, so only the long-lived categories do
    /// anything here.
    ///
    /// `capacity` is the granted size from [`Table::alloc`], not the
    /// requested one: the buffer arena's free is size-carrying and a
    /// chunk holds no metadata.
    ///
    /// `category` is the owner's, read by the caller at this call
    /// (`array::entity::category_of`), so a promotion that moves this
    /// storage needs no second field kept in step with it.
    fn free_storage(&self, category: MemoryCategory, p: *mut u8, capacity: usize) {
        unsafe { crate::memory::routing::body_free(category, p, capacity) };
    }

    /// The one draw of the per-table salt: the storage address run
    /// through the keyed hash under the per-process key
    /// (`hash::process_key`), and the flag saying the table has one.
    /// Idempotent by the flag, so whichever rung fires first draws and
    /// the other inherits. Never zero — a zero draw is remapped — so a
    /// drawn salt cannot masquerade as the unsalted state.
    ///
    /// Keyed under the per-process key so that a static reading of the
    /// artifact cannot compute the salt: the foldable seed enters
    /// nowhere — under `hash-folding` it travels inside the artifact,
    /// where an attacker holding the binary would read it
    /// (`rfc/model/maps.md`, "What the flood ladder becomes": every
    /// secret the ladder draws comes from the per-process key). It is
    /// not one-way: `strong_hash` is a placeholder invertible in its
    /// key, so a timing oracle recovers the salt, and `rfc/model/strings.md`
    /// ("Neither position is a defence") concedes that — the backstop is
    /// the ladder's bounded rebuilds, not the salt. What this still does
    /// not buy: storage addresses recycle across arena resets, so a salt
    /// can repeat within one process, and the long-key slot
    /// (`rfc/model/strings.md`) stays the unfilled function `strong_hash`
    /// stands in for.
    ///
    /// The triggers fire during an insert's chain walk, which needs
    /// entries, so the storage is never null here — asserted, because a
    /// null address would draw the same salt for every such table.
    fn draw_salt(&mut self, head: &StorageHead) {
        if self.flags & TABLE_RESEEDED != 0 {
            return;
        }

        debug_assert!(
            !head.storage().is_null(),
            "a draw before the first entry would salt every table alike"
        );
        self.flags |= TABLE_RESEEDED;
        self.salt = salt_from(head);
    }

    /// Draw again, over a table that took its rung bits from a source
    /// ([`Table::adopt_flood_state`]) — a copy indexes under a salt of
    /// its own. The bits are what bound the ladder, so the copy keeps
    /// them; the number is what a forged set is built against, and the
    /// copy's replay puts that set back key by key, so an inherited
    /// number would reproduce the source's chains slot for slot with
    /// both rebuilds already spent (`rfc/model/maps.md`, "Rung three,
    /// refusal").
    ///
    /// A no-op on a table with no salt drawn: the flag is the
    /// authority, and an unsalted copy indexes integer keys by value
    /// exactly as its source does.
    ///
    /// **Call it before the first insert**, for the reason
    /// [`Table::adopt_flood_state`] carries: the number decides which
    /// slot a key takes.
    pub(crate) fn redraw_salt(&mut self, head: &StorageHead) {
        if self.flags & TABLE_RESEEDED == 0 {
            return;
        }

        debug_assert_eq!(head.used(), 0, "the salt decides where a key is indexed");
        debug_assert!(
            !head.storage().is_null(),
            "a table holding a drawn salt holds the storage it was drawn from"
        );
        self.salt = salt_from(head);
    }

    /// Escalate to the keyed byte hash, once and one way. The response
    /// to *equal full hashes*: redrawing a salt cannot separate keys whose
    /// hashes agree, and doing so on that trigger is what made Perl's
    /// REHASH exploitable (CVE-2013-1667).
    ///
    /// Firing from an unsalted table draws the salt on the way, because
    /// the salt enters the keyed hash's key ([`Table::strong_key`]):
    /// without a draw every escalated table would scatter one colliding
    /// set identically, and per-table diversity is what the draw buys —
    /// the design's residual assumption, a new colliding set costing a
    /// break of a keyed PRF, wants the key per-table as well as
    /// per-process. That is a draw, not the redraw the Perl defect is
    /// about: a salt already drawn is left exactly as it was.
    fn escalate(&mut self, head: &StorageHead) {
        // Unreachable past escalation rather than a no-op there: the
        // trigger block refuses on a strong table instead of calling
        // down, and a silent return here would hide a caller that
        // stopped making that test.
        debug_assert!(
            self.flags & TABLE_STRONG == 0,
            "past escalation a trigger refuses instead of escalating"
        );
        self.draw_salt(head);
        self.flags |= TABLE_STRONG;
        if !head.storage().is_null() {
            self.rebuild_index(head);
        }
    }

    /// Draw the per-table salt and rebuild the index — the ladder's
    /// first rung, moving integer keys off by-value indexing. The
    /// response to a long chain of keys whose hashes *differ* — an
    /// accident or an integer flood. **A second firing escalates
    /// instead**, which is what bounds the attacker at one rebuild and
    /// one escalation per table (`rfc/model/arrays-hashtable.md`, "What
    /// the attacker can drive"). Without that bound the chain trigger
    /// fires on every insert an attacker chooses, each firing an
    /// O(`used`) rebuild, and the promised O(n) is O(n²).
    ///
    /// **The two rungs answer different failures**: this one, keys whose
    /// identities differ while their buckets coincide — its mix covers
    /// every kind's slot, the integer's value through [`mix_int`] and a
    /// string's cached hash through [`mix_word`] — and escalation, keys
    /// whose identity numbers agree while the keys differ
    /// (`dev/DECISIONS.md`, "rung one salts every kind's slot, under the
    /// per-process key", amending "the flood ladder's two rungs answer
    /// different key kinds").
    ///
    /// The salt is drawn at most once per table ([`Table::draw_salt`]),
    /// so there is no orbit to learn and no redraw to aim. A COW copy is
    /// a new table by that reckoning and draws its own before its first
    /// insert ([`Table::redraw_salt`]); what it inherits is the rung
    /// bits, so its second long chain escalates rather than rebuilding.
    fn reseed(&mut self, head: &StorageHead) {
        debug_assert!(
            self.flags & TABLE_STRONG == 0,
            "past escalation a trigger refuses instead of rebuilding"
        );
        if self.flags & TABLE_RESEEDED != 0 {
            self.escalate(head);
            return;
        }

        self.draw_salt(head);
        if !head.storage().is_null() {
            self.rebuild_index(head);
        }
    }

    /// Visit every live element's `Value`, in insertion order. A test
    /// window: production reaches the entries through the walk's coherent
    /// read, which is the enumeration the completeness rule is written
    /// against (`rfc/model/arrays-hashtable.md`, "Complete enumeration").
    ///
    /// The dense prefix `0..used` with holes skipped is that enumeration
    /// here, and the hole marker lives in `key_word`, outside the sixteen
    /// bytes the store barrier writes, so an ordinary value store cannot
    /// destroy it.
    #[cfg(test)]
    pub fn for_each_value(&self, head: &StorageHead, mut f: impl FnMut(Value)) {
        for k in 0..head.used() {
            let e = self.entry(head, k);
            if !e.is_hole() {
                f(e.value());
            }
        }
    }

    /// The same, for a walker that rewrites elements: what `f` returns is
    /// published as the new element.
    ///
    /// **By value in both directions.** A `&mut Value` into an entry would
    /// let a caller store a whole Box over the chain link the element's
    /// reserved bytes carry, and zero is a legal entry index rather than
    /// an end of chain, so the corruption would be a self-referencing
    /// entry (`array/entry.rs`). Returning the new Box instead routes
    /// every write through the one store that keeps the link.
    #[cfg(test)]
    pub fn for_each_value_mut(&mut self, head: &StorageHead, mut f: impl FnMut(Value) -> Value) {
        for k in 0..head.used() {
            if self.entry(head, k).is_hole() {
                continue;
            }

            let replaced = f(self.entry(head, k).value());
            unsafe { Entry::store_element(Self::entry_ptr(head, k), replaced) };
        }
    }

    /// Every string key that is a live entity, in insertion order. Keys
    /// are counted children too: a table holds a reference to each. A
    /// test window, like [`Table::for_each_value`].
    #[cfg(test)]
    pub fn for_each_string_key(&self, head: &StorageHead, mut f: impl FnMut(*mut LLString)) {
        for k in 0..head.used() {
            let e = self.entry(head, k);
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
    /// `category` is the owner's at this call and decides which allocator
    /// the storage goes back to, so it obeys [`Table::insert`]'s rule: a
    /// value cached across a promotion frees to the wrong one.
    ///
    /// The values are **not** released here: their order matters to the
    /// collector, so the entity wrapper walks and releases them first and
    /// then calls this. Nothing here reads a value.
    ///
    /// **The three words a trace reads go out inside the head's
    /// window**, because a dying array is still reachable: it dies while
    /// a collection holds its slot. Published
    /// one by one they offer readings no array was ever in — the chunk
    /// with the counts of the empty state, which for this representation
    /// strides the index region as entries. The window is also what
    /// makes the order of the three stores below free to change: without
    /// it the null chunk has to be published first and nothing says so.
    pub fn dispose(&mut self, head: &StorageHead, category: MemoryCategory) {
        let p = head.storage();
        let capacity = self.storage_capacity;
        head.begin_move();
        head.set_storage(std::ptr::null_mut());
        head.set_nslots(0);
        head.set_used(0);
        head.end_move();
        self.storage_capacity = 0;
        self.mask = 0;
        self.cap = 0;
        self.live = 0;
        self.holes = 0;
        self.free_storage(category, p, capacity);
    }

    /// Iterate live entries in insertion order. This reads no index at
    /// all, which is why the choice of index layer does not affect
    /// `foreach`. A test window, like [`Table::for_each_value`].
    #[cfg(test)]
    pub fn iter<'a>(&'a self, head: &'a StorageHead) -> impl Iterator<Item = &'a Entry> {
        (0..head.used())
            .map(move |i| self.entry(head, i))
            .filter(|e| !e.is_hole())
    }
}

#[cfg(test)]
mod tests;
