//! The acceptance question of `dev/design/debug-modes.md` §9, answered
//! from journal reads and nothing else: which strings died inside this
//! window. What it replaces is a ring of `(thread, address)` written by
//! hand for that one question. It needs the record sites, so it is the
//! `debug-journal` build's alone.

use super::*;

/// The journal's acceptance criterion, `dev/design/debug-modes.md` §9.
///
/// The four strings are created before any of them dies, so that the
/// four addresses are distinct while the window is being marked: a
/// death frees the slot, and the next string born there would answer
/// under the same address. Deaths are then read back per ring, which
/// is the other half of the same care — an address is only a name
/// while its thread is the one that wrote it.
///
/// The trustworthy *none* is the point, so the two rings that matter
/// are checked to have answered with records rather than with
/// `Unknown`: a hunt that concludes "no string died" from a lapped
/// ring has concluded nothing.
#[test]
fn which_strings_died_inside_the_window_is_answered_from_the_journal() {
    use crate::refcount::{EntityKind, MemoryCategory, RcHeader};
    let _sites = kinds::set_sites_for_test(kinds::DEFAULT_KINDS);
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let mut ctx = crate::memory::context::LLContext { arena: &mut arena };

    let make = |ctx: &mut crate::memory::context::LLContext, bytes: &[u8]| unsafe {
        let s = crate::string::ll_string_new(ctx, MemoryCategory::GcHeap, bytes);
        assert!(!s.is_null());
        s
    };

    let kill = |s: *mut crate::string::LLString| unsafe {
        assert!(crate::refcount::ll_release(s as *mut RcHeader));
        crate::object::ll_entity_die(s as *mut RcHeader);
    };

    let early = make(&mut ctx, b"before");
    let inside = make(&mut ctx, b"inside");
    let survivor = make(&mut ctx, b"survives");
    let late = make(&mut ctx, b"after");
    kill(early);

    let start = mark();
    kill(inside);
    let here = this_thread_identity();
    let (there, elsewhere) = std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the runtime started this thread"
        );
        let mut arena = crate::memory::arena::Arena::new();
        let mut ctx = crate::memory::context::LLContext { arena: &mut arena };
        let s =
            unsafe { crate::string::ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"elsewhere") };

        assert!(!s.is_null());
        let identity = this_thread_identity();
        unsafe {
            assert!(crate::refcount::ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }

        (identity, s as u64)
    })
    .join()
    .expect("the second thread panicked");
    let end = mark();
    kill(late);

    let answers = between(&start, &end);
    for identity in [here, there] {
        assert_ne!(identity, 0, "a thread of this test journaled nothing");
        let lapped = answers
            .iter()
            .any(|window| matches!(window, Window::Unknown { thread, .. } if *thread == identity));
        assert!(!lapped, "ring {identity} could not answer for the window");
    }

    let died: Vec<u64> = events(answers)
        .into_iter()
        .filter(|event| event.thread == here || event.thread == there)
        .filter(|event| {
            event.kind == kinds::KIND_ENTITY_DEATH && event.a == EntityKind::String as u64
        })
        .map(|event| event.subject)
        .collect();

    assert!(
        died.contains(&(inside as u64)) && died.contains(&elsewhere),
        "a string that died inside the window is missing from it: {died:x?}"
    );
    assert!(
        !died.contains(&(early as u64)),
        "a string that died before the window is inside it"
    );
    assert!(
        !died.contains(&(survivor as u64)) && !died.contains(&(late as u64)),
        "a string that outlived the window is in it"
    );

    kill(survivor);
}
