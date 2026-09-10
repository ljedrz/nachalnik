#!/usr/bin/env bash
#
# Measures what a test is worth. Applies a mutation that breaks something on purpose, runs the
# whole suite, and says which tests noticed - splitting them into the ones under measurement and
# everything else, because "was it caught" is the wrong question and "did anything *else* catch
# it" is the right one.
#
#   scripts/mutate.sh <patch> [pattern]
#
#     <patch>    a git-applicable diff that breaks something. `git diff > m.patch` after making
#                the edit by hand is the whole of how one is produced.
#     [pattern]  an extended regex naming the tests being measured, matched against test paths
#                as `cargo test` prints them. Without it, every failure is reported as `other`.
#
# See "a test's worth is measured" in AGENTS.md for what the answers mean, and for the three
# mistakes this exists to catch.

set -uo pipefail

patch=${1:-}
pattern=${2:-$'\x00nothing\x00'}

if [ -z "$patch" ] || [ ! -f "$patch" ]; then
    echo "usage: scripts/mutate.sh <patch> [pattern]" >&2
    exit 2
fi

# a mutation is applied to the working tree and taken back out of it, so anything already in
# there is at risk. Refusing is the only safe answer: the alternative is a script that can eat
# an afternoon's uncommitted work to measure a test
if [ -n "$(git status --porcelain)" ]; then
    echo "the working tree is not clean; commit or stash first" >&2
    exit 2
fi

if ! git apply "$patch"; then
    echo "the patch does not apply - it is probably older than the code it mutates" >&2
    exit 2
fi
trap 'git apply -R "$patch" 2>/dev/null' EXIT

# built first, and separately, because a mutation that does not compile produces no failing tests
# at all - which is indistinguishable from a mutation nothing caught unless somebody looks
if ! cargo build --workspace --all-features >/dev/null 2>&1; then
    echo "DID NOT BUILD - the mutation is not valid Rust, so it measures nothing"
    exit 1
fi

# `--no-fail-fast` because cargo stops after the first failing test *binary*, and the binaries
# after it are exactly where an independent test would have been
failing=$(cargo test --workspace --all-features --no-fail-fast 2>&1 |
    grep -oE '^test [a-zA-Z0-9_:]+ \.\.\. FAILED' |
    sed 's/^test //;s/ \.\.\. FAILED//' |
    sort -u)

if [ -z "$failing" ]; then
    echo "NOTHING CAUGHT IT - either the tests are weaker than they read, or the mutation is a"
    echo "no-op in disguise. Both have happened; neither is visible without checking which."
    exit 1
fi

measured=$(echo "$failing" | grep -E "$pattern")
other=$(echo "$failing" | grep -vE "$pattern")

printf 'under measurement: %s\n' "$(echo "$measured" | grep -c . )"
echo "$measured" | grep . | sed 's/^/  /'
printf 'everything else:   %s\n' "$(echo "$other" | grep -c . )"
echo "$other" | grep . | sed 's/^/  /'

if [ -z "$other" ]; then
    echo
    echo "nothing else caught it: this is coverage the workspace did not have."
else
    echo
    echo "the workspace already caught this. The tests under measurement may still be worth"
    echo "having, but not for this."
fi
