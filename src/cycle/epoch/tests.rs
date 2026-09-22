//! What the commit counter answers, and what a pinned reading does to it.

use super::*;

/// The turnover is a division rather than a wrap of the stamp's two bits: 64
/// commits stand in one epoch, and the 65th opens the next. The last case is
/// the wrap itself — four epochs on, the number the header carries is the one
/// it carried 256 commits ago, which is why a stale stamp is read against the
/// epoch beside it rather than trusted for its age alone.
#[test]
fn an_epoch_spans_sixty_four_commits() {
    assert_eq!(epoch_of(0), 0);
    assert_eq!(epoch_of(COMMITS_PER_EPOCH - 1), 0);
    assert_eq!(epoch_of(COMMITS_PER_EPOCH), 1);
    assert_eq!(epoch_of(COMMITS_PER_EPOCH * 3 + 5), 3);
    assert_eq!(epoch_of(COMMITS_PER_EPOCH * EPOCHS), 0);
}

/// The counter moves at a closed commit by one, and at nothing else a
/// collection does: the word is this thread's record's, so no other case's
/// collection stands between the two readings.
#[test]
fn a_closed_commit_is_counted() {
    let _g = crate::memory::block_pool::test_guard();
    let before = commits();
    commit_closed();
    assert_eq!(
        commits(),
        before + 1,
        "the commit is in this thread's count"
    );
}

/// A pin answers for this thread until it is dropped, and pins nest: a case
/// that stamps in one epoch and then in the next opens the second pin inside
/// the first and reads the first back afterwards.
#[test]
fn a_pin_holds_the_reading_and_gives_it_back() {
    let outer = pin(1);
    assert_eq!(current(), 1);

    {
        let _inner = pin(2);
        assert_eq!(current(), 2);
    }

    assert_eq!(current(), 1, "the inner pin gave the outer one back");
    drop(outer);
    assert_eq!(pinned(), None, "an unpinned thread reads the counter again");
}

/// A commit counts for the thread that closed it and for no other. The rate a
/// thread's stamps age at is its own collecting rate: a thread that collects
/// rarely beside a busy one would otherwise find every stamp of its own stale
/// at its next collection and prune nothing.
#[test]
fn a_neighbours_commits_leave_this_threads_epoch_alone() {
    let _g = crate::memory::block_pool::test_guard();
    let before = current();
    let neighbours = crate::cycle::testing::on_a_fresh_thread(|| {
        let mine = current();
        close_a_turnover_of_commits();
        (mine, current())
    });

    assert_eq!(
        current(),
        before,
        "a turnover closed on another thread moved this one's epoch"
    );
    assert_ne!(
        neighbours.0, neighbours.1,
        "the turnover moved the epoch of the thread that closed it"
    );
}
