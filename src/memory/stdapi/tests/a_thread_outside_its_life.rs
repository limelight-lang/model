//! `ll_alloc` on a thread `ll_thread_init` never started, and on one past its
//! `ll_thread_exit`, ends the process with a reason naming the entry point and
//! the state: no such thread exists for the crate to serve, and a null would
//! be read as an exhaustion. The large path answers the same, reading the
//! base block where the small path reads the heap slot.

use super::*;
use crate::test_support::a_child_run_ends_by_abort_saying;

#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn an_allocation_on_a_thread_never_started_ends_the_process() {
    const CHILD: &str = "LL_ALLOC_NEVER_STARTED_CHILD";
    if std::env::var_os(CHILD).is_some() {
        std::thread::spawn(|| {
            unsafe { ll_alloc(16, 8) };
        })
        .join()
        .unwrap();
        return;
    }

    a_child_run_ends_by_abort_saying(
        "memory::stdapi::tests::a_thread_outside_its_life::an_allocation_on_a_thread_never_started_ends_the_process",
        CHILD,
        "ll-model: ll_alloc on a thread ll_thread_init never started",
    );
}

#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn a_large_allocation_on_a_thread_never_started_ends_the_process() {
    const CHILD: &str = "LL_ALLOC_LARGE_NEVER_STARTED_CHILD";
    if std::env::var_os(CHILD).is_some() {
        std::thread::spawn(|| {
            unsafe { ll_alloc(MAX_SMALL + 1, 8) };
        })
        .join()
        .unwrap();
        return;
    }

    a_child_run_ends_by_abort_saying(
        "memory::stdapi::tests::a_thread_outside_its_life::a_large_allocation_on_a_thread_never_started_ends_the_process",
        CHILD,
        "ll-model: ll_alloc on a thread ll_thread_init never started",
    );
}

#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn an_allocation_past_the_threads_exit_ends_the_process() {
    const CHILD: &str = "LL_ALLOC_PAST_EXIT_CHILD";
    if std::env::var_os(CHILD).is_some() {
        std::thread::spawn(|| {
            assert!(
                crate::memory::heap::ll_thread_init(),
                "the runtime started this thread"
            );
            crate::memory::heap::ll_thread_exit();
            unsafe { ll_alloc(16, 8) };
        })
        .join()
        .unwrap();
        return;
    }

    a_child_run_ends_by_abort_saying(
        "memory::stdapi::tests::a_thread_outside_its_life::an_allocation_past_the_threads_exit_ends_the_process",
        CHILD,
        "ll-model: ll_alloc on a thread past its ll_thread_exit",
    );
}
