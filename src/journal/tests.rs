use super::*;

/// The kind a test writes when the kind plays no part: what these
/// tests are about is the ring, the window and the registry, and no
/// assertion here reads a kind. Past every kind that has a site
/// ([`kinds`]), and at the mask's last bit, so that a record written
/// here cannot be taken for a record of some site.
const ANY_KIND: Kind = 63;

/// How many rings are live and how many retired. Tests only, and the
/// live count is what a resurrected ring shows up in.
fn registry_counts() -> (usize, usize) {
    let registry = locked();
    (registry.live.len(), registry.retired.len())
}

/// Rings evicted and waiting for a live thread to free them.
fn pending_count() -> usize {
    locked().pending_free.len()
}

/// Free one retired ring by identity, the way the quota's eviction
/// frees the oldest. Tests only: firing the quota takes
/// `RETIRED_KEPT + 1` threads and 2 MiB of rings to observe one line
/// of arithmetic, while what the tests taking this are about is what a
/// window says once a ring is gone.
fn evict_retired_ring(thread: u64) -> bool {
    let ring = {
        let mut registry = locked();
        match registry
            .retired
            .iter()
            .position(|&ring| unsafe { (*ring).thread } == thread)
        {
            Some(at) => {
                registry.evicted += 1;
                registry.retired.remove(at)
            }
            None => return false,
        }
    };

    unsafe { crate::memory::stdapi::ll_free(ring as *mut u8) };
    true
}

/// Every event the answers carry, in the order the windows came in.
fn events(windows: Vec<Window>) -> Vec<Event> {
    windows
        .into_iter()
        .flat_map(|window| match window {
            Window::Records(events) => events,
            _ => Vec::new(),
        })
        .collect()
}

/// A thread that journals one record and then exits through the whole
/// exit sequence, as a dying thread does. Returns its ring identity.
fn a_journaling_thread(subject: u64) -> u64 {
    std::thread::spawn(move || {
        crate::memory::heap::ll_thread_init();
        record(ANY_KIND, 0, subject, 0, 0);
        let identity = this_thread_identity();
        crate::memory::heap::ll_thread_exit();
        identity
    })
    .join()
    .expect("the journaling thread panicked")
}

mod a_ring_across_a_threads_life;
mod a_thread_the_journal_could_not_serve;
mod the_answer_a_window_may_not_invent;
#[cfg(feature = "debug-journal")]
mod the_hunt_the_journal_was_built_for;
mod the_ring_and_the_window_over_it;
mod where_the_retirement_sits_inside_the_exit;
