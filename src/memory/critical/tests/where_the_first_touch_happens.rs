//! Where each thread-local with drop glue is touched for the first
//! time, and why that place is not the release path.
//!
//! **The first touch of such a thread-local can end the process**, and
//! nothing above it can report that. Rust registers the destructor
//! through glibc's `__cxa_thread_atexit_impl`, which `calloc`s 32 bytes
//! per registration and calls `__libc_fatal` on a null — "Fatal glibc
//! error: failed to register TLS destructor: out of memory". It does not
//! return a failure, so the ignored return in
//! `std::sys::thread_local::destructors::register` costs nothing: there
//! is nothing to ignore (`dev/DECISIONS.md`, "what the first touch of a
//! thread-local with drop glue may cost").
//!
//! So the placement carries what the cost cannot: `ll_thread_init` fills
//! both reserves before it builds anything, so every registration this
//! crate makes happens in one call, at a fixed point, before the thread
//! has done any work. The death stays a process death — init cannot
//! answer `false` to it — and what it gains is a place. This module pins
//! that placement: a change that puts a first touch back on the release
//! path fails here.

use super::*;

/// After `ll_thread_init`, both reserves are full — and a full reserve
/// is a reserve whose thread-local has been touched, which is the whole
/// of the claim.
///
/// The pool's thread cache is touched by the same fills, every block
/// they take coming through `BlockPool::get`. The exit guard is touched
/// by the same call, and what proves it is that a thread which only runs
/// `ll_thread_init` still gives its blocks back
/// (`cycle::queue::tests::the_floor_the_escrow_stands_on::`
/// `a_threads_whole_life_gives_every_block_back`).
#[test]
fn thread_init_touches_both_reserves_before_anything_can_release() {
    let _g = test_guard();

    let (critical_held, barrier_held) = std::thread::spawn(|| {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served this thread"
        );
        (blocks_held(), crate::memory::reserve::blocks_held())
    })
    .join()
    .unwrap();

    assert_eq!(
        critical_held, CRITICAL_BLOCKS,
        "the critical reserve is full, so its thread-local is registered"
    );
    assert!(
        barrier_held > 0,
        "and so is the barrier reserve's, which the same call fills"
    );
}

/// The population that keeps a first touch on the release path is the
/// thread the runtime never registered, and what this pins is that the
/// path is taken at all.
///
/// **It does not pin the first touch, and it cannot**: the probe below
/// fills the reserve before the release so that there is a block to
/// spend, and that fill is itself a touch of the same thread-local. The
/// registration is unobservable from inside the process in any case — the
/// failing case has already killed it — so what carries that half is the
/// disassembly recorded in `dev/DECISIONS.md`, "what the first touch of a
/// thread-local with drop glue may cost", and what carries this half is
/// the drawn block.
#[test]
#[cfg_attr(
    feature = "debug-journal",
    ignore = "the journal registers every thread at its first record site"
)]
fn an_unregistered_thread_reaches_the_reserve_from_its_release_path() {
    let _g = test_guard();

    let (before, after) = std::thread::spawn(|| {
        let untouched = blocks_held();
        assert!(replenish(), "the pool fills it for the release to spend");

        let mut header = crate::refcount::RcHeader::new(
            crate::refcount::MemoryCategory::GcHeap,
            crate::refcount::EntityKind::Object.to_flags(),
        );
        unsafe { crate::refcount::ll_retain(&raw mut header) };
        assert!(!unsafe { crate::refcount::ll_release(&raw mut header) });

        let spent = blocks_held();
        crate::cycle::queue::drain();
        drain_for_test();
        (untouched, spent)
    })
    .join()
    .unwrap();

    assert_eq!(before, 0, "nothing had filled the reserve on that thread");
    assert_eq!(
        after,
        CRITICAL_BLOCKS - 1,
        "and the release path spent one, so it reaches this module on a \
         thread the runtime never registered"
    );
}

/// Every `thread_local!` in the crate, by name, against a list.
///
/// **A convention held by a list, and it is written as one**: nothing
/// here reads whether a payload has drop glue, because a Rust test cannot
/// ask that of a type it does not name. What it does is refuse to let a
/// new thread-local appear unnoticed — adding one fails here, and the
/// person adding it reads the rule the failure names.
///
/// The rule: a per-thread structure that thread exit can reach holds a
/// raw pointer in a `Cell` and is freed by hand (`dev/DECISIONS.md`,
/// "thread exit owns the order its per-thread state dies in"). Four
/// declarations are exempt because they exist to have a destructor —
/// `RESERVE`, `CRITICAL`, `THREAD_CACHE`, `EXIT_GUARD` — and each of
/// those registers one on its first touch, at a cost this module's own
/// doc states. A fifth exemption is a decision, not an edit.
#[test]
fn the_crate_declares_these_thread_locals_and_no_others() {
    /// Sorted, and each name paired with whether its payload has drop
    /// glue — which is what decides where its first touch may happen.
    const DECLARED: &[(&str, bool)] = &[
        ("ACTIVE", false),
        ("ADMITTED", false),
        ("ALLOCATING", false),
        ("BLOCKS", false),
        ("CRITICAL", true),
        ("CURRENT_CONTEXT", false),
        ("DIED", false),
        ("DUE", false),
        ("EXIT_GUARD", true),
        ("EXIT_PHASE", false),
        ("HEAP", false),
        ("LATE_CELL", false),
        ("OWNER", false),
        ("PARKED", false),
        ("POOL_REQUESTS", false),
        ("RESERVE", true),
        ("RING", false),
        ("THREAD_BUFFER_ARENA", false),
        ("THREAD_CACHE", true),
        ("THREAD_HEAP", false),
        ("WEAK_TABLE", false),
        ("WINDOW", false),
        ("WRITTEN_BYTES", false),
    ];

    let mut found = Vec::new();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("src is readable") {
            let path = entry.expect("a readable entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }

            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }

            let source = std::fs::read_to_string(&path).expect("a readable file");
            found.extend(thread_local_names(&source));
        }
    }

    found.sort();
    let declared: Vec<String> = DECLARED
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    assert_eq!(
        found, declared,
        "the crate's thread-locals moved; a new one needs a line here and \
         a reading of what its first touch may cost"
    );
}

/// The `static` names declared inside every `thread_local!` block of one
/// source file, in the order they appear.
///
/// A brace count rather than a parse: the block ends at the `}` that
/// balances the `{` after the macro's name, which is enough because a
/// `thread_local!` body holds only declarations. The brace must follow
/// the name immediately, so that the macro named in a comment is read as
/// prose rather than as an invocation.
#[cfg(test)]
fn thread_local_names(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let bytes = source.as_bytes();
    let mut at = 0;
    while let Some(found) = source[at..].find("thread_local!") {
        let mut i = at + found + "thread_local!".len();
        // The brace has to be the next thing, or this was the macro's
        // name inside a comment rather than an invocation of it: scanning
        // on to the next `{` anywhere would harvest an unrelated block.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }

        if i >= bytes.len() || bytes[i] != b'{' {
            at = found + at + "thread_local!".len();
            continue;
        }

        let start = i;
        let mut depth = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }

        names.extend(statics_in(&source[start..i]));
        at = i.max(start + 1);
    }

    names
}

/// The name of every `static NAME:` in one `thread_local!` body.
#[cfg(test)]
fn statics_in(body: &str) -> Vec<String> {
    body.split("static ")
        .skip(1)
        .filter_map(|tail| {
            let name: String = tail
                .chars()
                .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
                .collect();
            let rest = tail[name.len()..].trim_start();
            (!name.is_empty() && rest.starts_with(':')).then_some(name)
        })
        .collect()
}
