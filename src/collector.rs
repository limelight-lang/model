//! The rc-walk collector side: one collection epoch as an explicit
//! state machine — Phase 1 WALK, Phase 2 DIFF/MARK, Phase 3
//! CONDEMN/FILTER of `rfc/model/gc/rc-walk.md`. Phase 4 lives on the
//! mutator (`crate::epoch` + `walk::drain_confirmed`); the only write
//! this side makes into an **entity** is the epoch stamp, everything
//! else it publishes going through the protocol's own statics (the
//! handshake flag, the verdict queue, the deferred-free activity bit).
//! Condemnation is collector-private since the eager-death amendment
//! (2026-07-27).
//!
//! The steps are public within the crate and callable one at a time:
//! that is what lets a test interleave mutator actions between walk,
//! condemn and re-check deterministically — the forcing harness the
//! danger cases demand (`rfc/model/gc/rc-walk-review.md`, layer 3) —
//! while [`run_epoch`] chains them with waits for the production shape:
//! a dedicated collector thread against a checkpointing mutator.
//!
//! The trigger — when an epoch runs at all — is an explicit call for
//! now. Thresholds (deferred memory, suspected components, time since
//! the last epoch) are measurements nobody has taken
//! (`rfc/model/gc/rc-walk.md`, "Open questions").

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::epoch as protocol;
use crate::journal::kinds::journal_event;
use crate::memory::deferred_free;
use crate::memory::heap::{EntityBlockSnapshot, snapshot_entity_blocks};
#[cfg(test)]
use crate::refcount::EntityKind;
use crate::refcount::{
    ENTITY_KIND_MASK, ENTITY_KIND_SHIFT, EPOCH_BYTE_MASK, EPOCH_BYTE_SHIFT, MEMORY_CATEGORY_MASK,
    MemoryCategory, RcHeader, collector_load_header, collector_stamp_epoch,
};
use crate::value::Value;
use crate::walk::{CellShape, garbage_components};

/// Epoch numbers cycle 1–255, skipping 0 (0 in the byte means
/// "never stamped"). After a wrap an entity can read as current and be
/// skipped once more — latency, not error.
static NEXT_EPOCH_NUMBER: AtomicU32 = AtomicU32::new(0);

fn next_epoch_number() -> u8 {
    (NEXT_EPOCH_NUMBER.fetch_add(1, Ordering::Relaxed) % 255) as u8 + 1
}

/// One recorded heap-internal edge: which walked row references which,
/// through which field — the field's raw word is kept so the Phase 3
/// re-check can re-read the exact cell it came from.
struct Edge {
    src: u32,
    dst: u32,
    /// Where the cell starts: the pointer slot, or a Box's payload word
    /// with the flags eight bytes above it. How far it runs is `shape`.
    field: usize,
    /// The raw word read at walk time.
    raw: u64,
    /// How wide the cell is, which decides whether there is a second
    /// word to re-check beside `field` and whether `field + 8` is even
    /// inside the entity.
    ///
    /// One byte carrying one bit, and it costs eight: the padding after
    /// it takes an edge from 24 bytes to 32, measured. Paid rather than
    /// packed because the list is transient — it lives for one epoch and
    /// is walked twice — and because the alternative is deriving the
    /// width from `src`'s kind at the re-check, which reads a header the
    /// mutator is writing.
    shape: CellShape,
}

impl Edge {
    /// Whether the cell this edge was read from still names the same
    /// child: the payload word unchanged, and for a sixteen-byte cell
    /// the flags beside it still saying that word is an entity address.
    ///
    /// **Eight bytes are not the cell**, which is why the width is carried
    /// (`dev/DECISIONS.md`, "an edge is re-checked by its shape, and its
    /// storage first"). A `Value` is published as two relaxed stores, the
    /// payload first (`array::entry::store_element`), so re-reading the
    /// payload alone confirms an edge the walk assembled out of two
    /// different values rather than acquitting it.
    ///
    /// The flags are tested rather than compared, and the reserved bytes
    /// above them are read as belonging to the container: an entry's
    /// chain link lives there (`array/entry.rs`) and an insert repoints
    /// it without touching a value, so comparing the whole word would
    /// acquit the component once per re-index. What the walk recorded
    /// about that word is one bit — the cell held a counted child — and
    /// that bit is what has to still hold.
    ///
    /// # Safety
    /// Every recorded cell stays readable for the epoch: a collector
    /// walks only inside one, and an epoch parks the frees that could
    /// take the memory back (`memory::deferred_free`).
    unsafe fn still_designates_its_child(&self) -> bool {
        let now = unsafe { (*(self.field as *const AtomicU64)).load(Ordering::Relaxed) };
        if now != self.raw {
            return false;
        }

        match self.shape {
            CellShape::Pointer => true,
            CellShape::Box => {
                let meta =
                    unsafe { (*((self.field + 8) as *const AtomicU64)).load(Ordering::Relaxed) };
                Value::refcounted_in_meta_word(meta)
            }
        }
    }
}

/// Counters a test can assert against; also the epoch's report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EpochStats {
    /// Mature GcHeap rows snapshotted.
    pub walked: usize,
    /// Entities stamped new this epoch (allocate-black skips).
    pub stamped_new: usize,
    /// Child pointers that mapped to no walked row — immature targets,
    /// non-GcHeap targets, or garbage bytes; dropped, conservative.
    pub dropped_edges: usize,
    /// Candidate components Phase 2 produced.
    pub candidates: usize,
    /// Components acquitted by the Phase 3 re-check — dropped in the
    /// collector's private tables; nothing is posted (eager-death
    /// amendment, 2026-07-27).
    pub acquitted: usize,
    /// Components confirmed and posted.
    pub confirmed: usize,
}

/// One collection epoch, stepped by its owner (the collector thread, or
/// a test forcing an interleaving).
pub(crate) struct Epoch {
    pub number: u8,
    pub stats: EpochStats,
    blocks: Vec<EntityBlockSnapshot>,
    /// The dense census: `slot_rows[first_slot[b] + s]` is the walked
    /// row of block `b`'s slot `s`, `u32::MAX` for none. Sized by the
    /// snapshot (virgin tails included), filled by pass 1 as rows are
    /// recorded, consumed by pass 2's child lookup ([`census_row`]).
    first_slot: Vec<u32>,
    slot_rows: Vec<u32>,
    /// Walked mature GcHeap entities, their snapshot rows, and edges.
    entities: Vec<*mut RcHeader>,
    rows: Vec<u32>,
    /// Flags as read with each row — the kind bits steer pass 2.
    flags: Vec<u32>,
    /// Per row, the version of the storage its cells were read out of,
    /// and `None` for a row that recorded no movable cell — a kind
    /// holding its cells in its own slot, or an array the walk gave up on
    /// ([`crate::walk::trace_cells`]). Pass 1 sizes it beside `rows` and
    /// `flags` and pass 2 assigns by index, so a row that pass 2 ever
    /// leaves early keeps a version of its own rather than the next
    /// row's; the Phase 3 re-check is the only reader.
    storage_versions: Vec<Option<usize>>,
    edges: Vec<Edge>,
    /// Candidate components as indices into `entities`. Condemnation
    /// is this list and nothing else — collector-private since the
    /// eager-death amendment (2026-07-27); the heap never learns a
    /// verdict exists.
    candidates: Vec<Vec<u32>>,
    /// Handshake ack level that must be passed before the phase that
    /// recorded it may proceed.
    acks_needed: u64,
    closed: bool,
}

impl Drop for Epoch {
    /// An epoch abandoned without [`Epoch::close`] (a panic on the
    /// collector side) owes no acquittals since the eager-death
    /// amendment — condemnation is collector-private, so there is no
    /// mutator-side state to heal — but it must still **wait for its
    /// posted confirmations to be acked** before releasing the deferral
    /// window: releasing early lets the next epoch open over an
    /// undrained queue, putting two epochs' verdicts in flight (review
    /// finding, 2026-07-27, `rfc/model/gc/rc-walk.md`). The wait may
    /// block until the owning thread's next checkpoint — the one place
    /// this design accepts a wait, on a dying collector thread. Then
    /// the window is released; a stuck activity bit would park every
    /// free in the process forever.
    fn drop(&mut self) {
        if !self.closed {
            while protocol::outstanding_verdicts() != 0 {
                std::thread::yield_now();
            }

            deferred_free::end_epoch();
        }
    }
}

// Safety: the entity pointers are opaque ids on this side — every
// dereference goes through the relaxed-atomic collector helpers, and
// Phase 4 (the only teardown) runs on the owning mutator.
unsafe impl Send for Epoch {}

impl Epoch {
    /// Open an epoch: activate deferred free and request the handshake
    /// that publishes it. **The snapshot must wait for the ack** — a
    /// mutator that has not observed the activity bit can still recycle
    /// a slot the snapshot would include (`memory/deferred_free.rs`).
    pub fn open() -> Epoch {
        let number = next_epoch_number();
        journal_event!(crate::journal::kinds::KIND_EPOCH_BEGIN, 0, number as u64, 0);
        deferred_free::begin_epoch();
        let acks_needed = protocol::handshake_acks() + 1;
        protocol::request_handshake();
        Epoch {
            number,
            stats: EpochStats::default(),
            blocks: Vec::new(),
            first_slot: Vec::new(),
            slot_rows: Vec::new(),
            entities: Vec::new(),
            rows: Vec::new(),
            flags: Vec::new(),
            storage_versions: Vec::new(),
            edges: Vec::new(),
            candidates: Vec::new(),
            acks_needed,
            closed: false,
        }
    }

    /// Has the handshake the previous step requested been acked?
    pub fn acked(&self) -> bool {
        protocol::handshake_acks() >= self.acks_needed
    }

    /// Snapshot the region registry and every entity block. No cursor
    /// is read: commissioning zeroes every slot header, so the walk
    /// scans whole blocks and virgin slots skip on the occupancy test
    /// (`heap::EntityBlockSnapshot`).
    pub fn snapshot(&mut self) {
        debug_assert!(
            self.acked(),
            "snapshot before the activity bit was published"
        );
        self.blocks = snapshot_entity_blocks();
        let mut total: u32 = 0;
        self.first_slot = self
            .blocks
            .iter()
            .map(|b| {
                let first = total;
                total = total
                    .checked_add(b.slots as u32)
                    .expect("census slot ids overflow u32");
                first
            })
            .collect();
        self.slot_rows = vec![u32::MAX; total as usize];
    }

    /// Phase 1 — WALK: per slot, the three-way classification (free /
    /// new / mature), a row and out-edges for every mature GcHeap
    /// entity. Reads are relaxed atomics, stale by design; the child
    /// test is one dense census lookup ([`census_row`]), which subsumes
    /// the occupancy, boundary and epoch-byte validation — an id that
    /// maps to no row contributes to its target's RC and never to IN.
    ///
    /// Two passes, separately steppable ([`Epoch::walk_rows`] then
    /// [`Epoch::walk_edges`]): a count can be read long before the
    /// fields it guards — that window is DC1's raw material
    /// (`rfc/model/gc/rc-walk-danger-cases.md`), and the forcing tests
    /// interleave mutator actions between the passes.
    pub fn walk(&mut self) {
        self.walk_rows();
        self.walk_edges();
    }

    /// Phase 1, pass 1: classify every snapshotted slot and record a
    /// refcount row per mature GcHeap entity.
    pub fn walk_rows(&mut self) {
        for (block_index, b) in self.blocks.iter().enumerate() {
            for s in 0..b.slots {
                // Entity block: stride. Retained block: the reset's
                // object index, because a bump-filled block has none
                // (`memory/retained.rs`).
                let slot = match &b.index {
                    Some(index) => index[s] as *mut RcHeader,
                    None => (b.payload + s * b.class_size) as *mut RcHeader,
                };

                let word = unsafe { collector_load_header(slot) };
                let refcount = word as u32;
                let flags = (word >> 32) as u32;
                if refcount == 0 {
                    continue; // free (or mid-teardown: occupancy is exact)
                }

                let stamp = ((flags & EPOCH_BYTE_MASK) >> EPOCH_BYTE_SHIFT) as u8;
                if stamp == 0 || stamp == self.number {
                    // New since the last epoch: stamp and skip
                    // (allocate-black). A racing mutator word store may
                    // bury the stamp — one more epoch of latency.
                    unsafe { collector_stamp_epoch(slot, self.number) };
                    self.stats.stamped_new += 1;
                    continue;
                }

                if flags & MEMORY_CATEGORY_MASK != MemoryCategory::GcHeap as u32 {
                    continue; // mature but unwalked: a root source
                }

                self.slot_rows[self.first_slot[block_index] as usize + s] =
                    self.entities.len() as u32;
                self.entities.push(slot);
                self.rows.push(refcount);
                self.flags.push(flags);
                self.storage_versions.push(None);
            }
        }

        self.stats.walked = self.entities.len();
    }

    /// Phase 1, pass 2: out-edges of every row, through the collector
    /// tracer; the child test is [`census_row`].
    pub fn walk_edges(&mut self) {
        for i in 0..self.entities.len() {
            let entity = self.entities[i];
            let kind = (self.flags[i] & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT;
            let blocks = &self.blocks;
            let first_slot = &self.first_slot;
            let slot_rows = &self.slot_rows;
            let edges = &mut self.edges;
            let dropped = &mut self.stats.dropped_edges;
            // The relaxed instantiation of the one tracing walk. The
            // mutator races these reads; a torn Box or a stale cell costs
            // a phantom or a missed edge, never a wild dereference —
            // every row here is mature, so the class word at `+8` was
            // published epochs ago and every handshake since ordered that
            // store before this load.
            let storage_version = unsafe {
                crate::walk::trace_cells::<crate::walk::RelaxedCells>(entity, kind, |cell| {
                    match census_row(blocks, first_slot, slot_rows, cell.child as usize) {
                        Some(dst) => edges.push(Edge {
                            src: i as u32,
                            dst,
                            field: cell.addr,
                            raw: cell.raw,
                            shape: cell.shape,
                        }),
                        None => *dropped += 1,
                    }
                })
            };

            self.storage_versions[i] = storage_version;
        }
    }

    /// Whether the cells recorded for `row` are still where the walk
    /// read them.
    ///
    /// The recorded address cannot answer it. A chunk an array leaves is
    /// **parked** rather than recycled — the epoch defers every
    /// buffer-chunk free (`memory::deferred_free`) — so a re-read there
    /// compares a frozen copy of the walk value against the walk value and
    /// answers "unchanged" for a cell the array no longer has. The version
    /// the walk validated its reading against does answer, and any change
    /// acquits: the direction
    /// [`crate::array::head::StorageHead::coherent`] takes when it gives
    /// up, one epoch of leak and nothing freed early. What the check buys,
    /// what it costs and why the handshake alone is not enough are
    /// `dev/DECISIONS.md`, "an edge is re-checked by its shape, and its
    /// storage first".
    ///
    /// A source that died in between needs no case of its own, and no
    /// source outside the component reaches here: the mark walk propagates
    /// forward, so an edge out of a marked row has a marked target and
    /// neither end can be a candidate (`walk::garbage_components`).
    ///
    /// # Safety
    /// `row` was walked this epoch, so its entity address is one the
    /// snapshot found and the epoch keeps readable.
    unsafe fn row_still_has_its_cells(&self, row: u32) -> bool {
        let Some(walked) = self.storage_versions[row as usize] else {
            return true;
        };

        // Which entity holds the version is the kind's business. An
        // array keeps it in the head at a fixed offset; a class with
        // cells outside its body is asked through its descriptor,
        // because that offset on an object is the class word
        // (`dev/DECISIONS.md`, "a class with cells outside itself carries
        // one flag and one group of five").
        // The kind comes from the snapshot, never from the header: the
        // mutator writes that word as one relaxed atomic store, and a
        // plain load beside it is a data race rather than a stale value.
        let entity = self.entities[row as usize];
        let kind = self.flags[row as usize] & crate::refcount::ENTITY_KIND_MASK;
        if kind == crate::refcount::EntityKind::Array.to_flags() {
            let head = unsafe {
                crate::array::entity::storage_head(entity as *mut crate::array::entity::LLArray)
            };
            return unsafe { (*head).version() == walked };
        }

        let cls = unsafe { (*(entity as *mut crate::object::Object)).class };
        match unsafe { crate::class::Class::outside_cells(cls) } {
            Some(group) => unsafe { (group.recheck)(entity as *mut u8, cls, walked) },
            None => true,
        }
    }

    /// Phase 2 — DIFF and MARK, in private memory (`garbage_components`).
    pub fn judge(&mut self) {
        let pairs: Vec<(u32, u32)> = self.edges.iter().map(|e| (e.src, e.dst)).collect();
        self.candidates = garbage_components(self.entities.len(), &self.rows, &pairs);
        self.stats.candidates = self.candidates.len();
    }

    /// Phase 3, first half — request the handshake whose ack makes the
    /// mutator's prior writes visible to the re-check. Condemning itself
    /// writes nothing: Phase 2's candidate list is the condemnation,
    /// private to this epoch since the eager-death amendment
    /// (2026-07-27, which retired the shared condemned byte).
    pub fn condemn(&mut self) {
        self.acks_needed = protocol::handshake_acks() + 1;
        protocol::request_handshake();
    }

    /// Phase 3, second half — the snapshot-comparison filter, then one
    /// message per **confirmed** component. **Any difference acquits
    /// the whole component**: a changed count, a moved edge
    /// (`rfc/model/gc/rc-walk.md`, Phase 3; comparison, not
    /// recomputation — canonised 2026-07-26). An acquittal posts
    /// nothing: the hypothesis is dropped here, in private, and the
    /// component is re-judged next epoch (eager-death amendment).
    pub fn recheck_and_post(&mut self) {
        debug_assert!(self.acked(), "re-check before the handshake ack");
        // Distribute the recorded edges to their candidate components
        // once — one pass over `edges[]`, not one per component (the
        // per-component scan was quadratic, and a churning mutator
        // makes the edge list large).
        let mut component_of = vec![u32::MAX; self.entities.len()];
        for (id, component) in self.candidates.iter().enumerate() {
            for &i in component {
                component_of[i as usize] = id as u32;
            }
        }

        let mut component_edges: Vec<Vec<u32>> = vec![Vec::new(); self.candidates.len()];
        for (k, edge) in self.edges.iter().enumerate() {
            for member in [edge.src, edge.dst] {
                let c = component_of[member as usize];
                if c != u32::MAX {
                    component_edges[c as usize].push(k as u32);
                    break; // once per edge, even with both ends inside
                }
            }
        }

        // Components are taken one at a time, not wholesale: posting is
        // the point of no return per component, and whatever a panic
        // leaves unposted simply dies with the epoch — an unposted
        // condemnation is private state, nothing owes it anything.
        for id in 0..self.candidates.len() {
            let component = std::mem::take(&mut self.candidates[id]);
            let mut clean = true;
            // Counts against the walk snapshot.
            for &i in &component {
                let word = unsafe { collector_load_header(self.entities[i as usize]) };
                if word as u32 != self.rows[i as usize] {
                    clean = false;
                    break;
                }
            }

            // The storage each recorded cell was read out of, before
            // any cell is re-read: an address inside a chunk the array
            // has left is readable only because the epoch parks the
            // free, and the check that knows the bytes stopped being
            // the array's belongs ahead of the read that depends on it.
            // Pass 2 records edges row by row, so the sources arrive in
            // runs and one comparison covers a whole array's worth.
            if clean {
                let mut asked = u32::MAX;
                for &k in &component_edges[id] {
                    let src = self.edges[k as usize].src;
                    if src == asked {
                        continue;
                    }

                    asked = src;
                    if !unsafe { self.row_still_has_its_cells(src) } {
                        clean = false;
                        break;
                    }
                }
            }

            // Every cell this component's edges were read from, against
            // the words the walk read there. An edge is filed under the
            // component when either end is a member, so a member's edge
            // into a root is re-read like the rest — the cell belongs to
            // the source either way.
            if clean {
                for &k in &component_edges[id] {
                    if !unsafe { self.edges[k as usize].still_designates_its_child() } {
                        clean = false;
                        break;
                    }
                }
            }

            if clean {
                self.stats.confirmed += 1;
                let members: Vec<*mut RcHeader> = component
                    .iter()
                    .map(|&i| self.entities[i as usize])
                    .collect();
                protocol::post_confirmation(members);
            } else {
                self.stats.acquitted += 1;
            }
        }
    }

    /// May the epoch end? Only after every verdict message is
    /// acknowledged: that is what keeps an id naming one entity from
    /// walk to drain, and at most one epoch's verdicts in flight, ever.
    pub fn can_close(&self) -> bool {
        protocol::outstanding_verdicts() == 0
    }

    /// End the epoch: deactivate deferred free. The parked backlog is
    /// flushed by each owning thread at its next checkpoint.
    pub fn close(mut self) -> EpochStats {
        debug_assert!(self.can_close(), "epoch closed with verdicts in flight");
        self.closed = true;
        deferred_free::end_epoch();
        journal_event!(
            crate::journal::kinds::KIND_EPOCH_END,
            0,
            self.number as u64,
            self.stats.confirmed as u64
        );
        self.stats
    }
}

/// The walked-row id of a child address, or `None` — the dense census
/// lookup of Phase 1's pass 2: block by the alignment mask
/// (`heap::block_payload_of`) matched against the snapshot's sorted
/// payloads, slot by division, row by the per-slot array pass 1 filled.
/// A garbage word can point anywhere, so an address off a slot start is
/// rejected by the remainder test, and a block absent from the snapshot
/// — heap, arena, mid-commission — finds no payload match. Replaced a
/// HashMap keyed by entity address: same verdicts, no hashing and no
/// build pass over the walked set (`dev/BENCHMARKS.md`, "dense census in
/// the epoch walk: 2–3× on the walk step").
fn census_row(
    blocks: &[EntityBlockSnapshot],
    first_slot: &[u32],
    slot_rows: &[u32],
    child: usize,
) -> Option<u32> {
    let payload = crate::memory::heap::block_payload_of(child);
    if child < payload {
        return None; // inside the block's header line
    }

    let block_index = blocks.binary_search_by_key(&payload, |b| b.payload).ok()?;
    let b = &blocks[block_index];
    let slot = match &b.index {
        // Retained former-arena block: no stride to divide by, so the
        // slot is the position of an exact match in the object index.
        // A miss rejects interior and stale addresses for the same
        // reason the remainder test does below.
        Some(index) => index.binary_search(&child).ok()? as u32,
        None => {
            // In-block offsets fit u32 (blocks are 64 KiB): divide narrow.
            let offset = (child - payload) as u32;
            let class_size = b.class_size as u32;
            let slot = offset / class_size;
            if slot as usize >= b.slots || slot * class_size != offset {
                return None; // the tail fragment, or off a slot start
            }

            slot
        }
    };

    let row = slot_rows[first_slot[block_index] as usize + slot as usize];
    (row != u32::MAX).then_some(row)
}

/// One full epoch, blocking: the production shape, run on a dedicated
/// collector thread against a mutator that reaches checkpoints. Every
/// wait is a spin-yield — the epoch's pace is bounded by the mutator's
/// checkpoint cadence (finding F2's accepted limit).
// No ABI trigger yet, deliberately: when an epoch runs at all is
// rc-walk.md's open question 1 — a measurement, not a guess. Until the
// vertical slice exists, tests are the only driver.
#[cfg_attr(not(test), expect(dead_code))]
pub(crate) fn run_epoch() -> EpochStats {
    let mut epoch = Epoch::open();
    while !epoch.acked() {
        std::thread::yield_now();
    }

    epoch.snapshot();
    epoch.walk();
    epoch.judge();
    if epoch.candidates.is_empty() {
        return epoch.close();
    }

    epoch.condemn();
    while !epoch.acked() {
        std::thread::yield_now();
    }

    epoch.recheck_and_post();
    while !epoch.can_close() {
        std::thread::yield_now();
    }

    epoch.close()
}

#[cfg(test)]
mod tests;
