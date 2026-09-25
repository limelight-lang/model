//! The collector thread's life: its birth at the first pressure collection,
//! one elder however often it is asked, a refused base block
//! as a birth retried after the interval, a round that reaches the records
//! beyond the caller's and claims nothing of a record on the free list, a
//! panicking round that hands the word back for the next birth; and its
//! wakes — a wake that finds every count below the threshold makes a round
//! and no batch, a mutator's poll signals at the threshold of its own
//! registrations and not before, and the fallback timer lengthens after
//! empty rounds, holds over work it could not take, and comes back to its
//! minimum on a batch and on a freeing disposition. What
//! a round does for a mutator — the batch over the ring behind its writer —
//! is `the_batch`'s.

use super::*;
use crate::cycle::testing::Sent;
use crate::cycle::worker::testing::{self, ThreadState};
use crate::memory::block_pool::test_guard;

/// This thread's record, which the guard's initialisation drew.
fn record() -> *mut MutatorRecord {
    let record = mutator_record::this_thread_record();
    assert!(
        !record.is_null(),
        "the guard's init drew this thread's record"
    );
    record
}

/// A registered thread the case drives by jobs, each run on that thread
/// with its arena; between jobs it runs `between_jobs`, and it lives until
/// the case drops it.
struct Mutator {
    record: *mut MutatorRecord,
    jobs: std::sync::mpsc::Sender<Box<dyn FnOnce(&mut crate::memory::arena::Arena) + Send>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Mutator {
    /// A mutator that, between jobs, does what a mutator's polls do at its
    /// byte — consent to a request — and clears the `POSTED` a batch leaves,
    /// standing in for the disposition a case that reads batch after batch
    /// with no collection between makes at its end.
    fn start() -> Self {
        Self::start_idling_with(|_| {
            crate::cycle::token::read_and_act_on_this_thread();
            unsafe { &*mutator_record::this_thread_record() }.clear_posted_for_test();
        })
    }

    /// A mutator that polls between jobs as a running one does
    /// (`crate::gc::ll_gc_maybe_collect`), so that a batch's `POSTED` is
    /// collected over; `freed` sums what its polls freed.
    fn start_polling(freed: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
        Self::start_idling_with(move |_| {
            let count = unsafe { crate::gc::ll_gc_maybe_collect() };
            freed.fetch_add(count, std::sync::atomic::Ordering::Relaxed);
        })
    }

    fn start_idling_with(
        between_jobs: impl Fn(&mut crate::memory::arena::Arena) + Send + 'static,
    ) -> Self {
        let (jobs, inbox) =
            std::sync::mpsc::channel::<Box<dyn FnOnce(&mut crate::memory::arena::Arena) + Send>>();
        let (tell, told) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            assert!(
                crate::memory::heap::ll_thread_init(),
                "the pool served the mutator thread"
            );
            tell.send(Sent(mutator_record::this_thread_record()))
                .expect("the case waits");
            let mut arena = crate::memory::arena::Arena::new();
            loop {
                match inbox.recv_timeout(std::time::Duration::from_millis(1)) {
                    Ok(job) => job(&mut arena),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => between_jobs(&mut arena),
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }

            crate::cycle::queue::release_queue_segments();
        });
        let record = told
            .recv()
            .expect("the mutator thread started")
            .into_inner();
        Self {
            record,
            jobs,
            thread: Some(thread),
        }
    }

    /// Run `job` on the mutator's thread and wait for its answer.
    fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut crate::memory::arena::Arena) -> T + Send + 'static,
    ) -> T {
        let (tell, told) = std::sync::mpsc::channel();
        self.send(move |arena| {
            tell.send(Sent(job(arena))).expect("the case waits");
        });
        told.recv().expect("the job ran").into_inner()
    }

    /// Run `job` on the mutator's thread without waiting for it: for a job
    /// that blocks until the case lets it go.
    fn send(&self, job: impl FnOnce(&mut crate::memory::arena::Arena) + Send + 'static) {
        self.jobs
            .send(Box::new(job))
            .expect("the mutator thread runs");
    }
}

impl Drop for Mutator {
    fn drop(&mut self) {
        let (jobs, _) = std::sync::mpsc::channel();
        drop(std::mem::replace(&mut self.jobs, jobs));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Ends the collector thread when the case ends, on a panic as on a return,
/// so that a failed case leaves no thread rounding under the next one.
struct RetireOnDrop;

impl Drop for RetireOnDrop {
    fn drop(&mut self) {
        testing::retire();
    }
}

/// Whether `reached` answered true within `within`, asked every millisecond.
fn wait_until(mut reached: impl FnMut() -> bool, within: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + within;
    loop {
        if reached() {
            return true;
        }

        if std::time::Instant::now() > deadline {
            return false;
        }

        // As a mutator waits: its byte read between two sleeps, a request
        // consented to, `POSTED` armed.
        crate::cycle::token::read_and_act_on_this_thread();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// How long a case waits for a birth or a round: under Miri a round over
/// a few hundred roots takes minutes of wall, so the wait is minutes too.
const A_BIRTH: std::time::Duration =
    std::time::Duration::from_secs(if cfg!(miri) { 900 } else { 10 });

/// Take a standing ring after `interval` while the guard stands, and after
/// the module's own again when it drops — the dial of two case files and of
/// the probe, so that a case which fails between the setter and its own
/// restore leaves no interval behind it
/// (`worker::testing::take_standing_after`).
struct StandingInterval;

impl StandingInterval {
    fn of(interval: std::time::Duration) -> Self {
        testing::take_standing_after(Some(interval));
        Self
    }
}

impl Drop for StandingInterval {
    fn drop(&mut self) {
        testing::take_standing_after(None);
    }
}

/// Birth the elder over `records`, its wait between rounds the timer's own,
/// and wait for its first round.
fn born_over(records: &[*mut MutatorRecord]) {
    testing::confine_rounds_to_records(records);
    testing::wait_between_rounds_for(None);
    testing::permit_births(true);
    let _ = testing::take_rounds();
    let _ = testing::take_round_times();
    let _ = testing::take_outcomes();
    ensure_thread();
    assert!(
        wait_until(|| testing::take_rounds() >= 1, A_BIRTH),
        "the elder was born and made its first round"
    );
}

/// The frame a case about the thread's birth starts from: the elder unborn,
/// this thread's record the only one a round visits, births permitted and
/// the spawn count zeroed. The returned guard ends the thread with the case.
fn ready_for_a_birth() -> RetireOnDrop {
    assert_eq!(testing::thread_state(), ThreadState::Unborn);
    testing::confine_rounds_to(record());
    testing::permit_births(true);
    let end = RetireOnDrop;
    let _ = testing::take_spawns();
    end
}

/// What every refused birth leaves behind: a call inside
/// [`BIRTH_RETRY_INTERVAL`] spawns nothing, and one after it births the
/// thread.
fn the_interval_holds_and_a_call_after_it_births() {
    ensure_thread();
    assert_eq!(
        testing::take_spawns(),
        0,
        "a refused birth is not retried at once"
    );
    std::thread::sleep(BIRTH_RETRY_INTERVAL);
    ensure_thread();
    assert!(
        wait_until(|| testing::thread_state() == ThreadState::Alive, A_BIRTH),
        "a call after the interval birthed one"
    );
    assert_eq!(testing::take_spawns(), 1);
}

/// Birth the elder and make its next round panic, returning once the word
/// reads unborn again: what that leaves is each case's own reading.
fn a_panicking_round_hands_the_word_back() {
    ensure_thread();
    assert!(wait_until(
        || testing::thread_state() == ThreadState::Alive,
        A_BIRTH
    ));
    testing::panic_at_the_next_visit();
    assert!(
        wait_until(|| testing::thread_state() == ThreadState::Unborn, A_BIRTH),
        "the panic unwound out of the thread and the word went back"
    );
}

/// The threshold a case's serve reads at: one entry, the ring of a case
/// holding a few.
const ANY_ENTRY: usize = 1;

/// Empty every lane and P, so that a case starts from a known queue on a
/// harness thread another case used.
fn reset_lanes() {
    crate::cycle::queue::verdicts::discard_standing_verdicts();
    crate::cycle::queue::release_queue_segments();
    crate::memory::critical::drain_for_test();
    crate::gc::disarm();
    // Both spares, so that the first ring's block is no reserve draw, which
    // would arm the poll a case reads as a signal.
    assert!(
        crate::cycle::queue::refill_spares(),
        "the pool served both spares"
    );
}

/// A class of one counted Box property, which [`crate::cycle::testing::ring`]
/// links members through.
fn node_class(name: &str) -> *const crate::class::Class {
    crate::class::ClassBuilder::new(name)
        .prop("next", true)
        .build()
}

#[test]
fn the_first_pressure_collection_births_one_thread_whose_rounds_claim_and_release_the_record() {
    let _g = test_guard();
    let _end = ready_for_a_birth();
    reset_lanes();
    let _ = testing::take_mutators_served();
    testing::serve_rounds_at(ANY_ENTRY);

    // An empty lane: the collection has nothing to do, and births at its
    // end.
    unsafe { crate::cycle::collect::collect_under_pressure() };
    assert!(
        wait_until(|| testing::thread_state() == ThreadState::Alive, A_BIRTH),
        "the first pressure collection birthed the thread"
    );
    unsafe { crate::cycle::collect::collect_under_pressure() };
    assert_eq!(
        testing::thread_state(),
        ThreadState::Alive,
        "a second pressure collection births no second thread"
    );
    assert_eq!(testing::take_spawns(), 1);

    // A round claims only a mutator with work: one garbage ring in R, which
    // the batch takes and the case collects at its end. The count moves
    // after the serve returns, which is after the release.
    let class = node_class("BirthRingNode");
    let mut arena = crate::memory::arena::Arena::new();
    let _ring = unsafe { crate::cycle::testing::ring(&mut arena, [class, class]) };
    assert!(
        wait_until(|| testing::take_mutators_served() >= 1, A_BIRTH),
        "a round claimed and released this thread's record"
    );

    testing::retire();
    assert_eq!(testing::thread_state(), ThreadState::Unborn);
    assert_eq!(
        unsafe { crate::gc::ll_gc_collect_cycles() },
        2,
        "the ring the batch proposed is collected out of P"
    );
    crate::cycle::queue::release_queue_segments();
}

/// The birth asks the global allocator for nothing on the thread that
/// births: the thread is created by the OS entry on a stack the slot keeps,
/// so the pressure path, where the manager has just refused, meets no
/// allocation whose refusal is an abort (`dev/DECISIONS.md`, "the reset
/// window's memory comes from the manager, and an allocation it cannot get
/// is a refusal"). What libc allocates inside `pthread_create` is answered
/// by its error code and is not this reading, which is the crate's
/// `#[global_allocator]` alone.
#[test]
#[cfg_attr(
    miri,
    ignore = "under Miri the birth is the std spawn, whose allocations are the exemption this case reads past"
)]
fn a_birth_asks_the_global_allocator_for_nothing() {
    let _g = test_guard();
    let _end = ready_for_a_birth();

    let _ = crate::test_support::allocation_probe::take_heap_allocations();
    ensure_thread();
    let heap = crate::test_support::allocation_probe::take_heap_allocations();
    assert!(
        wait_until(|| testing::thread_state() == ThreadState::Alive, A_BIRTH),
        "the call birthed the thread"
    );
    assert_eq!(testing::take_spawns(), 1);
    assert_eq!(
        heap, 0,
        "global-allocator calls the birth made on this thread"
    );
}

/// The mapping the slot keeps, read back from the kernel: the guard below
/// the stack is mapped without any access, the stack above it readable and
/// writable, each of its stated size, and the pair adjacent — so a frame
/// past the stack's low end faults rather than writing on.
#[cfg(all(target_os = "linux", not(miri)))]
#[test]
fn the_collectors_stack_stands_on_a_guard_the_kernel_refuses_access_to() {
    let _g = test_guard();
    let record = record();
    testing::confine_rounds_to(record);
    testing::permit_births(true);
    let _end = RetireOnDrop;
    ensure_thread();
    assert!(wait_until(
        || testing::thread_state() == ThreadState::Alive,
        A_BIRTH
    ));

    let base = super::birth::stack_base_of(ELDER);
    assert_ne!(base, 0, "the birth mapped the slot's stack");
    let maps = std::fs::read_to_string("/proc/self/maps").expect("the kernel lists the mappings");
    // `start-end perms ...` per line, hexadecimal.
    let span = |line: &str| -> Option<(usize, usize, String)> {
        let mut fields = line.split_whitespace();
        let (start, end) = fields.next()?.split_once('-')?;
        let perms = fields.next()?.to_owned();
        Some((
            usize::from_str_radix(start, 16).ok()?,
            usize::from_str_radix(end, 16).ok()?,
            perms,
        ))
    };
    let guard = maps
        .lines()
        .filter_map(span)
        .find(|(start, _, _)| *start == base)
        .expect("the guard is a mapping of its own");
    let stack_start = base + super::birth::STACK_GUARD_BYTES;
    let stack = maps
        .lines()
        .filter_map(span)
        .find(|(start, _, _)| *start == stack_start)
        .expect("the stack is a mapping of its own above the guard");

    assert_eq!(
        (guard.1 - guard.0, &guard.2[..3]),
        (super::birth::STACK_GUARD_BYTES, "---"),
        "(size, access) of the guard"
    );
    assert_eq!(
        (stack.1 - stack.0, &stack.2[..3]),
        (super::birth::COLLECTOR_STACK_BYTES, "rw-"),
        "(size, access) of the stack"
    );
}

/// The thread carries its slot's name for the OS, which is what a profiler
/// or a debugger lists it under.
#[cfg(all(target_os = "linux", not(miri)))]
#[test]
fn the_collector_thread_is_named_for_the_os() {
    let _g = test_guard();
    let record = record();
    testing::confine_rounds_to(record);
    testing::permit_births(true);
    let _end = RetireOnDrop;
    ensure_thread();
    assert!(wait_until(
        || testing::thread_state() == ThreadState::Alive,
        A_BIRTH
    ));

    let named = std::fs::read_dir("/proc/self/task")
        .expect("the kernel lists the tasks")
        .filter_map(|task| std::fs::read_to_string(task.ok()?.path().join("comm")).ok())
        .any(|comm| comm.trim_end() == "ll-collector");
    assert!(named, "no task of this process is named ll-collector");
}

/// Birth the thread with its rounds confined to this record and its wait
/// between rounds pinned at `wait`, and wait for its first round to be
/// over: from here a round happens on a wake alone.
fn born_waiting_for(record: *mut MutatorRecord, wait: std::time::Duration) {
    testing::confine_rounds_to(record);
    testing::wait_between_rounds_for(Some(wait));
    testing::permit_births(true);
    let _ = testing::take_spawns();
    let _ = testing::take_rounds();
    ensure_thread();
    assert!(
        wait_until(|| testing::take_rounds() >= 1, A_BIRTH),
        "the thread was born and made its first round"
    );
    let _ = testing::take_mutators_served();
}

/// Wake the thread and wait for the round the wake starts.
fn wake_for_a_round() {
    let _ = testing::take_rounds();
    assert!(wake(ELDER), "the elder received the wake");
    assert!(
        wait_until(|| testing::take_rounds() >= 1, A_BIRTH),
        "the wake started a round"
    );
}

/// Longer than any wait a case makes on the thread, so that a round inside
/// the case is a wake's and never the timer's.
const PAST_THE_CASE: std::time::Duration = std::time::Duration::from_secs(30);

/// The wait a case gives an unsignalled thread to make a round it must not.
const A_ROUNDS_ABSENCE: std::time::Duration = std::time::Duration::from_millis(200);

#[test]
fn a_wake_whose_counts_are_below_the_threshold_makes_a_round_and_no_batch() {
    let _g = test_guard();
    let record = record();
    let _end = RetireOnDrop;
    reset_lanes();
    born_waiting_for(record, PAST_THE_CASE);

    // One ring of two, below the threshold: the wake's round reads the
    // count and takes nothing.
    let class = node_class("BelowThresholdNode");
    let mut arena = crate::memory::arena::Arena::new();
    let _small = unsafe { crate::cycle::testing::ring(&mut arena, [class, class]) };
    wake_for_a_round();
    assert_eq!(
        testing::take_mutators_served(),
        0,
        "no batch below the threshold"
    );
    assert_eq!(crate::cycle::queue::verdicts::verdict_count(), 0);

    // The count decides and not the wake: at the threshold the same wake's
    // round makes a batch over every root.
    let members =
        unsafe { crate::cycle::testing::long_ring(&mut arena, class, SOFT_THRESHOLD - 2) };
    wake_for_a_round();
    assert_eq!(
        testing::take_mutators_served(),
        1,
        "a batch at the threshold"
    );
    assert_eq!(
        crate::cycle::queue::verdicts::verdict_count(),
        SOFT_THRESHOLD,
        "one verdict per root"
    );

    testing::retire();
    assert_eq!(
        unsafe { crate::gc::ll_gc_collect_cycles() },
        SOFT_THRESHOLD,
        "both rings are collected out of P"
    );
    drop(members);
    crate::cycle::queue::release_queue_segments();
}

#[test]
fn a_poll_signals_once_a_registration_filled_a_block_and_not_before() {
    let _g = test_guard();
    let record = record();
    let _end = RetireOnDrop;
    reset_lanes();
    born_waiting_for(record, PAST_THE_CASE);

    // Entries inside the tail block signal nothing: no round through a wait
    // the pinned wait outlasts.
    let class = node_class("SignalNode");
    let mut arena = crate::memory::arena::Arena::new();
    let _small = unsafe { crate::cycle::testing::ring(&mut arena, [class, class]) };
    assert!(!crate::cycle::queue::signal_is_due());
    let _ = testing::take_rounds();
    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0, "unarmed");
    std::thread::sleep(A_ROUNDS_ABSENCE);
    assert_eq!(
        testing::take_rounds(),
        0,
        "an unsignalled thread made no round"
    );

    // The registration that fills the tail block raises the flag; the poll
    // signals, the flag goes down, and the round the signal starts serves
    // the mutator.
    let members = unsafe {
        crate::cycle::testing::long_ring(&mut arena, class, crate::ring::BLOCK_ENTRIES - 1)
    };
    assert!(
        crate::cycle::queue::signal_is_due(),
        "the registration that left the tail block raised the flag"
    );
    assert_eq!(
        unsafe { crate::gc::ll_gc_maybe_collect() },
        0,
        "the poll is a signal and not a fire"
    );
    assert!(
        !crate::cycle::queue::signal_is_due(),
        "the signal was received"
    );
    assert!(
        wait_until(|| testing::take_rounds() >= 1, A_BIRTH),
        "the signal started a round"
    );
    assert!(
        wait_until(|| testing::take_mutators_served() >= 1, A_BIRTH),
        "the round served the mutator"
    );

    testing::retire();
    assert_eq!(
        unsafe { crate::gc::ll_gc_collect_cycles() },
        crate::ring::BLOCK_ENTRIES + 1
    );
    drop(members);
    crate::cycle::queue::release_queue_segments();
}

#[test]
fn a_signal_nobody_received_leaves_the_flag_standing() {
    let _g = test_guard();
    let _record = record();
    assert_eq!(testing::thread_state(), ThreadState::Unborn);
    reset_lanes();

    crate::cycle::queue::make_a_signal_due();
    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0, "unarmed");
    assert!(
        crate::cycle::queue::signal_is_due(),
        "no thread received the signal, so the next poll sends it again"
    );

    // The in-line collection's reading of R consumes what the flag stands
    // for.
    let class = node_class("UnreceivedSignalNode");
    let mut arena = crate::memory::arena::Arena::new();
    let _garbage = unsafe { crate::cycle::testing::ring(&mut arena, [class, class]) };
    assert_eq!(unsafe { crate::gc::ll_gc_collect_cycles() }, 2);
    assert!(!crate::cycle::queue::signal_is_due());
    crate::cycle::queue::release_queue_segments();
}

#[test]
fn the_fallback_timer_lengthens_after_empty_rounds_and_shortens_on_a_freeing_disposition() {
    let _g = test_guard();
    let record = record();
    let _end = RetireOnDrop;
    reset_lanes();
    born_waiting_for(record, PAST_THE_CASE);
    assert_eq!(
        testing::timer_interval(),
        FALLBACK_INTERVAL_MIN * 2,
        "the first round was empty and doubled the minimum"
    );

    // Empty rounds double the interval up to the maximum and no further.
    let mut interval = testing::timer_interval();
    while interval < FALLBACK_INTERVAL_MAX {
        wake_for_a_round();
        let next = testing::timer_interval();
        assert_eq!(next, (interval * 2).min(FALLBACK_INTERVAL_MAX));
        interval = next;
    }
    wake_for_a_round();
    assert_eq!(testing::timer_interval(), FALLBACK_INTERVAL_MAX);

    // A round with a batch returns the interval to the minimum, so that a
    // backlog above the threshold drains at a batch per minimum; the empty
    // round after it doubles again.
    let class = node_class("TimerNode");
    let mut arena = crate::memory::arena::Arena::new();
    let members = unsafe { crate::cycle::testing::long_ring(&mut arena, class, SOFT_THRESHOLD) };
    wake_for_a_round();
    assert_eq!(testing::take_mutators_served(), 1);
    assert_eq!(testing::timer_interval(), FALLBACK_INTERVAL_MIN);
    wake_for_a_round();
    assert_eq!(testing::timer_interval(), FALLBACK_INTERVAL_MIN * 2);

    // A round that reads a mutator at the threshold and cannot serve it — the
    // mutator holding its own token — holds the interval; released, the next
    // round's batch returns it to the minimum.
    let second = unsafe { crate::cycle::testing::long_ring(&mut arena, class, SOFT_THRESHOLD) };
    let holding = crate::cycle::token::HeldToken::take();
    wake_for_a_round();
    assert_eq!(testing::take_mutators_served(), 0, "the token was held");
    assert_eq!(testing::timer_interval(), FALLBACK_INTERVAL_MIN * 2);
    drop(holding);
    wake_for_a_round();
    assert_eq!(testing::take_mutators_served(), 1);
    assert_eq!(testing::timer_interval(), FALLBACK_INTERVAL_MIN);
    wake_for_a_round();
    assert_eq!(testing::timer_interval(), FALLBACK_INTERVAL_MIN * 2);

    // The mutator's poll disposes of the proposal, the collection it fires
    // frees the ring, and the note the poll leaves brings the next round's
    // interval back to its minimum; the round after that is empty again.
    assert_eq!(
        unsafe { crate::gc::ll_gc_maybe_collect() },
        2 * SOFT_THRESHOLD,
        "the poll's collection freed both rings"
    );
    assert!(
        !crate::cycle::queue::signal_is_due(),
        "the fire's reading of R lowered the flag"
    );
    std::thread::sleep(A_ROUNDS_ABSENCE);
    assert_eq!(testing::take_rounds(), 0, "so the poll sent no wake");
    wake_for_a_round();
    assert_eq!(testing::timer_interval(), FALLBACK_INTERVAL_MIN);
    wake_for_a_round();
    assert_eq!(testing::timer_interval(), FALLBACK_INTERVAL_MIN * 2);

    testing::retire();
    drop(members);
    drop(second);
    crate::cycle::queue::release_queue_segments();
}

#[test]
fn a_refused_base_block_is_a_birth_a_later_call_repeats() {
    let _g = test_guard();
    let _end = ready_for_a_birth();

    testing::refuse_the_next_births_base_block();
    ensure_thread();
    assert_ne!(
        testing::thread_state(),
        ThreadState::Alive,
        "a thread whose base block was refused never started"
    );
    assert!(
        wait_until(|| testing::thread_state() == ThreadState::Unborn, A_BIRTH),
        "and the process is without a thread again"
    );
    assert_eq!(testing::take_spawns(), 1);

    the_interval_holds_and_a_call_after_it_births();
}

/// A stack the operating system refuses is a birth that did not happen,
/// on the calling thread and before any thread exists: the slot stays
/// unborn, the refusal holds the interval as a refused base block does, and
/// a call after the interval births.
#[cfg(all(unix, not(miri)))]
#[test]
fn a_refused_stack_is_a_birth_a_later_call_repeats() {
    let _g = test_guard();
    let _end = ready_for_a_birth();

    super::birth::REFUSE_NEXT_STACK.store(true, Ordering::Relaxed);
    ensure_thread();
    assert_eq!(
        testing::thread_state(),
        ThreadState::Unborn,
        "a birth whose stack was refused made no thread"
    );
    assert_eq!(testing::take_spawns(), 0);

    the_interval_holds_and_a_call_after_it_births();
}

/// A create the operating system refuses, after the stack was granted, is
/// a birth that did not happen: the slot stays unborn with its stack kept,
/// the refusal holds the interval, and a call after it births.
#[cfg(all(unix, not(miri)))]
#[test]
fn a_refused_create_is_a_birth_a_later_call_repeats() {
    let _g = test_guard();
    let _end = ready_for_a_birth();

    super::birth::REFUSE_NEXT_CREATE.store(true, Ordering::Relaxed);
    ensure_thread();
    assert_eq!(
        testing::thread_state(),
        ThreadState::Unborn,
        "a birth whose create was refused made no thread"
    );
    assert_ne!(
        super::birth::stack_base_of(ELDER),
        0,
        "the stack granted before the refusal is the slot's to keep"
    );
    assert_eq!(testing::take_spawns(), 0);

    the_interval_holds_and_a_call_after_it_births();
}

/// The word goes unborn after the runtime exit the thread runs itself, on
/// the panic path as on the loop's own: what the thread recorded at its
/// word is at least one exit sequence run, where a word stored before the
/// exit would record none. The join at the next birth then waits on the
/// guard's own pass and glibc's teardown alone.
#[test]
fn the_word_goes_unborn_after_the_threads_own_runtime_exit() {
    let _g = test_guard();
    let record = record();
    testing::confine_rounds_to(record);
    testing::permit_births(true);
    let _end = RetireOnDrop;
    let _ = testing::take_exits_before_the_word();

    a_panicking_round_hands_the_word_back();
    let exits = testing::take_exits_before_the_word();
    assert!(
        (1..usize::MAX).contains(&exits),
        "exit sequences run when the word was stored: {exits} (MAX is a word never recorded)"
    );
}

#[test]
fn a_round_reaches_a_record_beyond_the_callers_and_leaves_a_free_one_alone() {
    let _g = test_guard();
    // A record another thread lived in and gave back, pinned so that no
    // thread of another case takes it while this one reads it.
    let free = crate::cycle::testing::on_a_fresh_thread(|| {
        let record = mutator_record::this_thread_record();
        mutator_record::pin_for_test(record, true);
        Sent(record)
    })
    .into_inner();
    assert!(mutator_record::registry_lists_free(free));
    assert!(
        unsafe { (*free).token.is_held() },
        "held by the exit's claim"
    );

    // The round, driven on this thread: it skips this thread's own record,
    // reaches the free one, and claims nothing of it — the exit's claim
    // stands, and a compare-and-swap from free fails on it.
    testing::confine_rounds_to(free);
    let _ = testing::take_records_visited();
    let _ = testing::take_mutators_served();
    round(ELDER, ANY_ENTRY, &mut Standing::new(ELDER));
    assert!(
        testing::take_records_visited() >= 1,
        "the walk reached past the caller's record"
    );
    assert_eq!(testing::take_mutators_served(), 0);
    assert!(unsafe { (*free).token.is_held() });
    testing::confine_rounds_to(std::ptr::null_mut());
    mutator_record::pin_for_test(free, false);
}

/// The wake is a word the thread's wait takes, not a signal the thread has
/// to be waiting for: one sent before the wait ends the wait at once, and
/// the wait after it, with nothing sent, sleeps its timeout out. Sent to an
/// empty slot here, which answers false — the wake is lost to the poll's
/// reckoning and still stands in the word until a birth clears it.
#[test]
fn a_wake_sent_before_the_wait_ends_it_at_once_and_is_spent_by_it() {
    let _g = test_guard();
    assert_eq!(testing::thread_state(), ThreadState::Unborn);
    testing::stand_in_as_the_elder();

    assert!(!wake(ELDER), "no thread stands to be woken");
    let before = Instant::now();
    wait_for_a_wake(ELDER, Duration::from_secs(2));
    let ended_by_the_wake = before.elapsed();

    let before = Instant::now();
    wait_for_a_wake(ELDER, Duration::from_millis(100));
    let slept_out = before.elapsed();

    assert!(
        ended_by_the_wake < Duration::from_millis(500),
        "the wait slept {ended_by_the_wake:?} past a wake already sent"
    );
    assert!(
        slept_out >= Duration::from_millis(100),
        "the second wait returned after {slept_out:?}: the first left the word standing"
    );
}

/// A wake sent while no thread stands is lost: the birth that follows
/// clears the word before it announces itself, so the new thread makes its
/// first round at birth and none for a wake nobody sent it. A stale word
/// would end its first wait at once — a second round inside a window the
/// pinned wait should leave empty.
#[test]
fn a_wake_sent_to_an_empty_slot_is_not_the_next_births_second_round() {
    let _g = test_guard();
    let record = record();
    let _end = RetireOnDrop;
    reset_lanes();
    assert!(!wake(ELDER), "no thread stands: the wake is lost");

    testing::confine_rounds_to(record);
    testing::wait_between_rounds_for(Some(PAST_THE_CASE));
    testing::permit_births(true);
    let _ = testing::take_spawns();
    let _ = testing::take_rounds();
    ensure_thread();
    assert!(wait_until(
        || testing::thread_state() == ThreadState::Alive,
        A_BIRTH
    ));
    std::thread::sleep(A_ROUNDS_ABSENCE);
    assert_eq!(
        testing::take_rounds(),
        1,
        "rounds since the birth: one is its own, a second is the stale word ending the first wait"
    );
}

#[test]
fn a_round_that_panics_leaves_the_word_unborn_for_the_next_birth() {
    let _g = test_guard();
    let record = record();
    testing::confine_rounds_to(record);
    testing::permit_births(true);
    let _end = RetireOnDrop;
    let _ = testing::take_spawns();

    a_panicking_round_hands_the_word_back();

    // Which is what lets the next call birth again.
    ensure_thread();
    assert!(wait_until(
        || testing::thread_state() == ThreadState::Alive,
        A_BIRTH
    ));
    assert_eq!(testing::take_spawns(), 2);
}

mod the_batch;
mod the_cap_at_zero;
mod the_cap_set_under_work;
mod the_ceiling;
mod the_epoch_clock;
mod the_live_list;
mod the_merged_lane;
mod the_reading_before_the_claim;
mod the_recall;
mod the_rig;
mod the_siblings;
mod the_standing_list;
mod the_take_after_an_interval;
mod under_stress;
mod what_a_grown_k_costs;
mod what_a_take_costs;
