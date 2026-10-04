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
# A turn lost to the endpoint (a free model's upstream 429s) is carried on, up to six times, in the
# same worktree: the saved session resumed with a line saying so and the task lines it never got.
# Headless goes on reading lines after a failed turn, so most often every line has arrived and the
# carry-on is all a resume sends.
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

params() {
    if [ -f "$SWEEPS/params.txt" ]; then
        grep -v '^[[:space:]]*$' "$SWEEPS/params.txt" | sed 's|^|/params |'
    fi
}
run() {
    TMPDIR=$out/tmp timeout $((DEADLINE + 300)) "$BIN" --headless --config-file "$SWEEPS/fr.json" \
        --deadline "$DEADLINE" --sandbox-read "$REPO/.git" "$@" >> "$out/$name.jsonl" 2>> "$out/$name.err"
    echo "exit $?" >> "$out/$name.err"
}

{ params; grep -v '^[[:space:]]*$' "$tasks"; } | run
for round in 1 2 3 4 5 6; do
    [ "$(grep '^exit' "$out/$name.err" | tail -1)" = "exit 1" ] || break
    snap=$(grep -o 'kamchatka -r [^`]*' "$out/$name.err" | tail -1 | cut -d' ' -f3)
    [ -n "$snap" ] && [ -f "$snap" ] || break
    sent=$(python3 -c "import json, sys; print(sum(i['kind'].get('kind') == 'user_message' \
        for i in json.load(open(sys.argv[1]))['items']))" "$snap")
    sleep $((60 * round))
    {
        params
        echo "Your last turn failed on a rate limit upstream. Carry on with what you were last" \
            "asked, from where you left off."
        grep -v '^[[:space:]]*$' "$tasks" | tail -n +$((sent + 1))
    } | run -r "$snap"
done
git -C "$wt" diff > "$out/$name.diff" 2> /dev/null
git -C "$REPO" worktree remove --force "$wt"
echo "done $name" >> "$out/$name.err"
