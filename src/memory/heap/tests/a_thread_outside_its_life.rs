//! `ll_thread_init` is the one initialisation a thread gets, made once per
//! life; a second call on a started thread is refused, and an entity
//! allocation or a bulk reserve on a thread never started ends the process
//! with a reason naming the entry point.

use super::*;
use crate::test_support::a_child_run_ends_by_abort_saying;

/// The refusal is a `debug_assert`, which this build carries, inside an
/// `extern "C"` function: the panic cannot unwind, so the second call ends
/// the process saying why. A release build answers `true` and touches
/// nothing, which no test in this profile can see.
#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn a_second_init_on_a_started_thread_is_refused() {
    const CHILD: &str = "LL_SECOND_INIT_CHILD";
    if std::env::var_os(CHILD).is_some() {
        std::thread::spawn(|| {
            assert!(ll_thread_init(), "the runtime started this thread");
            ll_thread_init()
        })
        .join()
        .unwrap();
        return;
    }

    a_child_run_ends_by_abort_saying(
        "memory::heap::tests::a_thread_outside_its_life::a_second_init_on_a_started_thread_is_refused",
        CHILD,
        "ll_thread_init is called once per life of a thread, and this thread has started",
    );
}

#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn an_entity_allocation_on_a_thread_never_started_ends_the_process() {
    const CHILD: &str = "LL_ENTITY_ALLOC_NEVER_STARTED_CHILD";
    if std::env::var_os(CHILD).is_some() {
        std::thread::spawn(|| {
            unsafe { entity_alloc(32) };
        })
        .join()
        .unwrap();
        return;
    }

    a_child_run_ends_by_abort_saying(
        "memory::heap::tests::a_thread_outside_its_life::an_entity_allocation_on_a_thread_never_started_ends_the_process",
        CHILD,
        "ll-model: entity_alloc on a thread ll_thread_init never started",
    );
}

#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn a_large_entity_allocation_on_a_thread_never_started_ends_the_process() {
    const CHILD: &str = "LL_LARGE_ENTITY_ALLOC_NEVER_STARTED_CHILD";
    if std::env::var_os(CHILD).is_some() {
        std::thread::spawn(|| {
            unsafe { entity_alloc(MAX_SMALL + 1) };
        })
        .join()
        .unwrap();
        return;
    }

    a_child_run_ends_by_abort_saying(
        "memory::heap::tests::a_thread_outside_its_life::a_large_entity_allocation_on_a_thread_never_started_ends_the_process",
        CHILD,
        "ll-model: entity_alloc on a thread ll_thread_init never started",
    );
}

#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn a_reserve_on_a_thread_never_started_ends_the_process() {
    const CHILD: &str = "LL_ENTITY_RESERVE_NEVER_STARTED_CHILD";
    if std::env::var_os(CHILD).is_some() {
        std::thread::spawn(|| {
            let mut cells = [std::ptr::null_mut::<u8>(); 4];
            let mut contiguous = 0;
            unsafe { ll_entity_reserve(32, cells.len(), cells.as_mut_ptr(), &mut contiguous) };
        })
        .join()
        .unwrap();
        return;
    }

    a_child_run_ends_by_abort_saying(
        "memory::heap::tests::a_thread_outside_its_life::a_reserve_on_a_thread_never_started_ends_the_process",
        CHILD,
        "ll-model: ll_entity_reserve on a thread ll_thread_init never started",
    );
}
