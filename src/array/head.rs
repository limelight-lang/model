//! The words a concurrent walker is allowed to read, and the bracket
//! that makes reading them coherent.
//!
//! An array has more than one storage representation
//! (`rfc/model/arrays.md`), and the migration from one to the next
//! replaces the representation under a walker that is mid-stride. A
//! version counter kept inside a representation could not bracket that:
//! it dies with the bytes it protects. So the counter, the chunk, the
//! two counts and the tag saying which representation is present live
//! **here**, and the migration is one more mover inside the same window
//! (`PLAN.md` S7.1, Sage 2026-08-11).
//!
//! **This struct is a field of the entity — `LLArray { rc, head,
//! storage }` — and not a prefix inside either representation.** The
//! placement is the type rule the crate has paid for twice: a mutating
//! table or vector operation is reached through `&mut (*a).storage`, and
//! a `&mut` asserts uniqueness over its whole range whatever the fields
//! inside it are, the interior-mutability exemption belonging to shared
//! references alone (`dev/POSTMORTEM.md`, 2026-08-10). A head inside the
//! representation would therefore sit inside that borrow, and the
//! walker's read of it is undefined behaviour rather than a race the
//! atomics settle. Outside it, the two references are disjoint: every
//! caller derives `&StorageHead` and `&mut Table` field by field from the
//! one `*mut LLArray`, and no `&mut` in the crate ever spans this struct
//! ([`crate::array::entity::as_table_mut`]).
//!
//! Three rules hold this together, and none is negotiable:
//!
//! - **Every field is atomic**, because a walker reads all of them and
//!   each byte it reads must be written by one store of the same width.
//!   A field only the mutator touches belongs in the representation's
//!   own tail, never in this struct.
//! - **The head is reached shared, always.** A `&mut StorageHead` would
//!   be the same defect one level down, so nothing takes one — the
//!   mutating methods here take `&self` and write through the atomics.
//! - **The tag is loaded inside the bracket and branched on only after
//!   it validates.** The read set does not depend on the tag, so a
//!   stale tag can never select a stride: it is discarded with
//!   everything else read beside it.
//!
//! And one rule that belongs to whoever writes the chunk rather than to
//! this struct: **`used` never falls while `storage` stays the same.**
//! A walker holds the pair from one accepted reading and strides
//! `0..used` in that chunk, so a count that fell would leave it striding
//! indices the mutator has taken back — and an insert writes a fresh
//! entry's key word plainly, since no reader can reach an index above the
//! published count. Lowering `used` in place would put those plain writes
//! under the walker, and a half-written key word above `KEY_HOLE` is a
//! phantom in-edge: the one direction that frees a live entity. Every
//! operation that lowers the count therefore publishes a different chunk
//! with it — `Table::move_entries` a fresh one, both `dispose` bodies a
//! null one. This was true by accident until S13.1 and is load-bearing
//! now (Critic, S13.1).
//!
//! One operation lowers the count in the chunk it keeps, and it is
//! exempt for a reason that does not generalise:
//! `Vector::sever_entries` empties a component the drain has already
//! confirmed garbage, so the only writer that could follow it is the
//! teardown, and the elements it does write go out as atomic stores
//! (`vector::store_element`) rather than the plain key word the rule is
//! about. A representation whose elements are written plainly, or one
//! that can be inserted into after a sever, may not copy this.
//!
//! **The window covers a release as well as a move.** `dispose` writes
//! the same words growth does, so a reader that took them one at a time
//! could pair a live chunk with the empty counts; the bracket is what
//! keeps the order of the stores inside either body from carrying the
//! argument (`PLAN.md` S13.2).

use std::sync::atomic::{AtomicPtr, AtomicU8, AtomicUsize, Ordering, fence};

/// How many times a walker re-reads a head whose elements keep moving
/// before it gives up on this epoch. Small on purpose: growth,
/// compaction and migration are all rare, so a second disagreement means
/// the walker is unlucky rather than starved, and giving up leaks one
/// epoch's worth rather than freeing anything early.
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

/// A reading of the head that a walker may act on: taken between two
/// equal even versions, so the four words describe one moment.
pub(crate) struct CoherentView {
    /// The even version both readings agreed on. A reader that keeps an
    /// address out of this chunk keeps this beside it: the address stays
    /// readable after a move — the epoch parks the free — so re-reading
    /// it answers about a chunk nobody writes any more, and this number
    /// is what says the chunk stopped being the array's
    /// (`collector::Edge`, `PLAN.md` S13.4).
    pub(crate) version: usize,
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
    /// the move is in progress. A walker reads it, then the four words
    /// below, then reads it again, and starts over unless both readings
    /// are the same even number. A stale-but-coherent view is a missed
    /// edge, which the epoch's later phases repair; an incoherent one is
    /// an edge that never existed, which nothing repairs.
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
    /// Written once at construction, and once more by a migration, both
    /// times inside the window. A representation that cannot be migrated
    /// away from never writes it again.
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

    /// The version a walker validates its reading against, read on its
    /// own.
    ///
    /// The Phase 3 re-check reads it this way: it holds the number a
    /// [`CoherentView`] agreed on and asks whether the elements have
    /// moved since. An odd answer is a move in progress and equals no
    /// recorded version, so a caller comparing for equality needs no
    /// case of its own for it.
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
    /// **`None` is safe and leaks rather than frees early.** An entity
    /// the walk does not enumerate becomes a root source — its out-edges
    /// land in `RC` and never in `IN` — so its children are computed
    /// roots and survive one more epoch
    /// (`rfc/model/gc/retained-block-walk.md`, the derived-roots
    /// corollary). That is what makes a bounded retry the right answer
    /// rather than an unbounded one.
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

            // All five, unconditionally and before any branch on the
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
            // retry bound takes, and it costs one epoch rather than a
            // stride over bytes with no agreed meaning.
            let Some(tag) = decode_tag(tag) else {
                debug_assert!(false, "a storage head carries a tag nothing writes");
                return None;
            };

            return Some(CoherentView {
                version: before,
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
