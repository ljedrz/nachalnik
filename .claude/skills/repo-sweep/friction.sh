#!/bin/bash
# One headless kamchatka session doing ordinary work in a scratch worktree, for friction.py.
#
#   friction.sh NAME TASKFILE        each line of TASKFILE is one message
#
# The worktree is $SWEEPS/fr/wt-NAME, made from HEAD and removed afterwards; the session's
# snapshot lands under $SWEEPS/fr/tmp/kamchatka, and the prose in $SWEEPS/fr/NAME.err ends in
# `done NAME`. Needs $SWEEPS/fr.json (the snippet in SKILL.md writes it), the key in the environment
# and a release build.
#
# `drive.py` sends a line once the turn before it has been answered, and carries a turn the endpoint
# failed on with `/continue`. A session it gave up on is resumed, up to six times, in the same
# worktree: `/continue` and the task lines it never got.
set -u
SWEEPS=${SWEEPS:-${TMPDIR:-/tmp}/sweeps}
here=$(cd "$(dirname "$0")" && pwd)
REPO=$(git -C "$here" rev-parse --show-toplevel)
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
} > "$out/$name.in"
TMPDIR=$out/tmp DRIVE_OUT=$out DRIVE_RESUMES=6 \
    python3 "$here/drive.py" "$name" "$out/$name.in" "$BIN" \
    --headless --config-file "$SWEEPS/fr.json" --deadline "$DEADLINE" --sandbox-read "$REPO/.git"
echo "exit $?" >> "$out/$name.err"
git -C "$wt" diff > "$out/$name.diff" 2> /dev/null
git -C "$REPO" worktree remove --force "$wt"
echo "done $name" >> "$out/$name.err"
