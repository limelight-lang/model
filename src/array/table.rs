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

use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::array::entry::{Entry, MAX_ENTRIES, NONE};
use crate::memory::block_pool::BLOCK_PAYLOAD;
use crate::refcount::{MemoryCategory, RcHeader};
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
pub(crate) const EQUAL_HASH_LIMIT: u32 = 8;

/// The second trigger: chain length. This catches families whose hashes
/// differ but whose slots coincide, including an integer flood. Generous,
/// since the honest maximum is 4-8 even at millions of keys.
pub(crate) const CHAIN_LIMIT: u32 = 32;

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

/// How many times a walker re-reads a table whose entries keep moving
/// before it gives up on this epoch. Small on purpose: growth and
/// compaction are rare, so a second disagreement means the walker is
/// unlucky rather than starved, and giving up leaks one epoch's worth
/// rather than freeing anything early.
const COHERENT_READ_ATTEMPTS: usize = 4;

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
    /// Published atomically because the concurrent collector reads it
    /// while this thread writes it: a plain write against a relaxed load
    /// is a data race, which is undefined behaviour rather than the torn
    /// value the epoch is built to absorb.
    storage: AtomicPtr<u8>,
    /// Bytes really granted for `storage`, which is not always what was
    /// asked for: a reused buffer-arena chunk may be larger, and the free
    /// that returns it carries the size, since a chunk holds no metadata
    /// of its own. Freeing with the requested size would lose the
    /// difference from the block's free list. In the two categories that
    /// never free — the request arena and the immortal region — this holds
    /// the requested size and nothing reads it.
    storage_capacity: usize,
    /// Atomic for the same reason as `storage`; the entries begin
    /// `nslots * 4` bytes into the chunk, so a walker needs both.
    nslots: AtomicUsize,
    mask: usize,
    cap: usize,
    /// Entries written so far, holes included. Iteration and the arena
    /// reset's tracer both scan `0..used`. Atomic, and **published after
    /// the entry it counts is written**: a reader that saw the count
    /// first would read an entry nobody had written yet.
    used: AtomicUsize,
    /// Live entries.
    live: usize,
    holes: usize,
    /// Zero from birth. Meaningful only while [`TABLE_RESEEDED`] is set:
    /// the ladder's first rung draws it, and nothing else writes it.
    salt: u64,
    /// The next append key: one past the highest integer key ever
    /// inserted, [`NEXT_FREE_NONE`] while none has been. Removal never
    /// rewinds it — PHP's `nNextFreeElement` — and a COW copy inherits
    /// it ([`Table::adopt_append_state`]), because the replay skips
    /// holes and would otherwise rewind past a removed high key.
    ///
    /// A negative key moves it too: PHP 8.3 semantics, where
    /// `$a[-5] = 1; $a[] = 2;` appends at −4 (assumption recorded in
    /// `PLAN.md` S2.4, Edmond's to overturn — the pre-8.3 answer is one
    /// comparison away).
    next_free: i64,
    /// Bumped twice by every operation that moves entries — growth and
    /// compaction — odd while the move is in progress. A concurrent
    /// walker reads it, then `storage`, `nslots` and `used`, then reads it
    /// again, and starts over unless both readings are the same even
    /// number. That is the whole bound on walking a table that a mutator
    /// is rearranging: a stale-but-coherent view of the entries is a
    /// missed edge, which the epoch's later phases repair, while an
    /// incoherent one is an edge that never existed, which nothing does.
    version: AtomicUsize,
    /// The table's one-bit state, [`TABLE_STRONG`] and
    /// [`TABLE_RESEEDED`]. One byte rather than a `bool` apiece because
    /// the strategy tag joins them: `rfc/model/arrays.md` gives an array
    /// three storage strategies and two bits to name the current one, and
    /// the entity's flags word has no free bit to put them in
    /// (`PLAN.md`, "The strategy tag and the `arrays.md` hole").
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
/// [`Table::append_key`] refuses rather than wrapping. Bit 4, leaving
/// bits 2–3 for the storage-strategy tag the plan reserves.
const TABLE_APPEND_EXHAUSTED: u8 = 1 << 4;

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
    /// the flood itself (`PLAN.md` S2.1, Edmond 2026-08-07).
    ///
    /// No category: which memory this table's storage comes from is the
    /// owning entity's header to say, and this reads it there
    /// ([`Table::category`], `dev/DECISIONS.md` 2026-08-07).
    pub const fn empty() -> Self {
        Table {
            storage: AtomicPtr::new(std::ptr::null_mut()),
            storage_capacity: 0,
            nslots: AtomicUsize::new(0),
            mask: 0,
            cap: 0,
            used: AtomicUsize::new(0),
            version: AtomicUsize::new(0),
            live: 0,
            holes: 0,
            salt: 0,
            next_free: NEXT_FREE_NONE,
            flags: 0,
        }
    }

    /// The key the next append takes, or `None` once `i64::MAX` has been
    /// a key — the refusal `PLAN.md` S2.4 requires in place of wrapping.
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

    /// The per-table salt, which a COW copy inherits through
    /// [`Table::adopt_flood_state`] so that a copied table indexes its
    /// keys exactly as the original did. [`TABLE_RESEEDED`] is the
    /// authority on whether one has been drawn; zero happens to mean
    /// "not drawn" as well, because the draw never yields it.
    ///
    /// A test window on purpose: on an escalated table the salt keys
    /// `strong_hash`, and an accessor exported past the crate would hand
    /// that key to anything linking against the runtime. The inheritance
    /// itself runs through [`Table::adopt_flood_state`], which reads the
    /// field directly.
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

    /// The memory an array's storage comes from, read from `owner`'s
    /// header — **the only authority there is**. A copy of the category
    /// in the table would be a second fact to keep in step with the
    /// first, and it drifted once already: a refused promotion left it
    /// reading `RequestArena` under a heap array, so the next storage
    /// came from whatever request arena was mounted (`2e55036`,
    /// `dev/DECISIONS.md` 2026-08-07).
    ///
    /// **The owner is a parameter and is never derived from a reference
    /// to the table.** A table is embedded in its array one `RcHeader`
    /// past the header, so the address is a subtraction away — but a
    /// reference to the body carries provenance over the body alone, and
    /// reading the header is an atomic load, which asks for a write
    /// permission that a shared reference cannot grant at any offset.
    /// Only Miri sees it; every other build performs the read and reports
    /// nothing.
    ///
    /// The debug assertion is what states the requirement out loud: given
    /// anything but an array entity, this answers with whatever that
    /// memory holds.
    ///
    /// Callers hold `owner` as a raw pointer to the live array this table
    /// belongs to.
    #[inline]
    pub(crate) fn category_of(owner: *const RcHeader) -> MemoryCategory {
        let flags = unsafe { crate::refcount::header_flags(owner) };
        debug_assert_eq!(
            (flags & crate::refcount::ENTITY_KIND_MASK) >> crate::refcount::ENTITY_KIND_SHIFT,
            crate::refcount::EntityKind::Array as u32,
            "a table read a header that is not an array's: moved out of its entity?"
        );
        MemoryCategory::from_flags(flags)
    }

    /// True once the table has escalated to the keyed byte hash.
    #[inline]
    pub fn is_strong(&self) -> bool {
        self.flags & TABLE_STRONG != 0
    }

    /// Take `source`'s flood state — the salt and both rung bits, which
    /// is what a copy of an attacked table owes: an escalated table
    /// copied through a fresh [`Table::empty`] would otherwise re-insert
    /// the attacker's whole collision set under the hash it escalated
    /// away from, and copying an array is the ordinary thing the
    /// language does. The salt travels with [`TABLE_RESEEDED`], because
    /// a copy that kept the bit and not the number would index through
    /// `mix_int(k, 0)` — a mix every attacker can compute offline.
    ///
    /// **Call it before the first insert.** The mode decides how a key is
    /// hashed, so a table that adopts it afterwards has already indexed
    /// its entries the other way.
    #[inline]
    pub(crate) fn adopt_flood_state(&mut self, source: &Table) {
        debug_assert_eq!(self.used(), 0, "the mode decides how a key is indexed");
        debug_assert!(
            source.flags & TABLE_STRONG == 0 || source.flags & TABLE_RESEEDED != 0,
            "an escalated table always holds a drawn salt: escalate draws on the way"
        );
        self.salt = source.salt;
        self.flags = (self.flags & !TABLE_FLOOD_STATE) | (source.flags & TABLE_FLOOD_STATE);
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
        self.used.load(Ordering::Relaxed)
    }

    /// Publish the entry count. **Called after the entry it counts is
    /// written**, never before: a walker that saw the count first would
    /// read an entry nobody had written.
    #[inline]
    fn set_used(&self, n: usize) {
        self.used.store(n, Ordering::Release);
    }

    /// The storage chunk, or null before the first insert.
    #[inline]
    fn storage(&self) -> *mut u8 {
        self.storage.load(Ordering::Relaxed)
    }

    /// Publish the storage chunk. Release, so a walker that acquires this
    /// pointer sees the entries already copied into it.
    #[inline]
    fn set_storage(&self, p: *mut u8) {
        self.storage.store(p, Ordering::Release);
    }

    /// Open a window in which entries move. The version goes odd, and a
    /// walker that sees an odd reading — or two different readings around
    /// its own — starts over (`PLAN.md`, item 12).
    #[inline]
    fn begin_entry_move(&self) {
        let v = self.version.load(Ordering::Relaxed);
        self.version.store(v + 1, Ordering::Release);
    }

    /// Close it. Even again, and everything the move wrote is published
    /// before the walker can accept the reading.
    #[inline]
    fn end_entry_move(&self) {
        let v = self.version.load(Ordering::Relaxed);
        self.version.store(v + 1, Ordering::Release);
    }

    /// The dense entry array and how many entries it holds, read
    /// coherently, or `None` when the mutator kept moving them.
    ///
    /// The three words a walker needs — the chunk, the offset the entries
    /// start at, and how many there are — are written independently, so a
    /// walker that read them one by one could stride a fresh count over a
    /// stale chunk. Growth would be caught by comparing the chunk address
    /// before and after; compaction would not, because it slides live
    /// entries down *inside the same chunk*. Hence the version: odd while
    /// entries move, and changed across any move that completed.
    ///
    /// **`None` is safe and leaks rather than frees early.** An entity the
    /// walk does not enumerate becomes a root source — its out-edges land
    /// in `RC` and never in `IN` — so its children are computed roots and
    /// survive one more epoch (`rfc/model/gc/retained-block-walk.md`, the
    /// derived-roots corollary). That is what makes a bounded retry the
    /// right answer rather than an unbounded one.
    ///
    /// # Safety
    /// The table is live. Under a concurrent mutator every word this reads
    /// is atomic; nothing else in the table may be read here.
    pub(crate) unsafe fn coherent_entries(t: *const Table) -> Option<(*mut Entry, usize)> {
        // Raw pointers per field, never a `&Table`: a shared reference
        // would retag the whole struct, and the mutator is writing the
        // words beside these — `mask`, `cap`, `live` — with ordinary
        // stores. Only the four atomics below may be read here.
        let version = unsafe { &(*t).version };
        let storage_word = unsafe { &(*t).storage };
        let nslots_word = unsafe { &(*t).nslots };
        let used_word = unsafe { &(*t).used };
        for _ in 0..COHERENT_READ_ATTEMPTS {
            let before = version.load(Ordering::Acquire);
            if before % 2 != 0 {
                continue;
            }
            let storage = storage_word.load(Ordering::Relaxed);
            let nslots = nslots_word.load(Ordering::Relaxed);
            let used = used_word.load(Ordering::Relaxed);
            if version.load(Ordering::Acquire) != before {
                continue;
            }
            if storage.is_null() {
                return Some((std::ptr::null_mut(), 0));
            }
            return Some((
                unsafe { storage.add(entries_offset(nslots)) } as *mut Entry,
                used,
            ));
        }
        None
    }

    /// The version a walker validates its reading against.
    ///
    /// Tests only: [`coherent_entries`](Self::coherent_entries) reads the
    /// counter itself, from a raw pointer it must not turn into a
    /// reference, so this accessor exists for the tests that assert the
    /// counter moves — and outside them it is a dead read that warns.
    #[cfg(test)]
    #[inline]
    pub(crate) fn version(&self) -> usize {
        self.version.load(Ordering::Acquire)
    }

    /// Index slots before the dense entry array.
    #[inline]
    fn nslots(&self) -> usize {
        self.nslots.load(Ordering::Relaxed)
    }

    #[inline]
    fn set_nslots(&self, n: usize) {
        self.nslots.store(n, Ordering::Release);
    }

    /// The storage and the bytes granted for it, or a null pointer when
    /// the table has never grown.
    ///
    /// Read by promotion, which needs the address to find the block
    /// holding the storage when a carry was refused. Every *operation*
    /// over the storage is a method here — this hands out the address, not
    /// the right to walk it.
    #[inline]
    pub(crate) fn storage_and_capacity(&self) -> (*mut u8, usize) {
        (self.storage(), self.storage_capacity)
    }

    /// Carry this table's storage out of `arena`, which is about to
    /// reset, and record that the table now belongs to the GC heap.
    ///
    /// The entity's header stays where it is — promotion retains the block
    /// holding it — while the storage is arena memory that would go back
    /// to the pool and be handed to somebody else. **Nothing inside the
    /// storage points into it**: every chain link is a `u32` index, which
    /// is what makes a flat copy legal and is pinned by a test.
    ///
    /// Two routes, chosen by where the storage came from, the same pair a
    /// string's payload takes:
    ///
    /// - **OS-direct** (over a block payload): the arena forgets the run
    ///   and the storage keeps its address. Nothing is allocated, so
    ///   nothing can be refused — which matters, because a reset has no
    ///   caller left to report a refusal to.
    /// - **In-block**: a fresh buffer-arena chunk, copied. Bounded by a
    ///   block payload, so the copy is bounded too.
    ///
    /// Nothing here records where the storage now lives. The category is
    /// the header's to say and promotion rewrites the header a moment
    /// later, so every later free of this storage reads the new answer
    /// with no second field to keep in step (`dev/DECISIONS.md`
    /// 2026-08-07). A refused carry is safe under the new category too:
    /// promotion stamps the storage's block
    /// `BLOCK_KIND_RETAINED` right after, and that is the one kind
    /// `buffer_free_longlived_payload` leaves alone — the same mechanism
    /// that protects a string's uncarried payload, rather than a second
    /// one of our own.
    ///
    /// **False when the copy was refused**, with the storage untouched.
    ///
    /// # Safety
    /// The table must be a live request-arena table of `arena`,
    /// mid-reset, and `owner` the array entity holding it.
    pub(crate) unsafe fn carry_out_of(
        &mut self,
        owner: *const RcHeader,
        arena: *mut crate::memory::arena::Arena,
    ) -> bool {
        debug_assert_eq!(
            Self::category_of(owner),
            MemoryCategory::RequestArena,
            "only an arena table is carried out of a reset"
        );
        if self.storage().is_null() {
            return true;
        }

        if self.storage_capacity > BLOCK_PAYLOAD {
            // True whatever the log says, for the reason
            // `string::carry_payload_out_of` gives: a miss means nothing
            // will free the run, so the storage keeps its address and
            // leaks — the safe direction — while reporting a refusal would
            // send the caller into stamping `BLOCK_KIND_RETAINED` over the
            // run's own header.
            let forgotten = unsafe { (*arena).forget_large(self.storage()) };
            debug_assert!(forgotten, "an OS-direct storage the arena never logged");
            return true;
        }

        // The destination is named rather than read from the header,
        // which still says `RequestArena` here: promotion rewrites it
        // after the carry, so that everything the survivor owns moves
        // while the category still describes where it lives
        // (`promote.rs`).
        let (fresh, granted) = unsafe {
            crate::memory::routing::body_alloc(
                std::ptr::null_mut(),
                MemoryCategory::GcHeap,
                self.storage_capacity,
            )
        };
        if fresh.is_null() {
            return false;
        }
        unsafe { std::ptr::copy_nonoverlapping(self.storage(), fresh, self.storage_capacity) };
        self.set_storage(fresh);
        self.storage_capacity = granted;
        true
    }

    #[inline]
    fn slots(&self) -> *mut u32 {
        self.storage() as *mut u32
    }

    #[inline]
    fn entries(&self) -> *mut Entry {
        unsafe { self.storage().add(entries_offset(self.nslots())) as *mut Entry }
    }

    /// The entry at `i`. Callers hold `i < used`.
    #[inline]
    pub fn entry(&self, i: usize) -> &Entry {
        debug_assert!(i < self.used());
        unsafe { &*self.entries().add(i) }
    }

    /// A raw pointer to the entry at `i`, which is what every write to a
    /// published entry goes through: the element and the key word are
    /// stored atomically ([`Entry::store_element`]), and an atomic store
    /// needs a pointer rather than a reference to reach the bytes.
    #[inline]
    fn entry_ptr(&self, i: usize) -> *mut Entry {
        debug_assert!(i < self.used());
        unsafe { self.entries().add(i) }
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
            if self.flags & TABLE_RESEEDED != 0 {
                mix_int(e.hash_or_key as i64, self.salt)
            } else {
                e.hash_or_key
            }
        } else if self.flags & TABLE_STRONG != 0 {
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
    ///
    /// **A copy, not a reference into the entry.** An entry keeps its
    /// chain link inside the element's reserved bytes, so the Box has to
    /// be handed out through `Entry::value`, which clears them
    /// (`array/entry.rs`). The copy also means a caller holds no borrow of
    /// the table, so a value read before a `remove` names an entity the
    /// caller must have its own reference to.
    pub fn get(&self, key: Key) -> Option<Value> {
        if self.storage().is_null() {
            return None;
        }
        let sh = self.slot_hash(key);
        let mut i = unsafe { *self.slots().add(sh as usize & self.mask) };
        while i != NONE {
            let e = self.entry(i as usize);
            if Self::entry_matches(e, key) {
                return Some(e.value());
            }
            i = e.link();
        }
        None
    }

    #[inline]
    pub fn contains(&self, key: Key) -> bool {
        self.get(key).is_some()
    }

    /// **The caller publishes its references before it inserts.** The
    /// entry is written raw and this counts nothing, so between the write
    /// and the caller's retain the table names a child no reference backs.
    /// While nothing walked an array concurrently that window was
    /// invisible; the tracer reads one now, and a phantom in-edge pushes
    /// the key toward looking unrooted. `barrier::store_category_barrier`
    /// is the operation to publish through — it takes an already-retained
    /// reference and returns the entity the entry must name, which is a
    /// different one when the barrier copied an arena value out
    /// (`array::entity::separate` is the worked example).
    ///
    /// Insert or overwrite. Returns `None` when the storage could not
    /// grow — an allocation refusal reports rather than aborting, and the
    /// table is unchanged. `Some(true)` means a new key was added.
    ///
    /// The old value of an overwritten key is returned to the caller
    /// rather than dropped here: releasing it is the owner's, because the
    /// order matters to the collector.
    ///
    /// **Key ownership** (`PLAN.md` S2.2): storing a *new* string key
    /// consumes the caller's reference — the retain published before this
    /// call becomes the table's one reference per stored key, given back
    /// by [`Table::remove`] or by teardown. The overwrite arm keeps the
    /// entry's original key and never stores the caller's, so there
    /// `added == false` also says the key reference stays the caller's:
    /// give it back through the barrier's `drop_ref` or reuse it, but do
    /// not count it as stored.
    pub fn insert(
        &mut self,
        owner: *const RcHeader,
        key: Key,
        value: Value,
    ) -> Option<(bool, Option<Value>)> {
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
        if !self.storage().is_null() {
            let mut i = unsafe { *self.slots().add(sh as usize & self.mask) };
            while i != NONE {
                let matched = Self::entry_matches(self.entry(i as usize), key);
                if matched {
                    // The element is published atomically and the chain
                    // link it carries is kept, which is what
                    // `Entry::store_element` exists for: the collector may
                    // be reading this very word.
                    let old = self.entry(i as usize).value();
                    unsafe { Entry::store_element(self.entry_ptr(i as usize), value) };
                    return Some((false, Some(old)));
                }
                chain_len += 1;
                let e = self.entry(i as usize);
                if !e.is_int_key() && e.hash_or_key == stored_hash {
                    equal_hashes += 1;
                }
                i = e.link();
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

        if self.used() == self.cap && !self.grow(owner) {
            return None;
        }

        let slot = sh as usize & self.mask;
        let k = self.used();
        let head = unsafe { *self.slots().add(slot) };
        // **The entry is written before the count that admits it.** A
        // concurrent walker reads `used` to bound its stride, so a count
        // published first offers it an entry nobody has written yet. That
        // is also why this goes through the raw entry pointer rather than
        // `entry_ptr`, which asserts the index is already inside the count.
        unsafe {
            let e = &mut *self.entries().add(k);
            match key {
                Key::Int(v) => e.set_int_key(v),
                // `stored_hash`, not `sh`: the entry holds the key's own
                // identity, which is the string's cached hash. In strong
                // mode `sh` is a different number entirely, and storing it
                // here would make the key unfindable by its own hash.
                Key::Str(s) => e.set_string_key(s, stored_hash),
            }
            Entry::store_element_and_link(&raw mut *e, value, head);
        }
        self.set_used(k + 1);
        unsafe { *self.slots().add(slot) = k as u32 };
        self.live += 1;
        if let Key::Int(v) = key {
            self.note_int_key(v);
        }
        Some((true, None))
    }

    /// Remove `key`, returning its value **and its key entity** for the
    /// caller to release. Unlinking leaves nothing behind: the chain is
    /// genuinely shorter, which is the property an open-addressed index
    /// cannot have.
    ///
    /// The second half of the pair is the ownership rule of `PLAN.md`
    /// S2.2: the table owes one reference per stored string key, and
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
    pub fn remove(&mut self, key: Key) -> Option<(Value, *mut LLString)> {
        if self.storage().is_null() {
            return None;
        }
        let sh = self.slot_hash(key);
        let slot = sh as usize & self.mask;
        let mut i = unsafe { *self.slots().add(slot) };
        let mut prev = NONE;
        while i != NONE {
            let matched = Self::entry_matches(self.entry(i as usize), key);
            let next = self.entry(i as usize).link();
            if matched {
                if prev == NONE {
                    unsafe { *self.slots().add(slot) = next };
                } else {
                    unsafe { Entry::store_link(self.entry_ptr(prev as usize), next) };
                }
                // The element goes first and the marker second: an
                // `undef` element carries no edge, so a collector that
                // reads between the two sees a live key over a value it
                // will not follow, which is a missed edge and not one that
                // never existed.
                let at = self.entry_ptr(i as usize);
                let old = self.entry(i as usize).value();
                let removed_key = self.entry(i as usize).string_key();
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
    /// Compaction **moves entries**, so an owner holding live iterator
    /// positions has to repair them; that obligation belongs to the
    /// entity wrapper, and this returns whether a compaction happened so
    /// the wrapper can act on it.
    fn grow(&mut self, owner: *const RcHeader) -> bool {
        if self.storage().is_null() {
            return self.realloc_storage(owner, 8);
        }
        // Zend's rule: reclaim holes rather than doubling when they are
        // more than a thirty-second of the live count.
        if self.holes > self.live / 32 + 1 {
            self.compact();
            return true;
        }
        match self.cap.checked_mul(2) {
            Some(n) if n <= MAX_ENTRIES => self.realloc_storage(owner, n),
            _ => false,
        }
    }

    /// Slide live entries down over the holes and rebuild every chain.
    /// Returns the number of entries that moved, which is what an
    /// iterator repair needs.
    pub fn compact(&mut self) -> usize {
        self.begin_entry_move();
        let moved = self.compact_entries();
        self.end_entry_move();
        moved
    }

    fn compact_entries(&mut self) -> usize {
        let mut w = 0usize;
        for r in 0..self.used() {
            if self.entry(r).is_hole() {
                continue;
            }
            if w != r {
                unsafe {
                    std::ptr::copy_nonoverlapping(self.entries().add(r), self.entries().add(w), 1)
                };
            }
            w += 1;
        }
        let moved = self.used() - w;
        self.set_used(w);
        self.holes = 0;
        self.rebuild_index();
        moved
    }

    fn rebuild_index(&mut self) {
        unsafe { std::ptr::write_bytes(self.slots(), 0xFF, self.nslots()) };
        for k in 0..self.used() {
            // Holes are skipped rather than linked: a hole's `key` field
            // is a sentinel, not a string, so reading bytes through it in
            // strong mode would dereference 1.
            if self.entry(k).is_hole() {
                unsafe { Entry::store_link(self.entry_ptr(k), NONE) };
                continue;
            }
            // The slot comes from the *mixed* integer or the string hash,
            // never from the stored word as-is.
            let sh = self.entry_slot_hash(self.entry(k));
            let slot = sh as usize & self.mask;
            let head = unsafe { *self.slots().add(slot) };
            // A relaxed atomic store, not a plain one: the link shares its
            // word with the element's tag and flags, which the collector
            // reads while this runs. The composition keeps those bytes, so
            // no version bracket is needed here either.
            unsafe { Entry::store_link(self.entry_ptr(k), head) };
            unsafe { *self.slots().add(slot) = k as u32 };
        }
    }

    /// Allocate storage for `cap` entries and move the existing entries
    /// into it. False on refusal, with the table left exactly as it was —
    /// an allocation failure reports to a frame that can raise rather
    /// than aborting.
    fn realloc_storage(&mut self, owner: *const RcHeader, cap: usize) -> bool {
        if cap > MAX_ENTRIES {
            return false;
        }
        let nslots = pow2ge(cap) * 2;
        let bytes = match storage_bytes(nslots, cap) {
            Some(b) => b,
            None => return false,
        };
        let (mem, granted) = self.alloc(owner, bytes);
        if mem.is_null() {
            return false;
        }
        self.begin_entry_move();
        let old_storage = self.storage();
        let old_capacity = self.storage_capacity;
        let old_used = self.used();
        let old_entries = if old_storage.is_null() {
            std::ptr::null_mut()
        } else {
            self.entries()
        };

        self.set_storage(mem);
        self.storage_capacity = granted;
        self.set_nslots(nslots);
        self.mask = nslots - 1;
        self.cap = cap;
        if !old_entries.is_null() {
            unsafe { std::ptr::copy_nonoverlapping(old_entries, self.entries(), old_used) };
        }
        self.rebuild_index();
        self.end_entry_move();
        self.free_storage(owner, old_storage, old_capacity);
        true
    }

    /// The storage, routed by the table's category
    /// (`memory::routing::body_alloc`), with the bytes really granted
    /// reported alongside the pointer.
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
    fn alloc(&self, owner: *const RcHeader, bytes: usize) -> (*mut u8, usize) {
        unsafe {
            crate::memory::routing::body_alloc(
                std::ptr::null_mut(),
                Self::category_of(owner),
                bytes,
            )
        }
    }

    /// Sever every live entry: null its element, drop its key, and
    /// collect both into `displaced` — **without releasing them**. The
    /// array's half of the rc-walk drain's "sever and free"
    /// (`rfc/model/gc/rc-walk.md`, Phase 4); the caller owes one drop per
    /// collected entry.
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
    /// reading it would act on.
    pub(crate) fn sever_entries(&mut self, displaced: &mut Vec<*mut RcHeader>) {
        for i in 0..self.used() {
            let e = self.entry(i);
            if e.is_hole() {
                continue;
            }
            let value = e.value();
            let key = e.string_key();
            // The table's own store rather than the barrier's: a barrier
            // write publishes a whole Box, zeroed reserved bytes and all,
            // which would set this entry's chain link to 0 — a legal entry
            // index rather than an end of chain (`array/entry.rs`).
            let at = self.entry_ptr(i);
            unsafe {
                Entry::store_element(at, Value::null());
                Entry::make_hole(at);
            }
            if value.is_refcounted() {
                displaced.push(value.entity_ptr());
            }
            if !key.is_null() {
                displaced.push(key as *mut RcHeader);
            }
        }
        self.live = 0;
        self.holes = self.used();
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
    /// The category comes from the owning entity's header
    /// ([`Table::category_of`]), so a promotion that moves this storage
    /// needs no second field kept in step with it.
    fn free_storage(&self, owner: *const RcHeader, p: *mut u8, capacity: usize) {
        unsafe { crate::memory::routing::body_free(Self::category_of(owner), p, capacity) };
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
    pub fn make_ref(
        &mut self,
        owner: *const RcHeader,
        key: Key,
    ) -> *mut crate::reference::LLReference {
        if self.storage().is_null() {
            return std::ptr::null_mut();
        }
        let sh = self.slot_hash(key);
        let mut i = unsafe { *self.slots().add(sh as usize & self.mask) };
        while i != NONE {
            if Self::entry_matches(self.entry(i as usize), key) {
                let current = self.entry(i as usize).value();
                if current.tag() == crate::value::Tag::Reference {
                    return current.entity_ptr() as *mut crate::reference::LLReference;
                }
                let category = Self::category_of(owner);
                let boxed =
                    unsafe { crate::reference::ll_reference_new(std::ptr::null_mut(), category) };
                if boxed.is_null() {
                    return std::ptr::null_mut();
                }
                unsafe { (*boxed).value = current };
                unsafe {
                    Entry::store_element(
                        self.entry_ptr(i as usize),
                        Value::entity(
                            crate::value::Tag::Reference,
                            boxed as *mut crate::refcount::RcHeader,
                        ),
                    )
                };
                return boxed;
            }
            i = self.entry(i as usize).link();
        }
        std::ptr::null_mut()
    }

    /// The one draw of the per-table salt: the storage address run
    /// through `hash_bytes`, and the flag saying the table has one.
    /// Idempotent by the flag, so whichever rung fires first draws and
    /// the other inherits. Never zero — `hash_bytes` remaps zero away —
    /// so a drawn salt cannot masquerade as the unsalted state.
    ///
    /// `hash_bytes` rather than an avalanche of `address ^ seed`: the
    /// avalanche is a bijection, so one recovered salt would hand back
    /// `address ^ seed` exactly, and one leaked address the seed itself.
    /// Behind rapidhash the seed sits where every cached string hash
    /// already puts it. What this does not buy: storage addresses
    /// recycle across arena resets, so a salt can repeat, and under
    /// `hash-folding` the seed is a build constant — the durable key for
    /// an escalated table is the long-key slot's per-process never-folded
    /// key (`rfc/model/strings.md`), which `strong_hash`'s own doc names
    /// as the unfilled slot this stands in for.
    ///
    /// The triggers fire during an insert's chain walk, which needs
    /// entries, so the storage is never null here — asserted, because a
    /// null address would draw the same salt for every such table.
    fn draw_salt(&mut self) {
        if self.flags & TABLE_RESEEDED != 0 {
            return;
        }
        debug_assert!(
            !self.storage().is_null(),
            "a draw before the first entry would salt every table alike"
        );
        self.flags |= TABLE_RESEEDED;
        self.salt = crate::hash::hash_bytes(&(self.storage() as u64).to_le_bytes());
    }

    /// Escalate to the keyed byte hash, once and one way. The response
    /// to *equal full hashes*: redrawing a salt cannot separate keys whose
    /// hashes agree, and doing so on that trigger is what made Perl's
    /// REHASH exploitable (CVE-2013-1667).
    ///
    /// Firing from an unsalted table draws the salt on the way, because
    /// the keyed hash's key *is* the salt: left at zero it would be a
    /// key every attacker knows, and the design's residual assumption —
    /// a new colliding set costs a break of a keyed PRF — needs the key
    /// unpredictable. That is a draw, not the redraw the Perl defect is
    /// about: a salt already drawn is left exactly as it was.
    fn escalate(&mut self) {
        if self.flags & TABLE_STRONG != 0 {
            return;
        }
        self.draw_salt();
        self.flags |= TABLE_STRONG;
        if !self.storage().is_null() {
            self.rebuild_index();
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
    /// **The two rungs defend different key kinds**, which is why both
    /// exist and why the order is this one. The draw moves integer keys,
    /// whose slot becomes `mix_int(k, salt)`, and cannot move string
    /// keys at all: below `strong` a string's slot *is* its cached hash,
    /// which no salt enters. Escalation answers the string side — it
    /// rehashes string keys with a keyed function over their bytes — and
    /// moves integer keys only in the one case where it also had to
    /// draw, firing on a still-unsalted table ([`Table::escalate`]). So
    /// a pure string flood spends one useless rebuild before the rung
    /// that answers it, and a pure integer flood is answered by the
    /// first rung or not at all.
    ///
    /// The salt is drawn at most once per table ([`Table::draw_salt`]),
    /// so there is no orbit to learn and no redraw to aim. A COW copy
    /// inherits the drawn salt rather than drawing again
    /// ([`Table::adopt_flood_state`]): its second long chain escalates.
    fn reseed(&mut self) {
        if self.flags & TABLE_STRONG != 0 {
            return;
        }
        if self.flags & TABLE_RESEEDED != 0 {
            self.escalate();
            return;
        }
        self.draw_salt();
        if !self.storage().is_null() {
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
    pub fn for_each_value(&self, mut f: impl FnMut(Value)) {
        for k in 0..self.used() {
            let e = self.entry(k);
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
    pub fn for_each_value_mut(&mut self, mut f: impl FnMut(Value) -> Value) {
        for k in 0..self.used() {
            if self.entry(k).is_hole() {
                continue;
            }
            let replaced = f(self.entry(k).value());
            unsafe { Entry::store_element(self.entry_ptr(k), replaced) };
        }
    }

    /// Every string key that is a live entity, in insertion order. Keys
    /// are counted children too: a table holds a reference to each.
    pub fn for_each_string_key(&self, mut f: impl FnMut(*mut LLString)) {
        for k in 0..self.used() {
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
    pub fn dispose(&mut self, owner: *const RcHeader) {
        let p = self.storage();
        let capacity = self.storage_capacity;
        self.set_storage(std::ptr::null_mut());
        self.storage_capacity = 0;
        self.set_nslots(0);
        self.mask = 0;
        self.cap = 0;
        self.set_used(0);
        self.live = 0;
        self.holes = 0;
        self.free_storage(owner, p, capacity);
    }

    /// Iterate live entries in insertion order. This reads no index at
    /// all, which is why the choice of index layer does not affect
    /// `foreach`.
    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        (0..self.used())
            .map(|i| self.entry(i))
            .filter(|e| !e.is_hole())
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
    /// A table **inside its array**, which is the only place a table
    /// lives: the memory its storage comes from is the owning entity's
    /// header to say, so a headerless table has nothing to answer with
    /// (`Table::category_of`, `dev/DECISIONS.md` 2026-08-07). Derefs to
    /// the table, so a test reads as if it held one.
    struct Owned(*mut crate::array::entity::LLArray);

    /// The operations that need the owner, wrapped so that a test writes
    /// them as if the table found its own header. It cannot: a reference
    /// to the body carries provenance over the body, so the entity
    /// pointer has to arrive from outside ([`Table::category_of`]).
    impl Owned {
        fn owner(&self) -> *const RcHeader {
            self.0 as *const RcHeader
        }

        fn insert(&mut self, key: Key, value: Value) -> Option<(bool, Option<Value>)> {
            let owner = self.owner();
            unsafe { (*self.0).table.insert(owner, key, value) }
        }

        fn make_ref(&mut self, key: Key) -> *mut crate::reference::LLReference {
            let owner = self.owner();
            unsafe { (*self.0).table.make_ref(owner, key) }
        }

        fn dispose(&mut self) {
            let owner = self.owner();
            unsafe { (*self.0).table.dispose(owner) };
        }
    }

    impl std::ops::Deref for Owned {
        type Target = Table;
        fn deref(&self) -> &Table {
            unsafe { &(*self.0).table }
        }
    }
    impl std::ops::DerefMut for Owned {
        fn deref_mut(&mut self) -> &mut Table {
            unsafe { &mut (*self.0).table }
        }
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            unsafe {
                (*self.0).table.dispose(self.0 as *const RcHeader);
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

    #[test]
    fn an_empty_table_finds_nothing_and_does_not_allocate() {
        let m = t();
        assert!(m.is_empty());
        assert!(m.get(Key::Int(0)).is_none());
        assert!(!m.contains(Key::Int(1)));
        assert_eq!(m.used(), 0);
        let _ = ctx();
    }

    /// Every operation that moves entries has to be visible to a walker
    /// that is reading them, and the version is how: odd while the move
    /// runs, changed afterwards. A walker validates its reading of
    /// `storage`, `nslots` and `used` against two readings of this
    /// (`PLAN.md`, item 12).
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

        m.compact();
        let after_compaction = m.version();
        assert!(
            after_compaction > after_growth,
            "compaction moved entries silently, and it does not change `storage`"
        );
        assert_eq!(after_compaction % 2, 0, "the window was left open");
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
        for i in 0..64i64 {
            m.insert(Key::Int(i), Value::int(i));
        }
        for i in 0..64i64 {
            assert_eq!(m.remove(Key::Int(i)).unwrap().0.as_int(), i);
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
            let _ = m.remove(Key::Int(i));
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
        unsafe { crate::string::ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, bytes) }
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
            let _ = m.remove(Key::Int(i));
        }
        m.compact();

        assert_eq!(
            m.make_ref(Key::Int(150)),
            r,
            "compaction moved the element, not the box"
        );
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
        // The old shape of this wrote a whole `Value` into the slot by
        // hand to stand in for a barrier. The element field is private
        // now and that write does not compile, so what is left to check is
        // the store the table really performs — and the link it has to
        // carry through, which the earlier shape could not see at all.
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
                (*carried.0).table.dispose(carried.0 as *const RcHeader);
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
        let owner = a as *const RcHeader;
        let m = unsafe { &mut (*a).table };
        for i in 0..1100i64 {
            m.insert(owner, Key::Int(i), Value::int(i));
        }
        assert!(
            m.storage_capacity > BLOCK_PAYLOAD,
            "the table never grew past one block, so this proves nothing"
        );
        for i in 0..1100i64 {
            assert_eq!(m.get(Key::Int(i)).unwrap().as_int(), i);
        }

        m.dispose(owner);
        set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    /// A refused carry leaves the storage where it is, and the array is a
    /// heap array from that moment on: promotion clears the category bits
    /// whether the carry succeeded or not. What the table must do is
    /// follow — route its next storage to the heap — and it does that by
    /// reading the header rather than a field of its own, which is why
    /// the four rewrites `carry_out_of` used to make are gone
    /// (`dev/DECISIONS.md`, 2026-08-07).
    ///
    /// The danger the rewrites guarded is unchanged: a table still
    /// answering `RequestArena` would take its next storage from whatever
    /// arena is mounted then, and that arena's reset would return the
    /// chunk to the pool with a heap array still pointing at it — a
    /// use-after-free rather than the leak a refusal looks like.
    #[test]
    fn a_refused_carry_leaves_the_next_storage_to_the_header() {
        use crate::memory::arena::Arena;
        use crate::memory::block_pool::{BLOCK_KIND_BUFFER, BLOCK_MASK, BLOCK_PAYLOAD, FORCE_OOM};
        use crate::memory::buffer_arena::buffer_free_longlived_payload;
        use crate::memory::context::set_current_context;
        use std::sync::atomic::Ordering;
        let _g = crate::memory::block_pool::test_guard();

        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut context = LLContext { arena: arena_ptr };
        let context_ptr: *mut LLContext = &mut context;
        set_current_context(context_ptr);

        let a = unsafe { crate::array::entity::ll_array_new(MemoryCategory::RequestArena) };
        let owner = a as *const RcHeader;
        let m = unsafe { &mut (*a).table };
        for i in 0..8i64 {
            m.insert(owner, Key::Int(i), Value::int(i));
        }
        assert!(
            m.storage_capacity <= BLOCK_PAYLOAD,
            "an in-block storage is the only one that can be refused"
        );

        FORCE_OOM.store(true, Ordering::Relaxed);
        let carried = unsafe { m.carry_out_of(owner, arena_ptr) };
        FORCE_OOM.store(false, Ordering::Relaxed);
        assert!(!carried, "the copy was meant to be refused and was not");
        assert_eq!(
            Table::category_of(owner),
            MemoryCategory::RequestArena,
            "the carry decided a category of its own instead of leaving it to the header"
        );

        // What promotion does to a survivor a moment later, and the whole
        // of what the table needs from it: clear the category bits, which
        // leaves 00 — the GC heap (`promote.rs`).
        unsafe { (*a).rc.flags &= !crate::refcount::MEMORY_CATEGORY_MASK };

        // The storage itself stays in the arena block, which promotion
        // stamps retained a moment later; what must have moved is where
        // the *next* one comes from.
        let (fresh, granted) = m.alloc(owner, 64);
        assert!(!fresh.is_null());
        let kind = unsafe { *(((fresh as usize) & !BLOCK_MASK) as *const u32) };
        assert_eq!(
            kind, BLOCK_KIND_BUFFER,
            "the table still allocates from the arena it was carried out of"
        );
        unsafe { buffer_free_longlived_payload(fresh, granted) };

        set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }
}
