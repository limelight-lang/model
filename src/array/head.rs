//! The words a concurrent walker is allowed to read, and the bracket
//! that makes reading them coherent.
//!
//! An array has more than one storage representation
//! (`rfc/model/arrays.md`), and the migration to the next one replaces
//! the representation under a walker that is mid-stride. A version
//! counter kept inside a representation dies with the bytes it protects,
//! so the counter, the chunk, the two counts and the tag live here, and
//! the migration is one more mover inside the same window.
//!
//! **This struct is a field of the entity, `LLArray { rc, head, storage
//! }`, rather than a prefix inside either representation.** A mutating
//! table or vector operation is reached through `&mut (*a).storage`, and
//! a `&mut` asserts uniqueness over its whole range whatever the fields
//! inside it are, so a head inside that borrow is read by the walker as
//! undefined behaviour rather than as a race the atomics settle
//! (`dev/POSTMORTEM.md`, "an atomic field does not survive a `&mut` over
//! the struct"). Outside it the two references are disjoint: every
//! caller derives `&StorageHead` and `&mut Table` field by field from the
//! one `*mut LLArray`, and no `&mut` in the crate spans this struct
//! ([`crate::array::entity::as_table_mut`]).
//!
//! Three rules hold it together:
//!
//! - **Every field is atomic.** A walker reads all of them, and each byte
//!   it reads is written by one store of the same width. A field only the
//!   mutator touches belongs in the representation's own tail.
//! - **The head is reached shared.** A `&mut StorageHead` would be the
//!   same defect one level down, so the mutating methods here take
//!   `&self` and write through the atomics.
//! - **The tag is loaded inside the bracket** and branched on only after
//!   it validates, so a stale tag cannot select a stride.
//!
//! And one rule for whoever writes the chunk: **`used` never falls while
//! `storage` stays the same.** A walker holds the pair from one accepted
//! reading and strides `0..used` in that chunk, while an insert writes a
//! fresh entry's key word plainly, no reader being able to reach an index
//! above the published count. A count that fell in place would put those
//! plain writes under the walker, where a half-written key word at or
//! above `KEY_SENTINEL_LIMIT` is a phantom in-edge: the one direction
//! that frees a live entity. So every operation that lowers the count
//! publishes a different chunk with it, `Table::move_entries` a fresh
//! one and both `dispose` bodies a null one.
//!
//! `Vector::sever_entries` is the one exemption, and it does not
//! generalise: it empties a component the drain has already confirmed
//! garbage, so the only writer that can follow is the teardown, whose
//! elements go out as atomic stores (`vector::store_element`) rather than
//! as the plain key word the rule is about. A representation whose
//! elements are written plainly, or one that can be inserted into after a
//! sever, may not copy this.
//!
//! **The window covers a release as well as a move.** `dispose` writes
//! the same words growth does, so a reader taking them one at a time
//! could pair a live chunk with the empty counts.

use std::sync::atomic::{AtomicPtr, AtomicU8, AtomicUsize, Ordering, fence};

/// How many times a trace re-reads a head whose elements keep moving
/// before it gives the array up. Small on purpose: growth, compaction
/// and migration are all rare, so a second disagreement means the reader
/// is unlucky rather than starved, and giving up leaks one collection's
/// worth rather than freeing anything early.
const COHERENT_READ_ATTEMPTS: usize = 4;

/// Which storage representation the head describes, numbered as
/// `rfc/model/arrays.md` numbers the strategies.
///
/// `Typed` has no producer in this crate: the compiler that proves
/// monomorphism does not exist yet. The number is reserved rather than
/// reused, so the tag and the design agree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum StorageTag {
    Typed = 1,
    Vector = 2,
    Hash = 3,
}

/// A reading of the head that a trace may act on: the four words below
/// taken between two equal even versions, so they describe one moment.
///
/// The version they agreed on is **not** kept beside them. It was, for
/// `rc-walk`'s re-check, which re-read a recorded cell and asked whether
/// the chunk was still the array's; that phase is gone, and S38.0's
/// reader answers no version either (`PLAN.md`).
pub(crate) struct CoherentView {
    pub(crate) tag: StorageTag,
    /// Null when the representation has never allocated.
    pub(crate) storage: *mut u8,
    /// Index slots ahead of the elements. Zero in every representation
    /// that has no index.
    pub(crate) nslots: usize,
    /// Elements the walker may stride, holes included where the
    /// representation has them.
    pub(crate) used: usize,
}

/// The head itself. Laid out `repr(C)` and held by the entity rather
/// than by a representation, so its address is the same under every tag
/// and no borrow of a representation covers it.
#[repr(C)]
pub struct StorageHead {
    /// Bumped twice by every operation that moves elements — growth,
    /// compaction, and the migration between representations — odd while
    /// the move is in progress. A trace reads it, then the four words
    /// below, then reads it again, and starts over unless both readings
    /// are the same even number. A stale-but-coherent view is a missed
    /// edge, and a missed edge only pins its target as a root; an
    /// incoherent one is an edge that never existed, which would take a
    /// live object's count down.
    ///
    /// One mover skips the bracket: `entity::carry_storage_out_of`
    /// copies an arena array's elements into a fresh chunk and publishes
    /// the chunk bare. It may, because a trace descends into GC-heap
    /// entities alone and that array still reads `RequestArena` —
    /// promotion rewrites the category after the carry. An array a trace
    /// can reach may not copy this, and the category that makes it safe
    /// is asserted in debug builds only.
    version: AtomicUsize,
    /// The one allocation the representation keeps its elements in.
    storage: AtomicPtr<u8>,
    /// Where the elements begin, expressed as the number of `u32` index
    /// slots ahead of them.
    nslots: AtomicUsize,
    /// Elements written so far. Published **after** the element it
    /// counts, so a reader that saw the count first would read an
    /// element nobody had written yet.
    used: AtomicUsize,
    /// Written twice at most: by `StorageHead::empty` at construction,
    /// and by the 2 → 3 migration, which is the only other writer the
    /// design allows ([`set_tag`](Self::set_tag)). 3 is final, so no
    /// third write exists to order against.
    tag: AtomicU8,
}

impl StorageHead {
    /// A head over nothing: the first insert allocates.
    pub(crate) const fn empty(tag: StorageTag) -> Self {
        StorageHead {
            version: AtomicUsize::new(0),
            storage: AtomicPtr::new(std::ptr::null_mut()),
            nslots: AtomicUsize::new(0),
            used: AtomicUsize::new(0),
            tag: AtomicU8::new(tag as u8),
        }
    }

    #[inline]
    pub(crate) fn storage(&self) -> *mut u8 {
        self.storage.load(Ordering::Relaxed)
    }

    /// Release, so a walker that sees the fresh pointer sees the
    /// elements already written into it.
    #[inline]
    pub(crate) fn set_storage(&self, p: *mut u8) {
        self.storage.store(p, Ordering::Release);
    }

    #[inline]
    pub(crate) fn nslots(&self) -> usize {
        self.nslots.load(Ordering::Relaxed)
    }

    #[inline]
    pub(crate) fn set_nslots(&self, n: usize) {
        self.nslots.store(n, Ordering::Release);
    }

    #[inline]
    pub(crate) fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }

    #[inline]
    pub(crate) fn set_used(&self, n: usize) {
        self.used.store(n, Ordering::Release);
    }

    /// The tag as the mutator sees it. A walker never calls this: it
    /// takes the tag from a [`CoherentView`], which is the only reading
    /// the bracket has validated. The mutator may read it plainly,
    /// because it is the only writer.
    #[inline]
    pub(crate) fn tag(&self) -> StorageTag {
        decode_tag(self.tag.load(Ordering::Relaxed)).expect("the tag is written by this crate only")
    }

    /// Stamp the representation the four words above now describe.
    ///
    /// **Called inside the window and nowhere else.** The tag decides how
    /// a walker strides the chunk, so a tag published beside the old
    /// chunk, or an old tag beside the new one, names a layout the bytes
    /// never had — the one reading no later phase repairs. The migration
    /// is the only caller (`array::entity::migrate_to_hash`), and it
    /// writes 3, which is final.
    #[inline]
    pub(crate) fn set_tag(&self, tag: StorageTag) {
        debug_assert!(
            self.version.load(Ordering::Relaxed) % 2 != 0,
            "the tag changes inside an open window"
        );
        self.tag.store(tag as u8, Ordering::Release);
    }

    /// Open a window in which elements move. The version goes odd, and a
    /// walker that sees an odd reading — or two different readings around
    /// its own — starts over.
    ///
    /// The odd value is stored plainly and the fence comes **after** it,
    /// because what must stay on this side of the fence is everything the
    /// move writes next. A release store orders the opposite side, what
    /// precedes it, and leaves the moves free to become visible before
    /// the odd version — the reading a walker would then accept as
    /// coherent. This is `ck_sequence_write_begin`'s shape and the reason
    /// for it (`dev/RESEARCH.md`, Concurrency Kit).
    #[inline]
    pub(crate) fn begin_move(&self) {
        let v = self.version.load(Ordering::Relaxed);
        self.version.store(v + 1, Ordering::Relaxed);
        fence(Ordering::Release);
    }

    /// Close it. Even again, and everything the move wrote is published
    /// before a walker can accept the reading.
    ///
    /// A release store is the right instrument here, and the asymmetry
    /// with [`begin_move`](Self::begin_move) is deliberate: the writes to
    /// order are the ones that precede this call.
    #[inline]
    pub(crate) fn end_move(&self) {
        let v = self.version.load(Ordering::Relaxed);
        self.version.store(v + 1, Ordering::Release);
    }

    /// The version word on its own, for a test that asks whether a move
    /// happened. An odd answer is a move in progress.
    #[cfg(test)]
    #[inline]
    pub(crate) fn version(&self) -> usize {
        self.version.load(Ordering::Acquire)
    }

    /// One reading of every word a walker may act on, or `None` when the
    /// mutator kept moving elements.
    ///
    /// The words are written independently, so a walker that read them
    /// one by one could stride a fresh count over a stale chunk. Growth
    /// would be caught by comparing the chunk address before and after;
    /// compaction would not, because it slides live elements down inside
    /// the same chunk, and a migration would not either, because it
    /// changes what the bytes mean rather than where they are. Hence the
    /// version.
    ///
    /// **`None` is safe and leaks rather than frees early.** An entity a
    /// trace does not enumerate becomes a root source — its out-edges
    /// land in `RC` and never in `IN` — so its children read as
    /// externally referenced and survive one more collection
    /// (`rfc/model/gc/rc-cycle.md`, the `RC − IN` root identity). That is
    /// what makes a bounded retry the right answer rather than an
    /// unbounded one.
    ///
    /// # Safety
    /// `h` addresses a live head. Under a concurrent mutator every word
    /// this reads is atomic; nothing outside this struct may be read
    /// here.
    pub(crate) unsafe fn coherent(h: *const StorageHead) -> Option<CoherentView> {
        // A shared reference over the whole head, which is the exempted
        // case: the head is a struct of atomics and no `&mut` in the
        // crate spans it, the mutator reaching it shared as well. A raw
        // pointer arrives rather than a reference because the caller has
        // an entity address and nothing else.
        let head = unsafe { &*h };
        for _ in 0..COHERENT_READ_ATTEMPTS {
            let before = head.version.load(Ordering::Acquire);
            if before % 2 != 0 {
                continue;
            }

            // All four, unconditionally and before any branch on the
            // tag. A walker that read the tag first and then chose what
            // to read would have loaded at one representation's offsets
            // on the strength of the other's tag.
            let storage = head.storage.load(Ordering::Relaxed);
            let nslots = head.nslots.load(Ordering::Relaxed);
            let used = head.used.load(Ordering::Relaxed);
            let tag = head.tag.load(Ordering::Relaxed);
            // The fence, not the load, is what keeps the readings above
            // from being taken after the closing check: an acquire *load*
            // orders what follows it, so the words it is meant to
            // validate could be read past it and the check would validate
            // nothing. `ck_sequence_read_retry` fences and then loads
            // plainly, for this reason (`dev/RESEARCH.md`).
            fence(Ordering::Acquire);
            if head.version.load(Ordering::Relaxed) != before {
                continue;
            }

            // An unknown tag is this crate writing one it does not
            // define. Giving the array up is the same safe direction the
            // retry bound takes, and it costs one collection rather than
            // a stride over bytes with no agreed meaning.
            let Some(tag) = decode_tag(tag) else {
                debug_assert!(false, "a storage head carries a tag nothing writes");
                return None;
            };

            return Some(CoherentView {
                tag,
                storage,
                nslots,
                used,
            });
        }

        None
    }
}

/// The byte back as a tag, or `None` for a value nothing in this crate
/// stores. A free function rather than `TryFrom`, because both callers
/// are inside this module and one of them is on the walk.
#[inline]
fn decode_tag(byte: u8) -> Option<StorageTag> {
    match byte {
        1 => Some(StorageTag::Typed),
        2 => Some(StorageTag::Vector),
        3 => Some(StorageTag::Hash),
        _ => None,
    }
}
