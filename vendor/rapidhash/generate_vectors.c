/*
 * Generates the test vectors the Rust port is checked against.
 *
 * The author of rapidhash publishes none, so the reference header beside
 * this file is the source of truth and this program is the only thing that
 * reads it: it hashes a fixed input list under fixed seeds and prints
 * `src/hash/vectors.rs`. Running the crate's test suite does not need a C
 * compiler; regenerating the table does.
 *
 *   cc -O2 -o /tmp/generate_vectors vendor/rapidhash/generate_vectors.c
 *   /tmp/generate_vectors > src/hash/vectors.rs
 *
 * The inputs are chosen for the reference's branch boundaries rather than
 * for realism: every length at which its control flow changes, and one on
 * either side of it. The long inputs are not written out byte by byte —
 * they are filled by `filler_byte` below, which `src/hash/vectors.rs`'s
 * consumer reimplements. Change one and the other stops meaning anything,
 * so change both.
 */

#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "rapidhash.h"

/* Content of the generated long inputs, as a function of the offset. Any
 * cheap fixed sequence would do; this one is period-256 and hits every
 * byte value, so a port that drops the high bit of a byte is visible. */
static unsigned char filler_byte(size_t index) {
    return (unsigned char)(index * 167u + 13u);
}

static const uint64_t seeds[] = {
    0x0000000000000000ull,
    0x0000000000000001ull,
    0x0123456789abcdefull,
    0xffffffffffffffffull,
};

/* Human-readable inputs: the cases a reader wants to check by eye. */
static const char *literals[] = {
    "",
    "a",
    "ab",
    "abc",
    "abcd",
    "abcde",
    "abcdefg",
    "abcdefgh",
    "abcdefghi",
    "hello world",
    "limelight",
    "The quick brown fox jumps over the lazy dog",
};

/* Lengths that surround every branch boundary in rapidhash_internal: the
 * 4/8/16-byte short arms, the 112-byte bulk stride and its second lap, and
 * each rung of the 16-byte cascade below it. */
static const size_t filled_lengths[] = {
    0, 1, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33, 47, 48, 49,
    63, 64, 65, 79, 80, 81, 95, 96, 97, 111, 112, 113, 127, 128,
    223, 224, 225, 336, 337, 1000,
};

#define COUNT(a) (sizeof(a) / sizeof((a)[0]))

static void print_byte_literal(const unsigned char *bytes, size_t len) {
    printf("&[");
    for (size_t i = 0; i < len; i++) {
        printf("%s0x%02x", i ? ", " : "", bytes[i]);
    }
    printf("]");
}

int main(void) {
    printf("//! Reference vectors for the rapidhash port. **Generated file — do not edit.**\n");
    printf("//!\n");
    printf("//! Produced by `vendor/rapidhash/generate_vectors.c` from the vendored\n");
    printf("//! reference header, which is what defines the hash. Regenerate rather\n");
    printf("//! than adjust: a hand-edited expectation proves that the port agrees\n");
    printf("//! with whoever edited it.\n");
    printf("\n");

    printf("/// Seeds every input below is hashed under.\n");
    printf("pub(super) const SEEDS: [u64; %zu] = [\n", COUNT(seeds));
    for (size_t s = 0; s < COUNT(seeds); s++) {
        printf("    0x%016llx,\n", (unsigned long long)seeds[s]);
    }
    printf("];\n\n");

    printf("/// Each input, with its hash under every seed in [`SEEDS`], in that order.\n");
    printf("pub(super) const LITERAL_VECTORS: &[(&[u8], [u64; %zu])] = &[\n", COUNT(seeds));
    for (size_t l = 0; l < COUNT(literals); l++) {
        const unsigned char *input = (const unsigned char *)literals[l];
        size_t len = strlen(literals[l]);

        printf("    (");
        print_byte_literal(input, len);
        printf(", [");
        for (size_t s = 0; s < COUNT(seeds); s++) {
            printf("%s0x%016llx", s ? ", " : "",
                   (unsigned long long)rapidhash_withSeed(input, len, seeds[s]));
        }
        printf("]),\n");
    }
    printf("];\n\n");

    printf("/// Each generated length, with its hash under every seed in [`SEEDS`], in\n");
    printf("/// that order. The input's byte at `index` is `index * 167 + 13` truncated\n");
    printf("/// to eight bits, which the consuming test reproduces.\n");
    printf("pub(super) const FILLED_VECTORS: &[(usize, [u64; %zu])] = &[\n", COUNT(seeds));
    {
        size_t longest = 0;
        for (size_t i = 0; i < COUNT(filled_lengths); i++) {
            if (filled_lengths[i] > longest) {
                longest = filled_lengths[i];
            }
        }

        unsigned char *input = malloc(longest ? longest : 1);
        if (!input) {
            return 1;
        }
        for (size_t i = 0; i < longest; i++) {
            input[i] = filler_byte(i);
        }

        for (size_t l = 0; l < COUNT(filled_lengths); l++) {
            size_t len = filled_lengths[l];

            printf("    (%zu, [", len);
            for (size_t s = 0; s < COUNT(seeds); s++) {
                printf("%s0x%016llx", s ? ", " : "",
                       (unsigned long long)rapidhash_withSeed(input, len, seeds[s]));
            }
            printf("]),\n");
        }
        free(input);
    }
    printf("];\n");

    return 0;
}
