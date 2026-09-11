//! A token held from another thread, for the cases that need one: the
//! stand-in for a collector tracing this mutator's graph, until S38.0's
//! collector exists to take it itself.
//!
//! Two test trees hold a token this way — the token's own, and the entity
//! allocation's slow path, which reaches the wait through a refusal — and a
//! second copy of the holder would be a second opinion about when it lets go.
//! It lets go only once the token's own count says the owner is waiting,
//! because a case that only terminates terminates most easily when the wait
//! is never taken.

use super::TraceToken;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// A token pointer handed to another thread. The pointee is the test
/// thread's thread-local, and the guard that carries this joins the holder
/// before the test thread returns, which is what keeps the pointer valid.
pub(crate) struct Handed(pub(crate) *const TraceToken);

unsafe impl Send for Handed {}

impl Handed {
    /// The pointer, through a method so that a closure captures the wrapper
    /// rather than its field.
    pub(crate) fn token(&self) -> *const TraceToken {
        self.0
    }
}

/// Spin until the token's count of waits passes `before`, or a bound
/// passes — so a case whose owner never waits fails on an assertion rather
/// than hanging.
pub(crate) fn wait_for_a_waiter(token: *const TraceToken, before: usize) {
    let bound = Instant::now() + Duration::from_secs(10);
    while unsafe { (*token).waits() } == before && Instant::now() < bound {
        std::thread::yield_now();
    }
}

/// The calling thread's token, held from another thread — the stand-in for a
/// collector tracing this mutator's graph — until [`release`](Self::release)
/// or the guard's drop. The drop releases the holder and joins it, on the
/// unwind as well as on the return, so a failed assertion never leaves a
/// thread writing into a freed thread-local.
///
/// With `until_waited` the holder lets go on its own once the owner has gone
/// to wait on the token; without it, at `release`.
pub(crate) struct HeldByACollector {
    release: Option<mpsc::Sender<()>>,
    collector: Option<std::thread::JoinHandle<()>>,
}

impl HeldByACollector {
    /// Returns once the collector holds the token.
    pub(crate) fn take(token: *const TraceToken, until_waited: bool) -> Self {
        let handed = Handed(token);
        let (held_sender, held) = mpsc::channel();
        let (release, release_receiver) = mpsc::channel::<()>();
        let collector = std::thread::spawn(move || {
            let token = handed.token();
            let waits_before = unsafe { (*token).waits() };
            assert!(unsafe { (*token).try_take() }, "the owner was not tracing");
            held_sender.send(()).expect("the owner waits for this");
            if until_waited {
                wait_for_a_waiter(token, waits_before);
            } else {
                let _ = release_receiver.recv_timeout(Duration::from_secs(10));
            }

            unsafe { (*token).release() };
        });
        held.recv().expect("the collector took the token");
        Self {
            release: Some(release),
            collector: Some(collector),
        }
    }

    /// Let the holder go, and wait for it.
    pub(crate) fn release(&mut self) {
        drop(self.release.take());
        if let Some(collector) = self.collector.take() {
            collector.join().expect("the collector returned");
        }
    }
}

impl Drop for HeldByACollector {
    fn drop(&mut self) {
        self.release();
    }
}
