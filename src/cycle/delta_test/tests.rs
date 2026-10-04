//! The wait for the cut-off T on a token alone: answered, it ends at the
//! answer; unanswered, at the bound; recalled, at once.

use super::*;
use crate::cycle::token::TraceToken;

#[test]
fn an_answered_ask_ends_the_wait() {
    let token = TraceToken::new_held();
    token.ask_for_the_checkpoint();
    token.reach_the_checkpoint();
    let from = Instant::now();
    assert!(wait_for_the_checkpoint(&token, from, || false));
    assert!(from.elapsed() < CHECKPOINT_WAIT, "no wait past the answer");
    token.withdraw_the_checkpoint();
    token.reach_the_checkpoint();
    assert!(
        !token.checkpoint_reached(),
        "a withdrawn ask takes no answer"
    );
}

#[test]
fn an_unanswered_ask_ends_at_the_bound() {
    let token = TraceToken::new_held();
    token.ask_for_the_checkpoint();
    let from = Instant::now();
    assert!(!wait_for_the_checkpoint(&token, from, || false));
    assert!(from.elapsed() >= CHECKPOINT_WAIT);
}

#[test]
fn a_recall_ends_the_wait_at_once() {
    let token = TraceToken::new_held();
    token.ask_for_the_checkpoint();
    token.recall_for_test(true);
    let from = Instant::now();
    assert!(!wait_for_the_checkpoint(&token, from, || {
        token.recall_level() == crate::cycle::token::RECALL_STOP
    }));
    assert!(
        from.elapsed() < CHECKPOINT_WAIT,
        "the recall did not wait out the bound"
    );
    token.recall_for_test(false);
}
