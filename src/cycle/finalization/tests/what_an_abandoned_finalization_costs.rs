//! A finalization runs to the end of its chain, or the run fails loudly.
//!
//! What `confirm` writes cannot be taken back by the values that wrote it: the
//! guards come off through a counted release over a member list, and none of
//! these values holds one. A value dropped with members in it
//! therefore leaves every one of them carrying a reference no release matches,
//! so every later trace reads the component as externally referenced and no
//! collection can propose it again. Every window of the chain is pinned — the
//! one before the seal, the three the values after it open, and the two counts
//! a close matches against the members the confirm guarded.

use super::*;

/// An unreachable ring of two, confirmed into a fresh finalization, which the
/// caller then loses at one stage of the chain or another, and the two members
/// in the order the confirm left them.
///
/// Each case runs this in a child process: the abandonment ends in a panic out
/// of a `Drop` or out of a close's own count, and what the parent reads is
/// that panic's message.
///
/// # Safety
/// Called on a thread holding the test guard, with the arena alive for the
/// rest of the child's run.
unsafe fn confirmed_ring(
    arena: &mut Arena,
    class_name: &str,
) -> (Finalization, [*mut RcHeader; 2]) {
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
    (finalization, members)
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
        let (finalization, _) = unsafe { confirmed_ring(&mut arena, "FinalizationAbandonedNode") };
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
fn a_sealed_finalization_dropped_instead_of_run_fails() {
    const CHILD: &str = "LL_FINALIZATION_UNRUN_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let _g = test_guard();
        let mut arena = Arena::new();
        let (finalization, _) = unsafe { confirmed_ring(&mut arena, "FinalizationUnrunNode") };
        let invalidated = finalization.seal();
        assert_eq!(invalidated.members(), 2);
        drop(invalidated);
        return;
    }

    let output = child_run("a_sealed_finalization_dropped_instead_of_run_fails", CHILD);
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("dropped instead of running its destructors"),
        "the seal moves the obligation rather than discharging it"
    );
}

#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn a_destructor_pass_dropped_instead_of_closed_fails() {
    const CHILD: &str = "LL_FINALIZATION_UNCLOSED_PASS_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let _g = test_guard();
        let mut arena = Arena::new();
        let (finalization, _) =
            unsafe { confirmed_ring(&mut arena, "FinalizationUnclosedPassNode") };
        let pass = finalization.seal().destructors();
        drop(pass);
        return;
    }

    let output = child_run("a_destructor_pass_dropped_instead_of_closed_fails", CHILD);
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("a destructor pass holding guarded members"),
        "the pass carries the guards while user code runs, and abandoning it strands them"
    );
}

#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn a_pass_closed_over_fewer_members_than_it_guarded_fails() {
    const CHILD: &str = "LL_FINALIZATION_SHORT_PASS_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let _g = test_guard();
        let mut arena = Arena::new();
        let (finalization, _) = unsafe { confirmed_ring(&mut arena, "FinalizationShortPassNode") };
        // The component never reaches `run`, which is what a commit missing
        // one of its components leaves behind.
        let _ = finalization.seal().destructors().close();
        return;
    }

    let output = child_run(
        "a_pass_closed_over_fewer_members_than_it_guarded_fails",
        CHILD,
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("every guarded member runs its destructor"),
        "a component left out reaches the sever with its destructor unrun"
    );
}

#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn a_revalidation_dropped_instead_of_closed_fails() {
    const CHILD: &str = "LL_FINALIZATION_UNCLOSED_REVALIDATION_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let _g = test_guard();
        let mut arena = Arena::new();
        let (finalization, members) =
            unsafe { confirmed_ring(&mut arena, "FinalizationUnclosedRevalidationNode") };
        let mut pass = finalization.seal().destructors();
        unsafe { pass.run(&members) };
        drop(pass.close());
        return;
    }

    let output = child_run("a_revalidation_dropped_instead_of_closed_fails", CHILD);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("a revalidation holding guarded members"),
        "a component never read again keeps the guards the confirm wrote"
    );
}

#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn a_component_read_as_unreachable_and_dropped_with_its_guards_fails() {
    const CHILD: &str = "LL_FINALIZATION_UNRELEASED_COMPONENT_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let _g = test_guard();
        let mut arena = Arena::new();
        let (finalization, mut members) =
            unsafe { confirmed_ring(&mut arena, "FinalizationUnreleasedComponentNode") };
        let mut pass = finalization.seal().destructors();
        unsafe { pass.run(&members) };
        let mut revalidation = pass.close();
        drop(unsafe { revalidation.revalidate(&mut members) });
        return;
    }

    let output = child_run(
        "a_component_read_as_unreachable_and_dropped_with_its_guards_fails",
        CHILD,
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("dropped with its guards on"),
        "the answer carries the guards until the sever states they came off"
    );
}
