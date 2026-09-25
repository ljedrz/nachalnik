#!/bin/bash
# One headless kamchatka session doing ordinary work in a scratch worktree, for friction.py.
#
#   friction.sh NAME TASKFILE        each line of TASKFILE is one message
#
# The worktree is $SWEEPS/fr/wt-NAME, made from HEAD and removed afterwards; the session's
# snapshot lands under $SWEEPS/fr/tmp/kamchatka, and the prose in $SWEEPS/fr/NAME.err ends in
# `exit N`. Needs $SWEEPS/fr.json (friction_config.py writes it), the key in the environment and a
# release build.
set -u
SWEEPS=${SWEEPS:-${TMPDIR:-/tmp}/sweeps}
REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
BIN=${CARGO_TARGET_DIR:-$REPO/target}/release/kamchatka
DEADLINE=${DEADLINE:-1500}
name=$1 tasks=$(realpath "$2")
out=$SWEEPS/fr
wt=$out/wt-$name
mkdir -p "$out/tmp"
git -C "$REPO" worktree add --detach "$wt" HEAD > /dev/null 2>&1 || exit 1
cd "$wt"
{
    if [ -f "$SWEEPS/params.txt" ]; then
        grep -v '^[[:space:]]*$' "$SWEEPS/params.txt" | sed 's|^|/params |'
    fi
    grep -v '^[[:space:]]*$' "$tasks"
} | TMPDIR=$out/tmp timeout $((DEADLINE + 300)) "$BIN" --headless --config-file "$SWEEPS/fr.json" \
    --deadline "$DEADLINE" --sandbox-read "$REPO/.git" > "$out/$name.jsonl" 2> "$out/$name.err"
echo "exit $?" >> "$out/$name.err"
git -C "$wt" diff > "$out/$name.diff" 2> /dev/null
git -C "$REPO" worktree remove --force "$wt"
