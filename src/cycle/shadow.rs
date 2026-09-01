//! What one shadow row holds, and how a block's rows are laid out in the
//! collection's arena.
//!
//! A row is four bytes: two bits of colour and thirty of working count
//! (`rfc/model/gc/rc-cycle.md`, "Where the shadow count lives"). The
//! count starts at the entity's refcount and the trace subtracts the
//! internal edges it finds, so a row that still reads above zero when
//! the scan reaches it is held from outside the traced component.
//!
//! # Why the colour has a code for "untouched"
//!
//! The rows arrive dirty, and the array is never zeroed as a whole: at
//! the smallest size class that would be 16 320 bytes per touched block,
//! and 41–76 ms over the design's 717 MiB case against the bitmap's
//! 1.4 ms (`rfc/model/gc/rc-cycle.md`, "The rows are not zeroed
//! greedily"). What is zeroed instead is a **group of eight rows**
//! at its own first touch, which needs a bit per group to say whether
//! that has happened, and the zero the group init writes has to mean
//! "this slot has not been met". So colour zero is reserved for it, and
//! every meeting writes a colour that is not zero — which is what keeps
//! a met, unreachable, zero-count row distinguishable from a slot the
//! trace never reached, and what stops a second reach re-initialising a
//! row from the refcount it has already subtracted from.
//!
//! # The layout
//!
//! One allocation per touched block, out of the collection's arena:
//!
//! ```text
//! +0   block      the block header this array belongs to
//! +8   next       the touched list, newest first
//! +16  row_count  rows the index space holds, the bounds check
//! +20  population what the sweep owes this block
//! +24  rows       4 bytes each, dirty until their group is met
//! +24+4*R  groups one bit per group of eight rows, zeroed at allocation
//! ```
//!
//! `R` is `row_count` rounded up to a whole group, so a group init writes
//! eight rows without ever running past the last one: the bitmap begins
//! after the rounding and not after `row_count`.
//!
//! The touched list threads through the arrays themselves rather than
//! through runs of its own, so attaching a block to the sweep list and
//! reserving its rows are one allocation and one refusal: after the rows
//! exist, nothing about the attachment can fail (`crate::cycle::arena`,
//! "Enrolment cannot fail after the rows exist").
//!
//! **The arrays are the arena's, for one collection.** This module writes
//! their layout and reads their words; it holds no memory, allocates none and
//! frees none, so every address it hands back is live exactly as long as the
//! trace that reserved it (`rfc/model/gc/rc-cycle.md`, "Concurrency").
//!
//! Rows come before the bitmap so that a row's address is
//! `array + 24 + 4 × index`, one multiply-add. The bitmap's width varies
//! with the block's slot count, so putting it first would put that width
//! into the arithmetic every edge performs; behind the rows it is found
//! by a computation of its own, on the path that runs once per group.

use crate::cycle::row::Population;

/// Bits of one row given to the working count. The rest are the colour.
const COUNT_BITS: u32 = 30;

/// The largest working count a row can hold, and **"at least this many"
/// rather than "exactly this many"**.
///
/// A refcount above it is met at this value, so the row says the entity
/// has external references without saying how many, and
/// [`is_saturated`] is how the trace asks. The clause that follows is
/// that a saturated row is conservatively live and stays so: subtracting
/// an edge leaves it saturated ([`subtract`]), because the count it
/// holds is a lower bound and not a total (`rfc/model/gc/rc-cycle.md`,
/// "Saturation is absorbing, and that is a rule for every stage that
/// touches a row").
///
/// Without the clause the entity is readable as unreachable: a refcount of
/// `2^31` meets at `2^30 - 1`, and a trace that finds `2^30` internal edges to
/// it drives the row to zero while a billion external references stand. That
/// heap is 16 GiB at the smallest size class, so the case is reachable on a
/// large machine rather than absurd.
pub(crate) const COUNT_MAX: u32 = (1 << COUNT_BITS) - 1;

/// What the trace has decided about one entity, and the reserved zero
/// that says it has decided nothing.
///
/// No working code is zero, so a group the init has just cleared reads as
/// [`Untouched`](Color::Untouched) for every slot in it.
/// [`Unclassified`](Color::Unclassified) is written by
/// [`TraceScratchArena::ensure_row`](crate::cycle::arena::TraceScratchArena::ensure_row),
/// and the two verdicts by [`crate::cycle::scan`], which is the only writer
/// that reads a colour before it writes one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub(crate) enum Color {
    /// The slot has not been met in this collection. The value a group
    /// carries when its bit is set and nothing has been written into it
    /// since, and the value a large entity's block header carries from
    /// its commissioning.
    Untouched = 0,
    /// Unclassified: the working count is this entity's refcount less the
    /// internal edges the trace has subtracted so far.
    Unclassified = 1,
    /// The scan reached it with a zero working count and no live
    /// referrer, so it is a member of a candidate component.
    PotentiallyUnreachable = 2,
    /// The scan reached it from a row that had a live external
    /// reference, so the component it belongs to survives.
    Live = 3,
}

/// The colour of `row`.
#[inline]
pub(crate) fn color(row: u32) -> Color {
    match row >> COUNT_BITS {
        0 => Color::Untouched,
        1 => Color::Unclassified,
        2 => Color::PotentiallyUnreachable,
        _ => Color::Live,
    }
}

/// The working count of `row`, which carries meaning only while the
/// colour is not [`Color::Untouched`].
#[inline]
pub(crate) fn count(row: u32) -> u32 {
    row & COUNT_MAX
}

/// Whether `row`'s working count is a lower bound rather than a total, which
/// makes the entity conservatively live whatever the trace subtracts
/// from it ([`COUNT_MAX`]).
///
/// One name for the reading, so the clause is not spelled out as a
/// comparison at each site. The scan asks it of no row: it colours a working
/// count of zero potentially unreachable, and a lower bound of [`COUNT_MAX`]
/// is above zero, so
/// the absorbing clause is kept there by the same test that reads an
/// ordinary count.
#[inline]
pub(crate) fn is_saturated(row: u32) -> bool {
    count(row) == COUNT_MAX
}

/// A row of this colour and this count, the count **saturated** at
/// [`COUNT_MAX`] rather than wrapped into the colour.
#[inline]
pub(crate) fn compose(color: Color, count: u32) -> u32 {
    ((color as u32) << COUNT_BITS) | count.min(COUNT_MAX)
}

/// Take one internal edge off `row`'s working count, keeping its colour.
/// **The count stops at zero** rather than wrapping, and a test build
/// fails on a subtraction that would pass it.
///
/// The subtraction is what the mark does per edge, and it is written
/// here rather than at the call site because the open-coded form —
/// `compose(colour(r), count(r) - 1)` — turns a count of zero into
/// `u32::MAX`, which [`compose`] then clamps to [`COUNT_MAX`]: a row
/// that should read unreachable would read maximally live, and the ring it
/// belongs to would survive every collection.
///
/// A count that reaches zero with edges left to subtract means the trace
/// read more in-edges than the refcount held. The design permits that of
/// a dirty pass, because the counts it reads may be stale, and the exact
/// test on the owner's thread is what turns a candidate into a verdict
/// (`rfc/model/gc/rc-cycle.md`, "Speculative tracing and exact
/// validation"); so a release build clamps at zero. The only trace today
/// is the in-line owner trace, which reads exact counts ("Synchronous
/// collection is exact by construction", same document) and cannot find
/// more in-edges than the refcount holds, so a test build asserts `count
/// >= edges` before the subtraction: a double subtraction fails the suite
/// rather than clamping (`dev/CYCLE-COLLECTOR-REVIEW.md`, finding 6).
/// The clamp's below-zero arm is therefore reached by no test in the
/// gate's builds, since reaching it trips the assertion first.
///
/// The assertion cannot tell a speculative trace from the owner's; a
/// worker trace does not yet exist, and `PLAN.md` S38.0, where the second
/// thread arrives, is what conditions the assertion on whose pass this is.
///
/// **A saturated count is absorbing** and this call leaves it alone: it
/// is a lower bound, so what the subtraction knows about the remainder is
/// still "at least [`COUNT_MAX`]" ([`is_saturated`]).
///
/// # Safety
/// `row` is a row of a met entity, reached through
/// [`TraceScratchArena::ensure_row`](crate::cycle::arena::TraceScratchArena::ensure_row).
#[inline]
pub(crate) unsafe fn subtract(row: *mut u32, edges: u32) -> u32 {
    let word = unsafe { *row };
    if is_saturated(word) {
        return COUNT_MAX;
    }

    debug_assert!(
        count(word) >= edges,
        "a subtraction below the count: the owner trace found more in-edges \
         than the refcount holds"
    );

    let left = count(word).saturating_sub(edges);
    let updated = compose(color(word), left);
    unsafe { row.write(updated) };
    left
}

/// Give `row` this colour, keeping its working count: the scan decides
/// a colour from the count the mark left and never changes that count.
///
/// # Safety
/// `row` is a row of a met entity, reached through
/// [`TraceScratchArena::ensure_row`](crate::cycle::arena::TraceScratchArena::ensure_row)
/// or [`find_initialized_row`](crate::cycle::arena::find_initialized_row).
#[inline]
pub(crate) unsafe fn recolor(row: *mut u32, color: Color) {
    let word = unsafe { *row };
    unsafe { row.write(compose(color, count(word))) };
}

/// Rows one group covers, and the number of rows a group init writes at
/// once. Eight rows is 32 bytes, so a group init is half a cache line,
/// and one bit per group puts the whole bitmap of the widest block —
/// 4080 slots at the smallest size class — in 64 bytes.
///
/// A group is eight **consecutive rows**, and what that groups follows
/// from what the index space is. In an entity block it is eight
/// neighbouring slots, so a group init covers the 32-byte neighbourhood
/// a trace walking one object's children is likely to want next. In a
/// retained block it is eight neighbouring positions of the sorted
/// occupant index, which are eight ascending addresses of mixed size:
/// still neighbours in the block, no longer a fixed span of it. A large
/// entity has one row and no group at all.
pub(crate) const GROUP: u32 = 8;

/// The rows of one touched block, and its entry in the touched list.
///
/// The rows and the group bitmap follow this header in the same
/// allocation, which is why nothing outside this module constructs one:
/// the address of a row is arithmetic over the header's own address, and
/// a `RowArray` built anywhere but at the head of [`bytes_for`] bytes
/// names memory that was never reserved.
#[repr(C)]
pub(crate) struct RowArray {
    /// The block header these rows belong to, 64 KiB-aligned. What the
    /// sweep needs, since it walks the list rather than the heap.
    pub(crate) block: *mut u8,
    /// The next array in this collection's touched list, or null at the
    /// end of the chain.
    pub(crate) next: *mut RowArray,
    /// The block's index space, and the bound a row index is checked
    /// against: slots for an entity block, occupants for a retained
    /// one, and **zero for a large entity**, whose single row is a word
    /// of its own block header and needs no array (`crate::cycle::row`).
    /// The array reserves past this, up to a whole group ([`bytes_for`]).
    pub(crate) row_count: u32,
    /// Which of the three populations the block belongs to, so that the
    /// sweep knows which word it owes a null: the array pointer in the
    /// block's collector triple, or the large entity's row itself.
    pub(crate) population: Population,
}

/// Rows the array reserves for `row_count`, rounded up to a whole group so
/// that a group init always writes eight rows and never runs past the
/// last one.
#[inline]
const fn padded(row_count: u32) -> usize {
    (row_count as usize).next_multiple_of(GROUP as usize)
}

/// Bytes one array needs for `row_count` rows: the header, the rows, and one
/// bit per group.
///
/// Never above 16 408 bytes, at the smallest size class of a 64 KiB
/// block, so one array always fits the arena's one-block allocation
/// limit (`TraceScratchArena::alloc`).
pub(crate) const fn bytes_for(row_count: u32) -> usize {
    size_of::<RowArray>() + padded(row_count) * size_of::<u32>() + group_bytes(row_count)
}

// Bytes this thread has written into row arrays, which is what a first
// touch actually costs (tests only).
//
// Counted where the writes are rather than measured from outside, and
// per thread rather than globally, because the tests that write rows
// run beside each other. What it counts is the prologue and the bitmap
// at a block's reservation, and one group at each group's first touch;
// what the figures are and why they are counted here rather than timed
// is `dev/BENCHMARKS.md`, "what a block's first touch writes".
#[cfg(test)]
thread_local! {
    static WRITTEN_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Add `bytes` to `WRITTEN_BYTES`, and nothing at all without
/// `cfg(test)`: the two write sites call it either way.
#[inline]
fn note_written(bytes: usize) {
    #[cfg(test)]
    WRITTEN_BYTES.with(|written| written.set(written.get() + bytes));
    #[cfg(not(test))]
    let _ = bytes;
}

/// What the probe holds for this thread.
#[cfg(test)]
pub(crate) fn written_bytes() -> usize {
    WRITTEN_BYTES.with(std::cell::Cell::get)
}

/// Write the array's header and clear its group bitmap.
///
/// The rows themselves are left as the bump handed them over. Their
/// group bits say they are dirty, and a group is written before it is
/// read ([`ensure_group_initialized`]).
///
/// # Safety
/// `array` points at [`bytes_for(row_count)`](bytes_for) bytes of 8-aligned
/// scratch that outlive this collection, and `block` is the header of
/// the block those rows belong to.
pub(crate) unsafe fn init(
    array: *mut RowArray,
    block: *mut u8,
    row_count: u32,
    population: Population,
    next: *mut RowArray,
) {
    // Field by field and written rather than assigned: the arena bumps
    // memory with no value in it, so an assignment would drop a
    // `RowArray` that was never constructed.
    unsafe {
        (&raw mut (*array).block).write(block);
        (&raw mut (*array).next).write(next);
        (&raw mut (*array).row_count).write(row_count);
        (&raw mut (*array).population).write(population);
        groups(array).write_bytes(0, group_bytes(row_count));
    }

    note_written(size_of::<RowArray>() + group_bytes(row_count));
}

/// The row at `index`, whose group has been met.
///
/// # Safety
/// `array` is an initialised array and `index` is below its `row_count`.
#[inline]
pub(crate) unsafe fn row(array: *mut RowArray, index: u32) -> *mut u32 {
    unsafe { (array as *mut u8).add(size_of::<RowArray>()) as *mut u32 }
        .wrapping_add(index as usize)
}

/// Zero the whole group `index` belongs to if this is the group's first
/// touch, so that the row [`row`] hands back reads as
/// [`Color::Untouched`] rather than as whatever the block that held
/// this memory before left in it.
///
/// # Safety
/// As [`row`].
#[inline]
pub(crate) unsafe fn ensure_group_initialized(array: *mut RowArray, index: u32) {
    let group = index / GROUP;
    let (byte, bit) = unsafe { group_bit(array, index) };
    if unsafe { *byte } & bit != 0 {
        return;
    }

    unsafe {
        *byte |= bit;
        row(array, group * GROUP).write_bytes(0, GROUP as usize);
    }

    note_written(size_of::<u8>() + GROUP as usize * size_of::<u32>());
}

/// Whether the group carrying `index` has been zeroed in this
/// collection, which is what makes its row readable: an untouched
/// group's rows are whatever the block that held this memory before left
/// in them.
///
/// # Safety
/// As [`row`].
#[inline]
pub(crate) unsafe fn group_is_initialized(array: *mut RowArray, index: u32) -> bool {
    let (byte, bit) = unsafe { group_bit(array, index) };
    let bits = unsafe { *byte };
    bits & bit != 0
}

/// The bitmap byte carrying the group of `index`, and the bit inside it.
///
/// Two eights meet here and they are different numbers: [`GROUP`] rows
/// to a group, and `u8::BITS` groups to a byte of the bitmap.
///
/// # Safety
/// As [`row`].
#[inline]
unsafe fn group_bit(array: *mut RowArray, index: u32) -> (*mut u8, u8) {
    let group = index / GROUP;
    let byte = unsafe { groups(array).add((group / u8::BITS) as usize) };
    (byte, 1u8 << (group % u8::BITS))
}

/// The group bitmap, which sits past the rows.
///
/// # Safety
/// As [`row`].
#[inline]
unsafe fn groups(array: *mut RowArray) -> *mut u8 {
    let row_count = unsafe { (*array).row_count };
    unsafe { (array as *mut u8).add(size_of::<RowArray>() + padded(row_count) * size_of::<u32>()) }
}

/// Bytes the group bitmap of `row_count` rows occupies: one bit per group
/// of [`GROUP`] rows, rounded up to a whole byte.
#[inline]
const fn group_bytes(row_count: u32) -> usize {
    (padded(row_count) / GROUP as usize).div_ceil(u8::BITS as usize)
}

#[cfg(test)]
mod tests;
