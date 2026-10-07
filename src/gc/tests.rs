//! What `ll_gc_kind` answers in each build.

use super::*;

#[test]
#[cfg(feature = "gc-checkpoint")]
fn a_checkpoint_build_reports_the_checkpoint_collector() {
    assert_eq!(GC_KIND, GcKind::Checkpoint);
    assert_eq!(ll_gc_kind(), 1);
}

#[test]
#[cfg(feature = "gc-window")]
fn a_window_build_reports_the_window_collector() {
    assert_eq!(GC_KIND, GcKind::Window);
    assert_eq!(ll_gc_kind(), 2);
}
