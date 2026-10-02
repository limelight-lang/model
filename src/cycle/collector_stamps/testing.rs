//! What the cases over the collector's stamps read and set: the stamps
//! written and what they cost, and a hook run inside the walk.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Stamps the collectors wrote since a case last read it; process-wide,
/// because they are written on a collector's thread and the cases that read
/// them hold the pool's test guard.
static STAMPS: AtomicUsize = AtomicUsize::new(0);

/// Note one stamp written on `entity`, and run the hook a case installed when
/// its count is reached; true when it ran.
pub(super) fn note_stamped(entity: *mut crate::refcount::RcHeader) -> bool {
    STAMPS.fetch_add(1, Ordering::Relaxed);
    if let Some(order) = ORDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_mut()
    {
        order.push(entity as usize);
    }
    crate::cycle::worker::testing::note_stamped(crate::refcount::is_registered_candidate(unsafe {
        crate::refcount::mutator_flags(entity)
    }));
    after_a_stamped_row()
}

/// The entities stamped since a case began recording, in the order the walks
/// stamped them; `None` while nobody records.
static ORDER: Mutex<Option<Vec<usize>>> = Mutex::new(None);

/// Record the order of the stamps from here on.
pub(crate) fn record_the_order() {
    *ORDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Vec::new());
}

/// The entities stamped since [`record_the_order`], in order, and stop
/// recording.
pub(crate) fn take_the_order() -> Vec<usize> {
    ORDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
        .unwrap_or_default()
}

/// Stamps the collectors wrote since the last call, which leaves zero.
pub(crate) fn take_stamps() -> usize {
    STAMPS.swap(0, Ordering::Relaxed)
}

/// What the stamping costs the collector, per batch that ran it
/// (`dev/plans/S67.md`, S67.9, the Critic of 2026-09-30 on the collector
/// writing the stamps, finding 3): the walks, their stamps in all and at the
/// most, and the wall in all and at the longest.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Stamping {
    pub(crate) walks: usize,
    pub(crate) stamps: usize,
    pub(crate) stamps_most: usize,
    pub(crate) wall: std::time::Duration,
    pub(crate) longest: std::time::Duration,
}

static STAMPING: Mutex<Stamping> = Mutex::new(Stamping {
    walks: 0,
    stamps: 0,
    stamps_most: 0,
    wall: std::time::Duration::ZERO,
    longest: std::time::Duration::ZERO,
});

/// Note one walk's stamping, `stamps` of them in `wall`.
pub(super) fn note_stamping(stamps: usize, wall: std::time::Duration) {
    let mut stamping = STAMPING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    stamping.walks += 1;
    stamping.stamps += stamps;
    stamping.stamps_most = stamping.stamps_most.max(stamps);
    stamping.wall += wall;
    stamping.longest = stamping.longest.max(wall);
}

/// The stamping since the last call, which leaves it zero.
pub(crate) fn take_stamping() -> Stamping {
    std::mem::take(
        &mut *STAMPING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
}

/// The stamp after which the next walk runs the hook a case installed,
/// counted over the stamps written since the installation, with the hook;
/// process-wide, the case holding the pool's test guard.
static AFTER_STAMPS: Mutex<Option<(usize, Box<dyn FnOnce() + Send>)>> = Mutex::new(None);

/// Stamps written since the hook was installed.
static STAMPS_TOWARD_THE_HOOK: AtomicUsize = AtomicUsize::new(0);

/// Run `act` on the collector's thread once the next walks have written
/// `stamps` stamps, inside the walk.
pub(crate) fn after_stamped_rows(stamps: usize, act: Box<dyn FnOnce() + Send>) {
    STAMPS_TOWARD_THE_HOOK.store(0, Ordering::Relaxed);
    *AFTER_STAMPS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((stamps, act));
}

/// Count one stamp toward the installed hook, and run it when its count is
/// reached; true when it ran.
fn after_a_stamped_row() -> bool {
    let mut installed = AFTER_STAMPS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some((stamps, _)) = installed.as_ref() else {
        return false;
    };

    if STAMPS_TOWARD_THE_HOOK.fetch_add(1, Ordering::Relaxed) + 1 < *stamps {
        return false;
    }

    let (_, act) = installed.take().expect("read above");
    drop(installed);
    act();
    true
}
