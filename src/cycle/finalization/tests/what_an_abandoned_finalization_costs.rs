//! A finalization is sealed and then released, or the run fails loudly.
//!
//! What `confirm` writes cannot be taken back by this module: the guards come
//! off through the counted release of `PLAN.md` S36.5, which reads a member
//! list neither value here holds. A value dropped with members in it therefore
//! leaves every one of them carrying a reference no release matches, so every
//! later trace reads the component as externally referenced and no collection
//! can propose it again. Both halves of the span are pinned — the window
//! before the seal and the one after it.

use super::*;

/// An unreachable ring of two, confirmed into a fresh finalization, which the
/// caller then loses one way or the other.
///
/// Each case runs this in a child process: the abandonment ends in a panic out
/// of a `Drop`, and what the parent reads is that panic's message.
///
/// # Safety
/// Called on a thread holding the test guard, with the arena alive for the
/// rest of the child's run.
unsafe fn confirmed_ring(arena: &mut Arena, class_name: &str) -> Finalization {
    let node = ClassBuilder::new(class_name).prop("next", true).build();
    let mut context = LLContext { arena: &mut *arena };
    let first = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    let second = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    unsafe {
        store_prop(arena, first, prop_offset(0), second);
        store_prop(arena, second, prop_offset(0), first);
        assert!(!ll_release(first as *mut RcHeader));
        assert!(!ll_release(second as *mut RcHeader));
    }

    let mut finalization = Finalization::begin();
    let mut members = [first as *mut RcHeader, second as *mut RcHeader];
    assert_eq!(
        unsafe { finalization.confirm(&mut members) },
        ValidationResult::Unreachable
    );
    finalization
}

/// Run one case of this file again in a child process and answer what it
/// printed.
///
/// The child's own count is read rather than its status alone: `--exact` with
/// a name the harness cannot match runs nothing and exits zero, which every
/// assertion about a failure would pass over.
fn child_run(case: &str, marker: &str) -> std::process::Output {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(format!(
            "cycle::finalization::tests::what_an_abandoned_finalization_costs::{case}"
        ))
        .arg("--nocapture")
        .env(marker, "1")
        .output()
        .expect("the child runs this test again");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("running 1 test"),
        "the child ran this case rather than none"
    );
    assert!(
        !output.status.success(),
        "an abandoned finalization fails the run it is dropped in — and the \
         process where `panic = \"abort\"` is set, which the test profile is \
         not"
    );
    output
}

#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn a_finalization_dropped_instead_of_sealed_fails() {
    const CHILD: &str = "LL_FINALIZATION_DROPPED_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let _g = test_guard();
        let mut arena = Arena::new();
        let finalization = unsafe { confirmed_ring(&mut arena, "FinalizationAbandonedNode") };
        drop(finalization);
        return;
    }

    let output = child_run("a_finalization_dropped_instead_of_sealed_fails", CHILD);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("dropped instead of sealed"),
        "and it says what was dropped"
    );
}

#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn a_sealed_finalization_dropped_instead_of_released_fails() {
    const CHILD: &str = "LL_FINALIZATION_UNRELEASED_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let _g = test_guard();
        let mut arena = Arena::new();
        let finalization = unsafe { confirmed_ring(&mut arena, "FinalizationUnreleasedNode") };
        let invalidated = finalization.seal();
        assert_eq!(invalidated.members(), 2);
        drop(invalidated);
        return;
    }

    let output = child_run(
        "a_sealed_finalization_dropped_instead_of_released_fails",
        CHILD,
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("dropped instead of released"),
        "the seal moves the obligation rather than discharging it"
    );
}
