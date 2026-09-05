//! The survivor list of a retained former-arena block, and the count word
//! that returns the block.
//!
//! Why such a block cannot be traced without a list, and what goes
//! uncollected then: `rfc/model/gc/rc-cycle.md`, "Where the shadow count
//! lives" — a bump-filled block has no stride, so the trace reaches its
//! rows by binary search over the list — and
//! `docs/memory-manager.md`, "Arena reset: the settling loop".
//!
//! The list and the words that name it are the block's own
//! (`rfc/model/gc/rc-cycle.md`, "The survivor list of a retained block").
//! The collector line at the end of the block's header carries the list's
//! address, its length and the count word beside the shadow pointer
//! (`crate::memory::heap`), and the addresses themselves are written at
//! the reset into memory the arena already holds — the retained block's
//! own tail, else the reset's current block, else a fresh pool block,
//! placed by `promote::place_survivor_lists` through
//! `Arena::alloc_preferring`. No process-wide table names retained
//! blocks: every reader asks about one block whose address it holds, and
//! the test-only enumerator finds them by their kind in the region scan
//! (`dev/DECISIONS.md`, "a retained block's survivor list lives in the
//! arena's own memory, and the process registry goes").
//!
//! # What this module does not know
//!
//! What lives at those addresses. It stores block addresses and arrays of
//! addresses; entities, classes, refcounts and verdicts belong to the
//! layers above. It reads the first eight bytes of one of them, in one
//! place and for one purpose: `refcount::slot_state` decides how many of a
//! list's addresses are alive when the list is published.
//!
//! # The one requirement on an address
//!
//! Every address in a published list stays **readable** for as long as
//! the list is published. Both enumerators read its first eight bytes
//! without first testing that the block still exists, which they may because a
//! retained block leaves circulation only once its last survivor is gone,
//! and the list itself is in memory that leaves circulation no earlier: a
//! block holding another block's list is held for it, through the count
//! word, until that block returns.
//!
//! # The count word
//!
//! One 64-bit word on the collector line: live occupants in the low half,
//! and in the high half everything else the block is held for — a payload
//! the reset could not carry out, and a survivor list of another block
//! standing in this one ([`pin`]). The word is decremented atomically by
//! whichever thread performs a free, because `ll_free` is an ABI entry
//! that cannot be made owner-only, and the value the decrement returns
//! says whether the caller holds the last count and owes the block to the
//! pool through [`release_emptied`]. Both halves at zero is the whole
//! condition, and one word is what lets a decrement answer it without a
//! lock.
//!
//! A block may be retained for **bytes** rather than for occupants, when a
//! survivor's out-of-line payload could not be carried out of the dying
//! arena. It is then held by two populations and goes home when both are
//! empty: occupants counted at [`register`], payloads counted by [`pin`]
//! and spent by [`payload_freed`] (`dev/DECISIONS.md`, "a pinned block
//! goes home when its last payload is freed").
//!
//! The reset holds one payload count of its own per block it pins, from
//! the refusal until it has finished establishing occupant counts, and
//! spends it through [`hold_released`]. Why the count exists, and
//! why its release leaves the list standing: `dev/DECISIONS.md`, "the
//! reset holds a pin of its own, and releases it after the index is
//! real".

use std::sync::atomic::{AtomicU64, Ordering};

use crate::memory::block_pool::{BLOCK_KIND_FREE, BlockHeader, BlockPool, store_block_kind};
use crate::memory::heap::{block_hold_count, block_survivor_list, publish_block_survivor_list};

/// One live occupant, in the low half of the count word.
const OCCUPANT: u64 = 1;
/// One thing the block is held for beyond its occupants, in the high half:
/// a pinned payload, a list of another block, or the reset's own count.
const HOLD: u64 = 1 << 32;
/// The low half.
const OCCUPANTS: u64 = HOLD - 1;

/// The count word of retained block `block`.
///
/// # Safety
/// `block` must be the header of a mapped block stamped
/// `BLOCK_KIND_RETAINED`, for as long as the pointer is used.
#[inline]
unsafe fn count_word(block: usize) -> *const AtomicU64 {
    unsafe { block_hold_count(block as *mut u8) }
}

/// Publish `occupants`, the survivors promoted in retained block `block`,
/// as its survivor list at `destination`, and count how many of them are
/// alive.
///
/// `destination` is `occupants.len()` words the arena placed for the list
/// (`promote::place_survivor_lists`): inside `block`'s own tail, or
/// inside another retained block that [`pin`] has already been called for
/// on this list's behalf. The list is copied there and sorted in place,
/// because the trace's lookup is a binary search over it and the reset
/// builds it in discovery order. **Null when no memory could be placed**:
/// the count is published without a list, so the block stays retained and
/// returns by its deaths, and every edge into it answers untracked for
/// its life ([`occupant_index`]).
///
/// **An occupant already dead when the list is published is not counted.**
/// It has had its one death and will never reach [`occupant_freed`], so
/// counting it would hold the block for a survivor that no longer
/// exists — which is exactly the case a heap box behind `&` produces:
/// the element it made an escapee is promoted, and the box's logged
/// release kills it in the same reset, before the list exists.
///
/// **True when the block is empty already**, which is that same case
/// taken to its end: every occupant died inside the reset and nothing
/// pins the block, so nobody is left to report the last death and the
/// caller must return the block itself, through `ll_free(block)`, which
/// answers off the count word. The list is published either way.
///
/// # Safety
/// Every address in `occupants` must be readable, as the module doc
/// requires of anything published here. `destination` must be null or
/// writable for `occupants.len()` words and inside a block that stays
/// retained until this one returns. `block` must be stamped
/// `BLOCK_KIND_RETAINED` by this reset over a cleared collector line, and
/// no trace may address it yet.
#[must_use = "true means the block is empty and the caller owes it to the pool"]
pub(crate) unsafe fn register(block: usize, occupants: &[usize], destination: *mut usize) -> bool {
    let live = occupants
        .iter()
        .filter(|&&address| unsafe { is_occupied(address) })
        .count() as u64;
    if !destination.is_null() {
        let list = unsafe { std::slice::from_raw_parts_mut(destination, occupants.len()) };
        list.copy_from_slice(occupants);
        list.sort_unstable();
    }

    // The list before the count. The decrement that reaches zero, on
    // whichever thread performs it, synchronises with this increment and
    // spends the hold the list has on its holder through the address it
    // reads (`release_emptied`); published after the count, the address
    // could still read null to that decrement, and the holder's hold
    // would never be spent.
    unsafe { publish_block_survivor_list(block as *mut u8, destination, occupants.len()) };
    let held = unsafe { (*count_word(block)).fetch_add(live, Ordering::AcqRel) } + live;
    held == 0
}

/// One more thing the block is held for beyond its occupants: a payload
/// the reset could not carry out, so the bytes outlive the arena and the
/// block waits for their free as it waits for a live occupant's death
/// (module doc); the survivor list of another block, placed in this one
/// by the reset and spent when that block returns
/// ([`release_emptied`]); the reset's own count over the window in
/// which it has not yet established occupant counts
/// ([`hold_released`]); or the count a walk of the block's survivor list
/// keeps while it reads it, so that no free of another thread's can put the
/// block in the pool under the walk
/// (`crate::cycle::deferred_slot_reuse`). Counted rather than flagged,
/// because one block can hold the payloads and lists of several others and
/// each is spent on its own.
///
/// **Change this, change `cycle::deferred_slot_reuse::dispose_marks_of` too:**
/// that walk takes its count between naming the block it can be resumed
/// inside and its first disposal, and a panic site added here would be
/// repeated by the resumed walk inside an unwind, where a second panic
/// aborts.
///
/// # Safety
/// As [`count_word`].
pub(crate) unsafe fn pin(block: usize) {
    unsafe { (*count_word(block)).fetch_add(HOLD, Ordering::AcqRel) };
}

/// One occupant of retained `block` has been freed. **True** when
/// nothing holds the block afterwards, in which case the caller owes it
/// to the pool through [`release_emptied`].
///
/// False while another occupant, a pinned payload or a list of another
/// block still holds it. The value the decrement returns is the whole
/// answer, on whichever thread performs the free.
///
/// A count already at zero is a caller's mistake and is asserted on in
/// test builds; the reset returns an already-empty block through a
/// sentinel of its own (`stdapi::ll_free`'s retained arm) and never
/// through here. Double-free protection is not this counter's job —
/// `ll_free` asserts on the refcount word in test builds, one layer down.
///
/// # Safety
/// As [`count_word`].
#[must_use = "true means the block is empty and the caller owes it to the pool"]
pub(crate) unsafe fn occupant_freed(block: usize) -> bool {
    let before = unsafe { (*count_word(block)).fetch_sub(OCCUPANT, Ordering::AcqRel) };
    debug_assert!(
        before & OCCUPANTS != 0,
        "an occupant died in a retained block that counted none"
    );
    before - OCCUPANT == 0
}

/// One payload the block was pinned for has been freed — the death event
/// the bytes were said to lack, which is the owning entity's own free
/// reaching `buffer_arena::buffer_free_longlived_payload` and finding a
/// retained block under the pointer. **True** when nothing holds the
/// block afterwards, with the same duty on the caller as
/// [`occupant_freed`].
///
/// # Safety
/// As [`count_word`], and the block must have been pinned for the payload.
#[must_use = "true means the block is empty and the caller owes it to the pool"]
pub(crate) unsafe fn payload_freed(block: usize) -> bool {
    unsafe {
        spend_hold(
            block,
            "a payload died in a retained block that was not pinned for it",
        )
    }
}

/// Spend one hold of `block` ([`pin`]). **True** when nothing else holds
/// the block, and the list is then left standing rather than dropped, as
/// [`register`] leaves it — the caller returns the block through
/// `ll_free(block)`, which answers off the count word.
///
/// Two callers hold for a window rather than for a thing: the reset, over
/// the span in which an occupant count cannot yet be established for the
/// block (module doc), and the walk that returns dead-in-place marks, over
/// the span in which it reads the block's survivor list
/// (`crate::cycle::deferred_slot_reuse`).
///
/// A zero low half here means nothing counted holds the block, which
/// covers a block whose occupants all died inside the reset and one that
/// never held an occupant at all.
///
/// # Safety
/// As [`count_word`], and the caller must hold a count on the block.
#[must_use = "true means the block is empty and the caller owes it to the pool"]
pub(crate) unsafe fn hold_released(block: usize) -> bool {
    unsafe { spend_hold(block, "the reset released a pin it never took") }
}

/// Spend one count of the high half. The one of the release functions
/// where a miscount ends at the block pool rather than at `false`:
/// spending a hold that was never taken reports a block with live bytes
/// or a live list in it empty, so the pre-value is asserted on.
///
/// # Safety
/// As [`count_word`].
unsafe fn spend_hold(block: usize, what: &str) -> bool {
    let before = unsafe { (*count_word(block)).fetch_sub(HOLD, Ordering::AcqRel) };
    debug_assert!(before >> 32 != 0, "{what}");
    before - HOLD == 0
}

/// Hand a retained block that nothing holds any more back to the pool,
/// and spend the hold its survivor list has on the block the list stands
/// in.
///
/// The hold is spent **before** this block is restamped and put, so no
/// list ever names a returned block, and a holder that was held for
/// nothing else returns the same way in the same call
/// (`rfc/model/gc/rc-cycle.md`, "The survivor list of a retained
/// block"). The kind is stamped first, so a block reaching the pool
/// never carries the retained kind into its next life.
///
/// # Safety
/// `block` is a retained block whose count word reads zero in both
/// halves: [`occupant_freed`], [`payload_freed`] or
/// [`hold_released`] has just answered true for it, or [`register`]
/// did and the reset is returning it through its sentinel.
pub(crate) unsafe fn release_emptied(block: usize) {
    let (list, _) = unsafe { block_survivor_list(block as *mut u8) };
    let holder = if list.is_null() {
        block
    } else {
        BlockHeader::of_ptr(list as *const u8) as usize
    };

    let holder_emptied = holder != block
        && unsafe { spend_hold(holder, "a survivor list stood in a block not held for it") };

    let header = block as *mut BlockHeader;
    unsafe { store_block_kind(&raw const (*header).kind, BLOCK_KIND_FREE) };
    BlockPool::global().put(header);

    if holder_emptied {
        unsafe { release_emptied(holder) };
    }
}

/// Whether a survivor's slot holds a live entity, which `register` counts
/// through and which is the only thing this module reads through an
/// address.
///
/// `heap::for_each_entity_slot` asks the same question of these same
/// addresses through the same predicate rather than through this
/// function, so the two answer alike without one calling the other.
///
/// The state comes through `refcount`'s predicate rather than as a word of
/// its own, because the addresses in a list are promoted survivors —
/// published GC-heap headers whose byte 6 a collector writes
/// (`dev/DECISIONS.md`, "the header's access width is a correctness
/// rule"). `heap::for_each_entity_slot` applies this same test to these
/// same addresses, so the two must read at one width and answer alike.
///
/// **A survivor marked dead in place would be counted dead here**, and
/// `register` would then return a block to the pool with the mark still
/// standing on it. No such survivor can reach this call, and the ordering
/// is what says so: a mark is taken only where a trace has stamped the
/// block (`crate::cycle::deferred_slot_reuse`,
/// `classify_past_the_region`), while
/// this call runs at retention, over a collector line `promote::retain_block`
/// has just zeroed and before any trace can address the block. The
/// assertion below is the guard of that ordering.
///
/// # Safety
/// `address` must be readable at its first eight bytes, which is the count
/// and the mutator's half of the flags.
unsafe fn is_occupied(address: usize) -> bool {
    let state = unsafe { crate::refcount::slot_state(address as *const crate::refcount::RcHeader) };
    debug_assert_ne!(
        state,
        crate::refcount::SlotState::DeadInPlace,
        "a survivor being registered carries a dead-in-place mark, which means a trace \
         stamped this block before its list was published"
    );
    state == crate::refcount::SlotState::Live
}

/// Whether `block` counts a live occupant. A reset in flight asks it about
/// a block it retained, whose count it has not established yet, to tell
/// its own zero-count member from an occupant an earlier reset counted
/// (`memory::reset_window::absorbs_retained_free`).
///
/// The count and not the list: a block pinned for bytes alone never gets
/// a list, and one whose reset could place no list still counts its
/// occupants, and both keep counting their deaths.
///
/// # Safety
/// As [`count_word`].
pub(crate) unsafe fn has_live_occupants(block: usize) -> bool {
    // Relaxed: the word's own value is the whole answer and nothing is
    // read behind it; a free that is not absorbed goes on to the
    // decrement, which synchronises on its own.
    let word = unsafe { (*count_word(block)).load(Ordering::Relaxed) };
    word & OCCUPANTS != 0
}

/// How many live occupants `block` counts, which is the second
/// denominator of a traced-slot density (`PLAN.md` S40.1).
///
/// The low half of the same word [`has_live_occupants`] tests, and a
/// different number from [`occupant_count`]: the survivor list is the
/// index space the reset wrote once and the count word is what is alive
/// in it now, so the two separate at the first death inside a retained
/// block.
///
/// # Safety
/// As [`count_word`].
#[cfg(test)]
pub(crate) unsafe fn live_occupant_count(block: usize) -> u32 {
    // Relaxed, as `has_live_occupants` loads it: the value is the whole
    // answer and nothing is read behind it.
    let word = unsafe { (*count_word(block)).load(Ordering::Relaxed) };
    (word & OCCUPANTS) as u32
}

/// Whether nothing holds `block`: no live occupant, no pinned payload, no
/// list of another block, no count of the reset's own. What the reset's
/// return of an emptied block asserts before releasing it.
///
/// # Safety
/// As [`count_word`].
pub(crate) unsafe fn holds_nothing(block: usize) -> bool {
    let word = unsafe { (*count_word(block)).load(Ordering::Relaxed) };
    word == 0
}

/// Whether the reset that retained `block` published a survivor list for
/// it. False for a block held for bytes alone and for one whose reset
/// could place no list.
///
/// # Safety
/// As [`count_word`].
pub(crate) unsafe fn has_survivor_list(block: usize) -> bool {
    !unsafe { block_survivor_list(block as *mut u8) }.0.is_null()
}

/// How many things beyond its occupants `block` is held for
/// ([`pin`]): its pinned payloads, and a list of another block standing
/// in it. Zero for a block nobody pinned. What a test asks instead of
/// the block's kind, and why (`dev/DECISIONS.md`, "the arena carry is
/// the group's sixth member, and a refusal answers the bytes it left
/// behind").
///
/// # Safety
/// As [`count_word`].
#[cfg(test)]
pub(crate) unsafe fn pin_count(block: usize) -> usize {
    (unsafe { (*count_word(block)).load(Ordering::Relaxed) } >> 32) as usize
}

/// How many occupants retained block `block` lists, which is the size of
/// the index space [`occupant_index`] answers in — and so the number of
/// shadow rows the block needs (`crate::cycle::arena`).
///
/// `None` for a block with no list, which is a block held for bytes
/// alone ([`pin`]), one whose reset has not published it yet, or one
/// whose reset could place no list. The collector treats that as an edge
/// it cannot place, the same answer [`occupant_index`] gives it.
///
/// The count is stable for as long as a trace holds it: a list is
/// written once, by the reset that retained the block, and a block
/// cannot leave the pool for an arena while a trace can still address
/// it (`rfc/model/gc/rc-cycle.md`, "Zero-count entities pending slot
/// reuse").
///
/// # Safety
/// As [`count_word`].
pub(crate) unsafe fn occupant_count(block: usize) -> Option<usize> {
    let (list, count) = unsafe { block_survivor_list(block as *mut u8) };
    if list.is_null() {
        return None;
    }

    Some(count)
}

/// Where `addr` sits in retained block `block`'s survivor list, which
/// is the slot index the collector's shadow row array is keyed by: a
/// bump-filled block has mixed sizes and no stride, so position in the
/// sorted list stands in for the arithmetic an entity block's row uses
/// (`rfc/model/gc/rc-cycle.md`, "Where the shadow count lives").
///
/// `None` for an address the list does not name and for a block with
/// no list ([`occupant_count`]). The collector reads that as an external
/// live reference rather than as an error, which is the conservative
/// direction: an edge whose row cannot be found keeps its referent alive
/// instead of reading it as unreachable.
///
/// Two header loads and a binary search, and no lock: the words are the
/// block's own, published once by the reset that retained it.
///
/// # Safety
/// As [`count_word`].
pub(crate) unsafe fn occupant_index(block: usize, addr: usize) -> Option<usize> {
    let (list, count) = unsafe { block_survivor_list(block as *mut u8) };
    if list.is_null() {
        return None;
    }

    unsafe { std::slice::from_raw_parts(list, count) }
        .binary_search(&addr)
        .ok()
}

/// A copy of `block`'s survivor list, empty for a block with no list.
/// The same words the dispatch binary-searches, read another way, for a
/// test that wants to disagree with the search.
///
/// # Safety
/// As [`count_word`].
#[cfg(test)]
pub(crate) unsafe fn survivor_list_copy(block: usize) -> Vec<usize> {
    let (list, count) = unsafe { block_survivor_list(block as *mut u8) };
    if list.is_null() {
        return Vec::new();
    }

    unsafe { std::slice::from_raw_parts(list, count) }.to_vec()
}

/// The block `block`'s survivor list stands in — `block` itself when the
/// list is in its own tail — or zero for a block with no list. What a
/// test of the reset's placement reads.
///
/// # Safety
/// As [`count_word`].
#[cfg(test)]
pub(crate) unsafe fn survivor_list_holder(block: usize) -> usize {
    let (list, _) = unsafe { block_survivor_list(block as *mut u8) };
    if list.is_null() {
        return 0;
    }

    BlockHeader::of_ptr(list as *const u8) as usize
}

/// How many blocks of every carved region read `BLOCK_KIND_RETAINED`,
/// which is the number a leak of one looks like from outside. Reads the
/// kind of every block unsynchronised, so it is a test's question under
/// the block pool's test guard.
#[cfg(test)]
pub(crate) fn retained_block_count() -> usize {
    use crate::memory::block_pool::{BLOCK_KIND_RETAINED, BLOCK_SIZE, BLOCKS_PER_REGION};
    let mut count = 0;
    BlockPool::global().for_each_region(|region| {
        for i in 0..BLOCKS_PER_REGION {
            let block = unsafe { region.add(i * BLOCK_SIZE) } as *mut BlockHeader;
            let kind =
                unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) };
            if kind == BLOCK_KIND_RETAINED {
                count += 1;
            }
        }
    });

    count
}

/// A pool block commissioned as a retained block the way the reset
/// commissions one ([`commission_retained_block`]), holding nothing yet.
/// The caller owes it back to the pool through [`release_emptied`] once
/// nothing holds it.
#[cfg(test)]
pub(crate) fn bare_retained_block() -> usize {
    let block = BlockPool::global().get();
    assert!(!block.is_null(), "the pool refused a block");
    unsafe { commission_retained_block(block as usize) };
    block as usize
}

/// Commission `block` as a retained block the way the reset does — its
/// collector line cleared, then stamped `BLOCK_KIND_RETAINED` — so that
/// it holds nothing and lists nothing afterwards, whatever its line held
/// before (`promote::retain_block`).
///
/// # Safety
/// `block` is the header of a mapped block the caller holds, which no
/// trace can address.
#[cfg(test)]
pub(crate) unsafe fn commission_retained_block(block: usize) {
    let header = block as *mut BlockHeader;
    unsafe {
        crate::memory::heap::clear_collector_line(block as *mut u8);
        store_block_kind(
            &raw const (*header).kind,
            crate::memory::block_pool::BLOCK_KIND_RETAINED,
        );
    }
}

#[cfg(test)]
mod tests;
