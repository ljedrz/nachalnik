#!/bin/bash
# One headless kamchatka session writing tests for missed mutants, in a scratch worktree.
#
#   kill.sh NAME TASKFILE DIFFDIR     mutants_tasks.py writes both
#
# The worktree is $SWEEPS/mtr/wt-NAME with DIFFDIR copied in as `.mutants/`; what the session
# changed is kept as $SWEEPS/mtr/NAME.diff (tests only, and nothing of a mutation, if it did as it
# was asked - check), and the worktree and its target are removed. Needs $SWEEPS/mt.json, whose
# `sandbox-read` has to name the toolchain, the registry and this repository's `.git`: a
# `--sandbox-read` here would replace that list rather than add to it, and cargo would be gone.
set -u
SWEEPS=${SWEEPS:-${TMPDIR:-/tmp}/sweeps}
REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
BIN=${CARGO_TARGET_DIR:-$REPO/target}/release/kamchatka
DEADLINE=${DEADLINE:-3000}
name=$1 tasks=$(realpath "$2") diffs=$(realpath "$3")
out=$SWEEPS/mtr
# the worktree may live elsewhere (WTROOT): a build of the terminal crate is several gigabytes, and
# a temporary directory with a quota is where one first ran out
wt=${WTROOT:-$out}/wt-$name
mkdir -p "$out/tmp"
git -C "$REPO" worktree add --detach "$wt" HEAD > /dev/null 2>&1 || exit 1
cp -r "$diffs" "$wt/.mutants"
cd "$wt"
{
    if [ -f "$SWEEPS/params.txt" ]; then
        grep -v '^[[:space:]]*$' "$SWEEPS/params.txt" | sed 's|^|/params |'
    fi
    grep -v '^[[:space:]]*$' "$tasks"
} | TMPDIR=$out/tmp CARGO_TARGET_DIR=$wt/target GIT_CONFIG_GLOBAL=/dev/null timeout $((DEADLINE + 300)) "$BIN" --headless --config-file "$SWEEPS/mt.json" \
    --deadline "$DEADLINE" > "$out/$name.jsonl" 2> "$out/$name.err"
echo "exit $?" >> "$out/$name.err"
git -C "$wt" add -N . 2> /dev/null
# the crates' sources and tests and nothing else: a session leaves scripts, `.orig` files and
# build trees about, and none of them is what it was asked for
git -C "$wt" diff -- '*/src/*.rs' '*/tests/*' ':!**/target/**' > "$out/$name.diff" 2> /dev/null
# a test a session ran can leave a directory it took its own permissions off - one searchable and
# no more - and `worktree remove` then gives up half way, leaving the directory and a slot taken
chmod -R u+rwx "$wt" 2> /dev/null
git -C "$REPO" worktree remove --force "$wt"
