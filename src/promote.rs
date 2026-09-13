//! Arena death with promotion: the reset-time consumer of the escapee list
//! and its hold-counts (`rfc/model/memory/arena-reset.md`).
//!
//! The reset's own phase 1 — a different numbering from the object
//! teardown's, which `crate::object` owns — implements **retention only**
//! (`rfc/model/memory/arena-reset.md`, "Retention (dense blocks)", the
//! "Phasing" bullet). Sparse-block
//! evacuation is additive and lands later.
//!
//! The algorithm:
//!
//! 1. **Fixpoint** over the destructor log and the escapee list: from the
//!    escapees whose hold-count is still non-zero, mark the surviving
//!    subgraph, then run user destructors of dying, unescaped objects.
//!    Destructors run PHP code and may create new escapes or track new
//!    destructors, hence the loop. No holder slot is ever dereferenced, so
//!    a holder that died before now cannot dangle the reset.
//! 2. **Count.** External references are already each root's `refcount`,
//!    its `IS_ESCAPEE` hold-count kept live by the barrier and by holder
//!    teardown, so this pass adds internal edges between survivors and
//!    one compensating retain per heap entity a survivor holds: that
//!    entity's release-at-reset record assumed the holder would die, and
//!    the survivor now owes its own release at its real death. The same
//!    walk records what [`reconcile_cow_counts`] needs of it — each
//!    survivor's COW edges at that instant, and each compensating retain
//!    given to an already-promoted COW child. A **COW** survivor is
//!    counted apart, after the fixpoint, its count being a value the
//!    mutator reads and destructors being mutator code.
//! 3. **Retain blocks** carrying survivors: rewrite each survivor's
//!    category to GcHeap in place, stamp the blocks `BLOCK_KIND_RETAINED`
//!    and keep them out of the pool. The pointer-tag alternative was
//!    rejected exactly because this rewrite must be possible. A survivor
//!    that had a block to itself is the exemption: its block is a large
//!    entity's own allocation, which the arena took through
//!    `Arena::alloc_entity` and hands over here instead of retaining, out
//!    of the arena's large-run log and into nothing else, the run registry
//!    having held it since it was allocated
//!    (`rfc/model/memory/large-entities.md`).
//! 4. **Release-at-reset log**: one release per record, with real teardown
//!    dispatch for entities that die of it.
//!
//! Every traversal here — the mark, the re-trace and the count — goes
//! through the crate's one kind-dispatched tracer and never through a kind
//! test of promotion's own (`dev/DECISIONS.md`, "the reset traces through
//! one tracer"). The count pass takes `cells::trace_entity`, which hands it
//! the child; the mark and the re-trace take `cells::trace_cells`
//! underneath it, which hands them the cell as well, because a child the
//! arena cannot record has its cell emptied. The COW reconciliation walks
//! nothing: it reads the count pass's log.

use std::collections::HashMap;

use crate::journal::kinds::journal_event;
use crate::memory::arena::Arena;
use crate::memory::block_pool::{BLOCK_KIND_RETAINED, BlockHeader};
use crate::object::Object;
use crate::refcount::{
    ARENA_RESET_MARK, COW, IS_ESCAPEE, MEMORY_CATEGORY_MASK, MemoryCategory, RcHeader,
    header_refcount, ll_release, ll_retain, mutator_flags, set_header_refcount,
    update_header_flags,
};

/// Recursion guard for the reset fixpoint. Pure and non-recreating
/// destructors converge in rounds bounded by the object count; this caps
/// the pathological case (a destructor that endlessly creates new
/// destructor-bearing objects). Hitting it is an error, not a silent drop
/// of the un-settled tail — dropping it would dangle
/// (`rfc/model/memory/arena-reset.md`, "Recursion bound").
const ARENA_RESET_MAX_ROUNDS: usize = 10_000;

/// Full arena death: fixpoint, promotion by retention, deferred
/// releases, blocks home. Replaces bare `Arena::reset` wherever the
/// object model is in play.
///
/// `arena` is a raw pointer for the reason this whole function exists:
/// it runs `__destruct` bodies, and those reenter the runtime and
/// resolve this same arena to allocate, log escapes, or track more
/// destructors. An exclusive borrow held across the settling loop would
/// alias every one of those reentrant uses (audit H5). So each arena
/// operation below takes its own short-lived borrow, and **no borrow is
/// ever live across a call that can run user code** — which is also why
/// a round takes its log's segment chain out of the arena and walks it
/// afterwards, rather than acting inside a drain that still holds the
/// borrow (`Arena::take_destructors`).
///
/// **Answers how many edges it severed**: an edge whose child the arena had
/// no memory to record, whose slot was emptied so that nothing promoted
/// names an entity the reset never promoted. It counts edges rather than
/// children — two survivors naming one unrecorded child are two severances,
/// and a child severed from every holder can still be escaped afresh by a
/// later destructor round and promoted after all (`dev/DECISIONS.md`, "a survivor cell the pool
/// cannot supply severs the edge, and the reset finishes"). Zero is the
/// ordinary answer. The host learns it through `ll_arena_reset` once that
/// entry point has the pending channel `rfc/runtime/exceptions.md` puts it
/// on; until then the number is this return.
///
/// **The journal counts a second thing**, and the two figures part company
/// wherever a sever's unit is wider than one cell: a refused hash key
/// holes its whole entry, so its element loses an edge too, and every
/// emptied edge gets a `KIND_ARENA_RESET_SEVERED_EDGE` record while this
/// return counts refusals alone. A record carries the arena, the holder
/// and the child, so the refused child is not distinguished from the one
/// its entry took with it; distinguishing them needs a record kind of its
/// own, which no reader has asked for.
///
/// # Safety
/// The arena must not be reachable by running PHP code anymore (no
/// live stack); destructors invoked here may still allocate into it.
pub unsafe fn arena_reset_full(arena: *mut Arena) -> usize {
    journal_event!(
        crate::journal::kinds::KIND_ARENA_RESET_BEGIN,
        arena as u64,
        0,
        0
    );
    // The re-trace and the weak walk read survivor memory after the drain
    // that can kill a survivor, so the window opens first and closes with
    // this frame — including an unwind out of it (`memory::reset_window`).
    // Its storage is this frame's: the window boxes nothing.
    let mut window = crate::memory::reset_window::ResetWindow::closed();
    let _window = crate::memory::reset_window::open(&mut window, arena);
    // Edges severed because the arena had no memory to record the child
    // they named (`dev/DECISIONS.md`, "a survivor cell the pool cannot
    // supply severs the edge, and the reset finishes").
    let mut severed = 0usize;
    // Each COW survivor's count at the instant it was promoted, which is
    // the last instant the reset can attribute it to arena holders. What
    // happens to the count after that belongs to whoever changed it, and
    // the reconciliation keeps it as a delta.
    let mut cow_at_promotion: Vec<(*mut RcHeader, u32)> = Vec::new();
    // How many blocks this reset has taken out of circulation, for the
    // journal's third operand. The set that used to answer it also
    // answered "have I retained this one already", which the block's own
    // kind word answers now ([`retain_block`]).
    let mut retained_blocks = 0usize;
    // Blocks pinned for bytes this reset could not carry out, each held by
    // one count of the reset's own until it has finished establishing
    // occupant counts. Released after `finish_reset`, below, by walking the
    // chain they link into: the head, or `RESET_PIN_END` while no block has
    // been pinned. A block's own link word is also what says it is already
    // on the chain, so a second payload in the same block takes no second
    // pin.
    let mut pin_chain = crate::memory::heap::RESET_PIN_END;
    // Retained block → the survivors sharing it, built here rather than
    // read back off `survivors` at the end. A survivor can die inside
    // this reset, and the classification that decides whether it even
    // has a shared block is a load from its block header — sound while
    // it is alive and a read of returned memory afterwards, a survivor
    // in a block of its own having handed that block to the system at
    // its death (`dev/DECISIONS.md`, "Promotion classifies once").
    let mut by_block: HashMap<usize, Vec<usize>> = HashMap::new();
    // `survivors[..counted]` have already been counted and retained. New
    // survivors past it are the current round's delta.
    let mut counted = 0usize;
    let mut rounds = 0usize;

    // The whole reset is one settling loop — no separate "release tail".
    // Each pass settles the arena side (surviving escapees, the destructors
    // of the dying, and survivor re-traces), counts and retains what it
    // found, then runs the deferred releases. A release runs teardown that
    // can create more work (a released entity's `__destruct`, run while it
    // is still alive, may escape or allocate), so we loop until a pass
    // releases nothing. The recursion cap is the only backstop.
    loop {
        // --- Settle: escapees, dying destructors, survivor re-trace (H2).
        // Every `__destruct` runs here, with its object still fully alive;
        // nothing is freed until the survivor set below is final.
        loop {
            let mut progress = false;

            // The round's escapee records become the survivor chain's tail
            // in place, so a root never needs memory the arena might refuse.
            // A record whose count went back to zero — every holder let go —
            // is declined here and survives only if an internal edge reaches
            // it, which the descent below covers.
            let first_root = unsafe { (*arena).survivor_count() };
            let admitted = unsafe {
                (*arena).admit_escapees_as_survivors(|a| {
                    progress = true;
                    if !is_arena_entity(a) || mutator_flags(a) & IS_ESCAPEE == 0 {
                        return false;
                    }

                    mark_root(a)
                })
            };
            if admitted != 0 {
                severed += unsafe { descend_from(arena, first_root) };
            }

            // A destructor may store an arena object into an already-traced
            // survivor — arena→arena, not an escape — so after a round that
            // ran one the survivors' children are re-read (audit H2). Only
            // a pure destructor needs no re-trace, and the runtime has no
            // compile-time class to read, so every destructor body that
            // ran is taken as dirty; an entry with nothing left to run
            // counts for nothing. The bump cursor is no stand-in: the
            // object stored may already exist, reachable from the dying
            // object alone (`dev/DECISIONS.md`, "the re-trace runs after
            // every destructor round").
            let mut ran_a_destructor = false;
            unsafe { (*arena).take_destructors() }.for_each(|obj| {
                progress = true;
                // Escaped objects survive; they do not destruct.
                if unsafe { mutator_flags(obj) } & ARENA_RESET_MARK == 0 {
                    ran_a_destructor |=
                        unsafe { crate::object::run_user_destructor(obj as *mut Object) };
                }
            });

            if ran_a_destructor {
                severed += unsafe { retrace_survivors(arena) };
            }

            if !progress {
                break;
            }

            rounds += 1;
            assert!(
                rounds <= ARENA_RESET_MAX_ROUNDS,
                "arena reset did not converge"
            );
        }

        // --- Count + retain the new survivors, BEFORE any release. Their
        // compensating retains for held heap entities must land before the
        // matching release-log releases, or a heap child could hit zero and
        // free early. External refs are already the IS_ESCAPEE hold-count;
        // this adds internal arena→arena edges and those compensations.
        for surv in unsafe { (*arena).walk_survivors(counted) } {
            unsafe { count_children(surv) };
        }

        // A promotion-edge record the manager refused is an edge the
        // reconciliation will not count. The round's COW children each take
        // one `ll_retain` below, after their counts are captured, so the
        // retain stays in the child's delta: the refused edge is then
        // counted once, by the retain, and every recorded edge of the round
        // twice, which is a bounded leak and never an under-count
        // (`dev/DECISIONS.md`, "the COW count is the log's edges plus the
        // delta").
        let retain_the_rounds_children = crate::memory::reset_window::take_refused_promotion_edge();

        for surv in unsafe { (*arena).walk_survivors(counted) } {
            // Out-of-line memory comes with the survivor, before the
            // category stops describing where it lives. Asked through one
            // call, dispatched on the entity, so promotion keeps knowing
            // nothing about any layout (`rfc/model/strings.md`).
            if let ExternalCarry::Pinned(payload_block) =
                unsafe { carry_external_memory(arena, surv) }
            {
                // The bytes stay where they are and the block holding them
                // stays out of circulation with the survivors' blocks —
                // the same mechanism, for the same reason. Reset has no
                // caller left to report to, which is why there is a
                // fallback here at all.
                if payload_block != 0 {
                    let header = payload_block as *mut BlockHeader;
                    if unsafe { retain_block(header) } {
                        retained_blocks += 1;
                    }

                    // Pinned, and not merely retained: this block is held
                    // for bytes rather than for occupants, and an
                    // occupant's death says nothing about them. Without
                    // the pin, a survivor of the same block dying would
                    // hand the payload back to the pool. The bytes have a
                    // death event of their own — the owning entity's free
                    // — and it spends this pin (`retained.rs`, blocks
                    // retained for bytes; `dev/DECISIONS.md`, "a pinned
                    // block goes home when its last payload is freed").
                    unsafe { crate::memory::retained::pin(payload_block) };

                    // A second count, the reset's own, because that death
                    // event can arrive inside this reset: a release the
                    // drain below runs can kill the very survivor whose
                    // payload is pinned, and no occupant count exists
                    // to hold the block until `place_survivor_lists`
                    // (`dev/DECISIONS.md`, "the reset holds a pin of its
                    // own, and releases it after the index is real").
                    if unsafe { crate::memory::heap::block_reset_pin(payload_block as *mut u8) }
                        == 0
                    {
                        unsafe {
                            crate::memory::heap::set_block_reset_pin(
                                payload_block as *mut u8,
                                pin_chain,
                            );
                            crate::memory::retained::pin(payload_block);
                        }

                        pin_chain = payload_block;
                    }
                }
            }

            unsafe {
                if mutator_flags(surv) & COW != 0 {
                    cow_at_promotion.push((surv, header_refcount(surv)));
                }

                // 00 = GcHeap; drop the transient arena-reset mark and
                // IS_ESCAPEE. Every bit named is the mutator's, below bit
                // 16, so this is the narrow flags pair and the collector's
                // byte leaves the arena exactly as the survivor carried it
                // in. That byte has no reader yet; the step that gives it
                // one owes promotion a defined value, because promotion is
                // a publication (`dev/DECISIONS.md`, "the header's access
                // width is a correctness rule").
                update_header_flags(surv, |f| {
                    f & !(MEMORY_CATEGORY_MASK | ARENA_RESET_MARK | IS_ESCAPEE)
                });
            }

            // A survivor that had a block to itself keeps it, and none of
            // the retention machinery applies to it: the block is not
            // shared, so it has nothing to index, and its kind is what
            // routes the free — restamped `BLOCK_KIND_RETAINED` it would
            // send a multi-megabyte run to the 64 KiB block pool at the
            // entity's eventual death. What the reset does instead is
            // hand ownership over: the run leaves the arena's log, so the
            // reset stops freeing it, and the registry entry it was given
            // at allocation is what the walk finds it by from now on
            // (`rfc/model/memory/large-entities.md`).
            //
            // Omitting this arm is silent: nothing between the reset and
            // the entity's death looks wrong, which is why it is the one
            // of `large-entities.md`'s four rules for a surviving run
            // that carries a test of its own.
            let block = BlockHeader::of_ptr(surv as *const u8) as usize;
            if unsafe { is_in_a_block_of_its_own(surv) } {
                let forgotten = unsafe { (*arena).forget_large(surv as *mut u8) };
                debug_assert!(
                    forgotten,
                    "a promoted large entity was not one of this arena's runs"
                );
            } else {
                // The survivor list is taken here, in the one place that
                // classifies, so the two answers cannot disagree and
                // neither is asked of a dead entity.
                by_block.entry(block).or_default().push(surv as usize);
                if unsafe { retain_block(block as *mut BlockHeader) } {
                    retained_blocks += 1;
                }
            }
        }

        if retain_the_rounds_children {
            for surv in unsafe { (*arena).walk_survivors(counted) } {
                unsafe {
                    crate::cells::trace_entity(surv, |child| {
                        if mutator_flags(child) & COW != 0 {
                            ll_retain(child);
                        }
                    });
                }
            }
        }

        counted = unsafe { (*arena).survivor_count() };

        // --- Deferred releases. Teardown here (destructor first, then free)
        // may create new work; a new escape settles as an ordinary escape
        // next pass (the survivor it stored into is GcHeap by now). Loop
        // while the log yields anything.
        // Take the chain, then release off it: `die` runs `__destruct`,
        // which reenters and resolves this same arena, so no borrow of the
        // arena may be live here (audit H5). Entries those destructors
        // append begin the arena's fresh chain and the next pass takes
        // them — the same settling the loop already relies on (H7).
        let mut released = 0usize;
        unsafe { (*arena).take_release_log() }.for_each(|entity| {
            released += 1;
            unsafe {
                if ll_release(entity) {
                    die(entity);
                }
            }
        });
        if released == 0 {
            break;
        }

        rounds += 1;
        assert!(
            rounds <= ARENA_RESET_MAX_ROUNDS,
            "arena reset did not converge"
        );
    }

    // COW counts settle here, for the reason they were left alone until
    // now: the fixpoint is where mutator code runs, and on a COW entity
    // the count is what that code reads to decide whether a write may go
    // in place.
    unsafe { reconcile_cow_counts(&cow_at_promotion) };

    // The weak walk — after every destructor has settled and the
    // survivors' categories are rewritten, before the pages go back:
    // dying entries get their cells nulled, promoted survivors are
    // recognized by their new category and keep resolving
    // (`rfc/model/weak-references.md`, "Death notification"). Runs no
    // user code, so it cannot grow the logs behind the settled fixpoint.
    unsafe { crate::weak::drain_arena_weak_log(arena) };

    // The survivor lists gathered above are written into the arena's own
    // memory and published in the retained blocks' headers. A bump-filled
    // block has no stride to divide by, so this inventory is the only way
    // a trace can enumerate its occupants — without it they are root
    // sources and a ring among them never dies
    // (`rfc/model/gc/rc-cycle.md`, "Where the shadow count lives", the
    // retained-block arm). Published after the fixpoint has settled and
    // before the blocks are disposed of, so no death can arrive behind
    // the counts it establishes, and while the arena still holds its
    // blocks, which is where the lists go.
    #[cfg_attr(not(feature = "debug-journal"), allow(unused_variables))]
    let survivor_total = unsafe { (*arena).survivor_count() };
    let mut emptied = unsafe { place_survivor_lists(arena, by_block, &mut retained_blocks) };

    // The kind word is the membership test: `Arena::fresh_block` stamps
    // `BLOCK_KIND_ARENA` on every block it draws from the pool, so a block
    // of this arena reads `BLOCK_KIND_RETAINED` exactly when this reset
    // stamped it (`dev/plans/S47.md`, the Critic round of 2026-09-13).
    unsafe {
        (*arena).finish_reset(|block| {
            crate::memory::block_pool::load_block_kind(&raw const (*block).kind)
                == BLOCK_KIND_RETAINED
        })
    };

    // The reset's own pins go now, past the last point at which an
    // occupant count could still be established, which is what each was
    // held for. A block that empties on the release had its payload
    // freed inside the reset, so it joins the vector below: no later
    // death is left to report it.
    //
    // The link is read and cleared before the pin is spent, because a
    // block the release empties goes back to the pool below and its
    // header is the pool's from then on.
    let mut block = pin_chain;
    while block != crate::memory::heap::RESET_PIN_END {
        let next = unsafe { crate::memory::heap::block_reset_pin(block as *mut u8) };
        // Zero is "on no chain", so reading it here means a block left the
        // chain while the chain still named it. Named rather than followed:
        // the walk would otherwise read a collector line at address zero,
        // and in a release build read one wildly.
        debug_assert!(
            next != 0,
            "a pinned block left the reset's chain while it was still on it"
        );
        unsafe { crate::memory::heap::set_block_reset_pin(block as *mut u8, 0) };
        #[cfg(test)]
        PINS_SPENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if unsafe { crate::memory::retained::hold_released(block) } {
            emptied.push(block);
        }

        block = next;
    }

    // Blocks nothing holds at the end of this reset — every survivor died
    // inside it, the shape a heap reference box produces, where the
    // element it made an escapee is promoted and then torn down by the
    // box's own logged release; or a payload pinned it and was freed
    // inside it. No later death will report such a block empty, so the
    // reset hands it over itself, and only **after** `finish_reset`: the
    // arena's block chain is threaded through the very headers the pool
    // overwrites, so a block returned before that walk cuts the chain
    // under it. The route is `ll_free` of the block address rather than
    // the pool directly, because the block still reads
    // `BLOCK_KIND_RETAINED` and that arm is the one path which spends
    // the hold its list has on the block the list stands in, and which
    // S36.2's withholding will hold back while a collection reads the
    // block.
    for block in emptied {
        unsafe { crate::memory::stdapi::ll_free(block as *mut u8) };
    }

    // After the frees, so that every death this reset caused falls
    // between the pair (`journal::kinds::KIND_ARENA_RESET_BEGIN`). The
    // survivor total is read before `finish_reset`, which clears the chain
    // with the blocks it stood in.
    journal_event!(
        crate::journal::kinds::KIND_ARENA_RESET_END,
        arena as u64,
        survivor_total as u64,
        retained_blocks as u64
    );

    severed
}

/// Take `block` out of circulation as a retained former-arena block: the
/// one place the reset stamps `BLOCK_KIND_RETAINED`. **True when this call
/// is the one that stamped it**, so a caller counting retentions counts
/// each block once.
///
/// **The stamp is also the test**, which is what makes a second call safe:
/// the clearing below would otherwise zero the pins the reset has placed
/// on the block since the first call. A block of an arena being reset
/// reads `BLOCK_KIND_RETAINED` exactly when this reset stamped it, because
/// `Arena::fresh_block` stamps `BLOCK_KIND_ARENA` on every block it draws
/// from the pool and a retained block leaves the arena's chain.
///
/// **The whole collector line is cleared before the kind is published**,
/// and that is the whole reason this is a function. A retained block is
/// traced through a shadow row array like an entity block is, and the
/// pointer to that array lives in a word this block's previous life may
/// have written: an entity block writes it at every collection that
/// touches it, and only its own commissioning nulls it again. Beside it
/// stand the survivor list and count word of a previous retention, which
/// a block retained, returned, drawn by an arena and retained again would
/// otherwise carry into this one. The kind's release store publishes the
/// zeros, so a trace that reads `BLOCK_KIND_RETAINED` reads "no rows, no
/// list, nothing held" with it; the list itself is published later by a
/// release store of its own (`memory::heap::clear_collector_line`,
/// `memory::retained::register`).
///
/// # Safety
/// `block` is the header of a live 64 KiB block whose arena is being
/// reset, and which holds a survivor, a survivor's payload or a survivor
/// list.
unsafe fn retain_block(block: *mut BlockHeader) -> bool {
    let kind = unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) };
    if kind == BLOCK_KIND_RETAINED {
        return false;
    }

    unsafe {
        crate::memory::heap::clear_collector_line(block as *mut u8);
        crate::memory::block_pool::store_block_kind(&raw const (*block).kind, BLOCK_KIND_RETAINED);
    }

    true
}

/// Bring a survivor's out-of-line memory with it, if it has any.
/// One call, dispatched on the entity, so nothing about any layout leaks
/// into the reset: promotion holds a block by the address of a header and
/// does not otherwise look inside an entity.
///
/// A pinned payload answers the block holding the bytes, which the caller
/// keeps alive instead. Every arm computes that address out of the value it
/// already carried, so no second classification can disagree with the one
/// that answered.
///
/// # Safety
/// `surv` is a live survivor of `arena`, mid-reset.
unsafe fn carry_external_memory(arena: *mut Arena, surv: *mut RcHeader) -> ExternalCarry {
    match unsafe { external_memory(surv) } {
        External::StringPayload(s) => {
            if unsafe { crate::string::carry_payload_out_of(arena, s) } {
                ExternalCarry::Done
            } else {
                ExternalCarry::Pinned(block_holding(unsafe { (*s).data }))
            }
        }
        External::ArrayStorage(a) => {
            if unsafe { crate::array::entity::carry_storage_out_of(arena, a) } {
                ExternalCarry::Done
            } else {
                ExternalCarry::Pinned(block_holding(unsafe {
                    crate::array::entity::storage_address(a)
                }))
            }
        }
        // A class with cells outside its body answers both halves itself:
        // the group's `carry` is the only code that knows where the
        // storage is (`crate::cells::OutsideCells`).
        External::Outside(group) => match unsafe { (group.carry)(arena, surv) } {
            crate::cells::OutsideCarry::Carried | crate::cells::OutsideCarry::Nothing => {
                ExternalCarry::Done
            }
            crate::cells::OutsideCarry::Pinned { memory } => {
                ExternalCarry::Pinned(block_holding(memory))
            }
        },
        External::None => ExternalCarry::Done,
    }
}

/// The block header holding `memory`, or 0 for a null address — the one
/// place a pinned payload's address becomes a block, so a caller cannot
/// skip the mask.
fn block_holding(memory: *mut u8) -> usize {
    if memory.is_null() {
        0
    } else {
        BlockHeader::of_ptr(memory as *const u8) as usize
    }
}

/// What the reset does next about a survivor's out-of-line memory.
enum ExternalCarry {
    /// It came along, or there was none.
    Done,
    /// The bytes are a pinned payload: they stay put, and the caller
    /// retains and pins the block holding them.
    ///
    /// Zero is [`block_holding`]'s null-address guard and reaches no
    /// producer in this crate: both kind arms answer [`ExternalCarry::Done`]
    /// for a storage-less entity rather than reaching this variant, and a class
    /// hook that answers `Pinned` with a null address breaks
    /// [`crate::cells::OutsideCarry::Pinned`]'s contract. The caller pins
    /// nothing for it (`promote.rs`, the `payload_block != 0` test), so
    /// the value is a guard rather than a second state of the outcome.
    Pinned(usize),
}

/// What a survivor owns outside its own entity. Three shapes do today;
/// the rest answer [`External::None`] and cost one flags read.
enum External {
    None,
    StringPayload(*mut crate::string::LLStringDynamic),
    ArrayStorage(*mut crate::array::entity::LLArray),
    Outside(&'static crate::cells::OutsideCells),
}

/// Classify a survivor once, so the carry and the block it falls back on
/// cannot disagree about what the entity is.
///
/// # Safety
/// `surv` must be a live entity.
unsafe fn external_memory(surv: *mut RcHeader) -> External {
    use crate::refcount::{ENTITY_KIND_MASK, EntityKind};
    let flags = unsafe { mutator_flags(surv) };
    match flags & ENTITY_KIND_MASK {
        // Only the dynamic layout holds bytes out of line; an inline
        // string carries them behind its own header and moves with it.
        k if k == EntityKind::StringDynamic.to_flags() => {
            External::StringPayload(surv as *mut crate::string::LLStringDynamic)
        }
        k if k == EntityKind::Array.to_flags() => {
            External::ArrayStorage(surv as *mut crate::array::entity::LLArray)
        }
        // Both kinds that carry a class word at +8, because a specialized
        // teardown is not inherited while the group is: a subclass of a
        // hooked class owns the same storage.
        k if crate::refcount::carries_a_class_word(k) => {
            let cls = unsafe { (*(surv as *mut crate::object::Object)).class };
            match unsafe { crate::class::Class::outside_cells(cls) } {
                Some(group) => External::Outside(group),
                None => External::None,
            }
        }
        _ => External::None,
    }
}

/// Write each block's survivor list into the arena's own memory and
/// publish it in the block's header. The blocks that come back empty —
/// every occupant already dead when the list was published, and nothing
/// else holding them — are returned, and their disposal is the caller's.
///
/// The grouping is the caller's because only the promotion loop holds a
/// survivor at a moment it is certainly alive, and deciding which block a
/// survivor belongs to is a read of the survivor's memory
/// (`dev/DECISIONS.md`, "Promotion classifies once").
///
/// Where a list goes is the arena's answer, through `Arena::alloc_preferring`:
/// the retained block's own tail, else the reset's current block, else a
/// fresh pool block — and a block that holds another block's list is
/// retained as its holder and pinned once per list, so it returns only
/// after every block whose list it carries
/// (`rfc/model/gc/rc-cycle.md`, "The survivor list of a retained block").
/// Every list is placed before any block's count is read, because a
/// block's answer counts the lists standing in it, and a holder whose
/// own survivors all died inside the reset would otherwise report itself
/// empty before a later block's list landed in its tail. A refused
/// placement publishes the count without a list: the block stays
/// retained, returns by its deaths, and every edge into it answers
/// untracked for its life (`memory::retained::register`).
///
/// One list per block rather than one per reset: both enumerators reach
/// a block first — the trace by the 64 KiB alignment mask, the test-only
/// walk by scanning the region registry — so a list found from a block
/// address costs no second mapping (`dev/DECISIONS.md`, "retained
/// blocks are walked through a per-block object index").
///
/// # Safety
/// `arena` is the arena being reset, past its fixpoint and before
/// `finish_reset`, and every block in `by_block` is one of its bump
/// blocks, stamped retained by this reset.
unsafe fn place_survivor_lists(
    arena: *mut Arena,
    by_block: HashMap<usize, Vec<usize>>,
    retained_blocks: &mut usize,
) -> Vec<usize> {
    let mut placed: Vec<(usize, Vec<usize>, *mut usize)> = Vec::with_capacity(by_block.len());
    for (block, occupants) in by_block {
        let bytes = occupants.len() * size_of::<usize>();
        let list = unsafe { (*arena).alloc_preferring(block as *mut BlockHeader, bytes) };
        let list = list as *mut usize;
        if !list.is_null() {
            let holder = BlockHeader::of_ptr(list as *const u8) as usize;
            if holder != block {
                if unsafe { retain_block(holder as *mut BlockHeader) } {
                    *retained_blocks += 1;
                }

                // Not on the reset's pin chain, which carries the pins the
                // reset spends itself: this one is the list's, and
                // `retained::release_emptied` spends it when the block the
                // list belongs to returns.
                unsafe { crate::memory::retained::pin(holder) };
            }
        }

        placed.push((block, occupants, list));
    }

    let mut emptied = Vec::new();
    for (block, occupants, list) in placed {
        // A shared retained block keeps every byte it had, so a dead
        // occupant's address is still readable and `register` reads it —
        // which is the whole of what it asks of this caller.
        if unsafe { crate::memory::retained::register(block, &occupants, list) } {
            emptied.push(block);
        }
    }

    emptied
}

/// True for a survivor that occupies a block-aligned allocation alone
/// (`memory::large_entity`), which the arena's entity entry point gives an
/// entity past one block payload. Such a block is not shared with
/// anything, so the reset neither retains nor indexes it; the block kind
/// is the whole of the test, because a large-entity kind is only ever
/// stamped on a block that holds exactly one entity.
///
/// # Safety
/// `surv` is a live entity address.
#[inline]
unsafe fn is_in_a_block_of_its_own(surv: *mut RcHeader) -> bool {
    let block = BlockHeader::of_ptr(surv as *const u8);
    crate::memory::large_entity::is_large_entity(unsafe {
        crate::memory::block_pool::load_block_kind(&raw const (*block).kind)
    })
}

/// Entity teardown dispatch from a bare header — the uniform kind
/// switch. The release log can hold weak cells and reference boxes, and
/// a bare `ll_object_die` on one of those would read a class pointer
/// that is not there.
unsafe fn die(entity: *mut RcHeader) {
    unsafe { crate::object::ll_entity_die(entity) };
}

#[inline]
unsafe fn is_arena_entity(p: *mut RcHeader) -> bool {
    !p.is_null() && unsafe { crate::object::header_category(p) } == MemoryCategory::RequestArena
}

/// Admit one escapee record as a root of the survivor chain, **false for a
/// record this reset has already admitted**, which is what makes a
/// duplicate record cost one flags read.
///
/// A root keeps its `refcount`: that count is already its external
/// hold-count, unlike a survivor reached only by an internal edge, whose
/// count [`mark_child`] rebuilds from the edges that reach it.
///
/// # Safety
/// `root` is a live arena entity still carrying `IS_ESCAPEE`.
unsafe fn mark_root(root: *mut RcHeader) -> bool {
    if unsafe { mutator_flags(root) } & ARENA_RESET_MARK != 0 {
        return false;
    }

    unsafe { update_header_flags(root, |f| f | ARENA_RESET_MARK) };
    true
}

/// Admit one arena child, **false when the arena had no memory to record
/// it** and the caller owes its edge a sever.
///
/// Admission precedes the mark, so a refusal leaves no half-marked entity
/// and the mark bit and chain membership stay equal — which every pass
/// after this one reads as the same fact.
///
/// # Safety
/// `child` is a live arena entity and `arena` is the one being reset.
unsafe fn mark_child(arena: *mut Arena, child: *mut RcHeader) -> bool {
    let flags = unsafe { mutator_flags(child) };
    if flags & ARENA_RESET_MARK != 0 {
        return true; // already admitted, and its edge stands
    }

    // An unmarked child still carrying `IS_ESCAPEE` has an escapee record
    // waiting: the barrier logs one at the 0→1 transition of the hold-count
    // and `escape_lose` clears the flag at zero, so the flag standing means
    // a record stands too. The next round admits it as a root by compacting
    // that record where it lies, which allocates nothing and so cannot be
    // refused. Pushing it here would risk a sever on an edge the barrier
    // counted, and severing a counted edge is what leaves a promoted entity
    // holding a count nothing releases.
    if flags & IS_ESCAPEE != 0 {
        return true;
    }

    if !unsafe { (*arena).push_survivor(child) } {
        return false;
    }

    unsafe {
        update_header_flags(child, |f| f | ARENA_RESET_MARK);
        // A survivor reached only by an internal edge has no external
        // hold-count, so its count starts at zero and the counting pass
        // rebuilds it from the edges. `IS_ESCAPEE` is tested again rather
        // than read off `flags` above, because the arm that reaches here
        // has already answered for a child carrying it.
        //
        // A COW entity is the exception, because its count is live all
        // through the fixpoint: `values.md` maintains it in every memory
        // category, so a destructor's `unset` reaches `ll_release` and
        // decrements it. Zeroing here would make that decrement underflow
        // inside the reset. Its count is settled once instead, by
        // [`reconcile_cow_counts`], after the last destructor has run.
        if mutator_flags(child) & (IS_ESCAPEE | COW) == 0 {
            set_header_refcount(child, 0);
        }
    }

    true
}

/// Walk the survivors from `from` and admit every arena entity they name,
/// which is the whole of the descent: a child admitted here lands at the
/// chain's tail and this same walk reaches it, so the closure is reached
/// without a worklist of its own.
///
/// **An edge whose child the arena cannot record is severed**: the slot is
/// emptied, so nothing promoted names an entity this reset never promoted
/// (`dev/DECISIONS.md`, "a survivor cell the pool cannot supply severs the
/// edge, and the reset finishes"). Returns how many edges that was, which
/// is not how many children died: the refusal leaves the child unmarked, so
/// another edge to it is refused and severed in its turn, and a destructor
/// that escapes it afresh has it admitted as a root of the next round.
///
/// # Safety
/// `arena` is the arena being reset, mid-fixpoint.
unsafe fn descend_from(arena: *mut Arena, from: usize) -> usize {
    let mut severed = 0;
    let mut index = from;
    let mut walk = unsafe { (*arena).walk_survivors(index) };
    loop {
        // A walk runs dry either because the descent is done or because it
        // was made over a chain that was empty then and is not now. One
        // re-ask at the current index tells the two apart, and costs one
        // walk of the segment chain rather than a pointer into the arena
        // that the next append would retag.
        let s = match walk.next() {
            Some(s) => s,
            None => {
                walk = unsafe { (*arena).walk_survivors(index) };
                match walk.next() {
                    Some(s) => s,
                    None => break,
                }
            }
        };

        index += 1;
        // A survivor whose teardown completed inside this reset holds
        // nothing any more, and nothing may follow what its slots still
        // name (`memory::reset_window`).
        if unsafe { crate::memory::reset_window::is_torn_down(s) } {
            continue;
        }

        #[cfg(test)]
        unsafe {
            crate::memory::reset_window::note_walk(s)
        };

        let kind = unsafe { crate::cells::entity_kind(s) };
        unsafe {
            crate::cells::trace_cells::<crate::cells::PlainCells>(s, kind, |cell| {
                if !is_arena_entity(cell.child) {
                    return;
                }

                if !mark_child(arena, cell.child) {
                    sever_one_edge(arena, s, kind, cell);
                    severed += 1;
                }
            });
        }
    }

    severed
}

/// Empty the one cell a refused child came through, at the unit the
/// holder's layout leaves consistent, and journal every child that unit
/// displaced (`dev/DECISIONS.md`, "a sever takes the smallest unit its
/// holder's layout leaves consistent, and never lands on a counted edge").
///
/// **Nothing is released, and the holder's category is why.** A sever lands
/// only on a holder still `RequestArena`, and a store into such a holder
/// counts nothing into an arena child — `ll_retain` returns before the
/// counter for an arena non-COW entity — records a heap child's release at
/// the reset, and never gains an escape, the category barrier's two arms
/// both testing a mismatch. So every occupant a sever displaces, the
/// refused one and the collateral alike, holds no count of this holder's:
/// an arena child's count is rebuilt by `count_children` from the edges
/// that remain, a heap child's logged release fires once against the retain
/// its store took, and a COW child's word is replaced by the window's log.
/// The child's own `IS_ESCAPEE` is a consequence of that rule rather than
/// the rule (Sage, 2026-09-13, `dev/plans/S47.md`).
///
/// **More children may be displaced than the one refused.** A string key's
/// unit is its whole entry, so the element beside it loses its edge too;
/// each gets its own journal record, while the reset's severed count stays
/// one per refused edge.
///
/// # Safety
/// `cell` is one the tracer yielded for `entity` of `kind`, mid-reset.
#[cfg_attr(not(feature = "debug-journal"), allow(unused_variables))]
unsafe fn sever_one_edge(
    arena: *mut Arena,
    entity: *mut RcHeader,
    kind: u32,
    cell: crate::cells::Cell,
) {
    debug_assert_eq!(
        unsafe { crate::object::header_category(entity) },
        crate::refcount::MemoryCategory::RequestArena,
        "a sever on a promoted holder would empty an edge somebody counted"
    );
    // Not an assert: a holder of another category costs a bounded leak
    // rather than an early free, and this path exists to survive a memory
    // refusal rather than to end the request on one.
    unsafe {
        crate::cells::sever_cell(entity, kind, cell, &mut |child| {
            journal_event!(
                crate::journal::kinds::KIND_ARENA_RESET_SEVERED_EDGE,
                arena as u64,
                entity as u64,
                child as u64
            );
        })
    };
}

/// Pins of its own the reset has spent by walking its chain, so a test can
/// say that a second payload in one block took no second pin: the outcome
/// is the same block going home either way, and only the count separates
/// one pin from two. A plain static, as `RETRACES` is: every test that runs
/// a reset holds `block_pool::test_guard`.
#[cfg(test)]
static PINS_SPENT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The pins the reset's own chain has spent since the last read, cleared by
/// the read.
#[cfg(test)]
pub(crate) fn take_pins_spent() -> usize {
    PINS_SPENT.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// Re-trace passes since a test last read them, so a test can say whether
/// its destructor round was re-traced at all: a child the re-trace missed
/// and a child it never looked for read the same in the heap. A plain
/// static, as `reset_window`'s counters are: every test that runs a reset
/// holds `block_pool::test_guard`.
#[cfg(test)]
static RETRACES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The re-trace passes since the last read, cleared by the read.
#[cfg(test)]
pub(crate) fn take_retrace_count() -> usize {
    RETRACES.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// Re-read every survivor's current children and admit any newly-appeared
/// arena child. A destructor may have stored an arena object, fresh or
/// existing, into an already-traced survivor — an arena→arena store the
/// barrier does not escape — so that child would otherwise be missed and
/// dangle once the survivor is promoted (audit H2). Cheap when nothing
/// changed: an already-admitted child is skipped by the mark test.
///
/// # Safety
/// As [`descend_from`].
unsafe fn retrace_survivors(arena: *mut Arena) -> usize {
    #[cfg(test)]
    RETRACES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    unsafe { descend_from(arena, 0) }
}

/// Settle every COW survivor's count now that the fixpoint is over and
/// no user code can run again.
///
/// Three terms: `edges_at_promotion + (now - at) - K`. **The edges** are
/// the window's log of every edge a survivor held to this child when the
/// survivor was counted, and they replace what the count said at that
/// instant, because the holders that died with the arena never released
/// and there is no list of them to subtract. **The delta** is whatever
/// changed the count after promotion, carried across untouched: a
/// destructor may hand an already-promoted string to a heap object that
/// outlives the request, or drop the edge a promoted holder had, and both
/// are events on the count and on nothing the log can see. A release of a
/// logged edge — by the holder's teardown or by a store into its slot —
/// is one `-1` in the delta against the edge's one `+1`, so it is
/// subtracted exactly once; a retain of a new edge into a promoted holder
/// is one `+1` in the delta and nothing else. **`K`** takes back the
/// compensating retain `count_children` gives an already-promoted child,
/// whose edge the log carries as well (`dev/DECISIONS.md`, "the COW count
/// is the log's edges plus the delta").
///
/// `at_promotion` is each COW survivor's count at the instant its
/// category was rewritten — the last instant the reset can attribute it
/// to arena holders.
///
/// # Safety
/// The fixpoint has settled and no user code can run again before the
/// blocks are disposed of.
unsafe fn reconcile_cow_counts(at_promotion: &[(*mut RcHeader, u32)]) {
    if at_promotion.is_empty() {
        return;
    }

    // Address → count settled so far: the delta first, the log's terms on top.
    let mut settled: HashMap<usize, i64> = HashMap::with_capacity(at_promotion.len());
    for &(s, at) in at_promotion {
        let now = unsafe { header_refcount(s) } as i64;
        settled.insert(s as usize, now - at as i64);
    }

    // A correction naming no row of its own is dropped here, which is
    // what narrows the window's log — recorded for every COW child the
    // counting pass met — to this reset's own COW survivors
    // (`memory::reset_window::Correction`).
    crate::memory::reset_window::for_each_correction(|child, correction| {
        use crate::memory::reset_window::Correction;
        if let Some(entry) = settled.get_mut(&(child as usize)) {
            *entry += match correction {
                Correction::DeferredIncrement => 1,
                Correction::DeferredDecrement => -1,
            };
        }
    });

    for &(s, _) in at_promotion {
        let settled_count = settled[&(s as usize)];
        debug_assert!(
            settled_count >= 0,
            "a COW survivor lost more references than it had"
        );
        unsafe { set_header_refcount(s, settled_count.max(0) as u32) };
    }
}

/// True when `cells::trace_entity` enumerates **all** of this entity's
/// counted children rather than skipping it.
///
/// The tracer's own skips are conservative for the collector — an omitted
/// source only removes in-edges, so its targets stay pinned — and they
/// are the opposite of conservative for a pass that decides a count from
/// the edges it finds. Box is the kind left out: its payload is C memory
/// nothing here can read.
fn traceable_in_full(flags: u32) -> bool {
    use crate::refcount::{ENTITY_KIND_MASK, ENTITY_KIND_SHIFT, EntityKind};
    let kind = (flags & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT;
    const OBJECT: u32 = EntityKind::Object as u32;
    const LAZY: u32 = EntityKind::Lazy as u32;
    const REFERENCE: u32 = EntityKind::Reference as u32;
    const STRING: u32 = EntityKind::String as u32;
    const STRING_DYNAMIC: u32 = EntityKind::StringDynamic as u32;
    const WEAKREF: u32 = EntityKind::WeakRef as u32;
    const ARRAY: u32 = EntityKind::Array as u32;
    // A string of either layout and a WeakRef are leaves, so "skipped"
    // and "enumerated in full" are the same answer for them.
    matches!(
        kind,
        OBJECT | LAZY | REFERENCE | STRING | STRING_DYNAMIC | WEAKREF | ARRAY
    )
}

/// One counting pass over a survivor's reference slots: +1 to arena
/// children (internal edges), a compensating retain to heap entities
/// (their release-at-reset record no longer matches a dying holder), and
/// both of the reconciliation's correction terms recorded on the way
/// (`memory::reset_window::record_promotion_edge`,
/// `record_deferred_decrement`).
///
/// A record the manager refuses is answered by the round rather than here:
/// a refused edge retains the round's COW children once their counts are
/// captured, and a refused decrement leaves its retain standing, which
/// settles the child one too high and never too low
/// (`memory::reset_window`).
unsafe fn count_children(surv: *mut RcHeader) {
    debug_assert!(
        traceable_in_full(unsafe { mutator_flags(surv) }),
        "a survivor of a kind `trace_entity` skips would have its children's counts left \
         unbuilt and its COW edges unrecorded, not conservatively ignored"
    );
    unsafe {
        crate::cells::trace_entity(surv, |child| {
            // This pass is the instant the edge has to be recorded at:
            // after this round's destructors, before the category
            // rewrite, which is the instant the count it stands for is
            // captured and discarded (`dev/DECISIONS.md`, "the COW count
            // is the log's edges plus the delta").
            let cow = mutator_flags(child) & COW != 0;
            if cow {
                crate::memory::reset_window::record_promotion_edge(surv, child);
            }

            match crate::object::header_category(child) {
                MemoryCategory::RequestArena => {
                    set_header_refcount(child, header_refcount(child) + 1)
                }
                MemoryCategory::GcHeap => {
                    ll_retain(child);
                    // A retain of this pass's own, taken back by the
                    // reconciliation's K term.
                    if cow {
                        crate::memory::reset_window::record_deferred_decrement(child);
                    }
                }
                _ => {}
            }
        });
    }
}

#[cfg(test)]
mod tests;
