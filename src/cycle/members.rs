//! The list of entities a pressure collection takes out of its rows before it
//! gives the memory back.
//!
//! A collection off the poll needs no list at all: it holds its arena through
//! the teardown and reads the rows directly. A collection an allocation
//! failure started cannot, because the destructors it is about to run need the
//! memory the rows stand in — so the sweep that nulls every block's shadow
//! pointer writes the addresses it will tear down into a fixed region of the
//! thread's workspace, and the blocks go back behind it (`dev/DECISIONS.md`,
//! "the member list is the pressure path's alone").
//!
//! # The region, and what it costs
//!
//! One 64-byte control line and [`MEMBER_CAPACITY`] eight-byte records, the
//! second fixed region of the workspace after the withheld returns' control
//! line ([`crate::cycle::arena`]). The bytes are the thread's for its life, so
//! a harvest asks the memory manager for nothing and cannot be refused; what
//! it costs is bump the rows would otherwise have, and the figure is pinned
//! beside the prefix it makes.
//!
//! # Why the capacity is fixed and never grows
//!
//! A growth here would ask the allocation path whose refusal started the
//! collection. So the region takes what it holds and the sweep stops:
//! nothing is torn down, every candidate bit stands, and the roots are the
//! next trace's, which under pressure follows on the memory this teardown
//! returned.
//!
//! **A trace's whole set fits or none of it does.** The scan leaves a row
//! `PotentiallyUnreachable` only where no live referrer raised it
//! (`crate::cycle::scan`), so every referrer of such an entity carries that
//! colour too: a prefix of the set is not a set that can be torn down, its
//! members being named by the entities left behind. An overflow therefore
//! empties the list rather than handing back what fitted, and the driver
//! traces again over fewer roots (`PLAN.md` S36.7).
//!
//! # What holds the list between the sweep and the teardown
//!
//! A thread-local pointer at the control line, set at the arming and cleared
//! when the driver drops the [`StandingMembers`] it took. The window that
//! filled the list is gone by then — it is `ActiveTrace`'s drop that sweeps —
//! and the workspace outlives every window, so the records stand until the
//! driver has read them. A collection that a destructor of that teardown
//! starts is refused an arming by that same pointer, and its sweep gates on
//! **its own** arena's flag rather than on this one: the list stands for the
//! whole teardown, so a sweep that asked "is a list armed on this thread"
//! would append to another frame's
//! (`crate::cycle::arena::TraceScratchArena::arm_harvest`).

use std::cell::Cell;

use crate::refcount::RcHeader;

/// Records the region holds, and the most a single trace may take out of one
/// heap.
///
/// Derived rather than round: the lower bound is two median closures of the corpus
/// this collector is sized against, 381 entities each (`PLAN.md` S37), and the
/// ceiling is the three widest row arrays the bump must still hold beside it.
/// What revises it is a measurement of a real pressure collection, which
/// `PLAN.md` S40.1 takes.
pub(crate) const MEMBER_CAPACITY: u32 = 1_024;

/// The head of the list and the words the sweep reads beside it, resident in
/// the region of the workspace it stands in.
///
/// `Cell` rather than a lock: the writer is the thread whose sweep is running,
/// and it is the only thread that may address these bytes at all.
///
/// One 64-byte line of its own, so the records begin on a line and an append
/// writes no line a reader is on.
#[repr(C, align(64))]
struct MemberControl {
    /// Records written since the arming, and the length of the list a driver
    /// reads. Zero after an overflow, which is what makes a refused harvest
    /// indistinguishable from one that met nothing — [`overflowed`] is the
    /// word that tells them apart.
    ///
    /// [`overflowed`]: MemberControl::overflowed
    fill: Cell<u32>,
    /// What this arming takes before it gives up, never above
    /// [`MEMBER_CAPACITY`]. A word rather than the constant, so a case can
    /// stage an overflow without writing a thousand entities.
    capacity: Cell<u32>,
    /// Whether the sweep met more than the capacity holds.
    overflowed: Cell<bool>,
}

const _: () = assert!(size_of::<MemberControl>() == 64);
const _: () = assert!(align_of::<MemberControl>() == 64);

/// Bytes the member list takes out of the workspace: its control line and the
/// records behind it.
pub(crate) const MEMBERS_BASE_BYTES: usize =
    size_of::<MemberControl>() + MEMBER_CAPACITY as usize * size_of::<*mut RcHeader>();

thread_local! {
    /// The control line of this thread's armed or standing list, or null while
    /// neither. A `Cell` of a raw pointer and therefore no drop glue, which
    /// the thread-exit path requires of everything it can reach
    /// (`crate::memory::heap::ll_thread_exit`).
    static MEMBER_LIST: Cell<*mut MemberControl> = const { Cell::new(std::ptr::null_mut()) };
    /// Whether the driver holds the [`StandingMembers`] of the list above, so
    /// that a second take answers `None` rather than a second reader of one
    /// region.
    static MEMBER_LIST_HELD: Cell<bool> = const { Cell::new(false) };
}

/// Arm a harvest over `region`, so that the next sweep of this thread writes
/// its unreachable rows there.
///
/// **False when a list is already armed or still standing**, which is a
/// collection nested inside the teardown of another: the region holds one
/// list, and the outer driver has not finished reading it. Such a collection
/// traces and sweeps as an ordinary one does, harvesting nothing.
///
/// `capacity` is what this harvest takes before it gives up, and a capacity
/// above [`MEMBER_CAPACITY`] is a caller error rather than a clamp.
///
/// # Safety
/// `region` addresses [`MEMBERS_BASE_BYTES`] writable bytes, aligned to 64 and
/// owned by this thread until the list is released — which is the workspace's
/// member region and nothing else.
pub(crate) unsafe fn arm(region: *mut u8, capacity: u32) -> bool {
    assert!(
        capacity <= MEMBER_CAPACITY,
        "a harvest may not ask for more records than the region holds"
    );

    if !MEMBER_LIST.with(Cell::get).is_null() {
        return false;
    }

    let control = region as *mut MemberControl;
    // Field by field and written rather than assigned: the workspace arrives
    // from the pool with whatever its last owner left in it, so an assignment
    // would drop a `MemberControl` that was never constructed.
    unsafe {
        (&raw mut (*control).fill).write(Cell::new(0));
        (&raw mut (*control).capacity).write(Cell::new(capacity));
        (&raw mut (*control).overflowed).write(Cell::new(false));
    }

    MEMBER_LIST.with(|list| list.set(control));
    MEMBER_LIST_HELD.with(|held| held.set(false));
    true
}

/// Whether a list is armed or standing on this thread.
///
/// What a sweep asks beside its own state: a collection that armed one still
/// answers false here if a driver took and released the list before the close,
/// and appending to that is a write through a null control line.
pub(crate) fn is_armed() -> bool {
    !MEMBER_LIST.with(Cell::get).is_null()
}

/// Append `entity` to the armed list, and answer **false when it is full** —
/// the sweep's signal to stop reading rows and finish nulling pointers.
///
/// The overflow is remembered rather than counted: what the driver does with
/// it is trace again, and how far past the capacity this trace would have gone
/// says nothing about how far the next one will.
///
/// # Safety
/// The caller's own collection armed this list ([`arm`] answered true for it)
/// and has not ended its harvest.
pub(crate) unsafe fn push(entity: *mut RcHeader) -> bool {
    let control = MEMBER_LIST.with(Cell::get);
    debug_assert!(
        !control.is_null(),
        "a harvest appends only while it is armed"
    );

    let fill = unsafe { (*control).fill.get() };
    if fill == unsafe { (*control).capacity.get() } {
        unsafe { (*control).overflowed.set(true) };
        return false;
    }

    unsafe { records(control).add(fill as usize).write(entity) };
    unsafe { (*control).fill.set(fill + 1) };
    true
}

/// Give the whole harvest up, which is what a row the dispatch cannot place
/// costs: the list is emptied at [`end_harvest`] and the driver reads the
/// refusal the way it reads an overflow.
///
/// # Safety
/// As [`push`].
pub(crate) unsafe fn abandon() {
    let control = MEMBER_LIST.with(Cell::get);
    debug_assert!(
        !control.is_null(),
        "a harvest is given up only while it is armed"
    );

    unsafe { (*control).overflowed.set(true) };
}

/// End the harvest the sweep was running: an overflowed or abandoned list is
/// emptied, so that no driver tears down a part of a set.
///
/// # Safety
/// As [`push`].
pub(crate) unsafe fn end_harvest() {
    let control = MEMBER_LIST.with(Cell::get);
    debug_assert!(
        !control.is_null(),
        "a harvest ends only where one was armed"
    );

    if unsafe { (*control).overflowed.get() } {
        unsafe { (*control).fill.set(0) };
    }
}

/// The list this thread's last sweep filled, or **`None` when no harvest was
/// armed or the driver already holds it**.
///
/// The records stand until the answer is dropped, and the region takes no
/// second arming for as long as it lives.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the teardown that reads the list is `PLAN.md` S36.7's"
    )
)]
pub(crate) fn take_standing() -> Option<StandingMembers> {
    let control = MEMBER_LIST.with(Cell::get);
    if control.is_null() || MEMBER_LIST_HELD.with(Cell::get) {
        return None;
    }

    MEMBER_LIST_HELD.with(|held| held.set(true));
    Some(StandingMembers { control })
}

/// The members one pressure collection harvested, held by the driver that is
/// about to tear them down.
///
/// Dropping it releases the region for the next collection, so it is held for
/// exactly as long as the teardown reads the list.
pub(crate) struct StandingMembers {
    control: *mut MemberControl,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the teardown that reads the list is `PLAN.md` S36.7's"
    )
)]
impl StandingMembers {
    /// The entities the sweep took, in the order it met them: block by block
    /// of the touched list, and by ascending row inside each block.
    ///
    /// **Empty after an overflow**, which [`overflowed`](Self::overflowed)
    /// tells apart from a trace that met nothing unreachable.
    pub(crate) fn entities(&self) -> &[*mut RcHeader] {
        let fill = unsafe { (*self.control).fill.get() } as usize;
        unsafe { std::slice::from_raw_parts(records(self.control), fill) }
    }

    /// Whether the trace met more unreachable entities than the region holds,
    /// which is the driver's signal to trace again over fewer roots rather
    /// than to tear anything down.
    pub(crate) fn overflowed(&self) -> bool {
        unsafe { (*self.control).overflowed.get() }
    }
}

impl Drop for StandingMembers {
    fn drop(&mut self) {
        MEMBER_LIST.with(|list| list.set(std::ptr::null_mut()));
        MEMBER_LIST_HELD.with(|held| held.set(false));
    }
}

/// Refuse a thread exit that would leave a member list behind, called from
/// `heap::ll_thread_exit` beside the trace window's own refusal.
///
/// A list standing here names records inside the workspace block the exit is
/// about to hand to the pool, and its driver is a frame that will never run
/// again. The release profile ends the process on it, which is the same answer
/// the window gives (`crate::cycle::deferred_slot_reuse::dispose_thread_state`).
pub(crate) fn dispose_thread_state() {
    assert!(
        MEMBER_LIST.with(Cell::get).is_null(),
        "a thread cannot exit holding a harvested member list"
    );
}

/// The records, which begin where the control line ends.
///
/// # Safety
/// `control` is the control line of an armed region.
#[inline]
unsafe fn records(control: *mut MemberControl) -> *mut *mut RcHeader {
    unsafe { (control as *mut u8).add(size_of::<MemberControl>()) as *mut *mut RcHeader }
}

#[cfg(test)]
mod tests;
