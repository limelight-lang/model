//! The five conditions a non-zero decrement must satisfy to register a
//! cycle candidate, each shown rejecting on its own.
//!
//! **A scenario test cannot show this.** It observes one outcome — the
//! entity was registered or it was not — so a clause that never fires
//! reads exactly like a clause that fired and agreed. The counter past
//! the gate separates them: a clause is proved live by removing it from
//! an admitting word and watching the admission go.

use super::*;

/// A release that leaves a non-zero count, and the number of admissions
/// it produced. The count starts at 2 because the gate sits on the
/// non-zero decrement: reaching zero is a death, and a dying entity is
/// nobody's candidate.
fn admissions_for(flags: u32) -> usize {
    crate::refcount::take_admissions();
    let mut header = RcHeader { refcount: 2, flags };
    release(&mut header);
    assert_eq!(
        header.refcount, 1,
        "the decrement has to survive for the gate to be reached"
    );

    crate::refcount::take_admissions()
}

#[test]
fn each_clause_of_the_gate_rejects_on_its_own() {
    let admitting = EntityKind::Object.to_flags();
    assert_eq!(
        admissions_for(admitting),
        1,
        "a heap object losing one of two holders is the candidate case"
    );

    // Each entry removes one clause from the admitting word above, so a
    // clause that stopped being tested shows up as an admission here and
    // nowhere else.
    let rejecting = [
        (
            "a non-zero category, kept counted by COW so it reaches the gate",
            MemoryCategory::RequestArena as u32 | COW,
        ),
        (
            "a kind no ring can close through",
            EntityKind::String.to_flags(),
        ),
        ("a class proven acyclic", ACYCLIC_GATE),
        ("a proven owner", OWNERSHIP_MARK),
        ("an entity a queue entry already names", CANDIDATE_BIT),
    ];

    for (clause, extra) in rejecting {
        assert_eq!(
            admissions_for(admitting | extra),
            0,
            "{clause} must be refused by the gate"
        );
    }
}

/// The gate is one mask over five clauses rather than five tests, and
/// what makes that legitimate is that each clause is "this bit is zero".
/// A clause whose bit moved out of the mask would still be readable in
/// the constant, so the mask is checked against the clauses it claims.
#[test]
fn the_mask_covers_every_clause_and_nothing_else() {
    for (clause, bits) in [
        ("the memory category", MEMORY_CATEGORY_MASK),
        (
            "the kinds no ring closes through",
            KIND_ABOVE_THE_RING_RESERVE,
        ),
        ("the acyclic gate", ACYCLIC_GATE),
        ("the ownership mark", OWNERSHIP_MARK),
        ("the candidate bit", CANDIDATE_BIT),
    ] {
        assert_eq!(
            CANDIDATE_GATE_MASK & bits,
            bits,
            "{clause} is a clause of the gate and has to be in its mask"
        );
    }

    let clauses = MEMORY_CATEGORY_MASK
        | KIND_ABOVE_THE_RING_RESERVE
        | ACYCLIC_GATE
        | OWNERSHIP_MARK
        | CANDIDATE_BIT;
    assert_eq!(
        CANDIDATE_GATE_MASK & !clauses,
        0,
        "the mask claims a bit no clause named, so it refuses candidates \
         for a reason the design does not have"
    );
}

/// A decrement that reaches zero is a death, and the ordinary teardown
/// takes it. Registering it would put a queue entry on a slot the
/// allocator is about to hand out again.
#[test]
fn a_decrement_to_zero_registers_nothing() {
    crate::refcount::take_admissions();
    let mut header = RcHeader {
        refcount: 1,
        flags: EntityKind::Object.to_flags(),
    };
    assert!(release(&mut header), "the entity died");
    assert_eq!(
        crate::refcount::take_admissions(),
        0,
        "a death is not a candidate"
    );
}
