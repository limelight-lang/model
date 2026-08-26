//! What the collector reads while the mutator rearranges the very
//! chunk it is striding.
//!
//! **`cargo test` cannot judge this either** (`array::head`'s group
//! says the same of the head's placement): the walker's loads are
//! relaxed atomics, the mutator's writes are ordinary, and a run
//! reports nothing whichever way the entries are moved. What decides
//! it is Miri's data-race detector, so the test below is the regression
//! for the in-place slide and its verdict is read from a Miri run rather
//! than from the suite.
//!
//! Gated to `rc-walk`, and on the group rather than on the test: both
//! instruments it needs are that collector's — the relaxed reader and the
//! epoch whose flag parks a freed chunk. rc-trace walks nothing
//! concurrently, so the arrangement cannot be built there at all.
