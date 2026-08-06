//! The rc-walk collector side: one collection epoch as an explicit
//! state machine — Phase 1 WALK, Phase 2 DIFF/MARK, Phase 3
//! CONDEMN/FILTER of `rfc/model/gc/rc-walk.md`. Phase 4 lives on the
//! mutator (`crate::epoch` + `walk::drain_confirmed`); this side's only
//! writes to shared memory are the epoch stamps and the condemnation
//! bytes.
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

use std::sync::atomic::{AtomicU32, Ordering};

use crate::epoch as protocol;
use crate::memory::deferred_free;
use crate::memory::heap::{EntityBlockSnapshot, snapshot_entity_blocks};
#[cfg(test)]
use crate::refcount::EntityKind;
use crate::refcount::{
    ENTITY_KIND_MASK, ENTITY_KIND_SHIFT, EPOCH_BYTE_MASK, EPOCH_BYTE_SHIFT, MEMORY_CATEGORY_MASK,
    MemoryCategory, RcHeader, collector_load_header, collector_stamp_epoch,
};
use crate::walk::garbage_components;

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
    /// Address of the 8-byte cell the child pointer was read from (the
    /// pointer slot, or a Box's payload word).
    field: usize,
    /// The raw word read at walk time.
    raw: u64,
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
            unsafe {
                crate::walk::trace_cells::<crate::walk::RelaxedCells>(entity, kind, |cell| {
                    match census_row(blocks, first_slot, slot_rows, cell.child as usize) {
                        Some(dst) => edges.push(Edge {
                            src: i as u32,
                            dst,
                            field: cell.addr,
                            raw: cell.raw,
                        }),
                        None => *dropped += 1,
                    }
                })
            };
        }
    }

    /// Phase 2 — DIFF and MARK, in private memory (`garbage_components`).
    pub fn judge(&mut self) {
        let pairs: Vec<(u32, u32)> = self.edges.iter().map(|e| (e.src, e.dst)).collect();
        self.candidates = garbage_components(self.entities.len(), &self.rows, &pairs);
        self.stats.candidates = self.candidates.len();
    }

    /// Phase 3, first half — condemn every candidate component (a mark
    /// in this epoch's private tables and nothing else: the shared
    /// condemned byte is retired, eager-death amendment 2026-07-27),
    /// then request the handshake whose ack makes the mutator's prior
    /// writes visible to the re-check.
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
            // Recorded in-edge cells re-read against their walk values.
            if clean {
                for &k in &component_edges[id] {
                    let edge = &self.edges[k as usize];
                    let now = unsafe {
                        (*(edge.field as *const std::sync::atomic::AtomicU64))
                            .load(Ordering::Relaxed)
                    };
                    if now != edge.raw {
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
/// build pass over the walked set (`dev/BENCHMARKS.md`, 2026-07-28).
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
mod tests {
    use super::*;
    use crate::class::ClassBuilder;
    use crate::epoch::checkpoint;
    use crate::memory::arena::Arena;
    use crate::memory::context::LLContext;
    use crate::memory::heap::for_each_entity_slot;
    use crate::object::{Object, new_constructed};
    use crate::refcount::{ll_release, ll_retain};
    use crate::value::{Tag, Value};
    use std::sync::atomic::AtomicUsize;

    /// The concurrent collector reads cells relaxed-atomically and so
    /// keeps its own copy of the slot stride — which means a template's
    /// values, counted by its shape rather than by its class, have to be
    /// found here too. A class-driven walk finds nothing on a template
    /// (the class has no runs), and an under-counted in-degree makes the
    /// target look rooted: a ring through a template would never be
    /// collected.
    #[test]
    fn the_concurrent_tracer_sees_a_templates_values() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("InterpolatedString").template().build();
        // The shape lives in immortal memory, where a compiler-emitted
        // one lives: this walk recovers the shape word from a relaxed
        // integer load, so the address it casts back has to be one that
        // was exposed as an integer to begin with — Miri says so, and it
        // is right (`dev/WORKFLOW.md`, provenance).
        let parts = [crate::intern::intern_str(""), crate::intern::intern_str("")];
        let shape =
            crate::memory::immortal::immortal_alloc(size_of::<crate::template::TemplateShape>())
                as *mut crate::template::TemplateShape;
        unsafe {
            shape.write(crate::template::TemplateShape {
                value_count: 1,
                parts: parts.as_ptr(),
            })
        };

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let held = unsafe {
            crate::string::ll_string_new(&mut ctx, crate::refcount::MemoryCategory::GcHeap, b"v")
        };
        let values = [Value::entity(Tag::String, held as *mut RcHeader)];
        let t = unsafe {
            crate::template::ll_template_new(
                &mut ctx,
                cls,
                shape,
                &values,
                crate::refcount::MemoryCategory::GcHeap,
            )
        };

        let mut seen = Vec::new();
        unsafe {
            crate::walk::trace_cells::<crate::walk::RelaxedCells>(
                t as *mut RcHeader,
                EntityKind::Object as u32,
                |cell| seen.push(cell.child),
            )
        };
        assert_eq!(
            seen,
            vec![held as *mut RcHeader],
            "the template's value is invisible to the epoch's walk"
        );

        unsafe {
            if ll_release(t as *mut RcHeader) {
                crate::object::ll_entity_die(t as *mut RcHeader);
            }
            if ll_release(held as *mut RcHeader) {
                crate::object::ll_entity_die(held as *mut RcHeader);
            }
        }
        arena.reset(|_| {});
    }

    fn walked_addresses() -> Vec<usize> {
        let mut seen = Vec::new();
        unsafe { for_each_entity_slot(|e| seen.push(e as usize)) };
        seen
    }

    unsafe fn tie(a: *mut Object, offset: u32, b: *mut Object) {
        unsafe {
            Object::prop_at(a, offset).write(Value::entity(Tag::Object, b as *mut RcHeader));
        }
    }

    static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
        DESTRUCTS.fetch_add(1, Ordering::Relaxed);
    }

    /// Step one epoch to completion on this thread, playing both actors:
    /// the collector steps, with a mutator checkpoint wherever the
    /// protocol needs an ack or a drain.
    fn stepped_epoch() -> EpochStats {
        let mut e = Epoch::open();
        checkpoint(); // publish the activity bit
        e.snapshot();
        e.walk();
        e.judge();
        if e.stats.candidates > 0 {
            e.condemn();
            checkpoint(); // the Phase 3 ack
            e.recheck_and_post();
            checkpoint(); // the Phase 4 drain
        }
        let stats = e.close();
        checkpoint(); // the post-epoch flush of parked memory
        stats
    }

    /// Measurement probe, not a correctness test: collector-side cost
    /// of one epoch by live-set size and shape (`dev/BENCHMARKS.md`,
    /// 2026-07-27 session). Ignored so ordinary runs stay fast; run
    /// explicitly, release mode:
    ///
    /// ```
    /// cargo test --release --lib -- --ignored measure_epoch_cost --nocapture
    /// ```
    ///
    /// Two shapes per size: `singletons` (rows only, no edges) and
    /// `chain` (one traced edge per entity, every node externally
    /// retained so nothing is condemned). Four epochs each; the first
    /// is warm-up — read the later ones.
    #[test]
    #[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
    fn measure_epoch_cost() {
        use std::time::Instant;
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("EpochCost").prop("child", true).build();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        for &n in &[1_000usize, 10_000, 100_000] {
            for chained in [false, true] {
                let mut objects: Vec<*mut Object> = Vec::with_capacity(n);
                for i in 0..n {
                    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
                    if chained {
                        // Slot ref + our handle: rc 2, externally live,
                        // never condemned; the walk still chases the edge.
                        unsafe { ll_retain(obj as *mut RcHeader) };
                        if i > 0 {
                            unsafe { tie(objects[i - 1], 16, obj) };
                        }
                    }
                    objects.push(obj);
                }

                for round in 0..4 {
                    let t0 = Instant::now();
                    let mut e = Epoch::open();
                    checkpoint();
                    let t1 = Instant::now();
                    e.snapshot();
                    let t2 = Instant::now();
                    e.walk();
                    let t3 = Instant::now();
                    e.judge();
                    let t4 = Instant::now();
                    assert_eq!(e.stats.candidates, 0, "probe set must stay live");
                    let _ = e.close();
                    checkpoint();
                    let t5 = Instant::now();
                    println!(
                        "epoch_cost n={n} shape={} round={round}: handshake={:?} \
                         snapshot={:?} walk={:?} judge={:?} close+flush={:?} total={:?}",
                        if chained { "chain" } else { "singletons" },
                        t1 - t0,
                        t2 - t1,
                        t3 - t2,
                        t4 - t3,
                        t5 - t4,
                        t5 - t0,
                    );
                }

                // Teardown without a 100k-deep drop cascade: null every
                // slot first (raw writes — the counts the slots held are
                // settled by hand below), then every node dies childless
                // with a uniform two releases: {constructed-or-slot ref,
                // the probe's retain} for chained, one for singletons.
                if chained {
                    for &obj in &objects {
                        unsafe {
                            Object::prop_at(obj, 16).write(Value::null());
                        }
                    }
                }
                for &obj in objects.iter().rev() {
                    if chained {
                        unsafe { ll_release(obj as *mut RcHeader) };
                    }
                    if unsafe { ll_release(obj as *mut RcHeader) } {
                        unsafe { crate::object::ll_object_die(obj) };
                    }
                }
            }
        }
    }

    /// F3 made concrete: a ring created before its first epoch is
    /// stamped new there (allocate-black) and collected only by the
    /// second epoch — destructors, memory and all.
    #[test]
    fn a_garbage_ring_matures_one_epoch_and_dies_the_next() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("CollectorRing")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
        }

        let first = stepped_epoch();
        assert!(first.stamped_new >= 2, "creation epoch: allocate-black");
        assert_eq!(first.confirmed, 0);
        let seen = walked_addresses();
        assert!(seen.contains(&(a as usize)) && seen.contains(&(b as usize)));

        let second = stepped_epoch();
        assert!(second.walked >= 2, "stamped last epoch: mature now");
        assert_eq!(second.confirmed, 1, "the ring is one confirmed component");
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
        arena.reset(|_| {});
    }

    /// The epoch reaches a retained former-arena block the same way it
    /// reaches an entity block, though neither the walk nor the census
    /// can stride there: the reset's object index supplies the slot
    /// addresses, and the census resolves a child inside one by
    /// searching that index instead of dividing
    /// (`rfc/model/gc/retained-block-walk.md`). The ring is built and
    /// promoted before any epoch, so it matures in the first and dies
    /// in the second, exactly as a heap-born ring does.
    #[test]
    fn a_ring_promoted_into_a_retained_block_is_collected() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let node = ClassBuilder::new("EpochPromotedRing")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();
        let holder_cls = ClassBuilder::new("EpochPromotedHolder")
            .prop("head", true)
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let a = unsafe { new_constructed(&mut ctx, node, MemoryCategory::RequestArena) };
        let b = unsafe { new_constructed(&mut ctx, node, MemoryCategory::RequestArena) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
            assert!(crate::memory::barrier::ref_store(
                &mut arena,
                holder as *mut RcHeader,
                Object::prop_at(holder, 16),
                std::ptr::null_mut(),
                Value::entity(Tag::Object, a as *mut RcHeader),
            ));
        }

        unsafe { crate::promote::arena_reset_full(&mut arena) };
        assert_eq!(
            unsafe { (*crate::memory::block_pool::BlockHeader::of_ptr(a as *const u8)).kind },
            crate::memory::block_pool::BLOCK_KIND_RETAINED
        );
        unsafe {
            assert!(ll_release(holder as *mut RcHeader));
            crate::object::ll_object_die(holder);
        }

        let first = stepped_epoch();
        assert!(
            first.stamped_new >= 2,
            "the promoted pair is new to the collector"
        );
        let second = stepped_epoch();
        assert!(
            second.walked >= 2,
            "a retained block's occupants are walkable"
        );
        assert_eq!(
            second.confirmed, 1,
            "the promoted ring is one confirmed component"
        );
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        assert!(!walked_addresses().contains(&(a as usize)));
    }

    /// A frame-held ring is a computed root in every epoch — never a
    /// candidate, never condemned (the central identity; scenario 2).
    #[test]
    fn a_live_ring_is_never_condemned() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("CollectorLiveRing")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
            ll_retain(a as *mut RcHeader); // the frame
        }

        stepped_epoch(); // matures them
        let stats = stepped_epoch();
        assert_eq!(stats.candidates, 0, "RC − IN > 0 on a: rooted, ring marked");
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 0);

        // Genuine garbage once the frame lets go.
        assert!(!unsafe { ll_release(a as *mut RcHeader) });
        let stats = stepped_epoch();
        assert_eq!(stats.confirmed, 1);
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        arena.reset(|_| {});
    }

    /// The Phase 3 filter: a mutator touch between condemn and re-check
    /// (here a borrow, still held) acquits the whole component by
    /// snapshot difference. The acquittal is collector-private — no
    /// message, no duties — and the untouched ring is collected by the
    /// next epoch.
    #[test]
    fn a_touch_between_condemn_and_recheck_acquits() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("CollectorTouched")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
        }
        stepped_epoch(); // mature

        let mut e = Epoch::open();
        checkpoint();
        e.snapshot();
        e.walk();
        e.judge();
        assert_eq!(e.stats.candidates, 1);
        e.condemn();
        // The mutator borrows a member and still holds it at re-check
        // time: the count difference against the snapshot acquits.
        // (A touch-and-restore acquits nothing — that case is the
        // sibling test below.)
        unsafe { ll_retain(a as *mut RcHeader) };
        checkpoint(); // ack
        e.recheck_and_post();
        assert_eq!(
            e.stats.acquitted, 1,
            "the held borrow acquits by difference"
        );
        assert_eq!(e.stats.confirmed, 0);
        assert_eq!(
            crate::epoch::outstanding_verdicts(),
            0,
            "an acquittal posts nothing — it is dropped in private"
        );
        let _ = e.close();
        checkpoint(); // flush
        assert!(!unsafe { ll_release(a as *mut RcHeader) }); // borrow ends

        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            0,
            "acquitted: nothing died"
        );
        let seen = walked_addresses();
        assert!(seen.contains(&(a as usize)) && seen.contains(&(b as usize)));

        let stats = stepped_epoch();
        assert_eq!(stats.confirmed, 1, "untouched next epoch: collected");
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        arena.reset(|_| {});
    }

    /// The ABA case: a borrow taken and returned between condemn and
    /// re-check restores the snapshot exactly and the component
    /// **confirms** — correctly: the Phase 4 exact test proves every
    /// reference is in-component at drain time, so the transiently
    /// borrowed ring is garbage and is freed one epoch earlier than
    /// the old clear-on-touch filter allowed.
    #[test]
    fn a_touch_and_restore_between_condemn_and_recheck_confirms_and_frees() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("CollectorAba")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
        }
        stepped_epoch(); // mature

        let mut e = Epoch::open();
        checkpoint();
        e.snapshot();
        e.walk();
        e.judge();
        assert_eq!(e.stats.candidates, 1);
        e.condemn();
        unsafe {
            ll_retain(a as *mut RcHeader);
            assert!(!ll_release(a as *mut RcHeader)); // restored: ABA
        }
        checkpoint(); // ack
        e.recheck_and_post();
        assert_eq!(e.stats.confirmed, 1, "the restored count confirms");
        checkpoint(); // drain: exact test passes, the ring dies here
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2, "freed this epoch");
        let _ = e.close();
        checkpoint(); // flush
        arena.reset(|_| {});
    }

    /// Since the eager-death amendment a collector that condemns and
    /// dies before posting strands nothing: condemnation is private
    /// state and dies with the epoch — no stranded bytes, no owed
    /// acquittals. `Drop` releases the deferral window (a stuck
    /// activity bit would park every free forever) and the ring is
    /// collected cleanly next epoch.
    #[test]
    fn an_abandoned_epoch_strands_nothing() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("CollectorAbandoned")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
        }
        stepped_epoch(); // mature

        let mut e = Epoch::open();
        checkpoint();
        e.snapshot();
        e.walk();
        e.judge();
        assert_eq!(e.stats.candidates, 1);
        e.condemn();
        checkpoint(); // ack
        drop(e); // the collector dies before recheck_and_post

        assert_eq!(
            crate::epoch::outstanding_verdicts(),
            0,
            "nothing was posted, nothing is owed"
        );
        assert!(
            !crate::memory::deferred_free::active(),
            "the deferral window was released by the unwind"
        );
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 0, "the ring lives");

        let stats = stepped_epoch();
        assert_eq!(stats.confirmed, 1, "and is collected cleanly next epoch");
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        arena.reset(|_| {});
    }

    /// Allocate-black under a live epoch: a newcomer created after the
    /// snapshot is stamped, never judged, and its store pins the mature
    /// target as a root (scenario 4).
    #[test]
    fn a_mid_epoch_newcomer_is_skipped_and_pins_its_target() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("CollectorNewcomer")
            .prop("child", true)
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let target = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        stepped_epoch(); // target matures; its creation ref is the frame's

        let mut e = Epoch::open();
        checkpoint();
        e.snapshot();
        // Mid-epoch: allocate C and hand target's frame reference to it.
        let c = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe { tie(c, 16, target) }; // slot owns the ref the frame held
        e.walk();
        e.judge();
        // The newcomer is either past the snapshot cursor (never
        // visited) or in a reused slot (stamped and skipped) — both are
        // allocate-black; either way it contributes no row and no edge.
        assert_eq!(
            e.stats.candidates, 0,
            "target: RC 1 from an unwalked source, IN 0 — a computed root"
        );
        let _ = e.close();
        checkpoint();

        let seen = walked_addresses();
        assert!(seen.contains(&(target as usize)) && seen.contains(&(c as usize)));
        unsafe {
            assert!(ll_release(c as *mut RcHeader));
            crate::object::ll_object_die(c);
        }
        arena.reset(|_| {});
    }

    /// The A8 clause embodied: an edge into a slot reused mid-epoch maps
    /// to no walked row and is dropped — the newcomer in the recycled
    /// slot is never dragged into a component as a phantom non-root.
    #[test]
    fn an_edge_into_a_recycled_slot_is_dropped_not_recorded() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("CollectorRecycled")
            .prop("child", true)
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        // A victim freed BEFORE the epoch: its slot goes to the free
        // list and can be handed out mid-epoch, far below the cursor.
        let victim = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let victim_addr = victim as usize;
        stepped_epoch(); // holder matures
        unsafe {
            assert!(ll_release(victim as *mut RcHeader));
            crate::object::ll_object_die(victim);
        }

        let mut e = Epoch::open();
        checkpoint();
        e.snapshot();
        // Mid-epoch: the free list hands the victim's slot to a newcomer,
        // and the mature holder points at it.
        let newcomer = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        assert_eq!(
            newcomer as usize, victim_addr,
            "LIFO free list: slot reused"
        );
        unsafe { tie(holder, 16, newcomer) };
        e.walk();
        e.judge();
        assert!(e.stats.dropped_edges >= 1, "the immature edge was dropped");
        assert_eq!(e.stats.candidates, 0);
        let _ = e.close();
        checkpoint();

        let seen = walked_addresses();
        assert!(seen.contains(&(holder as usize)) && seen.contains(&(newcomer as usize)));
        unsafe {
            assert!(ll_release(holder as *mut RcHeader));
            crate::object::ll_object_die(holder);
        }
        arena.reset(|_| {});
    }

    /// A garbage word can point anywhere; one aimed at the interior of
    /// a live slot must be dropped, not snapped to that slot's row —
    /// the census division validates slot alignment, exactly as the
    /// address map it replaced did by exact-key match.
    #[test]
    fn an_edge_into_a_slot_interior_is_dropped() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("CollectorInterior")
            .prop("child", true)
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let target = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        stepped_epoch(); // both mature
        // A raw store of an interior address wearing an object tag — the
        // torn-ValueBox shape the walker must absorb by validation.
        unsafe {
            Object::prop_at(holder, 16).write(Value::entity(
                Tag::Object,
                (target as usize + 8) as *mut RcHeader,
            ));
        }

        let mut e = Epoch::open();
        checkpoint();
        e.snapshot();
        e.walk();
        e.judge();
        assert!(e.stats.dropped_edges >= 1, "the interior edge was dropped");
        assert_eq!(e.stats.candidates, 0);
        let _ = e.close();
        checkpoint();

        unsafe {
            Object::prop_at(holder, 16).write(Value::null());
            assert!(ll_release(holder as *mut RcHeader));
            crate::object::ll_object_die(holder);
            assert!(ll_release(target as *mut RcHeader));
            crate::object::ll_object_die(target);
        }
        arena.reset(|_| {});
    }

    /// DC1 forced end-to-end (`rfc/model/gc/rc-walk-danger-cases.md`) —
    /// the machine-found trace that defeats a byte-only filter: the walk
    /// reads s2's count, the mutator then inflates `IN` with self-loops
    /// stored **between the count pass and the field pass**, and the
    /// diff reads `crc 2 − in 2 = 0` — the frame reference is exactly
    /// the masked term. The sound design must catch it twice,
    /// independently: the Phase 3 count re-read, and — driven past the
    /// filter, as a broken byte-only confirm would — the Phase 4 exact
    /// test. (The kill itself, freeing s2 under the live frame, is the
    /// TLC battery's job: `MC_dc1.cfg`, 16 states.)
    #[test]
    fn dc1_a_stale_count_masked_by_self_loops_is_caught_twice() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("Dc1Mask")
            .prop("child", true)
            .prop("link", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let mk = |ctx: &mut LLContext| unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
        let (s1, s2, s3) = (mk(&mut ctx), mk(&mut ctx), mk(&mut ctx));
        unsafe {
            tie(s1, 16, s2); // the ring, f1
            tie(s2, 16, s1);
            tie(s1, 32, s3); // s1.f2 = s3
            ll_retain(s1 as *mut RcHeader); // fr1
        }
        stepped_epoch(); // everything matures

        unsafe {
            // m1: fr2 borrows s2.
            ll_retain(s2 as *mut RcHeader);
            // m2: drop fr1 — the ring is now garbage-shaped.
            assert!(!ll_release(s1 as *mut RcHeader));
            // m3: store(s2.f1, fr2) — first self-loop. Publish first,
            // then drop the displaced s1: it dies, cascading s3.
            ll_retain(s2 as *mut RcHeader);
            tie(s2, 16, s2);
            crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, s1 as *mut RcHeader);
        }
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            2,
            "s1 and s3 died ordinarily"
        );
        assert_eq!(unsafe { (*s2).rc.refcount }, 2, "fr2 + self-loop");

        let mut e = Epoch::open();
        checkpoint();
        e.snapshot();
        e.walk_rows(); // crc[s2] = 2, read here and now stale forever
        unsafe {
            // m5: the second self-loop lands between the passes.
            ll_retain(s2 as *mut RcHeader);
            tie(s2, 32, s2);
        }
        e.walk_edges(); // records BOTH self-edges: in[s2] = 2
        e.judge();
        assert_eq!(
            e.stats.candidates, 1,
            "the mask worked: {{s2}} is a candidate"
        );
        e.condemn();
        checkpoint();
        e.recheck_and_post();
        assert_eq!(e.stats.acquitted, 1, "gate 1: the count re-read sees 3 ≠ 2");
        assert_eq!(e.stats.confirmed, 0);
        let _ = e.close();
        checkpoint();
        assert!(walked_addresses().contains(&(s2 as usize)), "s2 lives");
        assert_eq!(
            unsafe { (*s2).rc.refcount },
            3,
            "fr2 + two self-loops, intact"
        );

        // Gate 2, independently: drive the same verdict PAST the filter,
        // exactly what a filterless confirm would post.
        crate::epoch::post_confirmation(vec![s2 as *mut RcHeader]);
        checkpoint();
        assert!(
            walked_addresses().contains(&(s2 as usize)),
            "exact test: 3 ≠ indeg 2"
        );
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            2,
            "no destructor ran on s2"
        );

        // fr2 lets go: rc 2 = in 2, genuine garbage now.
        assert!(!unsafe { ll_release(s2 as *mut RcHeader) });
        let stats = stepped_epoch();
        assert_eq!(stats.confirmed, 1);
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 3);
        assert!(!walked_addresses().contains(&(s2 as usize)));
        arena.reset(|_| {});
    }

    /// Free-running concurrency: the collector loops whole epochs while
    /// the mutator churns — allocating garbage rings, dropping them,
    /// checkpointing only as a side effect of its own allocations. The
    /// races this executes (collector byte stores against mutator word
    /// stores and field stores) are the design's accepted ones, now all
    /// through relaxed atomics; the assertions are the invariants that
    /// must hold whatever the interleaving: the live ring survives,
    /// every garbage destructor runs exactly once, nothing crashes.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "free-running mixed-size atomic races are the design's accepted model gap; Miri rejects them and cannot pace live threads — the stepped tests carry Miri coverage"
    )]
    fn a_free_running_mutator_survives_concurrent_epochs() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("CollectorStress")
            .prop("child", true)
            .prop("link", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let mk = |ctx: &mut LLContext| unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };

        // The canary: a frame-held ring that must survive every epoch.
        let (k1, k2) = (mk(&mut ctx), mk(&mut ctx));
        unsafe {
            tie(k1, 16, k2);
            tie(k2, 16, k1);
            ll_retain(k1 as *mut RcHeader);
        }

        let collector = std::thread::spawn(|| {
            let mut stats = Vec::new();
            for _ in 0..4 {
                stats.push(run_epoch());
            }
            stats
        });

        // The mutator: garbage rings and short-lived holders. Bounded —
        // an unthrottled loop out-produces the epochs and the deferral
        // window by orders of magnitude (measured: gigabytes of parked
        // garbage) — and after the bound it just keeps checkpointing.
        // Its own entity allocations are checkpoints too.
        let mut rings = 0usize;
        while !collector.is_finished() {
            if rings < 10_000 {
                let (a, b) = (mk(&mut ctx), mk(&mut ctx));
                unsafe {
                    tie(a, 16, b);
                    tie(b, 16, a);
                    tie(a, 32, k1); // an edge into the live ring, retained
                    ll_retain(k1 as *mut RcHeader);
                    // Both frame references gone: the ring floats, cyclic.
                }
                rings += 1;
                let holder = mk(&mut ctx);
                unsafe {
                    // A fresh holder is never condemned (allocate-black),
                    // so its release always reports the death.
                    assert!(ll_release(holder as *mut RcHeader));
                    crate::object::ll_object_die(holder);
                }
            }
            checkpoint();
            std::hint::spin_loop();
        }
        let epoch_stats = collector.join().unwrap();
        assert_eq!(epoch_stats.len(), 4);

        // Quiesce and sweep what the concurrent epochs did not reach:
        // rings born after the last snapshot need one epoch to mature
        // and one to die.
        checkpoint();
        let seen = walked_addresses();
        assert!(
            seen.contains(&(k1 as usize)) && seen.contains(&(k2 as usize)),
            "the live ring survived every interleaving"
        );
        stepped_epoch();
        stepped_epoch();
        stepped_epoch();
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed) as usize,
            2 * rings + rings, // both ring members + each holder... holders have no destructor? they do: same class
            "every garbage entity destructed exactly once"
        );
        let seen = walked_addresses();
        assert!(seen.contains(&(k1 as usize)) && seen.contains(&(k2 as usize)));

        // The canary dies only when the frame lets go — with the ring
        // references the garbage rings piled on it all dropped.
        assert_eq!(
            unsafe { (*k1).rc.refcount },
            2, // frame + k2's slot: every ring's retain was dropped
            "each collected ring released its edge into the live ring"
        );
        assert!(!unsafe { ll_release(k1 as *mut RcHeader) });
        stepped_epoch();
        stepped_epoch();
        let seen = walked_addresses();
        assert!(!seen.contains(&(k1 as usize)) && !seen.contains(&(k2 as usize)));
        arena.reset(|_| {});
    }

    /// The production shape: a real collector thread runs a full epoch
    /// while the mutator thread does nothing but reach checkpoints.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "spin-waits against a live thread; the stepped tests cover the logic under Miri"
    )]
    fn a_threaded_epoch_collects_a_mature_ring() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("CollectorThreaded")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
        }
        stepped_epoch(); // mature quietly first

        let collector = std::thread::spawn(run_epoch);
        // The mutator: checkpoints until the collector is done. The
        // entities belong to this thread; the drain runs here.
        let stats = loop {
            checkpoint();
            if collector.is_finished() {
                break collector.join().unwrap();
            }
            std::thread::yield_now();
        };
        checkpoint(); // flush parked memory

        assert_eq!(stats.confirmed, 1);
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
        arena.reset(|_| {});
    }
}
