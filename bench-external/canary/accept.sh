#!/bin/sh
# Disassembly acceptance for pair_canary. Run after every rebuild,
# before any figure is quoted: a canary whose body the optimizer
# deleted reads as a floor and prices nothing. Exits non-zero on the
# first arm that lost its accepted shape. The probe and its figures:
# dev/BENCHMARKS.md, 2026-08-16, "the pair against its canaries".
#
#   sh bench-external/canary/accept.sh bench-external/canary/pair_canary

set -e
binary="${1:-bench-external/canary/pair_canary}"
disasm=$(mktemp)
body_file=$(mktemp)
trap 'rm -f "$disasm" "$body_file"' EXIT
objdump -d -C "$binary" > "$disasm"

fail() {
    echo "REJECT: $1" >&2
    exit 1
}

# Extract the body of one function into $body_file: from its exact
# header line to the blank line objdump prints after each body. The
# needle ends at ">:", so a "[clone .cold]" header does not reopen the
# range, and an empty extraction is a failure of its own — a negative
# check over nothing would accept an arm nobody looked at.
extract() {
    awk -v needle="::$1(unsigned long)>:" '
        index($0, needle) { inside = 1; next }
        inside && $0 == "" { exit }
        inside { print }
    ' "$disasm" > "$body_file"
    test -s "$body_file" || fail "$1 not found in the disassembly"
}

extract naive_pair_body
grep -q 'addl.*\$0x1,(' "$body_file" || fail "naive arm lost its increment"
grep -q 'subl.*\$0x1,(' "$body_file" || fail "naive arm lost its decrement"
# The death branch specifically: a je into the cold clone. The loop's
# entry guard is also a je, so a bare 'je' test would pass without it.
grep -q 'je.*clone \.cold' "$body_file" \
    || fail "naive arm lost its death branch"

extract skeleton_body
grep -q 'mov' "$body_file" || fail "skeleton lost its pointer load"
if grep -qE 'addl|subl|lock|call' "$body_file"; then
    fail "skeleton grew an operation it must not have"
fi

for arm in ll_pair_body ll_pair_dup_body; do
    extract "$arm"
    grep -q 'call.*<ll_retain>' "$body_file" \
        || fail "$arm lost its ll_retain call"
    grep -q 'call.*<ll_release>' "$body_file" \
        || fail "$arm lost its ll_release call"
done

extract shared_pair_body
grep -q 'lock' "$body_file" || fail "shared_ptr arm lost its atomic operation"

extract plain_store_body
grep -q 'mov.*%r.*,(' "$body_file" || fail "plain store arm lost its store"
if grep -q 'call' "$body_file"; then
    fail "plain store arm grew a call"
fi

extract ll_create_die_body
grep -q 'call.*<ll_reference_new>' "$body_file" \
    || fail "ll_create_die arm lost its factory call"
grep -q 'call.*<ll_release>' "$body_file" \
    || fail "ll_create_die arm lost its release call"

extract malloc_free_body
grep -q 'call.*malloc' "$body_file" || fail "malloc arm lost its malloc"
grep -q 'call.*free' "$body_file" || fail "malloc arm lost its free"

echo "ACCEPTED: all eight arms keep their compiled shape"
