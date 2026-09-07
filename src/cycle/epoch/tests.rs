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

/// The counter moves at a closed commit and at nothing else. The assertion is
/// an inequality because the word is process-global: another thread's case may
/// close a commit of its own between the two reads, and what this case owns is
/// only that its own commit was counted.
#[test]
fn a_closed_commit_is_counted() {
    let before = COMMITS.load(Ordering::Relaxed);
    commit_closed();
    assert!(
        COMMITS.load(Ordering::Relaxed) > before,
        "the commit is in the count"
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
