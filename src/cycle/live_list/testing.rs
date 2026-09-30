//! What the cases over the live list read and set: the stamps a mutator wrote
//! from lists and where, and a bound on the chain lower than
//! [`super::MAX_BLOCKS`].

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

thread_local! {
    /// Lists this thread gave back as stale, since a case last read it.
    static STALE_LISTS_GIVEN_BACK: Cell<usize> = const { Cell::new(0) };
    /// Entries this thread stamped from lists since a case last read it.
    static STAMPS: Cell<usize> = const { Cell::new(0) };
    /// Lists this thread stamped from at a return of a block or a run rather
    /// than at a take, since a case last read it.
    static STAMPED_AT_A_RETURN: Cell<usize> = const { Cell::new(0) };
}

thread_local! {
    /// This thread's GC block figure before and after the last list it took
    /// over and gave back.
    static BLOCKS_ACROSS_A_LIST: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
}

pub(super) fn note_blocks_across_a_list(before: usize, after: usize) {
    BLOCKS_ACROSS_A_LIST.with(|figures| figures.set(Some((before, after))));
}

/// This thread's GC block figure before and after the last list it took
/// over and gave back, and `None` where it took none since the last call.
pub(crate) fn take_blocks_across_a_list() -> Option<(usize, usize)> {
    BLOCKS_ACROSS_A_LIST.with(Cell::take)
}

pub(super) fn note_stamps(entries: usize) {
    STAMPS.with(|stamps| stamps.set(stamps.get() + entries));
}

pub(super) fn note_a_stamp_at_a_return() {
    STAMPED_AT_A_RETURN.with(|lists| lists.set(lists.get() + 1));
}

thread_local! {
    /// Whether the list this thread is stamping from was taken at a return.
    static STAMPING_AT_A_RETURN: Cell<bool> = const { Cell::new(false) };
}

/// The stamping a list costs its mutator, at a take of the token and at a
/// return of a block or a run (`dev/plans/S67.md`, S67.9, run R3): the lists
/// stamped, their entries in all and at the most, and the wall in all and at
/// the longest.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Stamping {
    pub(crate) lists: usize,
    pub(crate) entries: usize,
    pub(crate) entries_most: usize,
    pub(crate) wall: std::time::Duration,
    pub(crate) longest: std::time::Duration,
}

/// At a take, and at a return.
static STAMPING: Mutex<[Stamping; 2]> = Mutex::new(
    [Stamping {
        lists: 0,
        entries: 0,
        entries_most: 0,
        wall: std::time::Duration::ZERO,
        longest: std::time::Duration::ZERO,
    }; 2],
);

/// Run `stamp` as a stamping at a return.
pub(super) fn at_a_return(stamp: impl FnOnce()) {
    STAMPING_AT_A_RETURN.with(|flag| flag.set(true));
    stamp();
    STAMPING_AT_A_RETURN.with(|flag| flag.set(false));
}

/// Note one list's stamping, `entries` of them in `wall`.
pub(super) fn note_stamping(entries: usize, wall: std::time::Duration) {
    let site = usize::from(STAMPING_AT_A_RETURN.with(Cell::get));
    let mut stamping = STAMPING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let site = &mut stamping[site];
    site.lists += 1;
    site.entries += entries;
    site.entries_most = site.entries_most.max(entries);
    site.wall += wall;
    site.longest = site.longest.max(wall);
}

/// The stampings at a take and at a return since the last call, which
/// leaves both zero.
pub(crate) fn take_stamping() -> [Stamping; 2] {
    std::mem::take(
        &mut *STAMPING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
}

/// Entries this thread stamped from lists since the last call, which leaves
/// zero.
pub(crate) fn take_stamps() -> usize {
    STAMPS.with(|stamps| stamps.replace(0))
}

/// Lists this thread stamped from at a return since the last call, which
/// leaves zero.
pub(crate) fn take_stamps_at_a_return() -> usize {
    STAMPED_AT_A_RETURN.with(|lists| lists.replace(0))
}

/// The bound a case set on the chain, zero for none. Process-wide, because
/// the chain is written on a collector's thread; the cases that set it run
/// under `memory::block_pool::test_guard`, as every case that drives a
/// collector does.
static CHAIN_BOUND: AtomicUsize = AtomicUsize::new(0);

pub(super) fn chain_bound() -> Option<usize> {
    match CHAIN_BOUND.load(Ordering::Relaxed) {
        0 => None,
        bound => Some(bound),
    }
}

/// Bound every chain to `blocks` until the guard drops.
pub(crate) fn bound_the_chain(blocks: usize) -> ChainBound {
    assert!(blocks > 0, "a chain of no block lists nothing");
    CHAIN_BOUND.store(blocks, Ordering::Relaxed);
    ChainBound
}

/// The bound [`bound_the_chain`] set, lifted at the drop, on the unwind too.
pub(crate) struct ChainBound;

impl Drop for ChainBound {
    fn drop(&mut self) {
        CHAIN_BOUND.store(0, Ordering::Relaxed);
    }
}

pub(super) fn note_a_stale_list_given_back() {
    STALE_LISTS_GIVEN_BACK.with(|lists| lists.set(lists.get() + 1));
}

/// Lists this thread gave back as stale since the last call, which leaves
/// zero.
pub(crate) fn take_stale_lists_given_back() -> usize {
    STALE_LISTS_GIVEN_BACK.with(|lists| lists.replace(0))
}

/// The row after which the next list walk runs the hook a case installed,
/// counted over that walk's listed rows on the collector's thread, with the
/// hook; process-wide, the case holding the pool's test guard.
static AFTER_ROWS: std::sync::Mutex<Option<(usize, Box<dyn FnOnce() + Send>)>> =
    std::sync::Mutex::new(None);

thread_local! {
    /// Rows the list walks on this thread listed since the hook was installed.
    static ROWS_LISTED: Cell<usize> = const { Cell::new(0) };
}

/// Run `act` on the collector's thread once the next list walks have listed
/// `rows` rows, inside the walk.
pub(crate) fn after_listed_rows(rows: usize, act: Box<dyn FnOnce() + Send>) {
    *AFTER_ROWS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((rows, act));
}

/// Count one listed row, and run the installed hook when its count is
/// reached; true when it ran.
pub(super) fn after_a_listed_row() -> bool {
    let mut installed = AFTER_ROWS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some((rows, _)) = installed.as_ref() else {
        ROWS_LISTED.with(|listed| listed.set(0));
        return false;
    };

    let listed = ROWS_LISTED.with(|listed| {
        listed.set(listed.get() + 1);
        listed.get()
    });
    if listed < *rows {
        return false;
    }

    let (_, act) = installed.take().expect("read above");
    drop(installed);
    ROWS_LISTED.with(|listed| listed.set(0));
    act();
    true
}
