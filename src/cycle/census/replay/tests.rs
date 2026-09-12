use super::*;

/// A shape of `blocks` with the segments a live ring's commit holds.
fn shape(blocks: Vec<BlockShape>, worklist: usize, components: usize) -> CollectionShape {
    CollectionShape {
        blocks,
        worklist_segments: worklist,
        component_segments: components,
        drop_segments: 0,
    }
}

/// An entity block of `class_bytes` with `groups_met` of its groups met.
fn slotted(class_bytes: usize, groups_met: u32) -> BlockShape {
    let index_space = (BLOCK_PAYLOAD / class_bytes) as u32;
    BlockShape {
        population: Population::Slotted,
        index_space,
        groups: shadow::group_count(index_space),
        groups_met,
    }
}

/// The bump grants the workspace to its last byte before it draws: fifty
/// class-256 arrays and the worklist's first segment are 56,960 bytes exactly,
/// and a fifty-first array draws one block and leaves a tail of nothing.
#[test]
fn the_bump_fills_the_workspace_to_the_byte_before_it_draws() {
    let fifty = shape(vec![slotted(256, 1); 50], 1, 0);
    let full = flat(&fifty);
    assert_eq!((full.draws, full.remainder), (0, 0));
    assert_eq!(full.granted, WORKSPACE_BUMP_BYTES);

    let fifty_one = shape(vec![slotted(256, 1); 51], 1, 0);
    let over = flat(&fifty_one);
    assert_eq!((over.draws, over.tails, over.tail_bytes), (1, 1, 0));
    assert_eq!(over.remainder, BLOCK_PAYLOAD - 1_056);
}

/// The flat replay reads the record: the figures `dev/BENCHMARKS.md`,
/// 2026-09-12 (S40.3) gives for three loads follow from their shapes.
#[test]
fn the_flat_replay_reproduces_the_record_from_the_shapes_alone() {
    let one_per_block_381 = shape(vec![slotted(256, 1); 381], 3, 2);
    let sparse = flat(&one_per_block_381);
    assert_eq!(
        (sparse.row_bytes_requested, sparse.row_bytes_granted),
        (400_812, 402_336)
    );
    assert_eq!(
        (sparse.draws, sparse.tails, sparse.tail_bytes),
        (6, 6, 4_320)
    );
    assert_eq!(sparse.remainder, 21_184);

    let full_block_at_32 = shape(vec![slotted(32, 255)], 16, 8);
    let full = flat(&full_block_at_32);
    assert_eq!(full.row_bytes_granted, 8_216);
    assert_eq!((full.draws, full.tails, full.tail_bytes), (1, 1, 2_984));
    assert_eq!(full.remainder, 11_200);

    let dense_381_at_256 = shape(vec![slotted(256, 32), slotted(256, 16)], 3, 2);
    let dense = flat(&dense_381_at_256);
    assert_eq!(dense.row_bytes_granted, 2_112);
    assert_eq!((dense.draws, dense.remainder), (0, 34_048));

    let one_per_block_256 = shape(vec![slotted(256, 1); 256], 2, 1);
    let sparse_256 = flat(&one_per_block_256);
    assert_eq!(
        (
            sparse_256.draws,
            sparse_256.tail_bytes,
            sparse_256.remainder
        ),
        (4, 2_592, 32_672)
    );

    let two_edges_one_per_block = shape(vec![slotted(256, 1); 381], 5, 2);
    let two_edges = flat(&two_edges_one_per_block);
    assert_eq!(
        (two_edges.draws, two_edges.tail_bytes, two_edges.remainder),
        (6, 4_320, 12_864)
    );
}

/// What each form writes at first touch, on a class-32 block: the flat form
/// its 24-byte prologue and 32-byte bitmap, the chunked form its 542-byte
/// directory, and both a group of 32 bytes per group met — so the chunked
/// form writes 486 bytes more on the block whatever its groups met.
#[test]
fn the_chunked_form_writes_its_directory_where_the_flat_form_writes_a_bitmap() {
    let full = shape(vec![slotted(32, 255)], 16, 8);
    assert_eq!(flat(&full).first_touch_writes, 24 + 32 + 255 * 32);
    assert_eq!(chunked(&full).first_touch_writes, 542 + 255 * 32);

    let two_groups = shape(vec![slotted(32, 2)], 1, 1);
    assert_eq!(
        chunked(&two_groups).first_touch_writes - flat(&two_groups).first_touch_writes,
        486
    );
}

/// The chunked form's two placements of 32 met rows in 256: a directory of
/// 96 bytes and 32 bytes per met group, 224 against 1,120, where the flat
/// form grants 1,056 for either.
#[test]
fn the_chunked_replay_prices_the_two_placements_by_groups_met() {
    let consecutive = chunked(&shape(vec![slotted(256, 4)], 1, 1));
    assert_eq!(
        (consecutive.row_requests, consecutive.row_bytes_granted),
        (4, 224)
    );
    let one_per_group = chunked(&shape(vec![slotted(256, 32)], 1, 1));
    assert_eq!(
        (one_per_group.row_requests, one_per_group.row_bytes_granted),
        (32, 1_120)
    );
    assert_eq!(
        (consecutive.continuations, one_per_group.continuations),
        (0, 0)
    );
}

/// A block whose groups are still being met when the bump leaves the block
/// its directory stands in takes one continuation, placed with the chunk that
/// found the bump gone: ninety one-group blocks and the first segment leave
/// 960 bytes of the workspace, the directory takes 576 of them, twelve chunks
/// the rest, and the thirteenth draws.
#[test]
fn a_continuation_is_placed_where_the_bump_left_the_directory_behind() {
    let mut blocks = vec![slotted(32, 1); 90];
    blocks.push(slotted(32, 255));
    let replayed = chunked(&shape(blocks, 1, 0));
    assert_eq!(replayed.continuations, 1);
    assert_eq!(
        (replayed.draws, replayed.tails, replayed.tail_bytes),
        (1, 1, 0)
    );
    assert_eq!(replayed.row_requests, 90 + 1 + 12 + 1 + 241);
    assert_eq!(replayed.row_bytes_granted, 91 * 576 + 253 * 32 + 576);
    assert_eq!(replayed.remainder, BLOCK_PAYLOAD - 576 - 241 * 32);
}

/// Both bounds bracket their form's replay on every shape above, and the
/// continuation charged at the most is the one the replay placed.
#[test]
fn the_bound_brackets_the_replay_of_either_form() {
    let mut with_continuation = vec![slotted(32, 1); 90];
    with_continuation.push(slotted(32, 255));
    let shapes = [
        shape(vec![slotted(256, 1); 50], 1, 0),
        shape(vec![slotted(256, 1); 381], 3, 2),
        shape(vec![slotted(32, 255)], 16, 8),
        shape(vec![slotted(256, 32), slotted(256, 16)], 3, 2),
        shape(with_continuation, 1, 0),
    ];
    for shape in &shapes {
        let flat_bound = flat_bound(shape);
        let flat_replay = flat(shape);
        assert!(
            (flat_bound.least..=flat_bound.most).contains(&flat_replay.draws),
            "flat: {flat_bound:?} around {}",
            flat_replay.draws
        );
        let chunked_bound = chunked_bound(shape);
        let chunked_replay = chunked(shape);
        assert!(
            (chunked_bound.least..=chunked_bound.most).contains(&chunked_replay.draws),
            "chunked: {chunked_bound:?} around {}",
            chunked_replay.draws
        );
        assert!(chunked_bound.continuations_at_most >= chunked_replay.continuations);
    }

    let last = chunked_bound(&shapes[4]);
    assert_eq!(
        (last.least, last.most, last.continuations_at_most),
        (1, 1, 1)
    );
}
