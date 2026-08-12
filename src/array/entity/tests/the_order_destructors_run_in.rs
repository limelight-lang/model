//! Zend's order, which a program observes because a `__destruct`
//! writes a log or closes a handle: depth first, and inside a level
//! the order the entries were inserted in. The drain reverses the
//! segment it is holding rather than the whole list, which is what
//! the mixed shape separates.

use super::*;

/// The order `__destruct` bodies run in when a nested array dies is
/// Zend's: depth first, and inside a level the order the entries were
/// inserted in. The drain has to reproduce it, because a program
/// observes it — a destructor writes a log, closes a handle, or reads
/// another object that is about to die.
///
/// `[[$b], $a]`: `$b` is one level down and first in the entry order,
/// so it goes first. Seen failing as `AB` on the drain's first shape,
/// which released `$a` where it found it and left the nested array
/// for the pop.
#[test]
fn a_nested_destructor_runs_before_a_later_sibling() {
    assert_eq!(destructor_order(Shape::NestedThenObject), "BA");
}

/// Two nested arrays, so the reversal of the held segment is what is
/// under test rather than the interleaving: `[[$b], [$c]]` runs `$b`
/// before `$c`. Seen failing as `CB`, the LIFO order of the pushes.
#[test]
fn nested_siblings_run_their_destructors_in_entry_order() {
    assert_eq!(destructor_order(Shape::TwoNested), "BC");
}

/// The mixed case both of the above are corners of:
/// `[$1, [$2, [$3], $4], $5]` runs `1 2 3 4 5`. It exercises a held
/// segment inside a held segment, which is where a reversal that
/// reversed the whole list rather than the segment would show.
#[test]
fn a_mixed_nesting_runs_its_destructors_in_zend_order() {
    assert_eq!(destructor_order(Shape::Mixed), "12345");
}
