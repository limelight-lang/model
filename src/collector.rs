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

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::epoch as protocol;
use crate::memory::deferred_free;
use crate::memory::heap::{EntityBlockSnapshot, snapshot_entity_blocks};
use crate::refcount::{
    ENTITY_KIND_MASK, ENTITY_KIND_SHIFT, EPOCH_BYTE_MASK, EPOCH_BYTE_SHIFT, EntityKind,
    MEMORY_CATEGORY_MASK, MemoryCategory, RcHeader, collector_condemn, collector_load_header,
    collector_stamp_epoch,
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
    /// Components acquitted by the Phase 3 re-check (message posted).
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
    /// Walked mature GcHeap entities, their snapshot rows, and edges.
    entities: Vec<*mut RcHeader>,
    rows: Vec<u32>,
    edges: Vec<Edge>,
    /// Candidate components as indices into `entities`.
    candidates: Vec<Vec<u32>>,
    /// Handshake ack level that must be passed before the phase that
    /// recorded it may proceed.
    acks_needed: u64,
    closed: bool,
}

impl Drop for Epoch {
    /// An epoch abandoned without [`Epoch::close`] (a panic on the
    /// collector side) still releases the deferral window — a stuck
    /// activity bit would park every free in the process forever.
    fn drop(&mut self) {
        if !self.closed {
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
            entities: Vec::new(),
            rows: Vec::new(),
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

    /// Snapshot the region registry and every entity block's cursor.
    pub fn snapshot(&mut self) {
        debug_assert!(self.acked(), "snapshot before the activity bit was published");
        self.blocks = snapshot_entity_blocks();
    }

    /// Phase 1 — WALK: per slot, the three-way classification (free /
    /// new / mature), a row and out-edges for every mature GcHeap
    /// entity. Reads are relaxed atomics, stale by design; the child
    /// test is one lookup in the walked-row map, which subsumes the
    /// occupancy, boundary and epoch-byte validation — an id that maps
    /// to no row contributes to its target's RC and never to IN.
    pub fn walk(&mut self) {
        // Pass 1: classify and collect rows.
        let mut flags_of: Vec<u32> = Vec::new();
        for b in &self.blocks {
            for s in 0..b.slots {
                let slot = (b.payload + s * b.class_size) as *mut RcHeader;
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
                self.entities.push(slot);
                self.rows.push(refcount);
                flags_of.push(flags);
            }
        }
        self.stats.walked = self.entities.len();

        // Pass 2: out-edges of every row, through the collector tracer.
        let ids: HashMap<usize, u32> = self
            .entities
            .iter()
            .enumerate()
            .map(|(i, &e)| (e as usize, i as u32))
            .collect();
        for i in 0..self.entities.len() {
            let entity = self.entities[i];
            let kind = (flags_of[i] & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT;
            trace_mature(entity, kind, |field, raw, child| {
                match ids.get(&(child as usize)) {
                    Some(&dst) => self.edges.push(Edge { src: i as u32, dst, field, raw }),
                    None => self.stats.dropped_edges += 1,
                }
            });
        }
    }

    /// Phase 2 — DIFF and MARK, in private memory (`garbage_components`).
    pub fn judge(&mut self) {
        let pairs: Vec<(u32, u32)> = self.edges.iter().map(|e| (e.src, e.dst)).collect();
        self.candidates = garbage_components(self.entities.len(), &self.rows, &pairs);
        self.stats.candidates = self.candidates.len();
    }

    /// Phase 3, first half — condemn every candidate member, then
    /// request the handshake whose ack makes the mutator's prior writes
    /// visible to the re-check.
    pub fn condemn(&mut self) {
        for component in &self.candidates {
            for &i in component {
                unsafe { collector_condemn(self.entities[i as usize]) };
            }
        }
        self.acks_needed = protocol::handshake_acks() + 1;
        protocol::request_handshake();
    }

    /// Phase 3, second half — the snapshot-comparison filter, then one
    /// message per component. **Any difference acquits the whole
    /// component**: a changed count, a moved edge, a cleared byte — and
    /// the bytes are read last, so a touch that landed after the
    /// condemnation cannot be missed by reading its byte too early
    /// (`rfc/model/gc/rc-walk.md`, Phase 3; comparison, not
    /// recomputation — canonised 2026-07-26). An acquittal is a message
    /// too: the duties are mutator work.
    pub fn recheck_and_post(&mut self) {
        debug_assert!(self.acked(), "re-check before the handshake ack");
        for component in std::mem::take(&mut self.candidates) {
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
                let members: std::collections::HashSet<u32> = component.iter().copied().collect();
                for edge in &self.edges {
                    if members.contains(&edge.dst) || members.contains(&edge.src) {
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
            }
            // Bytes last.
            if clean {
                for &i in &component {
                    let word = unsafe { collector_load_header(self.entities[i as usize]) };
                    if (word >> 32) as u32 & crate::refcount::CONDEMNED_BYTE_MASK == 0 {
                        clean = false;
                        break;
                    }
                }
            }
            let members: Vec<*mut RcHeader> =
                component.iter().map(|&i| self.entities[i as usize]).collect();
            if clean {
                self.stats.confirmed += 1;
                protocol::post_verdict(protocol::Verdict::Confirm, members);
            } else {
                self.stats.acquitted += 1;
                protocol::post_verdict(protocol::Verdict::Acquit, members);
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

/// Trace one mature entity's counted children with relaxed-atomic cell
/// reads, yielding `(cell address, raw word, child)` for each non-null
/// candidate. The mutator races these reads; a torn Box or a stale cell
/// costs a phantom or missed edge, never a wild dereference — the class
/// pointer at `+8` is safe to chase *because* the entity is mature: it
/// was published epochs ago, and every handshake since ordered that
/// store before this load.
fn trace_mature(entity: *mut RcHeader, kind: u32, mut visit: impl FnMut(usize, u64, *mut RcHeader)) {
    use std::sync::atomic::AtomicU64;
    #[inline]
    fn load_cell(addr: usize) -> u64 {
        unsafe { (*(addr as *const AtomicU64)).load(Ordering::Relaxed) }
    }

    const OBJECT: u32 = EntityKind::Object as u32;
    const LAZY: u32 = EntityKind::Lazy as u32;
    const REFERENCE: u32 = EntityKind::Reference as u32;
    match kind {
        OBJECT | LAZY => {
            let class = load_cell(entity as usize + 8) as *const crate::class::Class;
            let base = entity as usize;
            for run in unsafe { (*class).ptr_runs() } {
                for i in 0..run.count {
                    let cell = base + (run.offset + i * 8) as usize;
                    let raw = load_cell(cell);
                    if raw != 0 {
                        visit(cell, raw, raw as *mut RcHeader);
                    }
                }
            }
            for run in unsafe { (*class).box_runs() } {
                for i in 0..run.count {
                    let cell = base + (run.offset + i * 16) as usize;
                    let payload = load_cell(cell);
                    let meta = load_cell(cell + 8);
                    // Byte +9 of the Value is its flags; bit 0 is
                    // VALUE_REFCOUNTED (`value.rs` layout contract).
                    if (meta >> 8) as u8 & crate::value::VALUE_REFCOUNTED != 0 {
                        visit(cell, payload, payload as *mut RcHeader);
                    }
                }
            }
        }
        REFERENCE => {
            let cell = entity as usize + 8;
            let payload = load_cell(cell);
            let meta = load_cell(cell + 8);
            if (meta >> 8) as u8 & crate::value::VALUE_REFCOUNTED != 0 {
                visit(cell, payload, payload as *mut RcHeader);
            }
        }
        _ => {}
    }
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
    /// (here a borrow — retain then release, which also clears the
    /// bytes) acquits the whole component by snapshot difference; the
    /// acquittal message performs the duties, and the untouched ring is
    /// collected by the next epoch.
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
        // The mutator touches a member before the ack: a short borrow.
        unsafe {
            ll_retain(a as *mut RcHeader);
            assert!(!ll_release(a as *mut RcHeader));
        }
        checkpoint(); // ack
        e.recheck_and_post();
        assert_eq!(e.stats.acquitted, 1, "the touch acquits by difference");
        assert_eq!(e.stats.confirmed, 0);
        checkpoint(); // drain the acquittal (duties)
        let _ = e.close();
        checkpoint(); // flush

        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 0, "acquitted: nothing died");
        let seen = walked_addresses();
        assert!(seen.contains(&(a as usize)) && seen.contains(&(b as usize)));

        let stats = stepped_epoch();
        assert_eq!(stats.confirmed, 1, "untouched next epoch: collected");
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        arena.reset(|_| {});
    }

    /// Allocate-black under a live epoch: a newcomer created after the
    /// snapshot is stamped, never judged, and its store pins the mature
    /// target as a root (scenario 4).
    #[test]
    fn a_mid_epoch_newcomer_is_skipped_and_pins_its_target() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("CollectorNewcomer").prop("child", true).build();

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
        let cls = ClassBuilder::new("CollectorRecycled").prop("child", true).build();

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
        assert_eq!(newcomer as usize, victim_addr, "LIFO free list: slot reused");
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

    /// The production shape: a real collector thread runs a full epoch
    /// while the mutator thread does nothing but reach checkpoints.
    #[test]
    #[cfg_attr(miri, ignore = "spin-waits against a live thread; the stepped tests cover the logic under Miri")]
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
